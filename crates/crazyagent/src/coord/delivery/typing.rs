use super::super::AgentId;
use super::super::store::state;
use crate::engine::EngineError;
use crate::team::AgentStatus;
use crate::team::master::OwnerTyping;
use crate::terminal::{Host, HostError, PROMPT_BYTES_MAX, type_prompt};
use rusqlite::{Connection, OptionalExtension};
use std::time::Duration;

/// How long an agent terminal's app must have drawn nothing before the engine types into
/// it: a working harness redraws its spinner at least once a second.
pub const APP_QUIET: Duration = Duration::from_secs(3);

/// What one typing attempt did: typed the text and pressed Enter, held back for a reason
/// that may pass (a busy agent, the owner typing, a question on screen), or found no
/// terminal because the agent's session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typing {
    Typed,
    Held(&'static str),
    Gone,
}

/// Types text into a team agent's terminal as one submission, only while that agent
/// waits for input (DEC-104 Q13): its status is idle, or working on a task whose turn is
/// held for the commands it started in the background (DEC-119), its app drew nothing for
/// `APP_QUIET`, no question or permission prompt is on its screen or recorded for it, no
/// dropped turn still holds its input (see `master::input_released`), and the owner's keys
/// do not hold it back by the live master's rule. Control keys are
/// dropped, so the text cannot press keys. Takes the database, the terminal host, the owner
/// typing signal (none holds every agent back), the agent, whether its turn is held, the
/// text and the time in ms. Returns what happened, or a database failure.
pub fn type_into(
    db: &Connection,
    host: &dyn Host,
    owner: Option<&OwnerTyping>,
    agent: AgentId,
    held: bool,
    text: &str,
    now: i64,
) -> Result<Typing, EngineError> {
    if text.trim().is_empty() || text.len() > PROMPT_BYTES_MAX {
        return Ok(Typing::Held("coord_typing_text_invalid"));
    }
    let status: Option<String> = db
        .query_row(
            "SELECT status FROM team_agents WHERE id=?1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    let Some(status) = AgentStatus::ALL
        .into_iter()
        .find(|known| status.as_deref() == Some(known.name()))
    else {
        return Ok(Typing::Gone);
    };
    if !status.live() {
        return Ok(Typing::Gone);
    }
    if status != AgentStatus::Idle && !(held && status == AgentStatus::Working) {
        return Ok(Typing::Held("coord_agent_busy"));
    }
    let Some(owner) = owner else {
        return Ok(Typing::Held("coord_owner_unknown"));
    };
    if owner.holds(agent, now) {
        return Ok(Typing::Held("coord_owner_typing"));
    }
    if waiting(db, agent)? {
        return Ok(Typing::Held("coord_agent_waiting"));
    }
    let pane = crate::terminal::AgentId::team(agent).map_err(EngineError::from)?;
    let screen = match host.screen(&pane) {
        Ok(screen) => screen,
        Err(problem) => return Ok(unreached(&problem)),
    };
    if crate::team::events::question(&screen).is_some()
        || !crate::team::master::input_released(db, host, agent)?
    {
        return Ok(Typing::Held("coord_agent_waiting"));
    }
    match host.quiet(&pane) {
        Ok(Some(quiet)) if quiet >= APP_QUIET => {}
        Ok(Some(_)) => return Ok(Typing::Held("coord_agent_busy")),
        Ok(None) => return Ok(Typing::Held("coord_terminal_cannot_tell")),
        Err(problem) => return Ok(unreached(&problem)),
    }
    match type_prompt(host, &pane, text) {
        Ok(true) => Ok(Typing::Typed),
        Ok(false) => Ok(Typing::Held("coord_typing_text_invalid")),
        Err(problem) => Ok(unreached(&problem)),
    }
}

/// Says whether an agent waits on its user in its own terminal, as the coordination
/// layer's hooks or the engine's screen scan recorded it. Takes the database and the
/// agent. Returns the answer, or a database failure.
fn waiting(db: &Connection, agent: AgentId) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM coord_activity WHERE agent=?1 AND waiting IS NOT NULL)
             OR EXISTS(SELECT 1 FROM team_event_waiting WHERE agent=?1)",
        [agent],
        |row| row.get(0),
    )
    .map_err(state)
}

/// Reads a terminal failure as a typing outcome: an unknown terminal means the session
/// ended, anything else may pass. Takes the failure. Returns the outcome.
fn unreached(problem: &HostError) -> Typing {
    if problem.0 == "terminal_agent_unknown" {
        Typing::Gone
    } else {
        Typing::Held("coord_terminal_unreachable")
    }
}
