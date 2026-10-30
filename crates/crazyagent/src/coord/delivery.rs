use super::settings::{self, Delivery, HarnessKind};
use super::store::state;
use super::{AgentId, Message, MessageId, MessageKind, PENDING_MAX, Party, Receipt};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::team::master::OwnerTyping;
use crate::team::{MessageAuthor, MessageState};
use crate::terminal::{Host, PROMPT_BYTES_MAX};
use codex::Bridged;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use typing::Typing;

pub mod codex;
#[cfg(all(test, unix))]
mod tests;
pub mod typing;

/// How long a Claude Code agent's message waits for the CrazyAgent relay (K2a) before the
/// Auto setting types it instead (DEC-104 Q13).
pub const NATIVE_GRACE_MS: i64 = 60_000;
/// How long the engine waits before it tries an agent again after its messages could not
/// go: a busy agent, the owner typing, or a bridge that did not answer.
pub const RETRY_MS: i64 = 2_000;
/// The most recipients one tick tries, oldest waiting message first.
pub const RECIPIENTS_PER_TICK_MAX: usize = 16;
/// The most message text one engine-started turn carries in its prompt.
pub const NEXT_TURN_BYTES_MAX: usize = 8 * 1024;
/// The idempotency key prefix of the engine's note of a lead's workers' results.
pub const RESULTS_KEY: &str = "lead-results-";
/// The idempotency key prefix of the engine's note of a lead's workers' events.
pub const EVENTS_KEY: &str = "events-";
const SENDER_BYTES_MAX: usize = 48;
const HEAD_BYTES_MAX: usize = 160;
const CUT: &str = " [cut here; read the whole message with inspect_work]";
const RETRIES_KEPT_MAX: usize = 1024;

const _: () = assert!(HEAD_BYTES_MAX + CUT.len() + 512 < PROMPT_BYTES_MAX);
const _: () = assert!(NEXT_TURN_BYTES_MAX >= super::BODY_BYTES_MAX + HEAD_BYTES_MAX);
const _: () = assert!(SENDER_BYTES_MAX + 100 < HEAD_BYTES_MAX);

/// What a tick needs besides the database: the terminal host team agents run in, none
/// when they run headless; the owner typing signal; the team state folder that holds
/// each agent's home (`crate::team::runtime::agent_home`); and the idle agents whose
/// messages wait for the turn the engine wakes them with instead of being typed in.
pub struct Reach<'a> {
    pub host: Option<&'a dyn Host>,
    pub owner: Option<&'a OwnerTyping>,
    pub state: &'a Path,
    pub asleep: &'a BTreeSet<AgentId>,
}

/// The delivery loop's memory between ticks: when each agent whose messages could not go
/// may be tried again, the last refused wake of each agent, at most `RETRIES_KEPT_MAX`
/// agents each, and the last tick's failure code.
#[derive(Debug, Default)]
pub struct Deliveries {
    retry: BTreeMap<AgentId, i64>,
    refused: BTreeMap<AgentId, &'static str>,
    failed: Option<String>,
}

impl Deliveries {
    /// Remembers why the engine could not wake an agent for its messages, so the engine log
    /// names a refusal once until it changes or the agent is woken. Takes the agent and the
    /// refusal code, none once it was woken. Returns true when the refusal is new.
    pub fn wake_refused(&mut self, agent: AgentId, code: Option<&'static str>) -> bool {
        let new = match code {
            Some(code) => self.refused.insert(agent, code) != Some(code),
            None => {
                self.refused.remove(&agent);
                false
            }
        };
        while self.refused.len() > RETRIES_KEPT_MAX {
            let first = self.refused.keys().next().copied();
            if let Some(first) = first {
                self.refused.remove(&first);
            }
        }
        assert!(self.refused.len() <= RETRIES_KEPT_MAX);
        new
    }

    /// Runs [`Deliveries::tick`] for the team loop, which goes on when it fails: a failure
    /// code is written to the engine log once, until a different failure or a clean tick
    /// replaces it, as the adoption check does. Takes the database, the project, the time in
    /// ms and the reach. Returns nothing.
    pub fn tick_logged(
        &mut self,
        db: &Connection,
        project: &ProjectId,
        now: i64,
        reach: &Reach<'_>,
    ) {
        let failed = self
            .tick(db, project, now, reach)
            .err()
            .map(|problem| crate::protocol::plain_code(&problem.0).to_owned());
        if failed.is_some() && failed != self.failed {
            eprintln!(
                "team messages not delivered: {}",
                failed.as_deref().unwrap_or_default()
            );
        }
        self.failed = failed;
    }

    /// Moves one project's waiting messages toward their agents, once per team tick: the
    /// messages of an agent whose session died go back to Sent for its next session, and
    /// each live recipient gets its messages through its harness's channel under the
    /// project's Delivery setting (DEC-104 Q13). A message never goes to anyone else.
    /// Takes the database, the project, the time in ms and the reach. Returns any failure.
    pub fn tick(
        &mut self,
        db: &Connection,
        project: &ProjectId,
        now: i64,
        reach: &Reach<'_>,
    ) -> Result<(), EngineError> {
        requeue_stopped(db, project, now)?;
        for agent in recipients(db, project)? {
            if reach.asleep.contains(&agent) {
                self.retry.remove(&agent);
                continue;
            }
            if self.retry.get(&agent).is_some_and(|at| *at > now) {
                continue;
            }
            if deliver_to(db, project, agent, now, reach)? {
                self.retry.remove(&agent);
            } else {
                self.retry.insert(agent, now + RETRY_MS);
            }
        }
        while self.retry.len() > RETRIES_KEPT_MAX {
            let first = self.retry.keys().next().copied();
            if let Some(first) = first {
                self.retry.remove(&first);
            }
        }
        assert!(self.retry.len() <= RETRIES_KEPT_MAX);
        Ok(())
    }
}

/// Puts back the messages delivered or typed to agents of a project whose session has
/// stopped or failed, however it ended, so their next session gets them. Takes the
/// database, the project and the time in ms. Returns any failure.
fn requeue_stopped(db: &Connection, project: &ProjectId, now: i64) -> Result<(), EngineError> {
    let stopped: Vec<AgentId> = {
        let mut statement = db
            .prepare(
                "SELECT DISTINCT m.to_agent FROM coord_messages m JOIN team_agents a ON a.id=m.to_agent
                 WHERE m.project=?1 AND m.to_kind='agent' AND m.receipt IN ('delivered','typed')
                 AND a.status IN ('stopped','failed') LIMIT ?2",
            )
            .map_err(state)?;
        let limit = i64::try_from(RECIPIENTS_PER_TICK_MAX).map_err(state)?;
        statement
            .query_map(params![project.as_str(), limit], |row| row.get(0))
            .map_err(state)?
            .collect::<Result<_, _>>()
            .map_err(state)?
    };
    for agent in stopped {
        super::requeue_dead(db, agent, now)?;
    }
    Ok(())
}

/// Lists a project's live agents with messages waiting, the one waiting longest first.
/// Takes the database and the project. Returns at most `RECIPIENTS_PER_TICK_MAX` ids.
fn recipients(db: &Connection, project: &ProjectId) -> Result<Vec<AgentId>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT m.to_agent FROM coord_messages m JOIN team_agents a ON a.id=m.to_agent
             WHERE m.project=?1 AND m.to_kind='agent' AND m.receipt='sent'
             AND a.status IN ('starting','idle','working')
             GROUP BY m.to_agent ORDER BY MIN(m.id) LIMIT ?2",
        )
        .map_err(state)?;
    let limit = i64::try_from(RECIPIENTS_PER_TICK_MAX).map_err(state)?;
    let found: Vec<AgentId> = statement
        .query_map(params![project.as_str(), limit], |row| row.get(0))
        .map_err(state)?
        .collect::<Result<_, _>>()
        .map_err(state)?;
    assert!(found.len() <= RECIPIENTS_PER_TICK_MAX);
    Ok(found)
}

/// Delivers one agent's waiting messages, in order, through the first channel its
/// harness kind and Delivery setting allow: the master hears them through its own engine
/// input; Codex through its app-server bridge, else typing under Auto; Claude Code
/// through the relay, typed under Auto only after `NATIVE_GRACE_MS`; other kinds by
/// typing, or with their next engine-started turn under Native only. Takes the database,
/// the project, the agent, the time and the reach. Returns true when nothing is left to
/// retry soon, false when the agent should be tried again after `RETRY_MS`.
fn deliver_to(
    db: &Connection,
    project: &ProjectId,
    agent: AgentId,
    now: i64,
    reach: &Reach<'_>,
) -> Result<bool, EngineError> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT harness,role FROM team_agents WHERE id=?1 AND project=?2",
            params![agent, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(state)?;
    let Some((harness, role)) = row else {
        return Ok(true);
    };
    let messages = super::pending(db, agent, PENDING_MAX)?;
    if messages.is_empty() {
        return Ok(true);
    }
    if role == "master" {
        return to_master(db, project, &messages, now);
    }
    let kind = HarnessKind::from_harness(&harness);
    let delivery = match kind {
        Some(kind) => settings::delivery(db, project, kind)?,
        None => Delivery::default(),
    };
    let ready: &[Message] = match (kind, delivery) {
        (Some(HarnessKind::ClaudeCode), Delivery::NativeOnly) => return Ok(true),
        (Some(HarnessKind::ClaudeCode), Delivery::Auto) => {
            let old = messages
                .iter()
                .take_while(|message| now.saturating_sub(message.created) >= NATIVE_GRACE_MS)
                .count();
            &messages[..old]
        }
        (Some(HarnessKind::Codex), Delivery::TypingOnly) => messages.as_slice(),
        (Some(HarnessKind::Codex), _) => {
            let (text, carried) = lines(db, &messages, PROMPT_BYTES_MAX)?;
            let home = crate::team::runtime::agent_home(reach.state, agent);
            if codex::deliver(&home, &text) == Bridged::Delivered {
                mark_all(db, &carried, Receipt::Delivered, now)?;
                return Ok(carried.len() == messages.len());
            }
            if delivery == Delivery::NativeOnly {
                return Ok(false);
            }
            messages.as_slice()
        }
        (_, Delivery::NativeOnly) => return Ok(true),
        _ => messages.as_slice(),
    };
    if ready.is_empty() {
        return Ok(true);
    }
    let Some(host) = reach.host else {
        return Ok(false);
    };
    let (text, carried) = lines(db, ready, PROMPT_BYTES_MAX)?;
    match typing::type_into(db, host, reach.owner, agent, &text, now)? {
        Typing::Typed => {
            mark_all(db, &carried, Receipt::Typed, now)?;
            Ok(carried.len() == messages.len())
        }
        Typing::Held(_) | Typing::Gone => Ok(false),
    }
}

/// Hands a project master its waiting messages through the engine's own master input
/// (`feed_live` or the next master turn), which already waits for the owner, as one engine
/// message once none of the master's other messages wait; the engine message and the
/// Delivered receipts commit together, so a failure leaves both undone. Takes the database,
/// the project, the messages and the time. Returns true once they were handed over.
fn to_master(
    db: &Connection,
    project: &ProjectId,
    messages: &[Message],
    now: i64,
) -> Result<bool, EngineError> {
    let (text, carried) = lines(db, messages, crate::team::MESSAGE_BYTES_MAX)?;
    let tx =
        rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(state)?;
    let waiting: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM team_messages WHERE project=?1 AND state IN ('pending','sent')",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(state)?;
    if waiting > 0 {
        return Ok(false);
    }
    let appended = crate::team::append_message(
        &tx,
        project,
        MessageAuthor::Engine,
        &text,
        MessageState::Pending,
        now,
    )?;
    if appended.is_err() {
        return Ok(false);
    }
    mark_all(&tx, &carried, Receipt::Delivered, now)?;
    tx.commit().map_err(state)?;
    Ok(carried.len() == messages.len())
}

/// Tells whether an agent has a waiting message worth a turn of its own: a prompt, a
/// question or a reply, or the engine's note of its workers' results or events. Other
/// notes wait for its next turn. Takes the database and the agent. Returns the answer, or
/// a database failure.
pub fn waking(db: &Connection, agent: AgentId) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM coord_messages WHERE to_kind='agent' AND to_agent=?1
         AND receipt='sent' AND (kind IN ('prompt','question','reply') OR (from_kind='engine'
         AND (substr(idem_key,1,length(?2))=?2 OR substr(idem_key,1,length(?3))=?3))))",
        params![agent, RESULTS_KEY, EVENTS_KEY],
        |row| row.get(0),
    )
    .map_err(state)
}

/// Takes the messages waiting for an agent that fit in `NEXT_TURN_BYTES_MAX`, as the lines
/// the prompt of the turn the engine starts for it now ends with, "the next turn" of
/// DEC-104 Q13. Mark them with [`delivered`] once that prompt went out. Takes the database
/// and the agent. Returns the text, empty for none, and the ids it carries.
pub fn next_turn(db: &Connection, agent: AgentId) -> Result<(String, Vec<MessageId>), EngineError> {
    let messages = super::pending(db, agent, PENDING_MAX)?;
    if messages.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let (text, carried) = lines(db, &messages, NEXT_TURN_BYTES_MAX - 64)?;
    Ok((format!("\n\nMessages for you:\n{text}\n"), carried))
}

/// Marks messages carried by an engine-started turn Delivered. Takes the database, the
/// ids and the time in ms. Returns any failure; a receipt that moved on meanwhile stays.
pub fn delivered(db: &Connection, carried: &[MessageId], now: i64) -> Result<(), EngineError> {
    mark_all(db, carried, Receipt::Delivered, now)
}

fn mark_all(
    db: &Connection,
    carried: &[MessageId],
    receipt: Receipt,
    now: i64,
) -> Result<(), EngineError> {
    for message in carried {
        match super::mark(db, *message, receipt, now) {
            Ok(()) => {}
            Err(problem) if problem.0 == "coord_receipt_stale" => {}
            Err(problem) => return Err(problem),
        }
    }
    Ok(())
}

/// Writes messages as the lines an agent reads, oldest first, each starting with
/// `[crazyagent msg <id>] from <sender>:` so its read receipt can find it, until the next
/// would pass `max` bytes. The first always fits, its body cut with a note pointing to
/// `inspect_work` when it alone would pass. Takes the database, the messages and the byte
/// bound. Returns the text and the ids it carries.
fn lines(
    db: &Connection,
    messages: &[Message],
    max: usize,
) -> Result<(String, Vec<MessageId>), EngineError> {
    assert!(!messages.is_empty() && max > HEAD_BYTES_MAX + CUT.len() + 512);
    let mut text = String::new();
    let mut carried = Vec::new();
    for message in messages {
        let line = line(db, message, max)?;
        if !text.is_empty() && text.len() + 1 + line.len() > max {
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&line);
        carried.push(message.id);
    }
    assert!(!carried.is_empty() && text.len() <= max);
    Ok((text, carried))
}

fn line(db: &Connection, message: &Message, max: usize) -> Result<String, EngineError> {
    let sender = crate::team::bound_text(&sender(db, message.from)?, SENDER_BYTES_MAX);
    let what = match (message.kind, message.question, message.reply_to) {
        (MessageKind::Question, Some((target, severity)), _) => format!(
            "question (for {}, {}), answer with update_work: ",
            target.as_str(),
            severity.as_str()
        ),
        (MessageKind::Prompt, ..) => "work for you, answer with update_work: ".to_owned(),
        (MessageKind::Reply, _, Some(earlier)) => format!("Reply to msg {earlier}: "),
        (MessageKind::Note, ..) => "note, it takes no reply: ".to_owned(),
        _ => String::new(),
    };
    let head = format!("[crazyagent msg {}] from {sender}: {what}", message.id);
    assert!(head.len() <= HEAD_BYTES_MAX);
    let room = max - head.len();
    let line = if message.body.len() <= room {
        format!("{head}{}", message.body)
    } else {
        let body = crate::team::bound_text(&message.body, room - CUT.len());
        format!("{head}{body}{CUT}")
    };
    assert!(line.len() <= max);
    Ok(line)
}

fn sender(db: &Connection, from: Party) -> Result<String, EngineError> {
    let agent = match from {
        Party::Owner => return Ok("the owner".to_owned()),
        Party::Engine => return Ok("CrazyAgent".to_owned()),
        Party::Agent(agent) => agent,
    };
    let master: bool = db
        .query_row(
            "SELECT role='master' FROM team_agents WHERE id=?1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?
        .unwrap_or(false);
    if master {
        return Ok("the master".to_owned());
    }
    crate::team::events::label(db, agent)
}
