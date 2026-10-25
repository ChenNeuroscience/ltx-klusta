//! A per-user, crash-recoverable queue shared by independent Explorer invocations.
//! Admission and coordinator handoff use OS locks, released when a process exits.
use crate::progress::{Publisher, WorkProgress};
use crate::queue_progress::Progress;
use crate::{pipeline, Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub input: PathBuf,
    pub phase: Phase,
    pub created_ms: u64,
    #[serde(default)]
    pub started_ms: Option<u64>,
    #[serde(default)]
    pub progress: Option<WorkProgress>,
    pub finished_ms: Option<u64>,
    pub settings: pipeline::Settings,
    pub output: Option<pipeline::Output>,
    pub error: Option<String>,
}

pub struct Queue {
    pub root: PathBuf,
}

pub struct ConsoleResult {
    pub failed: bool,
    pub needs_pause: bool,
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn json_error(error: serde_json::Error) -> Error {
    Error::Message(format!("Queue record: {error}"))
}

fn save(path: &Path, job: &Job) -> Result<()> {
    let mut temporary = tempfile::Builder::new()
        .prefix(".write-")
        .tempfile_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut temporary, job).map_err(json_error)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

impl Queue {
    pub fn default_path() -> Result<PathBuf> {
        if cfg!(windows) {
            Ok(PathBuf::from(
                std::env::var_os("LOCALAPPDATA")
                    .ok_or(Error::Invalid("LOCALAPPDATA is not set"))?,
            )
            .join("ltxk/queue"))
        } else if cfg!(target_os = "macos") {
            Ok(
                PathBuf::from(std::env::var_os("HOME").ok_or(Error::Invalid("HOME is not set"))?)
                    .join("Library/Application Support/ltxk/queue"),
            )
        } else if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
            Ok(PathBuf::from(path).join("ltxk/queue"))
        } else {
            Ok(
                PathBuf::from(std::env::var_os("HOME").ok_or(Error::Invalid("HOME is not set"))?)
                    .join(".local/state/ltxk/queue"),
            )
        }
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        for name in [
            "pending", "running", "done", "failed", "logs", "work", "progress",
        ] {
            fs::create_dir_all(root.join(name))?;
        }
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    fn lock_file(&self, name: &str) -> Result<File> {
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(name))?)
    }
    fn admission(&self) -> Result<File> {
        let file = self.lock_file("admission.lock")?;
        file.lock()?;
        Ok(file)
    }
    fn files(&self, phase: &str) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(self.root.join(phase))? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }
    fn read(path: &Path) -> Result<Job> {
        let mut job: Job = serde_json::from_reader(File::open(path)?).map_err(json_error)?;
        // Progress is advisory and written atomically by this job's worker.
        job.progress = None;
        if let Some(started) = job.started_ms {
            let progress = path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("progress")
                .join(format!("{}.json", job.id));
            if let Ok(file) = File::open(progress) {
                if let Ok(state) = serde_json::from_reader::<_, WorkProgress>(file) {
                    if state.attempt_ms == started {
                        job.progress = Some(state);
                    }
                }
            }
        }
        Ok(job)
    }

    /// Submit atomically under one admission lock. Duplicate active recordings
    /// share a job even if they arrived through separate shell invocations.
    pub fn submit(&self, inputs: &[PathBuf], settings: &pipeline::Settings) -> Result<Vec<String>> {
        settings.validate()?;
        let mut paths = Vec::new();
        for input in inputs {
            let path = fs::canonicalize(input)?;
            pipeline::output_path(&path)?;
            if !path.is_file() {
                return Err(Error::Invalid("Queue input must be a recording file"));
            }
            paths.push(path);
        }
        let _guard = self.admission()?;
        let mut active = Vec::new();
        for phase in ["pending", "running"] {
            for path in self.files(phase)? {
                active.push(Self::read(&path)?);
            }
        }
        let mut ids = Vec::new();
        for input in paths {
            if let Some(job) = active.iter().find(|job| job.input == input) {
                ids.push(job.id.clone());
                continue;
            }
            let mut temporary = tempfile::Builder::new()
                .prefix(&format!("{}-", now()))
                .suffix(".json")
                .tempfile_in(self.root.join("pending"))?;
            let id = temporary
                .path()
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let job = Job {
                id: id.clone(),
                input,
                phase: Phase::Pending,
                created_ms: now(),
                started_ms: None,
                progress: None,
                finished_ms: None,
                settings: settings.clone(),
                output: None,
                error: None,
            };
            serde_json::to_writer_pretty(&mut temporary, &job).map_err(json_error)?;
            temporary.as_file().sync_all()?;
            temporary.keep().map_err(|error| Error::Io(error.error))?;
            active.push(job);
            ids.push(id);
        }
        Ok(ids)
    }

    pub fn records(&self) -> Result<Vec<Job>> {
        let _guard = self.admission()?;
        self.records_unlocked()
    }

    fn records_unlocked(&self) -> Result<Vec<Job>> {
        let mut jobs = Vec::new();
        for phase in ["pending", "running", "done", "failed"] {
            for path in self.files(phase)? {
                jobs.push(Self::read(&path)?);
            }
        }
        jobs.sort_by(|a, b| {
            a.created_ms
                .cmp(&b.created_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(jobs)
    }

    /// One process owns the coordinator lock until all workers finish. The
    /// final empty check and unlock happen under the admission lock, closing
    /// the race between a new submission and an idle coordinator exiting.
    pub fn drain(&self, limit: usize, log: &mut dyn Write) -> Result<bool> {
        let mut progress = Progress::new(self, &[], false)?;
        Ok(self
            .drain_inner(limit, log, &mut progress, true)?
            .unwrap_or(false))
    }

    /// Keep one visible queue session across independent Explorer launches.
    /// None means another visible console already owns this queue.
    pub fn console(
        &self,
        limit: usize,
        requested: &[String],
        log: &mut dyn Write,
    ) -> Result<Option<ConsoleResult>> {
        let visible = self.lock_file("console.lock")?;
        match visible.try_lock() {
            Ok(()) => (),
            Err(TryLockError::WouldBlock) => {
                writeln!(
                    log,
                    "Queued. Progress is shown in the existing ltx-klusta console."
                )?;
                return Ok(None);
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        writeln!(
            log,
            "ltx-klusta queue\nKeep this window open while processing.\nLogs: {}",
            self.root.join("logs").display()
        )?;
        let mut progress = Progress::new(self, requested, true)?;
        // Keep coordinator messages away from the alternate-screen dashboard.
        let mut session_log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("logs/queue.log"))?;
        let log: &mut dyn Write = if progress.interactive() {
            &mut session_log
        } else {
            log
        };
        let mut failed = false;
        let mut following = false;
        let final_records = loop {
            match self.drain_inner(limit, log, &mut progress, false)? {
                Some(result) => failed |= result,
                None if !following => {
                    writeln!(log, "Following the existing queue coordinator...")?;
                    following = true;
                }
                None => (),
            }
            progress.poll(self, log)?;
            {
                let _guard = self.admission()?;
                if self.files("pending")?.is_empty() && self.files("running")?.is_empty() {
                    // Release under admission before the caller waits for Enter.
                    // A new launch can then start a new visible session.
                    let snapshot = self.records_unlocked()?;
                    visible.unlock()?;
                    break snapshot;
                }
            }
            thread::sleep(Duration::from_millis(100));
        };
        progress.finish(self, final_records, log)?;
        Ok(Some(ConsoleResult {
            failed: failed || progress.failed(),
            needs_pause: !progress.interactive(),
        }))
    }

    fn drain_inner(
        &self,
        limit: usize,
        log: &mut dyn Write,
        progress: &mut Progress,
        report_existing: bool,
    ) -> Result<Option<bool>> {
        if !(1..=16).contains(&limit) {
            return Err(Error::Invalid("jobs must be between 1 and 16"));
        }
        let coordinator = self.lock_file("coordinator.lock")?;
        {
            let _guard = self.admission()?;
            match coordinator.try_lock() {
                Ok(()) => (),
                Err(TryLockError::WouldBlock) => {
                    if report_existing {
                        writeln!(log, "An existing coordinator will process these jobs. Use ltxk status to check progress.")?;
                    }
                    return Ok(None);
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
            // Holding this OS lock proves a previous coordinator is gone.
            for path in self.files("running")? {
                let mut job = Self::read(&path)?;
                job.phase = Phase::Pending;
                job.error = None;
                job.started_ms = None;
                job.finished_ms = None;
                save(&path, &job)?;
                fs::rename(
                    &path,
                    self.root.join("pending").join(path.file_name().unwrap()),
                )?;
                writeln!(log, "Recovered interrupted job {}", job.id)?;
            }
        }
        writeln!(
            log,
            "Processing queue {} with up to {limit} jobs",
            self.root.display()
        )?;
        let mut workers: Vec<JoinHandle<Result<bool>>> = Vec::new();
        let mut failed = false;
        loop {
            progress.poll(self, log)?;
            let mut i = 0;
            while i < workers.len() {
                if workers[i].is_finished() {
                    let worker = workers.swap_remove(i);
                    failed |= worker
                        .join()
                        .map_err(|_| Error::Invalid("Queue worker panicked"))??;
                } else {
                    i += 1;
                }
            }
            {
                let _guard = self.admission()?;
                let pending = self.files("pending")?;
                if pending.is_empty() && workers.is_empty() {
                    coordinator.unlock()?;
                    drop(_guard);
                    progress.poll(self, log)?;
                    return Ok(Some(failed));
                }
                for path in pending.into_iter().take(limit - workers.len()) {
                    let mut job = Self::read(&path)?;
                    job.phase = Phase::Running;
                    job.started_ms = Some(now());
                    save(&path, &job)?;
                    fs::rename(
                        &path,
                        self.root.join("running").join(path.file_name().unwrap()),
                    )?;
                    let root = self.root.clone();
                    workers.push(thread::spawn(move || Self::execute(root, job)));
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn execute(root: PathBuf, mut job: Job) -> Result<bool> {
        let queue = Self { root };
        let work = job
            .settings
            .work_dir
            .as_deref()
            .unwrap_or(&queue.root)
            .join("work")
            .join(&job.id);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<pipeline::Output> {
                // A recovered job owns its old private scratch directory too.
                if work.exists() {
                    fs::remove_dir_all(&work).map_err(|error| {
                        Error::Message(format!(
                            "Remove interrupted scratch {}: {error}",
                            work.display()
                        ))
                    })?;
                }
                fs::create_dir_all(&work).map_err(|error| {
                    Error::Message(format!("Create scratch {}: {error}", work.display()))
                })?;
                let mut log =
                    File::create(queue.root.join("logs").join(format!("{}.log", job.id)))?;
                let mut publisher = Publisher::new(
                    queue.root.join("progress").join(format!("{}.json", job.id)),
                    job.started_ms.unwrap(),
                );
                pipeline::cluster_in_observed(
                    &job.input,
                    &job.settings,
                    &work,
                    &mut log,
                    &mut |update| publisher.update(update),
                )
            },
        ));
        let outcome = outcome.unwrap_or(Err(Error::Invalid(
            "Worker panicked; no partial .clu file was published",
        )));
        if work.exists() {
            if let Err(error) = fs::remove_dir_all(&work) {
                if let Ok(mut log) = OpenOptions::new()
                    .append(true)
                    .open(queue.root.join("logs").join(format!("{}.log", job.id)))
                {
                    let _ = writeln!(log, "WARNING: scratch cleanup {}: {error}", work.display());
                }
            }
        }
        match outcome {
            Ok(output) => {
                job.output = Some(output);
                job.phase = Phase::Done;
            }
            Err(error) => {
                let message = error.to_string();
                let path = queue.root.join("logs").join(format!("{}.log", job.id));
                if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(log, "FAILED: {message}");
                }
                job.error = Some(message);
                job.phase = Phase::Failed;
            }
        }
        job.finished_ms = Some(now());
        let failed = job.phase == Phase::Failed;
        let _guard = queue.admission()?;
        let filename = format!("{}.json", job.id);
        let running = queue.root.join("running").join(&filename);
        save(&running, &job)?;
        fs::rename(
            running,
            queue
                .root
                .join(if failed { "failed" } else { "done" })
                .join(filename),
        )?;
        Ok(failed)
    }

    pub fn start_background(&self, limit: usize) -> Result<()> {
        use std::process::{Command, Stdio};
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("runner.log"))?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["resume", "--jobs", &limit.to_string(), "--queue-dir"])
            .arg(&self.root)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        command.spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn submissions_deduplicate_active_paths_and_coordinator_handoff_is_nonblocking() {
        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("a ! λ.1");
        fs::write(&input, "malformed").unwrap();
        let queue = Queue::open(temporary.path().join("queue")).unwrap();
        let settings = pipeline::Settings::default();
        let first = queue
            .submit(std::slice::from_ref(&input), &settings)
            .unwrap();
        assert_eq!(first, queue.submit(&[input], &settings).unwrap());
        let lock = queue.lock_file("coordinator.lock").unwrap();
        lock.lock().unwrap();
        assert!(!queue.drain(4, &mut Vec::new()).unwrap());
        assert_eq!(queue.records().unwrap().len(), 1);
    }
}
