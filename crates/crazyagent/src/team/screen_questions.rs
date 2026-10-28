use super::events::{SCAN_EVERY, Watched, choice};
use crate::protocol::ProjectId;
use crate::terminal::{AgentId, Host};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

/// The options a harness adds to its own question box so the owner can answer in words:
/// Claude Code's AskUserQuestion "Type something." and Codex's request_user_input "None of
/// the above". Permission and approval prompts never carry one.
const FREE_ANSWERS: [&str; 2] = ["Type something", "None of the above"];
/// The selection markers of those boxes: Claude Code's pointer and Codex's.
const BOX_MARKERS: [char; 2] = ['\u{276f}', '\u{203a}'];
/// Key hints only those boxes' footers show while the harness waits on them: Claude Code's
/// "Enter to select", and Codex's notes and submit hints.
const FOOTER_HINTS: [&str; 4] = [
    " to select",
    " to add notes",
    " to clear notes",
    " to submit",
];
/// The last screen rows, below the box's last option, one of which must be its footer.
const FOOTER_ROWS: usize = 2;
/// Scans in a row a logged question must be off its master's screen before it is closed.
const MISSES_TO_CLOSE: u8 = 2;
/// Terminal host refusals that mean the master's terminal no longer runs.
const GONE: [&str; 2] = ["terminal_agent_unknown", "terminal_ended"];

/// Finds a question a master's harness asks the owner in its own terminal: a choice box
/// with a free-answer option, the harness's own selection marker, and its footer among the
/// last FOOTER_ROWS screen rows below the options, so the harness is waiting on it. A
/// permission or approval prompt has no free answer, and a list the master printed above
/// its input prompt has no footer at the bottom; neither is one. Takes the screen bytes.
/// Returns the question and its options as one bounded line, or none.
pub fn owner_question(screen: &[u8]) -> Option<String> {
    let choice = choice(screen)?;
    let free = choice
        .options
        .iter()
        .any(|text| FREE_ANSWERS.contains(&text.trim_end_matches('.')));
    let footed = choice
        .below
        .iter()
        .rev()
        .take(FOOTER_ROWS)
        .any(|row| FOOTER_HINTS.iter().any(|hint| row.contains(hint)));
    (free && footed && BOX_MARKERS.contains(&choice.marker)).then(|| choice.line())
}

/// A question asked in a master's terminal that the owner questions log holds as pending:
/// its id in the log, the master's project and team agent id, and its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: u64,
    pub project: ProjectId,
    pub agent: i64,
    pub text: String,
}

/// What one scan asks of the owner questions log: log a question a master asks in its
/// terminal, or close a pending one, by id and its master's team agent id, because its box
/// left the screen; whether the owner answered it is the caller's to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Asked {
        project: ProjectId,
        agent: i64,
        text: String,
    },
    Left {
        id: u64,
        agent: i64,
    },
}

/// Reads live masters' screens for questions their harness asks the owner. A question on a
/// screen unchanged since the last scan, so the harness waits, is asked until the caller
/// reports it `logged` or the log holds it, and is not asked again while it stays on
/// screen, even once the owner dismissed it; a logged one has left after
/// MISSES_TO_CLOSE scans in a row without it on its master's screen. Masters that left are
/// read until their questions are closed.
#[derive(Debug, Default)]
pub struct MasterWatch {
    next: Option<Instant>,
    screens: BTreeMap<i64, u64>,
    held: BTreeMap<i64, String>,
    misses: BTreeMap<u64, u8>,
    askers: BTreeMap<i64, ProjectId>,
}

impl MasterWatch {
    /// Tells whether a scan is due, at most once each `SCAN_EVERY`, and starts the next wait.
    /// Takes the watch. Returns true when the caller should scan now.
    pub fn due(&mut self) -> bool {
        let now = Instant::now();
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.next = Some(now + SCAN_EVERY);
        true
    }

    /// The masters whose pending terminal questions the next scan needs: each live master,
    /// and each one that left while the log still held a question of its. Takes the live
    /// masters. Returns each master's project and team agent id, once.
    pub fn askers(&self, masters: &[Watched]) -> Vec<(ProjectId, i64)> {
        let mut askers: BTreeMap<i64, ProjectId> = self.askers.clone();
        for master in masters {
            askers.insert(master.agent, master.project.clone());
        }
        assert!(askers.len() <= self.askers.len() + masters.len());
        askers
            .into_iter()
            .map(|(agent, project)| (project, agent))
            .collect()
    }

    /// Records that the owner questions log took a question `scan` asked, so it is not asked
    /// again while it stays on its master's screen, even if the owner dismisses it before
    /// the next scan. Takes the watch, the master's team agent id, which the last scan read,
    /// and the question's line.
    pub fn logged(&mut self, agent: i64, text: &str) {
        assert!(self.screens.contains_key(&agent));
        self.held.insert(agent, text.to_owned());
    }

    /// Reads each live master's screen once and compares it with the log's pending terminal
    /// questions of every master `askers` named. A master no longer live or whose terminal
    /// ended shows none; another unreadable screen changes nothing. Takes the host, the live
    /// masters and the pending questions. Returns the questions to log and those that left.
    pub fn scan(
        &mut self,
        host: &dyn Host,
        masters: &[Watched],
        pending: &[Pending],
    ) -> Vec<Change> {
        let live: BTreeSet<i64> = masters.iter().map(|master| master.agent).collect();
        let mut shown: BTreeMap<i64, Option<String>> = BTreeMap::new();
        let mut changes = Vec::new();
        for master in masters {
            let Ok(name) = AgentId::team(master.agent) else {
                continue;
            };
            let screen = match host.screen(&name) {
                Ok(screen) => screen,
                Err(problem) if GONE.contains(&problem.0.as_str()) => {
                    self.screens.remove(&master.agent);
                    self.held.remove(&master.agent);
                    shown.insert(master.agent, None);
                    continue;
                }
                Err(_) => continue,
            };
            let mut digest = DefaultHasher::new();
            screen.hash(&mut digest);
            let digest = digest.finish();
            let idle = self.screens.insert(master.agent, digest) == Some(digest);
            let asked = owner_question(&screen);
            match &asked {
                Some(text)
                    if pending.iter().any(|question| {
                        question.agent == master.agent && &question.text == text
                    }) =>
                {
                    self.held.insert(master.agent, text.clone());
                }
                Some(text) if self.held.get(&master.agent) == Some(text) => {}
                Some(text) if idle => {
                    self.held.remove(&master.agent);
                    changes.push(Change::Asked {
                        project: master.project.clone(),
                        agent: master.agent,
                        text: text.clone(),
                    });
                }
                _ => {
                    self.held.remove(&master.agent);
                }
            }
            shown.insert(master.agent, asked);
        }
        self.screens.retain(|agent, _| live.contains(agent));
        self.held.retain(|agent, _| live.contains(agent));
        self.askers = pending
            .iter()
            .map(|question| (question.agent, question.project.clone()))
            .chain(
                masters
                    .iter()
                    .map(|master| (master.agent, master.project.clone())),
            )
            .collect();
        for question in pending {
            let on_screen = match shown.get(&question.agent) {
                Some(text) => text.as_deref() == Some(question.text.as_str()),
                None if live.contains(&question.agent) => continue,
                None => false,
            };
            if on_screen {
                self.misses.remove(&question.id);
                continue;
            }
            let misses = self.misses.entry(question.id).or_insert(0);
            *misses += 1;
            if *misses >= MISSES_TO_CLOSE {
                self.misses.remove(&question.id);
                changes.push(Change::Left {
                    id: question.id,
                    agent: question.agent,
                });
            }
        }
        let held: BTreeSet<u64> = pending.iter().map(|question| question.id).collect();
        self.misses.retain(|id, _| held.contains(id));
        assert!(self.screens.keys().all(|agent| live.contains(agent)));
        assert!(self.held.keys().all(|agent| live.contains(agent)));
        assert!(self.askers.len() <= masters.len() + pending.len());
        assert!(self.misses.values().all(|misses| *misses < MISSES_TO_CLOSE));
        assert!(changes.len() <= masters.len() + pending.len());
        changes
    }
}

#[cfg(all(test, unix))]
mod tests;
