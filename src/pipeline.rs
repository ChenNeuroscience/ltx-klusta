//! One recording, end to end: features, clustering, and the published `.clu` file.
use crate::dump::{Dump, Point};
use crate::extract::Extractor;
use crate::feature_store::{FeatureBuilder, FEATURE_MEMORY_LIMIT};
use crate::progress::{Stage, Update};
use crate::{cluster, Error, Features, Result, DEFAULT_BLOCK_SPIKES, MAX_BLOCK_SPIKES};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub features: String,
    pub cluster: cluster::Options,
    pub block_spikes: usize,
    #[serde(deserialize_with = "crate::concurrency::deserialize_jobs")]
    pub jobs: usize,
    pub force: bool,
    pub work_dir: Option<PathBuf>,
    /// Intermediate data to write as .npy files.
    pub dump: Vec<Point>,
    /// Where to write dumps (default: `NAME.dump.N` beside each recording).
    pub dump_dir: Option<PathBuf>,
    /// Stop after the last requested dump point and do not write `.clu`.
    pub dump_only: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            features: "3PC".into(),
            cluster: cluster::Options::default(),
            block_spikes: DEFAULT_BLOCK_SPIKES,
            jobs: crate::concurrency::default_jobs(),
            force: false,
            work_dir: None,
            dump: Vec::new(),
            dump_dir: None,
            dump_only: false,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        Features::parse(&self.features)?;
        self.cluster.validate()?;
        if !(1..=MAX_BLOCK_SPIKES).contains(&self.block_spikes) {
            return Err(Error::Invalid("block-spikes must be between 1 and 65536"));
        }
        if !(1..=crate::concurrency::MAX_JOBS).contains(&self.jobs) {
            return Err(Error::Invalid("jobs must be between 1 and 16"));
        }
        if self.dump_only && self.dump.is_empty() {
            return Err(Error::Invalid(
                "--dump-only requires at least one --dump point",
            ));
        }
        Ok(())
    }

    /// Load `path`, or `ltxk.json` beside the executable if it exists.
    /// Relative paths in the file are resolved against the file's folder.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let explicit = path.is_some();
        let path = match path {
            Some(path) => path.to_owned(),
            None => std::env::current_exe()?.with_file_name("ltxk.json"),
        };
        if !path.exists() && !path.is_symlink() {
            if explicit {
                return Err(Error::Message(format!(
                    "Configuration file does not exist: {}",
                    path.display()
                )));
            }
            return Ok(Self::default());
        }
        let mut settings: Self = serde_json::from_reader(File::open(&path)?)
            .map_err(|error| Error::Message(format!("Invalid configuration: {error}")))?;
        settings.resolve_paths(fs::canonicalize(path)?.parent().unwrap());
        Ok(settings)
    }

    pub fn resolve_paths(&mut self, base: &Path) {
        for path in [&mut self.work_dir, &mut self.dump_dir]
            .into_iter()
            .flatten()
        {
            if path.is_relative() {
                *path = base.join(&path);
            }
        }
    }

    /// Is clustering needed at all, given the dump settings?
    fn clusters(&self) -> bool {
        !self.dump_only || self.dump.iter().any(|&p| p > Point::Features)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Output {
    pub input: PathBuf,
    pub output: PathBuf,
    pub skipped: bool,
    pub spikes: usize,
    /// Includes label 1, the noise class, even if it is empty.
    pub labels: usize,
    pub score: Option<f64>,
    pub fitting_spikes: usize,
    pub stride: usize,
}

/// Filename-only eligibility for Explorer; opening the recording validates its contents.
pub fn is_recording_name(input: &Path) -> bool {
    if tetrode_number(input).is_err() {
        return false;
    }
    let kind = input
        .file_stem()
        .and_then(|stem| Path::new(stem).extension())
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    !["clu", "fet", "res", "spk"]
        .iter()
        .any(|sidecar| kind.eq_ignore_ascii_case(sidecar))
}

/// The tetrode number: `name.N` → `N`.
fn tetrode_number(input: &Path) -> Result<&str> {
    match input.extension().and_then(|e| e.to_str()) {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => Ok(n),
        _ => Err(Error::Invalid(
            "Tetrode filename must have a numeric extension",
        )),
    }
}

/// `folder/name.N` → `folder/name.clu.N`
pub fn output_path(input: &Path) -> Result<PathBuf> {
    let number = tetrode_number(input)?;
    Ok(input.with_extension(format!("clu.{number}")))
}

/// `folder/name.N` → `folder/name.dump.N`
pub fn default_dump_dir(input: &Path) -> Result<PathBuf> {
    let number = tetrode_number(input)?;
    Ok(input.with_extension(format!("dump.{number}")))
}

/// Cluster one recording using a private temporary scratch directory.
pub fn cluster_recording(input: &Path, settings: &Settings, log: &mut dyn Write) -> Result<Output> {
    let mut work = tempfile::Builder::new();
    work.prefix("ltxk-");
    let work = match &settings.work_dir {
        Some(parent) => work.tempdir_in(parent)?,
        None => work.tempdir()?,
    };
    cluster_in(input, settings, work.path(), log, &mut |_| Ok(()))
}

pub(crate) fn cluster_in(
    input: &Path,
    settings: &Settings,
    work: &Path,
    log: &mut dyn Write,
    notify: &mut dyn FnMut(Update) -> Result<()>,
) -> Result<Output> {
    settings.validate()?;
    let input = fs::canonicalize(input)?;
    let output = output_path(&input)?;
    let mut result = Output {
        input: input.clone(),
        output: output.clone(),
        skipped: false,
        spikes: 0,
        labels: 0,
        score: None,
        fitting_spikes: 0,
        stride: 1,
    };
    if output.exists() && !settings.force && !settings.dump_only {
        writeln!(log, "Skipped existing {}", output.display())?;
        result.skipped = true;
        return Ok(result);
    }
    let dump = match &settings.dump_dir {
        Some(dir) => dir.join(input.file_name().unwrap()),
        None => default_dump_dir(&input)?,
    };
    let dump = Dump::new(dump, &settings.dump)?;
    writeln!(log, "Clustering {}", input.display())?;

    writeln!(log, "Extracting {} features...", settings.features)?;
    notify(Update::Stage(Stage::Extracting))?;
    let started = Instant::now();
    let extractor = Extractor::open(
        &input,
        Features::parse(&settings.features)?,
        settings.block_spikes,
        log,
    )?;
    extractor.dump_pca(&dump)?;
    let (spikes, width) = (extractor.spikes(), extractor.width());
    let mut builder = FeatureBuilder::new(
        width,
        spikes,
        &work.join("features.bin"),
        FEATURE_MEMORY_LIMIT,
    )?;
    let mut dumped = dump.stream(Point::Features, "features", &[spikes, width])?;
    extractor.rows(|row| {
        if let Some(dumped) = &mut dumped {
            dumped.extend(row.iter().map(|&v| v as f32))?;
        }
        builder.push(row)
    })?;
    if let Some(dumped) = dumped {
        dumped.finish()?;
    }
    let extracted = Instant::now();

    notify(Update::Stage(Stage::Features))?;
    let store = builder.finish()?;
    store.dump_bounds(&dump)?;
    writeln!(log, "Features: {}", store.description())?;
    if std::env::var_os("LTXK_PROFILE").is_some() {
        writeln!(
            log,
            "Profile extract_s={:.6} cache_s={:.6}",
            (extracted - started).as_secs_f64(),
            extracted.elapsed().as_secs_f64()
        )?;
    }
    result.spikes = store.points;
    if !settings.clusters() {
        writeln!(log, "Dump only: features written; clustering skipped")?;
        return Ok(result);
    }

    let fit = cluster::run(&store, &settings.cluster, work, log, notify, &dump)?;
    result.score = Some(fit.score);
    result.fitting_spikes = fit.fitting_spikes;
    result.stride = fit.stride;
    notify(Update::TrialDone {
        clusters: fit.counts.iter().skip(1).filter(|&&n| n > 0).count(),
        noise: fit.counts[0],
    })?;
    if settings.dump_only {
        writeln!(log, "Dump only: .clu not written")?;
        return Ok(result);
    }

    notify(Update::Stage(Stage::Publishing))?;
    result.labels = publish(&fit, &output, settings.force)?;
    writeln!(
        log,
        "Written {}: {} spikes, {} signal clusters + noise; score {:.8}",
        output.display(),
        store.points,
        result.labels - 1,
        fit.score
    )?;
    Ok(result)
}

/// Write the best trial's labels as a `.clu` file: a label count, then one
/// label per spike, with 1 for noise and 2.. for non-empty clusters in order.
/// The file appears atomically, only once complete.
fn publish(fit: &cluster::Output, output: &Path, replace: bool) -> Result<usize> {
    let mut mapping = [1_u8; cluster::MAX_CLUSTERS];
    let mut labels = 1;
    for (c, &count) in fit.counts.iter().enumerate().skip(1) {
        if count > 0 {
            labels += 1;
            mapping[c] = labels as u8;
        }
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".ltxk-")
        .tempfile_in(output.parent().unwrap())?;
    {
        let mut writer = BufWriter::with_capacity(1 << 20, temporary.as_file_mut());
        writeln!(writer, "{labels}")?;
        let mut assignments = BufReader::new(File::open(&fit.labels)?);
        let mut buffer = [0; 65536];
        loop {
            let count = assignments.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            for &class in &buffer[..count] {
                writeln!(writer, "{}", mapping[usize::from(class)])?;
            }
        }
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    if replace {
        temporary
            .persist(output)
            .map_err(|error| Error::Io(error.error))?;
    } else {
        temporary.persist_noclobber(output).map_err(|error| {
            Error::Message(format!(
                "Could not publish {} (another job may have created it): {}",
                output.display(),
                error.error
            ))
        })?;
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_preserves_the_recording_name_and_tetrode_number() {
        assert_eq!(
            output_path(Path::new("folder/a.b.12")).unwrap(),
            Path::new("folder/a.b.clu.12")
        );
        assert_eq!(
            default_dump_dir(Path::new("folder/a.b.12")).unwrap(),
            Path::new("folder/a.b.dump.12")
        );
        assert!(output_path(Path::new("a.txt")).is_err());
        assert!(output_path(Path::new("a")).is_err());
    }
    #[test]
    fn invalid_or_unknown_configuration_is_not_silently_ignored() {
        assert!(serde_json::from_str::<Settings>(r#"{"target_spkies":42}"#).is_err());
        let settings: Settings =
            serde_json::from_str(r#"{"cluster":{"min_clusters":2,"max_clusters":3}}"#).unwrap();
        assert_eq!(settings.cluster.target_spikes, 25_000);
        assert_eq!(settings.cluster.min_clusters, 2);
        let dump: Settings = serde_json::from_str(r#"{"dump":["features","labels"]}"#).unwrap();
        assert_eq!(dump.dump, [Point::Features, Point::Labels]);
        assert!(serde_json::from_str::<Settings>(r#"{"dump":["everything"]}"#).is_err());
    }
    #[test]
    fn dump_only_runs_clustering_only_when_a_later_point_needs_it() {
        let mut settings = Settings {
            dump: vec![Point::Pca, Point::Features],
            dump_only: true,
            ..Settings::default()
        };
        assert!(!settings.clusters());
        settings.dump.push(Point::Models);
        assert!(settings.clusters());
        settings.dump.clear();
        assert!(settings.validate().is_err());
    }
}
