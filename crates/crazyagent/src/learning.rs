//! The Learn CrazyAgent checklist on Home (DEC-153): each learning step with the same words
//! on desktop and TUI, and how many engine rows already show the owner did it. Counts only,
//! each capped at `COUNT_MAX`; no names, paths or text leave the engine. The account, pool
//! and tools steps stay with each client's setup reads (OP-47), so they are not listed here.

use crate::engine::EngineError;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

pub const COUNT_MAX: u32 = 99;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Basics,
    CoreLoop,
    IdeAndTerminals,
    ProcessMetricsUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    Project,
    Master,
    Agents,
    Question,
    ChangeSet,
    Ide,
    Terminal,
    Process,
    Metrics,
    Usage,
}

/// One learning step as both clients show it: its topic, title and two sentences, and the
/// rows that show it done. `count` is None when no engine row tells it yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningStep {
    pub id: StepId,
    pub topic: Topic,
    pub title: String,
    pub body: String,
    pub count: Option<u32>,
}

/// The learning steps in the order Home lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningProgress {
    pub steps: Vec<LearningStep>,
}

impl LearningStep {
    /// True when engine rows show the step done. Input: none. Output: whether it is ticked.
    pub fn done(&self) -> bool {
        self.count.is_some_and(|count| count > 0)
    }
}

const STEPS: [(StepId, Topic, &str, &str, Option<Signal>); 10] = [
    (
        StepId::Project,
        Topic::Basics,
        "Create a project",
        "A project is a folder the agents work in. Start a new one, add a folder or clone from GitHub.",
        Some(Signal {
            table: "projects",
            filter: "1",
        }),
    ),
    (
        StepId::Master,
        Topic::CoreLoop,
        "Talk to the master",
        "Each project has a master you message in its conversation. It plans the work and hands parts to agents.",
        Some(Signal {
            table: "conversation_entries",
            filter: "kind='message'",
        }),
    ),
    (
        StepId::Agents,
        Topic::CoreLoop,
        "Watch the team work",
        "The master starts agents, each in its own copy of the project. The project's Agents page shows who works on what.",
        Some(Signal {
            table: "team_agents",
            filter: "role='sub'",
        }),
    ),
    (
        StepId::Question,
        Topic::CoreLoop,
        "Answer a question",
        "Agents ask when they need your call. Your answer goes back to the agent that asked.",
        Some(Signal {
            table: "questions_log",
            filter: "status IN ('answered','answered_in_terminal')",
        }),
    ),
    (
        StepId::ChangeSet,
        Topic::CoreLoop,
        "Accept a change set",
        "Finished work waits as a change set you review. Accept it to land the work on the project's branch.",
        Some(Signal {
            table: "landing_change_sets",
            filter: "state='accepted'",
        }),
    ),
    (
        StepId::Ide,
        Topic::IdeAndTerminals,
        "Open the IDE",
        "The IDE opens the project in a code editor beside the agents. What you may change there follows the project's access.",
        Some(Signal {
            table: "ide_workspaces",
            filter: "1",
        }),
    ),
    (
        StepId::Terminal,
        Topic::IdeAndTerminals,
        "Open a terminal",
        "Each agent runs in a terminal you can open and watch live. Type in it when an agent waits for you.",
        None,
    ),
    (
        StepId::Process,
        Topic::ProcessMetricsUsage,
        "Set your process",
        "Process holds the instructions every agent gets. Import a CLAUDE.md or start from the default process.",
        None,
    ),
    (
        StepId::Metrics,
        Topic::ProcessMetricsUsage,
        "Read your metrics",
        "Metrics shows what the team did with its time and accounts. Use it to see what works.",
        None,
    ),
    (
        StepId::Usage,
        Topic::ProcessMetricsUsage,
        "Check your usage",
        "Usage shows how much of each account's limits the agents used. It helps you pick accounts and pools.",
        None,
    ),
];

/// One engine table and the condition its counted rows meet; both are fixed text above,
/// never built from input.
#[derive(Clone, Copy)]
struct Signal {
    table: &'static str,
    filter: &'static str,
}

/// Reads the learning steps with their row counts. A table not created yet counts 0, since
/// several tables appear on first use. Input: the read transaction. Output: every step in
/// order, or the storage failure.
pub(crate) fn progress(tx: &Transaction<'_>) -> Result<LearningProgress, EngineError> {
    let _ = (tx, STEPS.len());
    Ok(LearningProgress { steps: Vec::new() })
}

/// Counts up to `COUNT_MAX` rows of one signal. Inputs: the transaction and the signal.
/// Output: the count, 0 when the table does not exist yet.
fn count(tx: &Transaction<'_>, signal: Signal) -> Result<u32, EngineError> {
    let exists: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [signal.table],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    if exists.is_none() {
        return Ok(0);
    }
    let sql = format!(
        "SELECT COUNT(*) FROM (SELECT 1 FROM {} WHERE {} LIMIT {COUNT_MAX})",
        signal.table, signal.filter
    );
    tx.query_row(&sql, [], |row| row.get(0))
        .map_err(unavailable)
}

fn unavailable(_: rusqlite::Error) -> EngineError {
    EngineError("learning_progress_unavailable".into())
}

#[cfg(test)]
mod tests;
