//! Read a tetrode recording and compute per-spike features in two streaming passes:
//! the first accumulates each channel's waveform covariance for PCA, the second
//! projects every spike. Waveforms are read in bounded blocks.
use crate::dump::{Dump, Point};
use crate::{Error, Result, BYTES_PER_SPIKE, CHANNELS, MAX_BLOCK_SPIKES, MAX_SPIKES, SAMPLES};
use nalgebra::{linalg::SymmetricEigen, DMatrix, DMatrixView};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub pcs: usize,
    pub amplitude: bool,
    pub voltage: bool,
}
impl Features {
    pub fn parse(text: &str) -> Result<Self> {
        let mut features = Self::default();
        for token in text.split(',') {
            match token {
                "A" => features.amplitude = true,
                "VT" => features.voltage = true,
                _ if token.len() == 3
                    && token.ends_with("PC")
                    && token.as_bytes()[0].is_ascii_digit() =>
                {
                    features.pcs = (token.as_bytes()[0] - b'0') as usize;
                }
                _ => {
                    return Err(Error::Invalid(
                        "Features must be a comma-separated list of 0PC–9PC, A, VT",
                    ))
                }
            }
        }
        if features.width() == 0 {
            return Err(Error::Invalid("Select at least one feature"));
        }
        Ok(features)
    }
    fn width(self) -> usize {
        self.pcs + usize::from(self.amplitude) + usize::from(self.voltage)
    }
}

#[derive(Debug)]
struct Header {
    spikes: u64,
    offset: u64,
}

fn checked_payload_size(spikes: u64) -> Result<u64> {
    if spikes < 2 {
        return Err(Error::Invalid("num_spikes must be at least 2"));
    }
    if spikes > MAX_SPIKES {
        return Err(Error::Invalid(
            "num_spikes exceeds the supported limit (2147483647)",
        ));
    }
    spikes
        .checked_mul(BYTES_PER_SPIKE as u64)
        .ok_or(Error::Invalid("Spike payload size overflow"))
}

fn read_header(reader: &mut (impl Read + Seek)) -> Result<Header> {
    let mut spikes = None;
    let mut line = Vec::new();
    let mut byte = [0];
    loop {
        if reader.read(&mut byte)? == 0 {
            return Err(Error::Invalid("Missing data_start in tetrode header"));
        }
        line.push(byte[0]);
        if line == b"data_start" {
            break;
        }
        if line.len() > 4096 {
            return Err(Error::Invalid("Invalid tetrode header line"));
        }
        if byte[0] == b'\n' {
            let text = std::str::from_utf8(&line)
                .map_err(|_| Error::Invalid("Invalid tetrode header text"))?;
            let mut words = text.split_whitespace();
            if words.next() == Some("num_spikes") {
                let count = words
                    .next()
                    .ok_or(Error::Invalid("Invalid num_spikes in tetrode header"))?;
                if count.starts_with('-') {
                    return Err(Error::Invalid("num_spikes must be at least 2"));
                }
                spikes = Some(
                    count
                        .parse::<u64>()
                        .map_err(|_| Error::Invalid("Invalid num_spikes in tetrode header"))?,
                );
            }
            line.clear();
        }
    }
    let spikes = spikes.ok_or(Error::Invalid("Missing num_spikes in tetrode header"))?;
    checked_payload_size(spikes)?;
    Ok(Header {
        spikes,
        offset: reader.stream_position()?,
    })
}

/// One channel's 50 samples within a 216-byte spike record.
fn waveform(record: &[u8], channel: usize) -> impl Iterator<Item = i8> + '_ {
    let start = channel * (4 + SAMPLES) + 4;
    record[start..start + SAMPLES].iter().map(|&b| b as i8)
}

/// Sums and sums of products of one channel's samples. Inputs are small
/// integers, so these sums are exact in f64 regardless of block boundaries.
struct Moments {
    sum: [f64; SAMPLES],
    product: DMatrix<f64>,
    minimum: i8,
    maximum: i8,
}
impl Moments {
    fn new() -> Self {
        Self {
            sum: [0.0; SAMPLES],
            product: DMatrix::zeros(SAMPLES, SAMPLES),
            minimum: i8::MAX,
            maximum: i8::MIN,
        }
    }
    fn covariance(&self, count: u64) -> DMatrix<f64> {
        let n = count as f64;
        DMatrix::from_fn(SAMPLES, SAMPLES, |row, column| {
            let (row, column) = (row.max(column), row.min(column));
            (self.product[(row, column)] - (self.sum[row] / n) * self.sum[column]) / (n - 1.0)
        })
    }
}

struct ChannelModel {
    channel: usize,
    mean: [f64; SAMPLES],
    /// SAMPLES x pcs, in descending eigenvalue order.
    directions: DMatrix<f64>,
    /// The sample with the largest variance, used by the VT feature.
    voltage_sample: usize,
}

fn principal_directions(covariance: DMatrix<f64>, count: usize) -> Result<DMatrix<f64>> {
    let eigen = SymmetricEigen::try_new(covariance, f64::EPSILON, 100_000)
        .ok_or(Error::Invalid("PCA eigensolver did not converge"))?;
    let mut order: Vec<_> = (0..SAMPLES).collect();
    order.sort_by(|&a, &b| eigen.eigenvalues[b].total_cmp(&eigen.eigenvalues[a]));
    let mut directions = DMatrix::zeros(SAMPLES, count);
    for (pc, &index) in order.iter().take(count).enumerate() {
        // Make the sign deterministic: the largest-magnitude loading is positive.
        let column = eigen.eigenvectors.column(index);
        let largest = if column.max().abs() >= column.min().abs() {
            column.max()
        } else {
            column.min()
        };
        let sign = if largest < 0.0 { -1.0 } else { 1.0 };
        directions.set_column(pc, &(column * sign));
    }
    Ok(directions)
}

/// A recording whose PCA models have been fitted, ready to stream feature rows.
pub(crate) struct Extractor {
    reader: BufReader<File>,
    header: Header,
    raw: Vec<u8>,
    models: Vec<ChannelModel>,
    features: Features,
    block_spikes: usize,
}
impl Extractor {
    pub(crate) fn open(
        input: &Path,
        features: Features,
        block_spikes: usize,
        log: &mut dyn Write,
    ) -> Result<Self> {
        if !(1..=MAX_BLOCK_SPIKES).contains(&block_spikes) {
            return Err(Error::Invalid("block-spikes must be between 1 and 65536"));
        }
        let mut reader = BufReader::with_capacity(1 << 20, File::open(input)?);
        let header = read_header(&mut reader)?;
        let end = header
            .offset
            .checked_add(checked_payload_size(header.spikes)?)
            .ok_or(Error::Invalid("Spike payload size overflow"))?;
        if reader.get_ref().metadata()?.len() < end {
            return Err(Error::Invalid(
                "Tetrode data is shorter than num_spikes requires",
            ));
        }
        writeln!(log, "{} spikes", header.spikes)?;
        let mut extractor = Self {
            reader,
            header,
            raw: vec![0; block_spikes * BYTES_PER_SPIKE],
            models: Vec::new(),
            features,
            block_spikes,
        };
        extractor.fit_models()?;
        Ok(extractor)
    }

    /// Visit the recording's spike records in blocks, from the start of the data.
    fn blocks(&mut self, mut visit: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        self.reader.seek(SeekFrom::Start(self.header.offset))?;
        let mut left = self.header.spikes;
        while left > 0 {
            let count = left.min(self.block_spikes as u64) as usize;
            let block = &mut self.raw[..count * BYTES_PER_SPIKE];
            self.reader.read_exact(block)?;
            visit(block)?;
            left -= count as u64;
        }
        Ok(())
    }

    fn fit_models(&mut self) -> Result<()> {
        let features = self.features;
        let mut moments: [Moments; CHANNELS] = std::array::from_fn(|_| Moments::new());
        let mut samples = vec![0.0; SAMPLES * self.block_spikes];
        self.blocks(|block| {
            let records = block.as_chunks::<BYTES_PER_SPIKE>().0;
            for (channel, moment) in moments.iter_mut().enumerate() {
                for (spike, record) in records.iter().enumerate() {
                    for (sample, value) in waveform(record, channel).enumerate() {
                        moment.minimum = moment.minimum.min(value);
                        moment.maximum = moment.maximum.max(value);
                        let value = f64::from(value);
                        moment.sum[sample] += value;
                        samples[spike * SAMPLES + sample] = value;
                        if features.pcs == 0 {
                            // VT needs only the variances.
                            moment.product[(sample, sample)] += value * value;
                        }
                    }
                }
                if features.pcs > 0 {
                    let x = DMatrixView::from_slice(
                        &samples[..records.len() * SAMPLES],
                        SAMPLES,
                        records.len(),
                    );
                    moment.product += x * x.transpose();
                }
            }
            Ok(())
        })?;
        for (channel, moment) in moments.into_iter().enumerate() {
            if moment.minimum == moment.maximum {
                continue; // A constant channel has no features.
            }
            let covariance = moment.covariance(self.header.spikes);
            let voltage_sample = (0..SAMPLES)
                .reduce(|best, s| {
                    if covariance[(s, s)] > covariance[(best, best)] {
                        s
                    } else {
                        best
                    }
                })
                .unwrap();
            let directions = if features.pcs > 0 {
                principal_directions(covariance, features.pcs)?
            } else {
                DMatrix::zeros(SAMPLES, 0)
            };
            self.models.push(ChannelModel {
                channel,
                mean: moment.sum.map(|sum| sum / self.header.spikes as f64),
                directions,
                voltage_sample,
            });
        }
        if self.models.is_empty() {
            return Err(Error::Invalid("No active channels in recording"));
        }
        Ok(())
    }

    pub(crate) fn spikes(&self) -> usize {
        self.header.spikes as usize
    }

    pub(crate) fn width(&self) -> usize {
        self.models.len() * self.features.width()
    }

    pub(crate) fn dump_pca(&self, dump: &Dump) -> Result<()> {
        let (channels, pcs) = (self.models.len(), self.features.pcs);
        dump.array(
            Point::Pca,
            "pca_channels",
            &[channels],
            self.models.iter().map(|m| m.channel as u8),
        )?;
        dump.array(
            Point::Pca,
            "pca_means",
            &[channels, SAMPLES],
            self.models.iter().flat_map(|m| m.mean),
        )?;
        dump.array(
            Point::Pca,
            "pca_components",
            &[channels, pcs, SAMPLES],
            // Column-major SAMPLES x pcs storage is row-major pcs x SAMPLES.
            self.models
                .iter()
                .flat_map(|m| m.directions.as_slice().iter().copied()),
        )
    }

    /// Visit each spike's features. Column order: every active channel's PCs,
    /// then every channel's amplitude, then every channel's VT.
    pub(crate) fn rows(mut self, mut visit: impl FnMut(&[f64]) -> Result<()>) -> Result<()> {
        let models = std::mem::take(&mut self.models);
        let features = self.features;
        let mut row = Vec::with_capacity(self.width());
        self.blocks(|block| {
            for record in block.as_chunks::<BYTES_PER_SPIKE>().0 {
                row.clear();
                for model in &models {
                    for direction in model.directions.column_iter() {
                        let score = waveform(record, model.channel)
                            .zip(model.mean)
                            .zip(direction.iter())
                            .map(|((sample, mean), loading)| (f64::from(sample) - mean) * loading)
                            .sum();
                        row.push(score);
                    }
                }
                if features.amplitude {
                    for model in &models {
                        let (low, high) = waveform(record, model.channel)
                            .fold((i8::MAX, i8::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
                        row.push(f64::from(i16::from(high) - i16::from(low)));
                    }
                }
                if features.voltage {
                    for model in &models {
                        let value = waveform(record, model.channel).nth(model.voltage_sample);
                        row.push(f64::from(value.unwrap()));
                    }
                }
                visit(&row)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn payload_sizes_cross_old_overflows_without_allocation() {
        for (spikes, bytes) in [
            (9_000_000, 1_944_000_000),
            (9_942_054, 2_147_483_664),
            (20_000_000, 4_320_000_000),
        ] {
            assert_eq!(checked_payload_size(spikes).unwrap(), bytes);
        }
        for count in [0, 1, MAX_SPIKES + 1, u64::MAX] {
            assert!(checked_payload_size(count).is_err());
        }
    }

    #[test]
    fn feature_selection() {
        let features = Features::parse("VT,3PC,A").unwrap();
        assert_eq!(features.width(), 5);
        for invalid in ["", "0PC", "10PC", "3pc", "A,"] {
            assert!(Features::parse(invalid).is_err());
        }
    }

    #[test]
    fn header_stops_before_binary_newline_bytes() {
        let mut file = io::Cursor::new(b"num_spikes 80\r\ndata_start\n\xff".to_vec());
        let header = read_header(&mut file).unwrap();
        assert_eq!(header.spikes, 80);
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'\n']);
    }

    #[test]
    fn integer_moments_match_centered_covariance() {
        let mut moment = Moments::new();
        let count = 17;
        for i in 0..count {
            let a = (i as f64 - 9.0) * 3.0;
            let b = (i % 5) as f64 - 2.0;
            moment.sum[0] += a;
            moment.sum[1] += b;
            moment.product[(0, 0)] += a * a;
            moment.product[(1, 0)] += a * b;
            moment.product[(1, 1)] += b * b;
        }
        let covariance = moment.covariance(count);
        let expected = (0..count)
            .map(|i| {
                ((i as f64 - 9.0) * 3.0 - moment.sum[0] / count as f64)
                    * ((i % 5) as f64 - 2.0 - moment.sum[1] / count as f64)
            })
            .sum::<f64>()
            / (count - 1) as f64;
        assert!((covariance[(0, 1)] - expected).abs() < 1e-12);
        assert_eq!(covariance[(0, 1)], covariance[(1, 0)]);
    }

    #[test]
    fn principal_directions_are_orthonormal_and_sorted() {
        let mut state = 943_u64;
        let mut moment = Moments::new();
        let count = 80;
        let mut samples = Vec::new();
        for _ in 0..count * SAMPLES {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            samples.push((state >> 56) as i8 as f64);
        }
        for spike in samples.chunks(SAMPLES) {
            for (i, &a) in spike.iter().enumerate() {
                moment.sum[i] += a;
                for (j, &b) in spike.iter().enumerate() {
                    moment.product[(i, j)] += a * b;
                }
            }
        }
        let covariance = moment.covariance(count as u64);
        let directions = principal_directions(covariance.clone(), 5).unwrap();
        let gram = directions.transpose() * &directions;
        assert!((gram - DMatrix::identity(5, 5)).norm() < 1e-12);
        let variances: Vec<f64> = directions
            .column_iter()
            .map(|d| (d.transpose() * &covariance * d)[0])
            .collect();
        assert!(variances.windows(2).all(|w| w[0] >= w[1]));
    }
}
