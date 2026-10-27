use super::events::{Watched, choice};
use crate::protocol::ProjectId;
use crate::terminal::{AgentId, Host};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};

/// The options a harness adds to its own question box so the owner can answer in words:
/// Claude Code's AskUserQuestion "Type something." and Codex's request_user_input "None of
/// the above". Permission and approval prompts never carry one.
const FREE_ANSWERS: [&str; 2] = ["Type something", "None of the above"];
/// Scans in a row a logged question must be off its master's screen before it is answered.
const MISSES_TO_CLOSE: u8 = 2;
/// Terminal host refusals that mean the master's terminal no longer runs.
const GONE: [&str; 2] = ["terminal_agent_unknown", "terminal_ended"];

/// Finds a question a master's harness asks the owner in its own terminal: a choice box
/// with a free-answer option. A permission or approval prompt has none and is not one.
/// Takes the screen bytes. Returns the question and its options as one bounded line, or none.
pub fn owner_question(screen: &[u8]) -> Option<String> {
    let choice = choice(screen)?;
    choice
        .options
        .iter()
        .any(|text| FREE_ANSWERS.contains(&text.trim_end_matches('.')))
        .then(|| choice.line())
}

/// A question asked in a master's terminal that the owner questions log holds as pending:
/// its id in the log, the master's project and team agent id, and its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: i64,
    pub project: ProjectId,
    pub agent: i64,
    pub text: String,
}

/// What one scan asks of the owner questions log: log a question a master asks in its
/// terminal, or mark a pending one answered there because its box left the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Asked {
        project: ProjectId,
        agent: i64,
        text: String,
    },
    Answered(i64),
}

/// Reads live masters' screens for questions their harness asks the owner. A question on a
/// screen unchanged since the last scan, so the harness waits, is logged once; a logged one
/// is answered after MISSES_TO_CLOSE scans in a row without it on its master's screen.
#[derive(Debug, Default)]
pub struct MasterWatch {
    screens: BTreeMap<i64, u64>,
    misses: BTreeMap<i64, u8>,
}

impl MasterWatch {
    /// Reads each live master's screen once and compares it with the log's pending terminal
    /// questions. A master no longer live or whose terminal ended shows none; another
    /// unreadable screen changes nothing. Takes the host, the live masters and the pending
    /// questions. Returns the questions to log and the ones to mark answered.
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
            if let Some(text) = &asked
                && idle
                && !pending
                    .iter()
                    .any(|question| question.agent == master.agent && &question.text == text)
            {
                changes.push(Change::Asked {
                    project: master.project.clone(),
                    agent: master.agent,
                    text: text.clone(),
                });
            }
            shown.insert(master.agent, asked);
        }
        self.screens.retain(|agent, _| live.contains(agent));
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
                changes.push(Change::Answered(question.id));
            }
        }
        let held: BTreeSet<i64> = pending.iter().map(|question| question.id).collect();
        self.misses.retain(|id, _| held.contains(id));
        assert!(self.screens.keys().all(|agent| live.contains(agent)));
        assert!(self.misses.values().all(|misses| *misses < MISSES_TO_CLOSE));
        assert!(changes.len() <= masters.len() + pending.len());
        changes
    }
}

#[cfg(all(test, unix))]
mod tests;
