//! Read job logs incrementally for foreground queue sessions and attached viewers.
use crate::dashboard::{elapsed, Dashboard};
use crate::queue::{now, Job, Phase, Queue};
use crate::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Tail {
    offset: u64,
    pending: Vec<u8>,
}
impl Tail {
    fn lines(&mut self, path: &Path) -> io::Result<Vec<String>> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() < self.offset {
            *self = Self::default();
        }
        file.seek(SeekFrom::Start(self.offset))?;
        self.offset += file.read_to_end(&mut self.pending)? as u64;
        let Some(end) = self.pending.iter().rposition(|&b| b == b'\n') else {
            return Ok(Vec::new());
        };
        let complete: Vec<_> = self.pending.drain(..=end).collect();
        Ok(String::from_utf8_lossy(&complete)
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

struct Entry {
    number: usize,
    phase: Option<Phase>,
    started_ms: Option<u64>,
    tail: Tail,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    waiting: usize,
    running: usize,
    done: usize,
    skipped: usize,
    failed: usize,
}
impl Counts {
    fn description(self) -> String {
        format!(
            "{} running, {} waiting, {} done ({} skipped), {} failed",
            self.running, self.waiting, self.done, self.skipped, self.failed
        )
    }
}

pub(crate) struct Progress {
    ignored: BTreeSet<String>,
    entries: BTreeMap<String, Entry>,
    counts: Counts,
    last_counts: Option<Counts>,
    heartbeat: Instant,
    title: bool,
    dashboard: Option<Dashboard>,
    limit: usize,
}
impl Progress {
    pub(crate) fn new(
        queue: &Queue,
        requested: &[String],
        title: bool,
        limit: usize,
    ) -> Result<Self> {
        let ignored = queue
            .records()?
            .into_iter()
            .filter(|job| {
                matches!(job.phase, Phase::Done | Phase::Failed) && !requested.contains(&job.id)
            })
            .map(|job| job.id)
            .collect();
        Ok(Self {
            ignored,
            entries: BTreeMap::new(),
            counts: Counts::default(),
            last_counts: None,
            heartbeat: Instant::now(),
            title,
            dashboard: if title { Dashboard::try_new() } else { None },
            limit,
        })
    }

    pub(crate) fn poll(&mut self, queue: &Queue, log: &mut dyn Write) -> Result<()> {
        self.render(queue, queue.records()?, log, false).map(|_| ())
    }

    fn render(
        &mut self,
        queue: &Queue,
        snapshot: Vec<Job>,
        log: &mut dyn Write,
        finished: bool,
    ) -> Result<bool> {
        let jobs: Vec<Job> = snapshot
            .into_iter()
            .filter(|job| !self.ignored.contains(&job.id))
            .collect();
        let mut counts = Counts::default();
        for job in &jobs {
            match job.phase {
                Phase::Pending => counts.waiting += 1,
                Phase::Running => counts.running += 1,
                Phase::Done => {
                    counts.done += 1;
                    counts.skipped += usize::from(job.output.as_ref().is_some_and(|o| o.skipped));
                }
                Phase::Failed => counts.failed += 1,
            }
        }
        self.counts = counts;
        if let Some(dashboard) = &mut self.dashboard {
            if self.last_counts != Some(counts) {
                set_title(&format!("ltx-klusta: {}", counts.description()));
            }
            self.last_counts = Some(counts);
            return dashboard.update(
                &jobs,
                &format!("{} workers max · {}", self.limit, counts.description()),
                &queue.root.join("logs"),
                finished,
            );
        }
        for job in &jobs {
            let next = self.entries.len() + 1;
            let entry = self.entries.entry(job.id.clone()).or_insert_with(|| Entry {
                number: next,
                phase: None,
                started_ms: None,
                tail: Tail::default(),
            });
            if entry.phase.is_none() {
                writeln!(
                    log,
                    "[{}] {}\n[{}] Log: {}",
                    entry.number,
                    job.input.display(),
                    entry.number,
                    queue
                        .root
                        .join("logs")
                        .join(format!("{}.log", job.id))
                        .display()
                )?;
            }
            if entry.started_ms != job.started_ms {
                entry.tail = Tail::default();
                entry.started_ms = job.started_ms;
            }
            if entry.phase.as_ref() != Some(&job.phase) {
                let state = match job.phase {
                    Phase::Pending => "Waiting",
                    Phase::Running => "Running",
                    Phase::Done if job.output.as_ref().is_some_and(|o| o.skipped) => {
                        "Skipped (output already exists)"
                    }
                    Phase::Done => "Done",
                    Phase::Failed => "FAILED",
                };
                writeln!(
                    log,
                    "[{}] {state} · elapsed {}",
                    entry.number,
                    elapsed(job, now())
                )?;
                if job.phase == Phase::Failed {
                    if let Some(error) = &job.error {
                        writeln!(log, "[{}] {error}", entry.number)?;
                    }
                }
                entry.phase = Some(job.phase.clone());
            }
            for line in entry
                .tail
                .lines(&queue.root.join("logs").join(format!("{}.log", job.id)))?
            {
                writeln!(log, "[{}] {line}", entry.number)?;
            }
        }
        if self.last_counts != Some(counts) || self.heartbeat.elapsed() >= Duration::from_secs(5) {
            let description = counts.description();
            writeln!(log, "Queue: {description}")?;
            for job in jobs.iter().filter(|job| job.phase == Phase::Running) {
                if job.started_ms.is_some() {
                    writeln!(
                        log,
                        "[{}] Elapsed {}",
                        self.entries[&job.id].number,
                        elapsed(job, now())
                    )?;
                }
            }
            if self.title {
                set_title(&format!("ltx-klusta: {description}"));
            }
            self.last_counts = Some(counts);
            self.heartbeat = Instant::now();
        }
        Ok(false)
    }

    pub(crate) fn finish(
        &mut self,
        queue: &Queue,
        mut snapshot: Vec<Job>,
        log: &mut dyn Write,
    ) -> Result<bool> {
        loop {
            if snapshot
                .iter()
                .any(|job| matches!(job.phase, Phase::Pending | Phase::Running))
            {
                return Ok(false);
            }
            let dismissed = self.render(queue, snapshot, log, true)?;
            if dismissed || !self.interactive() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            snapshot = queue.records()?;
        }
        writeln!(
            log,
            "Queue finished: {}\nLogs: {}",
            self.counts.description(),
            queue.root.join("logs").display()
        )?;
        Ok(true)
    }

    pub(crate) fn failed(&self) -> bool {
        self.counts.failed > 0
    }

    pub(crate) fn interactive(&self) -> bool {
        self.dashboard.is_some()
    }
}

#[cfg(windows)]
fn set_title(title: &str) {
    #[link(name = "Kernel32")]
    extern "system" {
        fn SetConsoleTitleW(title: *const u16) -> i32;
    }
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    // The buffer is NUL-terminated and lives through this synchronous call.
    unsafe {
        SetConsoleTitleW(wide.as_ptr());
    }
}
#[cfg(not(windows))]
fn set_title(_: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log_tail_waits_for_complete_utf8_lines_and_handles_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job.log");
        let mut tail = Tail::default();
        std::fs::write(&path, b"start \xce").unwrap();
        assert!(tail.lines(&path).unwrap().is_empty());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"\xbb\nsecond line\n").unwrap();
        assert_eq!(tail.lines(&path).unwrap(), ["start λ", "second line"]);
        assert!(tail.lines(&path).unwrap().is_empty());
        std::fs::write(&path, b"retry\n").unwrap();
        assert_eq!(tail.lines(&path).unwrap(), ["retry"]);
    }
}
