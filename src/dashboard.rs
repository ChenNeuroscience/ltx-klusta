//! Interactive queue display; workers publish observations without a pseudo-terminal.
use crate::queue::{now, Job, Phase};
use crate::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub(crate) struct Dashboard {
    terminal: DefaultTerminal,
    table: TableState,
    selected: Option<String>,
    notice: String,
}

pub(crate) fn elapsed(job: &Job, current_ms: u64) -> String {
    let Some(started) = job.started_ms else {
        return "—".into();
    };
    let seconds = job
        .finished_ms
        .unwrap_or(current_ms)
        .saturating_sub(started)
        / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

fn started(job: &Job) -> String {
    job.started_ms
        .and_then(|ms| DateTime::from_timestamp_millis(ms as i64))
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string()
        })
        .unwrap_or_else(|| "Not started".into())
}

fn stage(job: &Job) -> &str {
    match job.phase {
        Phase::Pending => "Waiting",
        Phase::Running => job
            .progress
            .as_ref()
            .map_or("Preparing", |p| p.stage.label()),
        Phase::Done if job.output.as_ref().is_some_and(|o| o.skipped) => "Skipped",
        Phase::Done => "Done",
        Phase::Failed => "FAILED",
    }
}

fn draw(
    frame: &mut Frame,
    jobs: &[Job],
    summary: &str,
    finished: bool,
    table: &mut TableState,
    notice: &str,
    current_ms: u64,
) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(7),
        Constraint::Length(2),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(summary).block(Block::bordered().title(if finished {
            " ltx-klusta · Queue complete "
        } else {
            " ltx-klusta · Processing queue "
        })),
        areas[0],
    );
    let rows = jobs.iter().map(|job| {
        let p = job.progress.as_ref();
        let trial = p
            .filter(|p| p.trials > 0)
            .map_or_else(|| "—".into(), |p| format!("{}/{}", p.trial, p.trials));
        let iteration = p
            .filter(|p| p.iteration > 0)
            .map_or_else(|| "—".into(), |p| p.iteration.to_string());
        let clusters = job
            .output
            .as_ref()
            .filter(|o| !o.skipped)
            .map(|o| o.labels.saturating_sub(1))
            .or_else(|| p.and_then(|p| p.clusters))
            .map_or_else(|| "—".into(), |n| n.to_string());
        let color = match job.phase {
            Phase::Failed => Color::Red,
            Phase::Running => Color::Cyan,
            Phase::Done => Color::Green,
            Phase::Pending => Color::Gray,
        };
        Row::new(vec![
            job.input
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            stage(job).into(),
            trial,
            iteration,
            clusters,
            elapsed(job, current_ms),
        ])
        .style(Style::default().fg(color))
    });
    let widget = Table::new(
        rows,
        [
            Constraint::Min(15),
            Constraint::Length(17),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Length(9),
        ],
    )
    .header(
        Row::new(["Recording", "Stage", "Trial", "Step", "Clusters", "Elapsed"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::bordered())
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("› ");
    frame.render_stateful_widget(widget, areas[1], table);
    let detail = table.selected().and_then(|i| jobs.get(i)).map_or_else(
        || "No recordings in this session. Drop files onto the launcher to add them.".into(), |job| {
            let p = job.progress.as_ref();
            let stats = p.filter(|p| p.spikes > 0).map_or_else(String::new, |p| format!(
                "Spikes: {} · fitting: {} · initial clusters this trial: {}{}", p.spikes, p.fitting_spikes,
                p.initial_clusters, if p.assigned > 0 { format!(" · assigned: {}", p.assigned) } else { String::new() }));
            format!("{}\nStarted: {} · Elapsed: {}\n{}\n{}", job.input.display(), started(job),
                elapsed(job, current_ms), stats, job.error.as_deref().unwrap_or(
                    "The Clusters column excludes noise. Trial and step describe the current fit; they are not an ETA."))
        });
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" Selected recording ")),
        areas[2],
    );
    let keys = if finished {
        "↑/↓ select · L open log · O logs folder · Enter/Q close"
    } else {
        "↑/↓ select · L open log · O logs folder · Ctrl+C interrupt (resume to recover)"
    };
    frame.render_widget(Paragraph::new(format!("{keys}\n{notice}")), areas[3]);
}

impl Dashboard {
    pub(crate) fn try_new() -> Option<Self> {
        if !io::stdin().is_terminal()
            || !io::stdout().is_terminal()
            || !io::stderr().is_terminal()
            || std::env::var_os("TERM").is_some_and(|v| v == "dumb")
        {
            return None;
        }
        match ratatui::try_init() {
            Ok(terminal) => Some(Self {
                terminal,
                table: TableState::default(),
                selected: None,
                notice: String::new(),
            }),
            Err(_) => {
                let _ = ratatui::try_restore();
                None
            }
        }
    }

    /// Returns true only when the user dismisses a completed queue.
    pub(crate) fn update(
        &mut self,
        jobs: &[Job],
        summary: &str,
        logs: &Path,
        finished: bool,
    ) -> Result<bool> {
        let mut jobs = jobs.to_vec();
        jobs.sort_by_key(|j| {
            (
                match j.phase {
                    Phase::Running => 0,
                    Phase::Pending => 1,
                    Phase::Failed => 2,
                    Phase::Done => 3,
                },
                j.created_ms,
                j.id.clone(),
            )
        });
        self.table.select(
            self.selected
                .as_ref()
                .and_then(|id| jobs.iter().position(|j| &j.id == id))
                .or(if jobs.is_empty() { None } else { Some(0) }),
        );
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Exiting kills workers and releases OS locks. The next launch recovers their records.
                        let _ = ratatui::try_restore();
                        std::process::exit(130);
                    }
                    KeyCode::Enter | KeyCode::Char('q') if finished => return Ok(true),
                    KeyCode::Down if !jobs.is_empty() => self.table.select(Some(
                        (self.table.selected().unwrap_or(0) + 1).min(jobs.len() - 1),
                    )),
                    KeyCode::Up if !jobs.is_empty() => self
                        .table
                        .select(Some(self.table.selected().unwrap_or(0).saturating_sub(1))),
                    KeyCode::Char('o' | 'O') => self.open(logs, false),
                    KeyCode::Char('l' | 'L') => {
                        if let Some(job) = self.table.selected().and_then(|i| jobs.get(i)) {
                            self.open(&logs.join(format!("{}.log", job.id)), true);
                        }
                    }
                    _ => (),
                }
            }
        }
        self.selected = self
            .table
            .selected()
            .and_then(|i| jobs.get(i))
            .map(|j| j.id.clone());
        self.terminal.draw(|frame| {
            draw(
                frame,
                &jobs,
                summary,
                finished,
                &mut self.table,
                &self.notice,
                now(),
            )
        })?;
        Ok(false)
    }

    fn open(&mut self, path: &Path, text: bool) {
        let mut command;
        #[cfg(target_os = "macos")]
        {
            command = Command::new("/usr/bin/open");
            if text {
                command.arg("-t");
            }
        }
        #[cfg(windows)]
        {
            command = Command::new(if text { "notepad.exe" } else { "explorer.exe" });
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = text;
            command = Command::new("xdg-open");
        }
        match command
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.notice = format!("Opened {}", path.display());
            }
            Err(error) => self.notice = format!("Could not open {}: {error}", path.display()),
        }
    }
}
impl Drop for Dashboard {
    fn drop(&mut self) {
        let _ = ratatui::try_restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_progress_and_freeze_completed_runtime() {
        let job = Job {
            id: "test".into(),
            input: "recording.1".into(),
            phase: Phase::Done,
            created_ms: 1000,
            started_ms: Some(2000),
            finished_ms: Some(67000),
            settings: Default::default(),
            output: None,
            error: None,
            progress: None,
        };
        assert_eq!(elapsed(&job, 999999), "1:05");
        let mut running = job.clone();
        running.finished_ms = None;
        assert_eq!(elapsed(&running, 3603000), "1:00:01");
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &[job],
                    "1 done",
                    true,
                    &mut TableState::default().with_selected(0),
                    "",
                    999999,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
        for expected in [
            "Queue complete",
            "recording.1",
            "Started:",
            "Elapsed: 1:05",
            "L open log",
        ] {
            assert!(text.contains(expected), "Missing {expected}: {text}");
        }
    }
}
