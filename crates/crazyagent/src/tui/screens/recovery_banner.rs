use super::new_project_input::cells;
use super::new_project_view::{Look, Row, wrap_words};
use super::project::plain_problem;
use crate::engine::owner::{BannerNotice, BannerRun, NoticeShown, RecoveryBanner, RunShown};
use crate::ledger::ActionKind;
use crate::protocol::ProjectId;
use crate::tui::client::recovery::{RecoveryEvent, RecoveryQuery, RecoveryReply};
use crate::tui::keys::Key;

#[cfg(all(test, unix))]
mod tests;

const POLL_MS: i64 = 5_000;
const GROUP_WIDTH: usize = 13;
const GROUP_ITEMS_SHOWN: usize = 2;
const GROUP_LINES_MAX: usize = 2;
const QUIET_CODES: [&str; 2] = ["trusted_connection_unavailable", "not_ready"];

/// One item the owner can act on in the banner: a failed resume by its agent, or a notice
/// by its ledger kind and number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Run(i64),
    Notice(ActionKind, i64),
}

/// One project's interrupted work above its tabs: what an engine stop cut short, what
/// resumed, what failed, and the ledger notices that wait for the owner. Ctrl-N picks the
/// next failed resume or notice; Ctrl-R retries the picked failed resume and Ctrl-D
/// dismisses the picked notice, the first of each until one is picked. On a delete cut
/// short Ctrl-D keeps the project and Ctrl-X asks for the delete again, which the project
/// page opens. Shows nothing once every run resumed and every notice is dismissed. Sends
/// one request at a time.
pub(in crate::tui) struct ProjectRecovery {
    project: ProjectId,
    banner: Option<RecoveryBanner>,
    problem: Option<String>,
    note: Option<String>,
    picked: Option<Pick>,
    queued: Option<RecoveryQuery>,
    sent: Option<RecoveryQuery>,
    read_ms: Option<i64>,
    delete_again: bool,
}

impl ProjectRecovery {
    /// Opens one project's banner with nothing read yet. Input: the project.
    pub(in crate::tui) fn new(project: ProjectId) -> Self {
        Self {
            project,
            banner: None,
            problem: None,
            note: None,
            picked: None,
            queued: None,
            sent: None,
            read_ms: None,
            delete_again: false,
        }
    }

    /// Whether the owner asked for a delete cut short to run again since the last call, which
    /// the project page answers by opening its remove ask. Output: true once per Ctrl-X.
    pub(in crate::tui) fn take_delete_again(&mut self) -> bool {
        let asked = std::mem::take(&mut self.delete_again);
        assert!(!self.delete_again);
        asked
    }

    /// The next request to send, once nothing is in flight: an owner action first, then a
    /// read when none was made, after an action, or every POLL_MS while a run still waits to
    /// resume. Input: now in ms. Output: the request, or none.
    pub(in crate::tui) fn next_query(&mut self, now_ms: i64) -> Option<RecoveryQuery> {
        if self.sent.is_some() {
            return None;
        }
        let query = match self.queued.take() {
            Some(action) => action,
            None => {
                let due = match self.read_ms {
                    None => true,
                    Some(at) => self.resuming() && now_ms.saturating_sub(at) >= POLL_MS,
                };
                if !due {
                    return None;
                }
                self.read_ms = Some(now_ms);
                RecoveryQuery::Read {
                    project: self.project.clone(),
                }
            }
        };
        self.sent = Some(query.clone());
        Some(query)
    }

    /// Takes the engine's answer to the request in flight; any other reply is ignored.
    /// A done action is followed by a fresh read. Input: the event.
    pub(in crate::tui) fn apply(&mut self, event: &RecoveryEvent) {
        if self.sent.as_ref() != Some(&event.query) {
            return;
        }
        self.sent = None;
        match (&event.query, &event.result) {
            (_, Ok(RecoveryReply::Banner(banner))) => {
                self.banner = Some(banner.clone());
                self.problem = None;
            }
            (RecoveryQuery::Read { .. }, Err(code)) => {
                self.problem = (!QUIET_CODES.contains(&code.as_str())).then(|| sentence(code));
            }
            (query, Ok(RecoveryReply::Done)) => {
                self.note = Some(done_note(query, self.banner.as_ref()));
                self.read_ms = None;
            }
            (_, Err(code)) => {
                self.note = Some(sentence(code));
                self.read_ms = None;
            }
        }
    }

    /// Forgets the request in flight when the connection is lost, so the next connection
    /// reads again.
    pub(in crate::tui) fn forget_reads(&mut self) {
        self.sent = None;
        self.queued = None;
        self.read_ms = None;
    }

    /// Whether a request is in flight.
    pub(in crate::tui) fn busy(&self) -> bool {
        self.sent.is_some() || self.queued.is_some()
    }

    /// Handles Ctrl-R (retry the picked failed resume), Ctrl-D (dismiss the picked notice),
    /// Ctrl-X on a picked delete cut short (ask for the delete again, sending nothing) and,
    /// with more than one of them, Ctrl-N (pick the next failed resume or notice) while the
    /// banner shows one. Input: the key. Output: whether the key was the banner's.
    pub(in crate::tui) fn key(&mut self, key: &Key) -> bool {
        let Some(banner) = self.banner.as_ref().filter(|banner| open(banner)) else {
            return false;
        };
        if *key == Key::Ctrl('x') {
            let cut_short = self
                .dismissed(banner)
                .is_some_and(|notice| notice.action_kind == ActionKind::ProjectDelete);
            self.delete_again = cut_short;
            assert!(!self.delete_again || !banner.notices.is_empty());
            return cut_short;
        }
        let action = match key {
            Key::Ctrl('r') => self.retried(banner).map(|run| RecoveryQuery::Retry {
                project: self.project.clone(),
                agent: run.agent,
            }),
            Key::Ctrl('d') => self.dismissed(banner).map(|notice| RecoveryQuery::Dismiss {
                project: self.project.clone(),
                kind: notice.action_kind,
                id: notice.id,
            }),
            Key::Ctrl('n') => {
                let picks = picks(banner);
                if picks.len() < 2 {
                    return false;
                }
                let at = self
                    .picked
                    .and_then(|picked| picks.iter().position(|pick| *pick == picked))
                    .unwrap_or(0);
                assert!(at < picks.len());
                self.picked = Some(picks[(at + 1) % picks.len()]);
                self.note = None;
                return true;
            }
            _ => return false,
        };
        if self.busy() {
            self.note = Some("Still working on the last choice. Try again in a moment.".into());
        } else if let Some(action) = action {
            self.note = None;
            self.queued = Some(action);
        }
        true
    }

    /// The failed resume Ctrl-R retries: the picked one while it is still failed, else the
    /// first. Input: the banner. Output: the run, or none when nothing failed.
    fn retried<'a>(&self, banner: &'a RecoveryBanner) -> Option<&'a BannerRun> {
        match self.picked {
            Some(Pick::Run(agent)) => banner
                .runs
                .iter()
                .find(|run| run.agent == agent && run.state == RunShown::Failed),
            _ => None,
        }
        .or_else(|| first_failed(banner))
    }

    /// The notice Ctrl-D dismisses: the picked one while it is still listed, else the first.
    /// Input: the banner. Output: the notice, or none when there is none.
    fn dismissed<'a>(&self, banner: &'a RecoveryBanner) -> Option<&'a BannerNotice> {
        match self.picked {
            Some(Pick::Notice(kind, id)) => banner
                .notices
                .iter()
                .find(|notice| notice.action_kind == kind && notice.id == id),
            _ => None,
        }
        .or_else(|| banner.notices.first())
    }

    fn resuming(&self) -> bool {
        self.banner.as_ref().is_some_and(|banner| {
            banner
                .runs
                .iter()
                .any(|run| matches!(run.state, RunShown::Interrupted | RunShown::Resuming))
        })
    }

    /// The banner's rows: a heading, one line or two per group with items, what the owner
    /// can do and the result of the last choice; a problem line when the read failed; none
    /// when nothing waits for the owner. While the Conversation composer holds the keys the
    /// key line says Tab comes first. Inputs: the width, the look and whether the composer
    /// holds the keys. Output: the rows.
    pub(in crate::tui) fn rows(&self, width: usize, look: &Look, composer: bool) -> Vec<Row> {
        let Some(banner) = self.banner.as_ref().filter(|banner| open(banner)) else {
            return match &self.problem {
                Some(problem) => vec![
                    Row::of(
                        format!("Interrupted work couldn't be read. {problem}"),
                        look.warning,
                    ),
                    Row::default(),
                ],
                None => Vec::new(),
            };
        };
        let waiting = banner
            .runs
            .iter()
            .filter(|run| run.state != RunShown::Resumed)
            .count()
            .saturating_add(banner.notices.len())
            .saturating_add(usize::try_from(banner.more).unwrap_or(usize::MAX));
        let mut rows = vec![
            Row::of("Interrupted work", look.heading)
                .push(format!("  {waiting} to resolve"), look.muted),
        ];
        let runs = |shown: &[RunShown]| -> Vec<&BannerRun> {
            banner
                .runs
                .iter()
                .filter(|run| shown.contains(&run.state))
                .collect()
        };
        let groups: [(&str, Vec<String>, ratatui::style::Style); 4] = [
            (
                "Interrupted",
                runs(&[RunShown::Interrupted, RunShown::Resuming])
                    .into_iter()
                    .map(interrupted_item)
                    .collect(),
                look.warning,
            ),
            (
                "Resumed",
                runs(&[RunShown::Resumed])
                    .into_iter()
                    .map(run_name)
                    .collect(),
                look.success,
            ),
            (
                "Failed",
                runs(&[RunShown::Failed])
                    .into_iter()
                    .map(failed_item)
                    .collect(),
                look.error,
            ),
            (
                "Check",
                banner.notices.iter().map(notice_item).collect(),
                look.warning,
            ),
        ];
        let text_width = width.saturating_sub(GROUP_WIDTH).max(1);
        for (label, items, style) in groups {
            if items.is_empty() {
                continue;
            }
            let mut text = items
                .iter()
                .take(GROUP_ITEMS_SHOWN)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            if items.len() > GROUP_ITEMS_SHOWN {
                text.push_str(&format!("; {} more", items.len() - GROUP_ITEMS_SHOWN));
            }
            for (index, line) in wrap_words(&text, text_width)
                .into_iter()
                .take(GROUP_LINES_MAX)
                .enumerate()
            {
                let lead = if index == 0 { label } else { "" };
                rows.push(Row::of(lead, style).pad(GROUP_WIDTH).push(line, look.fg));
            }
        }
        let mut keys = Vec::new();
        if let Some(run) = self.retried(banner) {
            keys.push(format!("Ctrl-R Retry {}", run.label));
        }
        match self.dismissed(banner) {
            Some(notice) if notice.action_kind == ActionKind::ProjectDelete => {
                keys.push("Ctrl-D Keep the project".to_owned());
                keys.push("Ctrl-X Delete again".to_owned());
            }
            Some(notice) => keys.push(format!("Ctrl-D Dismiss {}", notice_subject(notice))),
            None => {}
        }
        if picks(banner).len() > 1 {
            keys.push("Ctrl-N Next item".to_owned());
        }
        if !keys.is_empty() {
            let lead = if composer { "Tab first, then " } else { "" };
            for line in key_lines(lead, &keys, width) {
                rows.push(Row::of(line, look.accent));
            }
        }
        if let Some(note) = &self.note {
            rows.push(Row::of(note.clone(), look.muted));
        }
        rows.push(Row::default());
        rows
    }
}

/// Lays the key hints out in lines no wider than the banner: the lead, then the hints three
/// spaces apart, breaking only between hints so each key stays beside its action; a lead or
/// hint wider than a whole line wraps by word. Inputs: the lead, the hints and the width in
/// cells. Output: the lines.
fn key_lines(lead: &str, keys: &[String], width: usize) -> Vec<String> {
    let mut lines = wrap_words(lead, width);
    let mut line = lines.pop().unwrap_or_default();
    let mut gap = " ";
    for key in keys {
        let joined = if line.is_empty() {
            cells(key)
        } else {
            cells(&line) + cells(gap) + cells(key)
        };
        if joined <= width {
            if !line.is_empty() {
                line.push_str(gap);
            }
            line.push_str(key);
        } else {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut parts = wrap_words(key, width);
            line = parts.pop().unwrap_or_default();
            lines.extend(parts);
        }
        gap = "   ";
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Whether a banner holds anything for the owner to resolve.
fn open(banner: &RecoveryBanner) -> bool {
    !banner.notices.is_empty()
        || banner.more > 0
        || banner.runs.iter().any(|run| run.state != RunShown::Resumed)
}

fn first_failed(banner: &RecoveryBanner) -> Option<&BannerRun> {
    banner.runs.iter().find(|run| run.state == RunShown::Failed)
}

/// Every item the owner can act on, in the order Ctrl-N walks them: the failed resumes,
/// then the notices. Input: the banner. Output: the items.
fn picks(banner: &RecoveryBanner) -> Vec<Pick> {
    banner
        .runs
        .iter()
        .filter(|run| run.state == RunShown::Failed)
        .map(|run| Pick::Run(run.agent))
        .chain(
            banner
                .notices
                .iter()
                .map(|notice| Pick::Notice(notice.action_kind, notice.id)),
        )
        .collect()
}

fn run_name(run: &BannerRun) -> String {
    format!("{} ({})", run.label, run.harness)
}

fn interrupted_item(run: &BannerRun) -> String {
    let mut item = format!("{}, turn {}", run_name(run), run.turn);
    if let Some(asked) = &run.asked {
        item.push_str(&format!(": \"{asked}\""));
    }
    item.push_str(match (run.state, run.paused) {
        (RunShown::Resuming, _) => ", resuming now",
        (_, true) => ", resumes when you resume the team",
        _ => ", resumes on its own",
    });
    item
}

fn failed_item(run: &BannerRun) -> String {
    match &run.reason {
        Some(reason) => format!("{}: {reason}", run_name(run)),
        None => run_name(run),
    }
}

/// Names what one notice is about: an agent's start or worktree, or a change set.
fn notice_subject(notice: &BannerNotice) -> String {
    let agent = notice.label.as_deref().unwrap_or("an agent");
    let set = notice.change_set.as_deref().unwrap_or("a change set");
    match notice.action_kind {
        ActionKind::AgentSpawn => format!("{agent}'s start"),
        ActionKind::WorktreeCreate => format!("{agent}'s worktree"),
        ActionKind::LandingAccept => format!("change set {set}"),
        ActionKind::ProjectDelete => "this project's delete".into(),
        ActionKind::Message | ActionKind::Dispatch => "this notice".into(),
    }
}

fn notice_item(notice: &BannerNotice) -> String {
    let subject = notice_subject(notice);
    match (notice.action_kind, notice.shown) {
        (ActionKind::ProjectDelete, _) => {
            "A delete was cut short. Some folders may already be in the Trash.".into()
        }
        (ActionKind::AgentSpawn, NoticeShown::Unknown) => {
            format!("{subject} may or may not have run; check the agent first")
        }
        (ActionKind::WorktreeCreate, NoticeShown::Unknown) => {
            format!("{subject} may or may not have been added; check its folder first")
        }
        (_, NoticeShown::Landed) => format!("{subject} landed while CrazyAgent stopped"),
        (_, NoticeShown::Dropped) => format!("{subject} did not land; accept it again"),
        (_, NoticeShown::Unknown) => format!("{subject} has an unknown outcome"),
    }
}

fn done_note(query: &RecoveryQuery, banner: Option<&RecoveryBanner>) -> String {
    match query {
        RecoveryQuery::Retry { agent, .. } => {
            let label = banner
                .and_then(|banner| banner.runs.iter().find(|run| run.agent == *agent))
                .map_or_else(|| "The agent".to_owned(), |run| run.label.clone());
            format!("{label} will try to resume again.")
        }
        RecoveryQuery::Dismiss {
            kind: ActionKind::ProjectDelete,
            ..
        } => "The project stays.".into(),
        RecoveryQuery::Dismiss { .. } => "Dismissed.".into(),
        RecoveryQuery::Read { .. } => unreachable!("a read is answered with the banner"),
    }
}

/// One plain sentence for a refused Recovery request. Input: the code.
/// Output: the sentence.
fn sentence(code: &str) -> String {
    match code {
        "recovery_not_failed" => "That resume is no longer failed. The list is read again.".into(),
        "ledger_finished" | "ledger_missing" => {
            "That notice was already dismissed. The list is read again.".into()
        }
        "ledger_not_notice" => "That start is still being checked; it settles on its own.".into(),
        "recovery_unavailable" | "ledger_unavailable" => {
            "CrazyAgent couldn't read its records. Try again.".into()
        }
        "recovery_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. The list is read again.".into()
        }
        "owner_busy" => "CrazyAgent is busy with other requests. Try again in a moment.".into(),
        other => plain_problem(other),
    }
}
