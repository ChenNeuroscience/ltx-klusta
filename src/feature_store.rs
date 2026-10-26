//! Full-record f32 features, held in bounded RAM or streamed from a binary cache.
use crate::{Error, Result, MAX_SPIKES};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub(crate) const FEATURE_MEMORY_LIMIT: usize = 64 * 1024 * 1024;

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

enum Storage {
    Memory(Vec<f32>),
    Disk(PathBuf),
}
enum Pending {
    Memory(Vec<f32>),
    Disk(PathBuf, BufWriter<File>),
}

pub(crate) struct FeatureBuilder {
    storage: Pending,
    columns: usize,
    expected: usize,
    written: usize,
    minimum: Vec<f64>,
    maximum: Vec<f64>,
}
impl FeatureBuilder {
    pub(crate) fn new(
        columns: usize,
        points: usize,
        cache: &Path,
        memory_limit: usize,
    ) -> Result<Self> {
        if !(1..=64).contains(&columns) {
            return Err(Error::Invalid(
                "Classic tetrode clustering supports 1–64 feature columns",
            ));
        }
        if points < 2 || points as u64 > MAX_SPIKES {
            return Err(Error::Invalid("Invalid feature row count"));
        }
        let values = points
            .checked_mul(columns)
            .ok_or(Error::Invalid("Feature storage size overflow"))?;
        let bytes = values
            .checked_mul(4)
            .ok_or(Error::Invalid("Feature storage size overflow"))?;
        let mut memory = Vec::new();
        let storage = if bytes <= memory_limit && memory.try_reserve_exact(values).is_ok() {
            Pending::Memory(memory)
        } else {
            Pending::Disk(
                cache.to_owned(),
                BufWriter::with_capacity(1 << 20, File::create(cache)?),
            )
        };
        Ok(Self {
            storage,
            columns,
            expected: points,
            written: 0,
            minimum: vec![f64::INFINITY; columns],
            maximum: vec![f64::NEG_INFINITY; columns],
        })
    }

    pub(crate) fn push(&mut self, row: &[f64]) -> Result<()> {
        if row.len() != self.columns || self.written >= self.expected {
            return Err(Error::Invalid("Incorrect feature row shape"));
        }
        let mut values = [0_f32; 64];
        for (i, &value) in row.iter().enumerate() {
            let value = value as f32;
            if !value.is_finite() {
                return Err(Error::Invalid("Feature values must be finite"));
            }
            values[i] = value;
            self.minimum[i] = self.minimum[i].min(f64::from(value));
            self.maximum[i] = self.maximum[i].max(f64::from(value));
        }
        match &mut self.storage {
            Pending::Memory(memory) => memory.extend_from_slice(&values[..self.columns]),
            Pending::Disk(_, writer) => {
                let mut bytes = [0; 64 * 4];
                for (value, destination) in values[..self.columns]
                    .iter()
                    .zip(bytes.as_chunks_mut::<4>().0)
                {
                    destination.copy_from_slice(&value.to_le_bytes());
                }
                writer.write_all(&bytes[..self.columns * 4])?;
            }
        }
        self.written += 1;
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<FeatureStore> {
        if self.written != self.expected {
            return Err(Error::Invalid("Incorrect number of feature rows"));
        }
        let storage = match self.storage {
            Pending::Memory(memory) => Storage::Memory(memory),
            Pending::Disk(path, mut writer) => {
                writer.flush()?;
                Storage::Disk(path)
            }
        };
        let range: Vec<_> = self
            .maximum
            .iter()
            .zip(&self.minimum)
            .map(|(hi, lo)| hi - lo)
            .collect();
        let active = (0..self.columns).filter(|&i| range[i] > 0.).collect();
        Ok(FeatureStore {
            storage,
            columns: self.columns,
            active,
            minimum: self.minimum,
            range,
            points: self.written,
        })
    }
}

pub struct FeatureStore {
    storage: Storage,
    columns: usize,
    active: Vec<usize>,
    minimum: Vec<f64>,
    range: Vec<f64>,
    pub points: usize,
}
impl FeatureStore {
    pub fn dims(&self) -> usize {
        self.active.len()
    }
    pub(crate) fn description(&self) -> String {
        let location = match self.storage {
            Storage::Memory(_) => "RAM",
            Storage::Disk(_) => "binary disk cache",
        };
        format!(
            "{location}, {:.2} MiB of f32 features",
            (self.points * self.columns * 4) as f64 / 1048576.
        )
    }
    fn normalize(&self, mut value: impl FnMut(usize) -> f32, row: &mut Vec<f64>) {
        row.clear();
        for &i in &self.active {
            row.push((f64::from(value(i)) - self.minimum[i]) / self.range[i]);
        }
    }
    fn normalize_bytes(&self, raw: &[u8], row: &mut Vec<f64>) {
        self.normalize(
            |i| f32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap()),
            row,
        );
    }
    pub fn load_indices(&self, indices: &[usize]) -> Result<Data> {
        if indices.iter().any(|&i| i >= self.points) {
            return Err(Error::Invalid("Fitting index outside recording"));
        }
        let mut row = Vec::with_capacity(self.dims());
        let mut values = Vec::with_capacity(indices.len() * self.dims());
        match &self.storage {
            Storage::Memory(memory) => {
                for &index in indices {
                    let raw = &memory[index * self.columns..(index + 1) * self.columns];
                    self.normalize(|i| raw[i], &mut row);
                    values.extend_from_slice(&row);
                }
            }
            Storage::Disk(path) => {
                let mut input = BufReader::with_capacity(1 << 20, File::open(path)?);
                let mut raw = vec![0; self.columns * 4];
                let mut position = 0_u64;
                for &index in indices {
                    let requested = (index * raw.len()) as u64;
                    input.seek_relative(requested as i64 - position as i64)?;
                    input.read_exact(&mut raw)?;
                    position = requested + raw.len() as u64;
                    self.normalize_bytes(&raw, &mut row);
                    values.extend_from_slice(&row);
                }
            }
        }
        Ok(Data {
            values,
            dims: self.dims(),
            points: indices.len(),
        })
    }
    pub fn for_each(&self, mut visit: impl FnMut(&[f64]) -> Result<()>) -> Result<()> {
        let mut row = Vec::with_capacity(self.dims());
        match &self.storage {
            Storage::Memory(memory) => {
                for raw in memory.chunks_exact(self.columns) {
                    self.normalize(|i| raw[i], &mut row);
                    visit(&row)?;
                }
            }
            Storage::Disk(path) => {
                let mut input = BufReader::with_capacity(1 << 20, File::open(path)?);
                let row_bytes = self.columns * 4;
                let mut raw = vec![0; row_bytes * 4096];
                let mut left = self.points;
                while left > 0 {
                    let count = left.min(4096);
                    input.read_exact(&mut raw[..count * row_bytes])?;
                    for record in raw[..count * row_bytes].chunks_exact(row_bytes) {
                        self.normalize_bytes(record, &mut row);
                        visit(&row)?;
                    }
                    left -= count;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fitting_subset_uses_full_recording_bounds_and_omits_constants() {
        let dir = tempfile::tempdir().unwrap();
        for limit in [0, FEATURE_MEMORY_LIMIT] {
            let mut builder = FeatureBuilder::new(3, 3, &dir.path().join("cache"), limit).unwrap();
            for row in [[-2., 5., 3.], [0., 5., 4.], [6., 5., 7.]] {
                builder.push(&row).unwrap();
            }
            let store = builder.finish().unwrap();
            assert_eq!(store.load_indices(&[1]).unwrap().row(0), &[0.25, 0.25]);
        }
    }
    #[test]
    fn memory_and_disk_keep_identical_precision_bounds_and_row_order() {
        let dir = tempfile::tempdir().unwrap();
        let points = 140_000;
        let mut stores = Vec::new();
        for (name, limit) in [("ram", FEATURE_MEMORY_LIMIT), ("disk", 0)] {
            let mut builder =
                FeatureBuilder::new(3, points, &dir.path().join(name), limit).unwrap();
            for i in 0..points {
                builder
                    .push(&[i as f64 + 0.123456789, 5., i as f64 * 2. + 0.123456789])
                    .unwrap();
            }
            stores.push(builder.finish().unwrap());
        }
        assert!(matches!(stores[0].storage, Storage::Memory(_)));
        assert!(matches!(stores[1].storage, Storage::Disk(_)));
        assert!(!dir.path().join("ram").exists());
        let indices = [0, 1, 131_071, 131_072, 139_999, 17, 17, 131_072];
        let a = stores[0].load_indices(&indices).unwrap();
        let b = stores[1].load_indices(&indices).unwrap();
        assert_eq!(a.values, b.values);
        assert_eq!(a.dims, 2);
        assert_eq!(a.row(0), &[0., 0.]);
        assert_eq!(a.row(4), &[1., 1.]);
        let mut all_a = Vec::new();
        let mut all_b = Vec::new();
        stores[0]
            .for_each(|r| {
                all_a.extend_from_slice(r);
                Ok(())
            })
            .unwrap();
        stores[1]
            .for_each(|r| {
                all_b.extend_from_slice(r);
                Ok(())
            })
            .unwrap();
        assert_eq!(all_a, all_b);
        assert!(stores[0].load_indices(&[points]).is_err());
        assert!(stores[1].load_indices(&[points]).is_err());
    }
    #[test]
    fn large_count_chooses_disk_without_allocating_the_full_recording() {
        let dir = tempfile::tempdir().unwrap();
        let builder = FeatureBuilder::new(
            12,
            9_000_000,
            &dir.path().join("cache"),
            FEATURE_MEMORY_LIMIT,
        )
        .unwrap();
        assert!(matches!(builder.storage, Pending::Disk(..)));
        assert_eq!(
            std::fs::metadata(dir.path().join("cache")).unwrap().len(),
            0
        );
    }
    #[test]
    fn raw_f32_features_avoid_decimal_rounding_and_reject_bad_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = FeatureBuilder::new(1, 2, &dir.path().join("cache"), 1024).unwrap();
        let value = 1.23456789_f64;
        assert!(builder.push(&[f64::NAN]).is_err());
        assert!(builder.push(&[]).is_err());
        builder.push(&[value]).unwrap();
        builder.push(&[2.]).unwrap();
        let store = builder.finish().unwrap();
        let Storage::Memory(values) = store.storage else {
            panic!("Expected RAM storage");
        };
        assert_eq!(values[0].to_bits(), (value as f32).to_bits());
        assert_ne!(
            values[0].to_bits(),
            format!("{value:.5e}").parse::<f32>().unwrap().to_bits()
        );
        let short = FeatureBuilder::new(1, 2, &dir.path().join("short"), 1024).unwrap();
        assert!(short.finish().is_err());
    }
}
