//! Disk-backed features. Only the fitting subset lives in memory.
use crate::{Error, Result, MAX_SPIKES};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Data {
    pub values: Vec<f64>,
    pub dims: usize,
    pub points: usize,
}

impl Data {
    pub fn row(&self, index: usize) -> &[f64] {
        &self.values[index * self.dims..(index + 1) * self.dims]
    }

    pub fn subset(&self, indices: &[usize]) -> Self {
        let mut values = Vec::with_capacity(indices.len() * self.dims);
        for &index in indices {
            values.extend_from_slice(self.row(index));
        }
        Self {
            values,
            dims: self.dims,
            points: indices.len(),
        }
    }
}

pub struct FeatureStore {
    path: PathBuf,
    columns: usize,
    active: Vec<usize>,
    minimum: Vec<f64>,
    range: Vec<f64>,
    pub points: usize,
}

impl FeatureStore {
    /// Parse using f32, as the original KlustaKwik feature reader did, then
    /// normalize using the full recording's bounds, not the fitting subset's.
    pub fn create(features: &Path, cache: &Path) -> Result<Self> {
        let mut input = BufReader::with_capacity(1 << 20, File::open(features)?);
        let mut line = String::new();
        input.read_line(&mut line)?;
        let columns: usize = line
            .trim()
            .parse()
            .map_err(|_| Error::Invalid("Invalid feature count"))?;
        if !(1..=64).contains(&columns) {
            return Err(Error::Invalid(
                "Classic tetrode clustering supports 1–64 feature columns",
            ));
        }
        let mut output = BufWriter::with_capacity(1 << 20, File::create(cache)?);
        let mut minimum = vec![f64::INFINITY; columns];
        let mut maximum = vec![f64::NEG_INFINITY; columns];
        let mut points = 0;
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                break;
            }
            let mut count = 0;
            for word in line.split_whitespace() {
                if count == columns {
                    return Err(Error::Invalid("Too many columns in feature row"));
                }
                let value: f32 = word
                    .parse()
                    .map_err(|_| Error::Invalid("Invalid feature value"))?;
                if !value.is_finite() {
                    return Err(Error::Invalid("Feature values must be finite"));
                }
                minimum[count] = minimum[count].min(f64::from(value));
                maximum[count] = maximum[count].max(f64::from(value));
                output.write_all(&value.to_le_bytes())?;
                count += 1;
            }
            if count != columns {
                return Err(Error::Invalid("Incorrect number of columns in feature row"));
            }
            points += 1;
            if points as u64 > MAX_SPIKES {
                return Err(Error::Invalid("Too many feature rows"));
            }
        }
        if points < 2 {
            return Err(Error::Invalid("At least two feature rows are required"));
        }
        output.flush()?;
        let range: Vec<_> = maximum
            .iter()
            .zip(&minimum)
            .map(|(hi, lo)| hi - lo)
            .collect();
        // Constant dimensions contain no clustering information. Omitting them
        // also avoids the original reader's division by zero on such features.
        let active = (0..columns).filter(|&i| range[i] > 0.0).collect();
        Ok(Self {
            path: cache.to_owned(),
            columns,
            active,
            minimum,
            range,
            points,
        })
    }

    pub fn dims(&self) -> usize {
        self.active.len()
    }

    fn normalize(&self, raw: &[u8], row: &mut Vec<f64>) {
        row.clear();
        for &i in &self.active {
            let start = 4 * i;
            let value = f64::from(f32::from_le_bytes(
                raw[start..start + 4].try_into().unwrap(),
            ));
            row.push((value - self.minimum[i]) / self.range[i]);
        }
    }

    pub fn load_indices(&self, indices: &[usize]) -> Result<Data> {
        let mut input = BufReader::with_capacity(1 << 20, File::open(&self.path)?);
        let mut raw = vec![0; self.columns * 4];
        let mut row = Vec::with_capacity(self.dims());
        let mut values = Vec::with_capacity(indices.len() * self.dims());
        let mut position = 0_u64;
        for &index in indices {
            if index >= self.points {
                return Err(Error::Invalid("Fitting index outside recording"));
            }
            let requested = (index * raw.len()) as u64;
            // Keep nearby selected rows in the existing read buffer. seek()
            // discards it even when the requested row is already buffered.
            input.seek_relative(requested as i64 - position as i64)?;
            input.read_exact(&mut raw)?;
            position = requested + raw.len() as u64;
            self.normalize(&raw, &mut row);
            values.extend_from_slice(&row);
        }
        Ok(Data {
            values,
            dims: self.dims(),
            points: indices.len(),
        })
    }

    pub fn for_each(&self, mut visit: impl FnMut(&[f64]) -> Result<()>) -> Result<()> {
        let mut input = BufReader::with_capacity(1 << 20, File::open(&self.path)?);
        let row_bytes = self.columns * 4;
        let mut raw = vec![0; row_bytes * 4096];
        let mut row = Vec::with_capacity(self.dims());
        let mut left = self.points;
        while left > 0 {
            let count = left.min(4096);
            input.read_exact(&mut raw[..count * row_bytes])?;
            for record in raw[..count * row_bytes].chunks_exact(row_bytes) {
                self.normalize(record, &mut row);
                visit(&row)?;
            }
            left -= count;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_rows_cross_buffers_and_allow_repeated_or_backward_indices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.fet.1");
        let points = 140_000;
        let mut writer = BufWriter::new(File::create(&path).unwrap());
        writeln!(writer, "2").unwrap();
        for i in 0..points {
            writeln!(writer, "{i} {}", i * 2).unwrap();
        }
        writer.flush().unwrap();
        let store = FeatureStore::create(&path, &dir.path().join("cache")).unwrap();
        let indices = [0, 1, 131_071, 131_072, 139_999, 17, 17, 131_072];
        let data = store.load_indices(&indices).unwrap();
        for (row, index) in indices.into_iter().enumerate() {
            let expected = index as f64 / (points - 1) as f64;
            assert_eq!(data.row(row), &[expected, expected]);
        }
        assert!(store.load_indices(&[points]).is_err());
    }

    #[test]
    fn normalization_uses_full_bounds_and_removes_constant_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.fet.1");
        std::fs::write(&path, "3\n-2 5 3\n0 5 4\n6 5 7\n").unwrap();
        let store = FeatureStore::create(&path, &dir.path().join("cache")).unwrap();
        let selected = store.load_indices(&[1]).unwrap();
        assert_eq!(selected.dims, 2);
        assert_eq!(selected.row(0), &[0.25, 0.25]);
        let mut rows = Vec::new();
        store
            .for_each(|row| {
                rows.push(row.to_vec());
                Ok(())
            })
            .unwrap();
        assert_eq!(rows, [vec![0., 0.], vec![0.25, 0.25], vec![1., 1.]]);
    }
}
