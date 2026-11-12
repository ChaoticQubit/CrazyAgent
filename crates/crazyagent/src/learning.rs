//! The Learn CrazyAgent checklist on Home (DEC-153): each learning step with the same words
//! on desktop and TUI, and how many engine rows already show the owner did it. Counts only,
//! each capped at `COUNT_MAX`; no names, paths or text leave the engine. The account, pool
//! and tools steps stay with each client's setup reads (OP-47), so they are not listed here.
//! Steps no other row tells, and the owner's skip, are kept as marks in `learning_marks`.

use crate::engine::EngineError;
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

pub const COUNT_MAX: u32 = 99;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS learning_marks (
    step TEXT PRIMARY KEY,
    marked_ms INTEGER NOT NULL
)";

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

impl StepId {
    /// The step's stored name, the same as its snake case wire name. Input: none. Output: the
    /// fixed name its learning mark is kept under.
    fn name(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Master => "master",
            Self::Agents => "agents",
            Self::Question => "question",
            Self::ChangeSet => "change_set",
            Self::Ide => "ide",
            Self::Terminal => "terminal",
            Self::Process => "process",
            Self::Metrics => "metrics",
            Self::Usage => "usage",
        }
    }
}

/// A mark the owner's clients set where no other engine row tells the step: a terminal, the
/// Process, Metrics or Usage page opened, or the learning guide skipped. Closed by this enum;
/// besides these, the table keeps only the steps `latch` kept, under their `StepId` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkStep {
    Terminal,
    Process,
    Metrics,
    Usage,
    Skipped,
}

impl MarkStep {
    /// The stored name of the mark. Input: none. Output: its fixed snake case name.
    fn name(self) -> &'static str {
        match self {
            Self::Terminal => StepId::Terminal.name(),
            Self::Process => StepId::Process.name(),
            Self::Metrics => StepId::Metrics.name(),
            Self::Usage => StepId::Usage.name(),
            Self::Skipped => "skipped",
        }
    }
}

/// One learning step as both clients show it: its topic, title and two sentences, and how
/// many rows or marks show it done, 0 while none does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningStep {
    pub id: StepId,
    pub topic: Topic,
    pub title: String,
    pub body: String,
    pub count: u32,
}

/// The learning steps in the order Home lists them, and `skipped` while the owner has
/// skipped the guide and not asked to see it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningProgress {
    pub steps: Vec<LearningStep>,
    pub skipped: bool,
}

impl LearningStep {
    /// True when engine rows show the step done. Input: none. Output: whether it is ticked.
    pub fn done(&self) -> bool {
        self.count > 0
    }
}

impl LearningProgress {
    /// True when the engine holds the mark, so a client sends it only once. Input: the
    /// mark. Output: whether it is set.
    pub fn marked(&self, step: MarkStep) -> bool {
        let id = match step {
            MarkStep::Terminal => StepId::Terminal,
            MarkStep::Process => StepId::Process,
            MarkStep::Metrics => StepId::Metrics,
            MarkStep::Usage => StepId::Usage,
            MarkStep::Skipped => return self.skipped,
        };
        self.steps.iter().any(|step| step.id == id && step.done())
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

/// Creates the marks table when it is missing. Input: the engine database. Output: Ok, or
/// `learning_progress_unavailable`.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(unavailable)
}

/// Reads the learning steps with their counts: a step's rows when it has rows that tell it,
/// else its own learning mark. A table not created yet counts 0, since several tables appear
/// on first use. Input: the read transaction. Output: every step in order and whether the
/// guide is skipped, or the storage failure.
pub(crate) fn progress(tx: &Transaction<'_>) -> Result<LearningProgress, EngineError> {
    let mut steps = Vec::with_capacity(STEPS.len());
    for (id, topic, title, body, signal) in STEPS {
        let rows = match signal {
            Some(signal) => count(tx, signal)?,
            None => 0,
        };
        let count = if rows > 0 {
            rows
        } else {
            marked(tx, id.name())?
        };
        steps.push(LearningStep {
            id,
            topic,
            title: title.to_owned(),
            body: body.to_owned(),
            count,
        });
    }
    let skipped = marked(tx, MarkStep::Skipped.name())? > 0;
    Ok(LearningProgress { steps, skipped })
}

/// Sets or clears one mark. A repeat mark keeps its first time; clearing a mark never set
/// changes nothing. Inputs: the open transaction, the mark and whether it is set. Output:
/// `Applied`, or `learning_mark_unavailable` on a storage failure.
pub(crate) fn mark(
    connection: &Connection,
    step: MarkStep,
    on: bool,
) -> Result<Outcome, EngineError> {
    set(connection, step.name(), on)?;
    Ok(Outcome::Applied)
}

/// Keeps every step ticked now as a learning mark under the step's own name, so a removal
/// of rows (a project removed or deleted, a master session deleted) never unticks what the
/// owner already did. Runs in that removal's transaction before its rows go. Input: the
/// transaction. Output: Ok, or `learning_mark_unavailable`.
pub(crate) fn latch(tx: &Transaction<'_>) -> Result<(), EngineError> {
    for (id, _, _, _, signal) in STEPS {
        if let Some(signal) = signal
            && count(tx, signal)? > 0
        {
            set(tx, id.name(), true)?;
        }
    }
    Ok(())
}

/// Sets or clears one stored mark by its fixed name; a repeat keeps the first time. Inputs:
/// the connection, the name and whether it is set. Output: Ok, or
/// `learning_mark_unavailable`.
fn set(connection: &Connection, name: &'static str, on: bool) -> Result<(), EngineError> {
    let written = if on {
        connection.execute(
            "INSERT INTO learning_marks(step, marked_ms) VALUES(?1, ?2) ON CONFLICT(step) DO NOTHING",
            rusqlite::params![name, crate::team::now_ms()],
        )
    } else {
        connection.execute("DELETE FROM learning_marks WHERE step=?1", [name])
    };
    written.map_err(|_| EngineError("learning_mark_unavailable".into()))?;
    Ok(())
}

/// Reads one stored mark by its fixed name. Inputs: the transaction and the name. Output: 1
/// when it is set, else 0, also before the table exists.
fn marked(tx: &Transaction<'_>, name: &'static str) -> Result<u32, EngineError> {
    if !exists(tx, "learning_marks")? {
        return Ok(0);
    }
    tx.query_row(
        "SELECT COUNT(*) FROM learning_marks WHERE step=?1",
        [name],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

fn exists(tx: &Transaction<'_>, table: &'static str) -> Result<bool, EngineError> {
    let found: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(found.is_some())
}

/// Counts up to `COUNT_MAX` rows of one signal. Inputs: the transaction and the signal.
/// Output: the count, 0 when the table does not exist yet.
fn count(tx: &Transaction<'_>, signal: Signal) -> Result<u32, EngineError> {
    if !exists(tx, signal.table)? {
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
