use crate::{
    cluster, feature_store::FeatureStore, Backend, Error, Features, Result, DEFAULT_BLOCK_SPIKES,
    MAX_BLOCK_SPIKES,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub features: String,
    pub cluster: cluster::Options,
    pub backend: Backend,
    pub lapack_library: Option<PathBuf>,
    pub block_spikes: usize,
    pub jobs: usize,
    pub force: bool,
    pub work_dir: Option<PathBuf>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            features: "3PC".into(),
            cluster: cluster::Options::default(),
            backend: Backend::Nalgebra,
            lapack_library: None,
            block_spikes: DEFAULT_BLOCK_SPIKES,
            jobs: 4,
            force: false,
            work_dir: None,
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
        if !(1..=16).contains(&self.jobs) {
            return Err(Error::Invalid("jobs must be between 1 and 16"));
        }
        if self.backend == Backend::Nalgebra && self.lapack_library.is_some() {
            return Err(Error::Invalid(
                "--lapack-library cannot be used with --backend nalgebra",
            ));
        }
        Ok(())
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let explicit = path.is_some();
        let path = match path {
            Some(path) => path.to_owned(),
            None => std::env::current_exe()?.with_file_name("fancykk.json"),
        };
        if path.exists() || path.is_symlink() {
            let file = File::open(&path)?;
            let mut settings: Self = serde_json::from_reader(file)
                .map_err(|error| Error::Message(format!("Invalid configuration: {error}")))?;
            let parent = fs::canonicalize(path)?.parent().unwrap().to_owned();
            for value in [&mut settings.lapack_library, &mut settings.work_dir]
                .into_iter()
                .flatten()
            {
                if value.is_relative() {
                    *value = parent.join(&value);
                }
            }
            Ok(settings)
        } else if !explicit {
            Ok(Self::default())
        } else {
            Err(Error::Message(format!(
                "Configuration file does not exist: {}",
                path.display()
            )))
        }
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

pub fn output_path(input: &Path) -> Result<PathBuf> {
    // Share the extractor's numeric-extension validation.
    crate::feature_path(input, Path::new("unused"))?;
    let extension = input.extension().unwrap().to_str().unwrap();
    Ok(input.with_extension(format!("clu.{extension}")))
}

pub fn sort(input: &Path, settings: &Settings, log: &mut dyn Write) -> Result<Output> {
    let work = if let Some(parent) = &settings.work_dir {
        tempfile::Builder::new()
            .prefix("fancykk-")
            .tempdir_in(parent)?
    } else {
        tempfile::Builder::new().prefix("fancykk-").tempdir()?
    };
    sort_in(input, settings, work.path(), log)
}

pub fn sort_in(
    input: &Path,
    settings: &Settings,
    work: &Path,
    log: &mut dyn Write,
) -> Result<Output> {
    settings.validate()?;
    let input = fs::canonicalize(input)?;
    let output = output_path(&input)?;
    if output.exists() && !settings.force {
        writeln!(log, "Skipped existing {}", output.display())?;
        return Ok(Output {
            input,
            output,
            skipped: true,
            spikes: 0,
            labels: 0,
            score: None,
            fitting_spikes: 0,
            stride: 1,
        });
    }
    writeln!(log, "Sorting {}", input.display())?;
    let features = crate::extract(&crate::Options {
        input: input.clone(),
        output_base: work.join("recording"),
        info: work.join("count"),
        features: Features::parse(&settings.features)?,
        backend: settings.backend,
        lapack_library: settings.lapack_library.clone(),
        block_spikes: settings.block_spikes,
    })?;
    let store = FeatureStore::create(&features, &work.join("features.bin"))?;
    fs::remove_file(features)?;
    let result = cluster::run(&store, &settings.cluster, work, log)?;
    let mut mapping = [1_u8; cluster::MAX_CLUSTERS];
    let mut labels = 1;
    for (c, &count) in result.counts.iter().enumerate().skip(1) {
        if count > 0 {
            labels += 1;
            mapping[c] = labels as u8;
        }
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".fancykk-")
        .tempfile_in(output.parent().unwrap())?;
    {
        let mut writer = BufWriter::with_capacity(1 << 20, temporary.as_file_mut());
        writeln!(writer, "{labels}")?;
        let mut assignments = BufReader::new(File::open(&result.labels)?);
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
    if settings.force {
        temporary
            .persist(&output)
            .map_err(|error| Error::Io(error.error))?;
    } else {
        temporary.persist_noclobber(&output).map_err(|error| {
            Error::Message(format!(
                "Could not publish {} (another job may have created it): {}",
                output.display(),
                error.error
            ))
        })?;
    }
    writeln!(
        log,
        "Written {}: {} spikes, {} signal clusters + noise; score {:.8}",
        output.display(),
        store.points,
        labels - 1,
        result.score
    )?;
    Ok(Output {
        input,
        output,
        skipped: false,
        spikes: store.points,
        labels,
        score: Some(result.score),
        fitting_spikes: result.fitting_spikes,
        stride: result.stride,
    })
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
        assert!(output_path(Path::new("a.txt")).is_err());
    }
    #[test]
    fn invalid_or_unknown_configuration_is_not_silently_ignored() {
        assert!(serde_json::from_str::<Settings>(r#"{"target_spkies":42}"#).is_err());
        let settings: Settings =
            serde_json::from_str(r#"{"cluster":{"min_clusters":2,"max_clusters":3}}"#).unwrap();
        assert_eq!(settings.cluster.target_spikes, 25_000);
        assert_eq!(settings.cluster.min_clusters, 2);
    }
}
