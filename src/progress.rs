//! Structured, advisory job progress. It never selects data or changes model calculations.
use crate::{queue::now, Error, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    Preparing,
    Extracting,
    Features,
    Loading,
    Fitting,
    Splitting,
    Assigning,
    Publishing,
}
impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing",
            Self::Extracting => "PCA / features",
            Self::Features => "Feature cache",
            Self::Loading => "Loading subset",
            Self::Fitting => "Fitting",
            Self::Splitting => "Split search",
            Self::Assigning => "Assigning spikes",
            Self::Publishing => "Writing output",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkProgress {
    pub attempt_ms: u64,
    pub updated_ms: u64,
    pub stage: Stage,
    pub spikes: usize,
    pub fitting_spikes: usize,
    pub trial: usize,
    pub trials: usize,
    pub initial_clusters: usize,
    pub iteration: usize,
    pub clusters: Option<usize>,
    pub assigned: usize,
    pub noise: Option<usize>,
}

pub(crate) enum Update {
    Stage(Stage),
    Dataset {
        spikes: usize,
        fitting_spikes: usize,
    },
    Trial {
        current: usize,
        total: usize,
        initial: usize,
    },
    Iteration {
        iteration: usize,
        clusters: usize,
        splitting: bool,
    },
    Assigned {
        count: usize,
    },
    TrialDone {
        clusters: usize,
        noise: usize,
    },
}

pub(crate) struct Publisher {
    path: PathBuf,
    state: WorkProgress,
    last_write: Instant,
}
impl Publisher {
    pub(crate) fn new(path: PathBuf, attempt_ms: u64) -> Self {
        Self {
            path,
            state: WorkProgress {
                attempt_ms,
                ..Default::default()
            },
            last_write: Instant::now(),
        }
    }

    pub(crate) fn update(&mut self, update: Update) -> Result<()> {
        let previous_stage = self.state.stage;
        let force = match update {
            Update::Stage(stage) => {
                self.state.stage = stage;
                true
            }
            Update::Dataset {
                spikes,
                fitting_spikes,
            } => {
                self.state.spikes = spikes;
                self.state.fitting_spikes = fitting_spikes;
                true
            }
            Update::Trial {
                current,
                total,
                initial,
            } => {
                self.state.stage = Stage::Loading;
                self.state.trial = current;
                self.state.trials = total;
                self.state.initial_clusters = initial;
                self.state.iteration = 0;
                self.state.clusters = None;
                self.state.noise = None;
                self.state.assigned = 0;
                true
            }
            Update::Iteration {
                iteration,
                clusters,
                splitting,
            } => {
                self.state.stage = if splitting {
                    Stage::Splitting
                } else {
                    Stage::Fitting
                };
                self.state.iteration = iteration;
                self.state.clusters = Some(clusters);
                false
            }
            Update::Assigned { count } => {
                self.state.assigned = count;
                count == self.state.spikes
            }
            Update::TrialDone { clusters, noise } => {
                self.state.clusters = Some(clusters);
                self.state.noise = Some(noise);
                true
            }
        };
        if !force
            && previous_stage == self.state.stage
            && self.last_write.elapsed() < Duration::from_millis(250)
        {
            return Ok(());
        }
        self.state.updated_ms = now();
        let mut temporary = tempfile::Builder::new()
            .prefix(".progress-")
            .tempfile_in(self.path.parent().unwrap())?;
        serde_json::to_writer(&mut temporary, &self.state)
            .map_err(|e| Error::Message(e.to_string()))?;
        temporary.flush()?;
        // Progress is advisory: atomic visibility is needed, disk fsync is not.
        temporary
            .persist(&self.path)
            .map_err(|e| Error::Io(e.error))?;
        self.last_write = Instant::now();
        Ok(())
    }
}
