//! Optional binary snapshots of intermediate data, written as NumPy `.npy` files.
//!
//! Each file is a short text header (dtype and shape) followed by raw
//! little-endian values, so it can be read with `numpy.load`, or by skipping
//! the header and reading the raw array in any other language.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

/// Points in the pipeline at which data can be written out.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Point {
    /// Per-channel PCA: active channel numbers, mean waveforms and components
    Pca,
    /// Every spike's features as clustered (f32), plus normalization bounds
    Features,
    /// Each trial's fitting subset (spike indices)
    Subsets,
    /// Each trial's fitted mixture: weights, means and covariances
    Models,
    /// Each trial's label for every spike, using the model's component IDs
    Labels,
}

pub(crate) trait Element: Copy {
    const DESCR: &'static str;
    fn write_le(self, out: &mut impl Write) -> std::io::Result<()>;
}
macro_rules! element {
    ($type:ty, $descr:literal) => {
        impl Element for $type {
            const DESCR: &'static str = $descr;
            fn write_le(self, out: &mut impl Write) -> std::io::Result<()> {
                out.write_all(&self.to_le_bytes())
            }
        }
    };
}
element!(u8, "|u1");
element!(u32, "<u4");
element!(f32, "<f4");
element!(f64, "<f8");

/// Streams exactly `product(shape)` values into an `.npy` file.
pub(crate) struct NpyWriter<T> {
    out: BufWriter<File>,
    remaining: usize,
    element: PhantomData<T>,
}
impl<T: Element> NpyWriter<T> {
    pub(crate) fn create(path: &Path, shape: &[usize]) -> Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        let dims = match shape {
            [n] => format!("{n},"), // Python's one-element tuple
            _ => shape
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        };
        let mut header = format!(
            "{{'descr': '{}', 'fortran_order': False, 'shape': ({dims}), }}",
            T::DESCR
        );
        // Magic (6) + version (2) + length (2) + header + newline, padded to 64 bytes.
        let unpadded = 10 + header.len() + 1;
        header.extend(std::iter::repeat_n(
            ' ',
            unpadded.next_multiple_of(64) - unpadded,
        ));
        header.push('\n');
        out.write_all(b"\x93NUMPY\x01\x00")?;
        out.write_all(&(header.len() as u16).to_le_bytes())?;
        out.write_all(header.as_bytes())?;
        Ok(Self {
            out,
            remaining: shape.iter().product(),
            element: PhantomData,
        })
    }

    pub(crate) fn extend(&mut self, values: impl IntoIterator<Item = T>) -> Result<()> {
        for value in values {
            if self.remaining == 0 {
                return Err(Error::Invalid("Dump array received too many values"));
            }
            value.write_le(&mut self.out)?;
            self.remaining -= 1;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<()> {
        if self.remaining != 0 {
            return Err(Error::Invalid("Dump array received too few values"));
        }
        self.out.flush()?;
        Ok(())
    }
}

/// The requested dump points and where to write them. Inactive by default.
#[derive(Default)]
pub(crate) struct Dump {
    dir: PathBuf,
    points: Vec<Point>,
}
impl Dump {
    pub(crate) fn new(dir: PathBuf, points: &[Point]) -> Result<Self> {
        if !points.is_empty() {
            fs::create_dir_all(&dir)?;
        }
        Ok(Self {
            dir,
            points: points.to_vec(),
        })
    }

    pub(crate) fn wants(&self, point: Point) -> bool {
        self.points.contains(&point)
    }

    /// Open a streaming array if `point` was requested.
    pub(crate) fn stream<T: Element>(
        &self,
        point: Point,
        name: &str,
        shape: &[usize],
    ) -> Result<Option<NpyWriter<T>>> {
        if !self.wants(point) {
            return Ok(None);
        }
        NpyWriter::create(&self.dir.join(format!("{name}.npy")), shape).map(Some)
    }

    /// Write a whole array if `point` was requested.
    pub(crate) fn array<T: Element>(
        &self,
        point: Point,
        name: &str,
        shape: &[usize],
        values: impl IntoIterator<Item = T>,
    ) -> Result<()> {
        if let Some(mut writer) = self.stream(point, name, shape)? {
            writer.extend(values)?;
            writer.finish()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npy_header_is_aligned_and_describes_the_array() {
        let dir = tempfile::tempdir().unwrap();
        for (shape, expected) in [
            (vec![3], "(3,)"),
            (vec![2, 3], "(2, 3)"),
            (vec![0, 4], "(0, 4)"),
        ] {
            let path = dir.path().join("a.npy");
            let mut writer = NpyWriter::<f32>::create(&path, &shape).unwrap();
            writer
                .extend((0..shape.iter().product::<usize>()).map(|i| i as f32))
                .unwrap();
            writer.finish().unwrap();
            let bytes = fs::read(&path).unwrap();
            let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
            assert_eq!((10 + header_len) % 64, 0);
            let header = std::str::from_utf8(&bytes[10..10 + header_len]).unwrap();
            assert!(header.contains("'descr': '<f4'"), "{header}");
            assert!(header.contains(&format!("'shape': {expected}")), "{header}");
            assert!(header.ends_with('\n'));
            assert_eq!(
                bytes.len() - 10 - header_len,
                4 * shape.iter().product::<usize>()
            );
        }
    }

    #[test]
    fn writer_rejects_the_wrong_number_of_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = NpyWriter::<u8>::create(&dir.path().join("a.npy"), &[2]).unwrap();
        assert!(writer.extend([1, 2, 3]).is_err());
        let writer = NpyWriter::<u8>::create(&dir.path().join("b.npy"), &[2]).unwrap();
        assert!(writer.finish().is_err());
        let inactive = Dump::default();
        inactive.array(Point::Pca, "x", &[1], [1_u8]).unwrap();
        assert!(!dir.path().join("x.npy").exists());
    }
}
