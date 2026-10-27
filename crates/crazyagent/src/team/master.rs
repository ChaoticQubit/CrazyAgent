use super::tool::{self, Call};
use super::{TaskSource, now_ms};
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::harnesses::terminal_session::paste;
use crate::project_setup::StoredSetup;
use crate::protocol::ProjectId;
use crate::terminal::{AgentId, Host};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::Path;
use std::time::Duration;

pub const INPUT_TEXT_MAX: usize = 16 * 1024;
const OWNER_QUIET_MS: i64 = 10_000;
const DRAFT_ABANDONED_MS: i64 = 120_000;
const INPUT_SETTLE_MS: i64 = 30_000;
const PASTE_SETTLE: Duration = Duration::from_millis(200);
const CHANGES_LISTED_MAX: usize = 20;
const MASTER_TURNS_KEPT: i64 = 2000;
const UNREAD: &str = "The engine could not read this call. Call the tool again.";
const NOT_WIRED: &str = "Team messaging is not available in this build yet.";
const NO_TERMINALS: &str = "Not sent: the engine has no agent terminals right now.";
const AGENT_SHOWN_MAX: usize = 64;

/// Why the engine did not type into a project's live master. `NoMaster`: no live master
/// runs. `Busy`: a turn is open or an earlier engine input has not started its turn yet.
/// `OwnerTyping`: the owner typed within the quiet window or left an unsent draft.
/// `Invalid`: the text is empty or too long. `Failed`: the pane or state could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputRefused {
    NoMaster,
    Busy,
    OwnerTyping,
    Invalid,
    Failed(String),
}

/// Types a master's text into one sub-agent's terminal. Takes the engine database, the
/// terminal host, the project, the agent as the master named it and the text. Returns the
/// sentence the master reads back, or the plain reason it was not delivered.
pub type SendToAgent = fn(&Connection, &dyn Host, &ProjectId, &str, &str) -> Result<String, String>;

/// Describes a project's team for its master. Takes the engine database and the project.
/// Returns the status text, or the plain reason it could not be read.
pub type TeamStatusText = fn(&Connection, &ProjectId) -> Result<String, String>;

/// The team event handlers the live master's `message`, `answer` and `status` tools call,
/// provided by `team::events` and set on the runtime with `TeamRuntime::with_events`.
#[derive(Clone, Copy)]
pub struct TeamEvents {
    pub send_to_agent: SendToAgent,
    pub answer: SendToAgent,
    pub status: TeamStatusText,
}

/// What the engine answers a live master's team calls with: the live model access and
/// stored setup `admit` checks a task against, the engine runtime folder that holds the
/// change sets, the team event handlers and the terminal host, when the engine has them.
pub struct Answering<'a> {
    pub access: &'a ModelAccess,
    pub stored: &'a StoredSetup,
    pub landing: &'a Path,
    pub events: Option<&'a TeamEvents>,
    pub host: Option<&'a dyn Host>,
}

fn state(problem: impl std::fmt::Display) -> EngineError {
    EngineError(format!("team_state_unavailable: {problem}"))
}

/// Creates the live master tables: the per-project live switch with its turn and owner
/// typing state and the turns a live master ended. `names` keeps the role a delegation asked for.
/// Takes the engine database. Returns any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS team_live_masters (
                project TEXT PRIMARY KEY,
                live INTEGER NOT NULL,
                agent INTEGER,
                turn_open INTEGER NOT NULL DEFAULT 0,
                turns INTEGER NOT NULL DEFAULT 0,
                input_ms INTEGER,
                typed_ms INTEGER,
                draft INTEGER NOT NULL DEFAULT 0,
                problem TEXT
            );
            CREATE TABLE IF NOT EXISTS team_master_turns (
                agent INTEGER NOT NULL,
                turn INTEGER NOT NULL,
                project TEXT NOT NULL,
                started_ms INTEGER,
                finished_ms INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                PRIMARY KEY (agent, turn)
            );",
        )
        .map_err(state)
}

/// Turns a project's live master on or off: on, the runtime keeps the master's own app
/// running in a terminal; off, the master ends with its next stop. Takes the owner
/// transaction, the project and the switch. Returns any SQLite failure.
pub fn set_live(connection: &Connection, project: &ProjectId, on: bool) -> Result<(), EngineError> {
    connection
        .execute(
            "INSERT INTO team_live_masters(project,live) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET live=excluded.live",
            params![project.as_str(), on],
        )
        .map_err(state)?;
    Ok(())
}

/// Reads whether a project's live master is switched on. Takes the engine database and the
/// project. Returns the switch, off when never set.
pub fn live(connection: &Connection, project: &ProjectId) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT live FROM team_live_masters WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map(|on| on.unwrap_or(false))
        .map_err(state)
}

/// Keeps why a project's live master could not start, so the runtime says it once.
/// Takes the engine database, the project and the reason, none once it started.
/// Returns whether a new reason was recorded, or a SQLite failure.
pub(crate) fn problem(
    connection: &Connection,
    project: &ProjectId,
    reason: Option<&str>,
) -> Result<bool, EngineError> {
    let changed = connection
        .execute(
            "UPDATE team_live_masters SET problem=?2 WHERE project=?1 AND problem IS NOT ?2",
            params![project.as_str(), reason],
        )
        .map_err(state)?;
    Ok(changed > 0 && reason.is_some())
}

/// Records that a live master's pane runs, so engine input finds it. Takes the engine
/// database, the project and the team agent id. Returns any SQLite failure.
pub(crate) fn pane_ready(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "UPDATE team_live_masters SET agent=?2, turn_open=0, input_ms=NULL WHERE project=?1",
            params![project.as_str(), agent],
        )
        .map_err(state)?;
    Ok(())
}

/// Records that a live master's pane ended; another agent's entry is left alone.
/// Takes the engine database, the project and the team agent id. Returns any SQLite failure.
pub(crate) fn pane_ended(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<(), EngineError> {
    connection
        .execute(
            "UPDATE team_live_masters SET agent=NULL, turn_open=0, input_ms=NULL WHERE project=?1 AND agent=?2",
            params![project.as_str(), agent],
        )
        .map_err(state)?;
    Ok(())
}

/// Records that a live master's turn started, whoever typed it, which also ends any
/// pending engine input and the owner's draft. Takes the engine database and the agent.
/// Returns any SQLite failure.
pub(crate) fn turn_started(connection: &Connection, agent: i64) -> Result<(), EngineError> {
    connection
        .execute(
            "UPDATE team_live_masters SET turn_open=1, input_ms=NULL, draft=0 WHERE agent=?1",
            [agent],
        )
        .map_err(state)?;
    Ok(())
}

/// Records that a live master's turn ended: the turn closes and one `team_master_turns`
/// row keeps its number, start, end and outcome for usage. Takes the engine database, the
/// project, the agent, when the turn started if seen, the time and the outcome name.
/// Returns the turn number, counted per project, or a SQLite failure.
pub(crate) fn turn_ended(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    started_ms: Option<i64>,
    now: i64,
    outcome: &str,
) -> Result<i64, EngineError> {
    let tx =
        Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(state)?;
    tx.execute(
        "UPDATE team_live_masters SET turn_open=0, turns=turns+1 WHERE project=?1",
        [project.as_str()],
    )
    .map_err(state)?;
    let turn: i64 = tx
        .query_row(
            "SELECT turns FROM team_live_masters WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?
        .unwrap_or(1);
    tx.execute(
        "INSERT OR REPLACE INTO team_master_turns(agent,turn,project,started_ms,finished_ms,outcome) VALUES (?1,?2,?3,?4,?5,?6)",
        params![agent, turn, project.as_str(), started_ms, now, outcome],
    )
    .map_err(state)?;
    tx.execute(
        "DELETE FROM team_master_turns WHERE project=?1 AND turn <= ?2",
        params![project.as_str(), turn - MASTER_TURNS_KEPT],
    )
    .map_err(state)?;
    tx.commit().map_err(state)?;
    assert!(turn > 0);
    Ok(turn)
}

/// Types one engine message into a project's live master as one paste plus Enter, only
/// while the master is idle and the owner is not typing, so w-events can queue team events.
/// Takes the engine database, the terminal host, the project and the text.
/// Returns Ok once typed, or why it was not typed; nothing is typed on refusal.
pub fn master_input(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    text: &str,
) -> Result<(), InputRefused> {
    master_input_at(connection, host, project, text, now_ms())
}

/// [`master_input`] at a given time. Takes the same inputs and the time in milliseconds.
pub(crate) fn master_input_at(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    text: &str,
    now: i64,
) -> Result<(), InputRefused> {
    let text = text.trim();
    let plain = !text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t');
    if text.is_empty() || text.len() > INPUT_TEXT_MAX || !plain {
        return Err(InputRefused::Invalid);
    }
    let failed = |problem: rusqlite::Error| InputRefused::Failed(state(problem).0);
    let tx =
        Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(failed)?;
    type Row = (bool, Option<i64>, bool, Option<i64>, Option<i64>, bool);
    let row: Option<Row> = tx
        .query_row(
            "SELECT live,agent,turn_open,input_ms,typed_ms,draft FROM team_live_masters WHERE project=?1",
            [project.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(failed)?;
    let Some((true, Some(agent), turn_open, input_ms, typed_ms, draft)) = row else {
        return Err(InputRefused::NoMaster);
    };
    if turn_open || input_ms.is_some_and(|at| now - at < INPUT_SETTLE_MS) {
        return Err(InputRefused::Busy);
    }
    let since_typed = typed_ms.map_or(i64::MAX, |at| now - at);
    if since_typed < OWNER_QUIET_MS || (draft && since_typed < DRAFT_ABANDONED_MS) {
        return Err(InputRefused::OwnerTyping);
    }
    let pane = AgentId::team(agent).map_err(|problem| InputRefused::Failed(problem.0))?;
    tx.execute(
        "UPDATE team_live_masters SET input_ms=?2 WHERE project=?1",
        params![project.as_str(), now],
    )
    .map_err(failed)?;
    tx.commit().map_err(failed)?;
    let typed = paste(text)
        .iter()
        .try_for_each(|part| host.input(&pane, part.as_bytes()))
        .and_then(|()| {
            std::thread::sleep(PASTE_SETTLE);
            host.input(&pane, b"\r")
        });
    if let Err(problem) = typed {
        let _ = connection.execute(
            "UPDATE team_live_masters SET input_ms=NULL WHERE project=?1 AND input_ms=?2",
            params![project.as_str(), now],
        );
        return Err(InputRefused::Failed(problem.0));
    }
    Ok(())
}

/// Records keys the owner typed into a team agent's terminal, the owner typing signal that
/// holds engine input to a live master back. Takes the engine database, the team agent id,
/// the typed bytes and the time. Returns any SQLite failure; other agents are ignored.
pub fn owner_typed(
    connection: &Connection,
    agent: i64,
    bytes: &[u8],
    now_ms: i64,
) -> Result<(), EngineError> {
    let (submitted, drafting) = keys(bytes);
    connection
        .execute(
            "UPDATE team_live_masters SET typed_ms=?2,
             draft=CASE WHEN ?4 THEN 1 WHEN ?3 THEN 0 ELSE draft END
             WHERE agent=?1 AND live=1",
            params![agent, now_ms, submitted, drafting],
        )
        .map_err(state)?;
    Ok(())
}

/// Reads what typed bytes do to the owner's unsent line: whether they submit or clear it
/// (Enter, Ctrl+C, Ctrl+U) and whether text is left unsent after the last such key.
/// Escape sequences such as arrow keys are neither. Takes the bytes. Returns both facts.
fn keys(bytes: &[u8]) -> (bool, bool) {
    let mut submitted = false;
    let mut drafting = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;
        match byte {
            b'\r' | 0x03 | 0x15 => {
                submitted = true;
                drafting = false;
            }
            0x1b if bytes
                .get(index)
                .is_some_and(|next| matches!(next, b'[' | b'O')) =>
            {
                index += 1;
                let mut parameters = Vec::new();
                while let Some(&next) = bytes.get(index) {
                    index += 1;
                    if (0x40..=0x7e).contains(&next) {
                        break;
                    }
                    parameters.push(next);
                }
                if parameters == b"200" {
                    drafting = true;
                }
            }
            b'\n' | b'\t' => drafting = true,
            byte if byte >= 0x20 && byte != 0x7f => drafting = true,
            _ => {}
        }
    }
    (submitted, drafting)
}

/// Answers the calls a live master's team server left in its mailbox, at most
/// `CALLS_PER_TICK_MAX` per call, each checked again before it takes effect: `delegate`
/// admits the task at once, `message`, `answer` and `status` go to the team event handlers
/// and `changes` lists the pending change sets. Takes the engine database, what the
/// answers need and the mailbox. Returns how many calls were answered, or a SQLite failure.
pub fn serve_calls(
    connection: &mut Connection,
    answering: &Answering<'_>,
    mailbox: &Path,
) -> Result<usize, EngineError> {
    let taken = tool::take_calls(mailbox, tool::CALLS_PER_TICK_MAX);
    for (id, call) in &taken {
        let (text, error) = match call.clone().map(tool::checked) {
            None => (UNREAD.to_owned(), true),
            Some(Err(refused)) => (refused.to_owned(), true),
            Some(Ok(call)) => answer(connection, answering, call)?,
        };
        let _ = tool::answer_call(mailbox, id, &text, error);
    }
    Ok(taken.len())
}

fn answer(
    connection: &mut Connection,
    answering: &Answering<'_>,
    call: Call,
) -> Result<(String, bool), EngineError> {
    let project = &answering.stored.project_id;
    let typed =
        |handler: Option<SendToAgent>, agent: &str, text: &str| match (handler, answering.host) {
            (None, _) => (NOT_WIRED.to_owned(), true),
            (Some(_), None) => (NO_TERMINALS.to_owned(), true),
            (Some(handler), Some(host)) => match handler(connection, host, project, agent, text) {
                Ok(said) => (said, false),
                Err(reason) => (reason, true),
            },
        };
    Ok(match call {
        Call::Delegate {
            task,
            harness,
            role,
        } => {
            return delegate(
                connection,
                answering,
                &task,
                harness.as_deref(),
                role.as_deref(),
            );
        }
        Call::Message { agent, text } => typed(
            answering.events.map(|events| events.send_to_agent),
            &agent,
            &text,
        ),
        Call::Answer { agent, text } => {
            typed(answering.events.map(|events| events.answer), &agent, &text)
        }
        Call::Status => match answering.events {
            None => (NOT_WIRED.to_owned(), true),
            Some(events) => match (events.status)(connection, project) {
                Ok(text) => (text, false),
                Err(reason) => (reason, true),
            },
        },
        Call::Changes => changes(answering),
    })
}

fn delegate(
    connection: &mut Connection,
    answering: &Answering<'_>,
    task: &str,
    harness: Option<&str>,
    role: Option<&str>,
) -> Result<(String, bool), EngineError> {
    let project = &answering.stored.project_id;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(state)?;
    let admitted = super::admit(
        &tx,
        answering.access,
        answering.stored,
        harness,
        task,
        TaskSource::Master,
        None,
        now_ms(),
    )?;
    let id = match admitted {
        Ok(id) => id,
        Err(code) => {
            return Ok((
                format!("Not handed off: {}.", super::runtime::refusal(code)),
                true,
            ));
        }
    };
    let (chosen, status): (String, String) = tx
        .query_row(
            "SELECT harness,status FROM team_tasks WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(state)?;
    if let Some(role) = role.and_then(super::names::handle) {
        super::names::set_task_role(&tx, project, id, &role)?;
    }
    tx.commit().map_err(state)?;
    let waiting = if status == "needs_pool" {
        " It waits until the pool has a ready account for that harness."
    } else {
        ""
    };
    Ok((
        format!(
            "Task {id} is queued for a {chosen} sub-agent.{waiting} The engine tells you when it ends."
        ),
        false,
    ))
}

fn changes(answering: &Answering<'_>) -> (String, bool) {
    let listed = crate::landing::open(answering.landing)
        .and_then(|db| crate::landing::list(&db, &answering.stored.project_id));
    let sets = match listed {
        Ok(sets) => sets,
        Err(problem) => {
            return (
                format!(
                    "The change sets could not be read ({}).",
                    super::bound_text(&problem.0, 120)
                ),
                true,
            );
        }
    };
    let waiting: Vec<String> = sets
        .iter()
        .filter(|set| {
            matches!(
                set.state,
                crate::landing::ChangeState::Pending
                    | crate::landing::ChangeState::Conflict
                    | crate::landing::ChangeState::Updating
            )
        })
        .take(CHANGES_LISTED_MAX)
        .map(|set| {
            let state = match set.state {
                crate::landing::ChangeState::Conflict => "conflicts with the owner's branch",
                crate::landing::ChangeState::Updating => "being updated",
                _ => "waiting for review",
            };
            format!(
                "- {}: {} files, +{} -{}, {state}",
                super::bound_text(&set.agent.label, 80),
                set.files,
                set.added,
                set.removed
            )
        })
        .collect();
    if waiting.is_empty() {
        return ("No change set is waiting for the owner.".into(), false);
    }
    (
        format!(
            "Change sets waiting for the owner in Changes:\n{}",
            waiting.join("\n")
        ),
        false,
    )
}

/// The team event handlers the engine gives a live master's `message`, `answer` and `status`
/// tools: text typed into one of the project's running sub-agents, named by persona,
/// `@handle` or `<harness>-<id>`, and the project's running sub-agents with their names and
/// newest events.
pub const EVENTS: TeamEvents = TeamEvents {
    send_to_agent: message_agent,
    answer: answer_agent,
    status: status_text,
};

fn message_agent(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    text: &str,
) -> Result<String, String> {
    type_to_agent(connection, host, project, agent, text).map(|name| format!("Sent to {name}."))
}

fn answer_agent(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    text: &str,
) -> Result<String, String> {
    type_to_agent(connection, host, project, agent, text).map(|name| format!("Answered {name}."))
}

/// Types a master's text into the running sub-agent it names: a persona or `@handle` first,
/// then the `<harness>-<id>` names events use. Takes the database, terminal host, project,
/// the name and the text. Returns the agent's label, or the plain reason it was not sent.
fn type_to_agent(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: &str,
    text: &str,
) -> Result<String, String> {
    let shown = super::bound_text(agent, AGENT_SHOWN_MAX);
    let target = match super::names::resolve(connection, project, agent).map_err(unreadable)? {
        Some(id) => format!("team-{id}"),
        None => agent.to_owned(),
    };
    match super::events::send_to_agent(connection, host, project, &target, text)
        .map_err(unreadable)?
    {
        Ok(id) => agent_label(connection, id).map_err(unreadable),
        Err("team_message_invalid") => Err(format!(
            "Not sent: give plain text up to {} bytes.",
            super::events::SEND_BYTES_MAX
        )),
        Err("team_agent_not_running") => Err(format!("Not sent: {shown} is not running.")),
        Err("team_agent_unreachable") => Err(format!(
            "Not sent: the terminal of {shown} could not be reached."
        )),
        Err(_) => Err(format!(
            "Not sent: no running sub-agent of this project is called {shown}. Call status() for their names."
        )),
    }
}

fn agent_label(connection: &Connection, agent: i64) -> Result<String, EngineError> {
    let name = super::names::name(connection, agent)?;
    Ok(super::names::label(
        super::AgentRole::Sub,
        agent,
        name.as_ref(),
    ))
}

fn unreadable(problem: EngineError) -> String {
    format!(
        "Not done: the team state could not be read ({}).",
        super::bound_text(&problem.0, 120)
    )
}

/// Describes a project's running sub-agents for its live master, each named the way the
/// owner sees it and the way events name it. Takes the database and the project. Returns
/// the status text, or the plain reason it could not be read.
fn status_text(connection: &Connection, project: &ProjectId) -> Result<String, String> {
    let mut status = super::events::status(connection, project).map_err(unreadable)?;
    for line in &mut status.agents {
        let label = agent_label(connection, line.agent).map_err(unreadable)?;
        line.label = format!("{label} / {}", line.label);
    }
    Ok(status.text())
}

#[cfg(all(test, unix))]
#[path = "master/tests.rs"]
mod tests;
