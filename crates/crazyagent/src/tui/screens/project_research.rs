use super::new_project::harness_name;
use super::new_project_view::{Look, Row, columns, list_row, research_glyph, spread, wrap_words};
use super::project::{REFRESH_MS, plain_problem};
use super::project_team::{Help, Notice, TABS, title, window};
use super::project_time::{clock, duration};
use super::project_view::job_status;
use crate::project_setup::{ResearchJob, ResearchStatus, SetupQuery, SetupView};
use crate::protocol::ProjectId;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;

const WIDE: usize = 96;
const ERROR_LINES: usize = 2;
const CANCEL_NOT_READY: &str = "Stopping research from here is not ready yet.";
const RETRY_NOT_READY: &str = "Running research again from here is not ready yet.";
const LOG_NOT_READY: &str = "The live log is not ready yet.";

/// What the Research view asks its host to do after a key.
/// Ignored lets the host's own keys run; Report opens a finished report in the
/// project's Context tab.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) enum ResearchReply {
    Ignored,
    Handled,
    Report(String),
}

/// The live research jobs of one project with their progress.
/// Takes engine reads through `apply` and keys through `key`; never invents data.
/// Cancel, retry and the live log wait for the engine's lifecycle controls.
pub(in crate::tui) struct ResearchView {
    pub project: ProjectId,
    pub jobs: Option<Vec<ResearchJob>>,
    pub cursor: usize,
    pub notice: Option<Notice>,
    pub now_ms: i64,
    pub offset: i64,
    read_ms: Option<i64>,
    waiting: bool,
}

impl ResearchView {
    /// Opens the research of one project with nothing read yet.
    /// Takes the project.
    /// Returns the view; reads start with `next_query`.
    pub fn new(project: ProjectId) -> Self {
        Self {
            project,
            jobs: None,
            cursor: 0,
            notice: None,
            now_ms: 0,
            offset: 0,
            read_ms: None,
            waiting: false,
        }
    }

    /// Gives the next jobs read while research is still live, at most one at a
    /// time and one per refresh period.
    /// Takes the current time. Returns `SetupQuery::Jobs`, or none.
    pub fn next_query(&mut self, now_ms: i64) -> Option<SetupQuery> {
        self.now_ms = now_ms;
        let due = self.read_ms.is_none_or(|last| now_ms - last >= REFRESH_MS);
        if self.waiting || !due || !self.live() {
            return None;
        }
        self.waiting = true;
        Some(SetupQuery::Jobs {
            project: self.project.clone(),
        })
    }

    /// Forgets the read in flight after the engine connection dropped.
    /// Takes the view; the last jobs stay on screen.
    /// Returns nothing.
    pub fn forget_reads(&mut self) {
        self.waiting = false;
    }

    /// Says whether any research is still queued, running or waiting for a pool.
    /// Takes the view.
    /// Returns true before the first read too, so the first read is sent.
    pub fn live(&self) -> bool {
        self.jobs.as_ref().is_none_or(|jobs| {
            jobs.iter().any(|job| {
                matches!(
                    job.status,
                    ResearchStatus::Queued | ResearchStatus::Running | ResearchStatus::NeedsPool
                )
            })
        })
    }

    /// Applies one jobs read for this project.
    /// Takes the query that was sent and the engine's view or plain problem code.
    /// Returns nothing; other queries and other projects are left alone.
    pub fn apply(&mut self, query: &SetupQuery, result: Result<SetupView, String>) {
        let SetupQuery::Jobs { project } = query else {
            return;
        };
        if project != &self.project {
            return;
        }
        self.waiting = false;
        self.read_ms = Some(self.now_ms);
        match result {
            Ok(SetupView::Jobs(jobs)) => {
                assert!(jobs.iter().all(|job| job.project_id == self.project));
                self.cursor = self.cursor.min(jobs.len().saturating_sub(1));
                self.jobs = Some(jobs);
            }
            Ok(_) => {}
            Err(code) => self.notice = Some(Notice::problem(plain_problem(&code))),
        }
    }

    fn focused(&self) -> Option<&ResearchJob> {
        self.jobs.as_ref()?.get(self.cursor)
    }

    /// Handles one key: move, open a finished report, and the cancel and retry
    /// keys, which say plainly that they are not ready yet.
    /// Takes the key event. Returns what the host should do next.
    pub fn key(&mut self, key: KeyEvent) -> ResearchReply {
        let count = self.jobs.as_ref().map_or(0, Vec::len);
        match key.code {
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(count.saturating_sub(1)),
            KeyCode::Enter => {
                let Some(job) = self.focused() else {
                    return ResearchReply::Handled;
                };
                if job.status == ResearchStatus::Done {
                    return ResearchReply::Report(job.report_path.clone());
                }
                self.notice = Some(Notice::plain(
                    "The report opens here when this research is done.",
                ));
            }
            KeyCode::Char('c') => {
                let running = self.focused().is_some_and(|job| {
                    matches!(
                        job.status,
                        ResearchStatus::Queued
                            | ResearchStatus::Running
                            | ResearchStatus::NeedsPool
                    )
                });
                self.notice = Some(Notice::plain(if running {
                    CANCEL_NOT_READY
                } else {
                    "This research is not running."
                }));
            }
            KeyCode::Char('r') => {
                let stopped = self.focused().is_some_and(|job| {
                    matches!(
                        job.status,
                        ResearchStatus::Failed | ResearchStatus::Cancelled
                    )
                });
                self.notice = Some(Notice::plain(if stopped {
                    RETRY_NOT_READY
                } else {
                    "Only failed or stopped research can run again."
                }));
            }
            _ => return ResearchReply::Ignored,
        }
        ResearchReply::Handled
    }

    /// Draws the Research view for the body of a project tab.
    /// Takes the content width, the rows available and the look.
    /// Returns the body rows and the help keys.
    pub fn rows(&self, content: usize, room: usize, look: &Look) -> (Vec<Row>, Help) {
        let wide = content >= WIDE;
        let mut lines = vec![self.summary(content, look)];
        if wide {
            lines.push(Row::default());
        }
        let help = vec![
            ("\u{2191}\u{2193}", "Move"),
            ("Enter", "Open report"),
            ("c", "Cancel"),
            ("r", "Retry"),
            TABS,
            ("Esc", "Home"),
        ];
        let notice: Vec<Row> = self.notice.iter().map(|notice| notice.row(look)).collect();
        let Some(jobs) = &self.jobs else {
            lines.push(Row::of("Loading...", look.muted));
            lines.extend(notice);
            return (lines, help);
        };
        if jobs.is_empty() {
            lines.push(Row::of(
                "No research yet. Research you pick in New project runs here.",
                look.muted,
            ));
            lines.extend(notice);
            return (lines, help);
        }
        let widths = if wide {
            vec![24, 13, 30, content.saturating_sub(2 + 24 + 13 + 30 + 6)]
        } else {
            vec![23, 24, content.saturating_sub(2 + 23 + 24 + 4)]
        };
        let heads: &[&str] = if wide {
            &["Research", "Harness", "Status", "Progress"]
        } else {
            &["Research", "Status", "Progress"]
        };
        lines.push(
            Row::of("  ", Style::new()).cat(
                columns(
                    &widths,
                    heads
                        .iter()
                        .map(|head| Row::of(*head, look.muted))
                        .collect(),
                )
                .restyle(look.muted),
            ),
        );
        let panel = self.panel(content, look);
        let fixed = lines.len() + 1 + panel.len() + notice.len();
        let size = room.saturating_sub(fixed).max(1);
        let first = window(self.cursor, jobs.len(), size);
        for (index, job) in jobs.iter().enumerate().skip(first).take(size) {
            let mut cells = vec![Row::of(
                format!(
                    "{} {}",
                    look.glyph(research_glyph(job.kind)),
                    job.kind_label
                ),
                look.fg,
            )];
            if wide {
                cells.push(Row::of(harness_name(&job.harness, None), look.fg));
            }
            cells.push(job_status(job, self.now_ms, self.offset, look));
            cells.push(Row::of(progress(job, self.now_ms), look.muted));
            lines.push(list_row(
                columns(&widths, cells),
                content,
                index == self.cursor,
                true,
                look,
            ));
        }
        lines.push(Row::default());
        lines.extend(panel);
        lines.extend(notice);
        (lines, help)
    }

    fn summary(&self, content: usize, look: &Look) -> Row {
        let mut left = Row::of("Research", look.accent);
        let jobs = self.jobs.as_deref().unwrap_or_default();
        let count = |wanted: &[ResearchStatus]| {
            jobs.iter()
                .filter(|job| wanted.contains(&job.status))
                .count()
        };
        for (number, word, style) in [
            (count(&[ResearchStatus::Running]), "running", look.running),
            (count(&[ResearchStatus::Failed]), "failed", look.error),
            (
                count(&[ResearchStatus::Queued, ResearchStatus::NeedsPool]),
                "waiting",
                look.warning,
            ),
            (count(&[ResearchStatus::Done]), "done", look.success),
        ] {
            if number > 0 {
                left = left
                    .push("  ", Style::new())
                    .push(format!("{number} {word}"), style);
            }
        }
        left.pad(content)
    }

    fn panel(&self, content: usize, look: &Look) -> Vec<Row> {
        let Some(job) = self.focused() else {
            return Vec::new();
        };
        let entry =
            |label: &str, value: String| Row::of(label, look.muted).pad(14).push(value, look.fg);
        let when = match (job.status, job.started_ms, job.finished_ms) {
            (ResearchStatus::Failed, _, Some(at)) => {
                format!("Failed at {}", clock(at, self.offset))
            }
            (_, Some(start), Some(end)) => format!(
                "Started {}, took {}",
                clock(start, self.offset),
                duration(end - start)
            ),
            (_, Some(start), None) => format!(
                "Started {}, running {}",
                clock(start, self.offset),
                duration(self.now_ms - start)
            ),
            _ => format!("Queued {}", clock(job.created_ms, self.offset)),
        };
        let mut rows = vec![spread(
            content,
            Row::of(job.kind_label.clone(), look.heading),
            Row::of(when, look.muted),
        )];
        rows.push(entry(
            "Report",
            job.report_path.trim_start_matches("context/").to_owned(),
        ));
        rows.push(entry(
            "Harness",
            format!(
                "{}, tools {}",
                harness_name(&job.harness, None),
                if job.tools { "on" } else { "off" }
            ),
        ));
        rows.push(entry("Output", bytes(job.output_bytes)));
        if job.attempts > 1 {
            rows.push(entry("Tries", job.attempts.to_string()));
        }
        if let Some(error) = job
            .error
            .as_ref()
            .filter(|_| job.status == ResearchStatus::Failed)
        {
            rows.extend(
                wrap_words(error, content)
                    .into_iter()
                    .take(ERROR_LINES)
                    .map(|line| Row::of(line, look.error)),
            );
        }
        rows.push(
            Row::of("Log", look.muted)
                .pad(14)
                .push(LOG_NOT_READY, look.muted),
        );
        rows
    }
}

/// Describes how far one research job got, from what the engine reports.
/// Takes the job and the current time.
/// Returns a short plain line such as "12 KB written, heard 4 s ago".
pub(in crate::tui) fn progress(job: &ResearchJob, now_ms: i64) -> String {
    match job.status {
        ResearchStatus::Running if job.cancel_requested => "Stopping".into(),
        ResearchStatus::Running => {
            let heard = job
                .heartbeat_ms
                .map(|at| format!(", heard {} ago", duration(now_ms - at)))
                .unwrap_or_default();
            format!("{} written{heard}", bytes(job.output_bytes))
        }
        ResearchStatus::Queued => "Waiting for an agent".into(),
        ResearchStatus::NeedsPool => "Assign a pool in Settings".into(),
        ResearchStatus::Done => "Report ready".into(),
        ResearchStatus::Failed => job.error.as_deref().map(title).unwrap_or_default(),
        ResearchStatus::Cancelled => "Stopped".into(),
    }
}

/// Formats a byte count the short way.
/// Takes the count.
/// Returns text such as "512 B", "12 KB" or "3.4 MB".
pub(in crate::tui) fn bytes(count: u64) -> String {
    const KIB: u64 = 1024;
    if count < KIB {
        format!("{count} B")
    } else if count < KIB * KIB {
        format!("{} KB", count / KIB)
    } else {
        format!("{:.1} MB", count as f64 / (KIB * KIB) as f64)
    }
}

#[cfg(test)]
mod tests;
