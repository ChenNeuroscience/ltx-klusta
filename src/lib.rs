//! Two-pass feature extraction with memory bounded by block size, not spike count.

#[cfg(not(target_pointer_width = "64"))]
compile_error!("prekk requires a 64-bit target");

pub mod linalg;
mod native;
pub use linalg::Backend;
use linalg::Engine;
use nalgebra::DMatrix;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const SAMPLES: usize = 50;
pub const CHANNELS: usize = 4;
pub const BYTES_PER_SPIKE: usize = CHANNELS * (4 + SAMPLES);
pub const DEFAULT_BLOCK_SPIKES: usize = 4096;
pub const MAX_BLOCK_SPIKES: usize = 65536;
// Keep the previous public count bound; sums of signed-byte products remain
// exactly representable in f64 up to this count, independent of block boundaries.
pub const MAX_SPIKES: u64 = i32::MAX as u64;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(&'static str),
    Backend(String),
    Lapack { routine: &'static str, info: i32 },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Invalid(message) => f.write_str(message),
            Self::Backend(message) => f.write_str(message),
            Self::Lapack { routine, info } => write!(f, "LAPACK {routine} failed (INFO={info})"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

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

pub struct Options {
    pub input: PathBuf,
    pub output_base: PathBuf,
    pub info: PathBuf,
    pub features: Features,
    pub backend: Backend,
    pub lapack_library: Option<PathBuf>,
    pub block_spikes: usize,
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

pub fn feature_path(input: &Path, base: &Path) -> Result<PathBuf> {
    let extension = input.extension().ok_or(Error::Invalid(
        "Tetrode filename must have a numeric extension",
    ))?;
    if !extension
        .to_str()
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Error::Invalid(
            "Tetrode filename must have a numeric extension",
        ));
    }
    let mut name = base.as_os_str().to_owned();
    name.push(".fet.");
    name.push(extension);
    Ok(PathBuf::from(name))
}

fn canonical_output(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(fs::canonicalize(path)?);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(fs::canonicalize(parent)?.join(
        path.file_name()
            .ok_or(Error::Invalid("Invalid output path"))?,
    ))
}

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
        let mut result = DMatrix::zeros(SAMPLES, SAMPLES);
        for column in 0..SAMPLES {
            for row in column..SAMPLES {
                let value = (self.product[(row, column)]
                    - (self.sum[row] / count as f64) * self.sum[column])
                    / (count - 1) as f64;
                result[(row, column)] = value;
                result[(column, row)] = value;
            }
        }
        result
    }
}

struct ChannelModel {
    channel: usize,
    mean: [f64; SAMPLES],
    directions: DMatrix<f64>,
    voltage_sample: usize,
}

fn build_models(
    reader: &mut (impl Read + Seek),
    header: &Header,
    options: &Options,
    engine: &Engine,
    raw: &mut [u8],
) -> Result<Vec<ChannelModel>> {
    reader.seek(SeekFrom::Start(header.offset))?;
    let mut moments: [Moments; CHANNELS] = std::array::from_fn(|_| Moments::new());
    let mut samples = vec![0.0; SAMPLES * options.block_spikes];
    let mut left = header.spikes;
    while left > 0 {
        let count = left.min(options.block_spikes as u64) as usize;
        reader.read_exact(&mut raw[..count * BYTES_PER_SPIKE])?;
        for (channel, moment) in moments.iter_mut().enumerate() {
            for spike in 0..count {
                let start = spike * BYTES_PER_SPIKE + channel * (4 + SAMPLES) + 4;
                for sample in 0..SAMPLES {
                    let value = raw[start + sample] as i8;
                    moment.minimum = moment.minimum.min(value);
                    moment.maximum = moment.maximum.max(value);
                    let value = f64::from(value);
                    moment.sum[sample] += value;
                    if options.features.pcs > 0 {
                        samples[spike * SAMPLES + sample] = value;
                    } else if options.features.voltage {
                        moment.product[(sample, sample)] += value * value;
                    }
                }
            }
            if options.features.pcs > 0 {
                engine.accumulate(&samples[..count * SAMPLES], count, &mut moment.product);
            }
        }
        left -= count as u64;
    }
    let mut models = Vec::new();
    for (channel, moment) in moments.into_iter().enumerate() {
        if moment.minimum == moment.maximum {
            continue;
        }
        let covariance = moment.covariance(header.spikes);
        let mut voltage_sample = 0;
        for sample in 1..SAMPLES {
            if covariance[(sample, sample)] > covariance[(voltage_sample, voltage_sample)] {
                voltage_sample = sample;
            }
        }
        let mut directions = DMatrix::zeros(SAMPLES, options.features.pcs);
        if options.features.pcs > 0 {
            let (values, vectors) = engine.eigen(covariance)?;
            let mut order: Vec<_> = (0..SAMPLES).collect();
            order.sort_by(|&a, &b| values[b].total_cmp(&values[a]));
            for (pc, &index) in order.iter().take(options.features.pcs).enumerate() {
                let column = vectors.column(index);
                let (maximum, minimum) = (column.max(), column.min());
                let flip = if maximum.abs() >= minimum.abs() {
                    maximum < 0.0
                } else {
                    minimum < 0.0
                };
                let sign = if flip { -1.0 } else { 1.0 };
                directions.set_column(pc, &(column * sign));
            }
        }
        models.push(ChannelModel {
            channel,
            mean: moment.sum.map(|sum| sum / header.spikes as f64),
            directions,
            voltage_sample,
        });
    }
    if models.is_empty() {
        return Err(Error::Invalid("No active channels in recording"));
    }
    Ok(models)
}

pub fn extract(options: &Options) -> Result<PathBuf> {
    if !(1..=MAX_BLOCK_SPIKES).contains(&options.block_spikes) {
        return Err(Error::Invalid("block-spikes must be between 1 and 65536"));
    }
    let mut reader = BufReader::with_capacity(1 << 20, File::open(&options.input)?);
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
    let output = feature_path(&options.input, &options.output_base)?;
    let input = fs::canonicalize(&options.input)?;
    let (canonical_features, canonical_info) =
        (canonical_output(&output)?, canonical_output(&options.info)?);
    if input == canonical_features
        || input == canonical_info
        || canonical_features == canonical_info
    {
        return Err(Error::Invalid(
            "Input, feature output, and count output must be different files",
        ));
    }
    let engine = Engine::resolve(options.backend, options.lapack_library.as_deref())?;
    eprintln!(
        "{} spikes; {} backend; blocks of {} spikes",
        header.spikes,
        engine.description(),
        options.block_spikes
    );
    let mut raw = vec![0; options.block_spikes * BYTES_PER_SPIKE];
    let models = build_models(&mut reader, &header, options, &engine, &mut raw)?;
    reader.seek(SeekFrom::Start(header.offset))?;
    let mut writer = BufWriter::with_capacity(1 << 20, File::create(&output)?);
    let width = models.len() * options.features.width();
    writeln!(writer, "{width}")?;
    let mut features = Vec::with_capacity(width);
    let mut left = header.spikes;
    while left > 0 {
        let count = left.min(options.block_spikes as u64) as usize;
        reader.read_exact(&mut raw[..count * BYTES_PER_SPIKE])?;
        for record in raw[..count * BYTES_PER_SPIKE]
            .as_chunks::<BYTES_PER_SPIKE>()
            .0
        {
            features.clear();
            let wave = |channel: usize| {
                &record[channel * (4 + SAMPLES) + 4..(channel + 1) * (4 + SAMPLES)]
            };
            // Feature order: all channels' PCs, then all A, then all VT.
            for model in &models {
                for direction in model.directions.column_iter() {
                    let score = wave(model.channel)
                        .iter()
                        .zip(model.mean)
                        .zip(direction.iter())
                        .map(|((&sample, mean), loading)| {
                            (f64::from(sample as i8) - mean) * loading
                        })
                        .sum();
                    features.push(score);
                }
            }
            if options.features.amplitude {
                for model in &models {
                    let (mut minimum, mut maximum) = (i8::MAX, i8::MIN);
                    for &value in wave(model.channel) {
                        minimum = minimum.min(value as i8);
                        maximum = maximum.max(value as i8);
                    }
                    features.push(f64::from(i16::from(maximum) - i16::from(minimum)));
                }
            }
            if options.features.voltage {
                for model in &models {
                    features.push(f64::from(wave(model.channel)[model.voltage_sample] as i8));
                }
            }
            for (i, value) in features.iter().enumerate() {
                if i > 0 {
                    write!(writer, " ")?;
                }
                // Write six significant decimal digits; scientific text
                // is accepted by KlustaKwik and avoids huge per-record allocations.
                write!(writer, "{value:.5e}")?;
            }
            writeln!(writer)?;
        }
        left -= count as u64;
    }
    writer.flush()?;
    let mut info = BufWriter::new(File::create(&options.info)?);
    write!(info, "{}", header.spikes)?;
    info.flush()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn feature_selection_and_dotted_output_base() {
        let features = Features::parse("VT,3PC,A").unwrap();
        assert_eq!(features.width(), 5);
        for invalid in ["", "0PC", "10PC", "3pc", "A,"] {
            assert!(Features::parse(invalid).is_err());
        }
        assert_eq!(
            feature_path(Path::new("recording.12"), Path::new("session.part")).unwrap(),
            Path::new("session.part.fet.12")
        );
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
    }
}
