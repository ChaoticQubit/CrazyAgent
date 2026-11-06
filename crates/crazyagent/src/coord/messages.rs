use super::settings::{self, QuestionsTo};
use super::store::{
    Change, MESSAGE_COLUMNS, agent_in, check_text, failure, master_of, message, message_from_row,
    party_columns, party_in, state,
};
use super::{
    AgentId, BODY_BYTES_MAX, KEY_BYTES_MAX, Message, MessageId, MessageKind, NewMessage,
    PENDING_MAX, Party, QuestionFor, Receipt, Severity,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};

/// Sends one message and returns its id with receipt Sent. A question (both tags
/// required, Q14b) goes where its tags send it: For: Me to the owner; For: Lead to the
/// asker's lead, or the owner when the "Sub-agent questions go to" setting is Me, a lead
/// asking the master and the master the owner; For: Master to the running master, the
/// master asking the owner. A question re-sent up the chain names the one it passes on
/// in `reply_to`: only its recipient may pass it, never a For: Me one, and never at a
/// lower severity. Other kinds go to `to`; Reply only through `reply`. The same
/// idempotency key from the same sender returns the first id. Takes the database, the
/// message and the time in ms. Returns the id or a plain refusal.
pub fn send(db: &Connection, message: NewMessage, now: i64) -> Result<MessageId, EngineError> {
    check_text(&message.body, BODY_BYTES_MAX)?;
    if let Some(key) = &message.idempotency_key {
        check_key(key)?;
    }
    if message.kind == MessageKind::Reply {
        return Err(failure("coord_reply_invalid"));
    }
    let change = Change::begin(db)?;
    let tx = change.db();
    party_in(tx, &message.project, message.from)?;
    let (from_kind, from_agent) = party_columns(message.from);
    if let Some(key) = &message.idempotency_key {
        let first: Option<MessageId> = tx
            .query_row(
                "SELECT id FROM coord_messages WHERE project=?1 AND from_kind=?2
                 AND from_agent IS ?3 AND idem_key=?4",
                params![message.project.as_str(), from_kind, from_agent, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(state)?;
        if let Some(id) = first {
            change.commit()?;
            return Ok(id);
        }
    }
    let to = match (message.kind, message.question) {
        (MessageKind::Question, Some((target, severity))) => route(
            tx,
            &message.project,
            message.from,
            target,
            severity,
            message.reply_to,
        )?,
        (MessageKind::Question, None) => return Err(failure("coord_question_tags_missing")),
        (_, Some(_)) => return Err(failure("coord_question_tags_invalid")),
        (_, None) => {
            party_in(tx, &message.project, message.to)?;
            message.to
        }
    };
    if to == message.from {
        return Err(failure("coord_message_self"));
    }
    if to == Party::Engine {
        return Err(failure("coord_recipient_invalid"));
    }
    if let Some(work) = message.work {
        work_in(tx, &message.project, work)?;
    }
    if let Some(earlier) = message.reply_to
        && super::store::message(tx, earlier)?.project != message.project
    {
        return Err(failure("coord_message_unknown"));
    }
    let id = insert(tx, &message, to, now)?;
    if let Party::Agent(agent) = message.from {
        super::presence::seen(tx, agent, now)?;
    }
    change.commit()?;
    Ok(id)
}

/// Answers one prompt or question. A For: Me question takes the owner's answer only
/// (Q14b); any other is answered by its recipient, and the owner may answer anything in
/// the project. The answer goes back to the asker and the original becomes Replied.
/// Takes the database, who answers, the message answered, the text and the time in ms.
/// Returns the answer's id or a plain refusal.
pub fn reply(
    db: &Connection,
    from: Party,
    to_message: MessageId,
    body: String,
    now: i64,
) -> Result<MessageId, EngineError> {
    check_text(&body, BODY_BYTES_MAX)?;
    let change = Change::begin(db)?;
    let tx = change.db();
    let original = message(tx, to_message)?;
    party_in(tx, &original.project, from)?;
    if !matches!(original.kind, MessageKind::Prompt | MessageKind::Question) {
        return Err(failure("coord_reply_invalid"));
    }
    match (original.question, from) {
        (Some((QuestionFor::Me, _)), Party::Agent(_)) => {
            return Err(failure("coord_question_for_owner"));
        }
        (_, Party::Owner) => {}
        _ if original.to == from => {}
        _ => return Err(failure("coord_reply_not_addressed")),
    }
    if original.from == from {
        return Err(failure("coord_message_self"));
    }
    let answer = NewMessage {
        project: original.project.clone(),
        from,
        to: original.from,
        kind: MessageKind::Reply,
        question: None,
        work: original.work,
        reply_to: Some(original.id),
        body,
        idempotency_key: None,
    };
    let id = insert(tx, &answer, original.from, now)?;
    if original.receipt.may_become(Receipt::Replied) {
        set_receipt(tx, original.id, Receipt::Replied, now)?;
    }
    if let Party::Agent(agent) = from {
        super::presence::seen(tx, agent, now)?;
    }
    change.commit()?;
    Ok(id)
}

/// Lists the messages waiting for delivery to one agent, oldest first: receipt Sent.
/// Takes the database, the agent and the most to return, capped at `PENDING_MAX`.
/// Returns the messages.
pub fn pending(db: &Connection, to: AgentId, limit: usize) -> Result<Vec<Message>, EngineError> {
    let limit = i64::try_from(limit.min(PENDING_MAX)).map_err(state)?;
    let mut statement = db
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM coord_messages
             WHERE to_kind='agent' AND to_agent=?1 AND receipt='sent' ORDER BY id LIMIT ?2"
        ))
        .map_err(state)?;
    let rows = statement
        .query_map(params![to, limit], message_from_row)
        .map_err(state)?;
    let found = rows.collect::<Result<Vec<_>, _>>().map_err(state)?;
    assert!(found.len() <= PENDING_MAX);
    Ok(found)
}

/// Lists the messages one agent has not read yet, oldest first: receipt Sent,
/// Delivered or Typed. Takes the database, the agent and the most to return, capped at
/// `PENDING_MAX`. Returns the messages.
pub fn unread(db: &Connection, to: AgentId, limit: usize) -> Result<Vec<Message>, EngineError> {
    let limit = i64::try_from(limit.min(PENDING_MAX)).map_err(state)?;
    let mut statement = db
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM coord_messages
             WHERE to_kind='agent' AND to_agent=?1 AND receipt IN ('sent','delivered','typed')
             ORDER BY id LIMIT ?2"
        ))
        .map_err(state)?;
    let rows = statement
        .query_map(params![to, limit], message_from_row)
        .map_err(state)?;
    let found = rows.collect::<Result<Vec<_>, _>>().map_err(state)?;
    assert!(found.len() <= PENDING_MAX);
    Ok(found)
}

/// Lists the messages one agent sent, newest first, with how far each got. Takes the
/// database, the agent and the most to return, capped at `PENDING_MAX`. Returns them.
pub fn outbox(db: &Connection, from: AgentId, limit: usize) -> Result<Vec<Message>, EngineError> {
    let limit = i64::try_from(limit.min(PENDING_MAX)).map_err(state)?;
    let mut statement = db
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM coord_messages
             WHERE from_kind='agent' AND from_agent=?1 ORDER BY id DESC LIMIT ?2"
        ))
        .map_err(state)?;
    let rows = statement
        .query_map(params![from, limit], message_from_row)
        .map_err(state)?;
    let found = rows.collect::<Result<Vec<_>, _>>().map_err(state)?;
    assert!(found.len() <= PENDING_MAX);
    Ok(found)
}

/// Moves a message's receipt forward (`Receipt::may_become`). The same receipt again is
/// a no-op, since delivery is at least once. Takes the database, the message, the new
/// receipt and the time in ms. Returns Ok, `coord_message_unknown` or, for a receipt
/// that would move back or leave an ending, `coord_receipt_stale`.
pub fn mark(
    db: &Connection,
    message: MessageId,
    receipt: Receipt,
    now: i64,
) -> Result<(), EngineError> {
    let change = Change::begin(db)?;
    let current = super::store::message(change.db(), message)?.receipt;
    if current == receipt {
        return change.commit();
    }
    if !current.may_become(receipt) {
        return Err(failure("coord_receipt_stale"));
    }
    set_receipt(change.db(), message, receipt, now)?;
    change.commit()
}

/// Puts back every message delivered or typed to an agent whose session died before
/// reading it, so the next session gets it; it never goes to another agent. Takes the
/// database, the agent and the time in ms. Returns how many went back to Sent.
pub fn requeue_dead(db: &Connection, agent: AgentId, now: i64) -> Result<u32, EngineError> {
    let moved = db
        .execute(
            "UPDATE coord_messages SET receipt='sent', receipt_at=?2, turn=NULL
             WHERE to_kind='agent' AND to_agent=?1 AND receipt IN ('delivered','typed')",
            params![agent, now],
        )
        .map_err(state)?;
    u32::try_from(moved).map_err(state)
}

/// Names the Codex turn a bridge delivery put messages into (b19 P2), kept only while
/// each stays Delivered since every later receipt change clears it, so an abort of that
/// turn can put back what it dropped. Takes the database, the messages and the turn id.
/// Returns Ok or a SQLite failure.
pub(crate) fn delivered_into(
    db: &Connection,
    carried: &[MessageId],
    turn: &str,
) -> Result<(), EngineError> {
    for message in carried {
        db.execute(
            "UPDATE coord_messages SET turn=?2 WHERE id=?1 AND receipt='delivered'",
            params![message, turn],
        )
        .map_err(state)?;
    }
    Ok(())
}

/// Puts back to Sent one message the bridge delivered to an agent into a Codex turn that
/// ended aborted and dropped it before its session recorded it (b19 P2), so the agent's
/// next turn gets it; one that went into another turn, to another agent, or whose receipt
/// moved on, is left alone. Takes the database, the message, the agent, the turn id and
/// the time in ms. Returns Ok or a SQLite failure.
pub(crate) fn requeue_aborted(
    db: &Connection,
    id: MessageId,
    agent: AgentId,
    turn: &str,
    now: i64,
) -> Result<(), EngineError> {
    db.execute(
        "UPDATE coord_messages SET receipt='sent', receipt_at=?4, turn=NULL
         WHERE id=?1 AND to_kind='agent' AND to_agent=?2 AND receipt='delivered' AND turn=?3",
        params![id, agent, turn, now],
    )
    .map_err(state)?;
    Ok(())
}

/// Finds whom a question goes to from its tags and the asker's place (Q14, Q14b), and
/// checks a passed-on question against the one it passes on. Takes the transaction, the
/// project, the asker, both tags and the question passed on, if any. Returns the
/// recipient or a plain refusal.
fn route(
    tx: &Connection,
    project: &ProjectId,
    from: Party,
    target: QuestionFor,
    severity: Severity,
    passed: Option<MessageId>,
) -> Result<Party, EngineError> {
    let Party::Agent(asker) = from else {
        return Err(failure("coord_question_from_agent"));
    };
    let row = agent_in(tx, project, asker)?;
    if let Some(earlier) = passed {
        let earlier = message(tx, earlier)?;
        let Some((earlier_for, earlier_severity)) = earlier.question else {
            return Err(failure("coord_question_forward_invalid"));
        };
        if earlier_for == QuestionFor::Me {
            return Err(failure("coord_question_for_owner"));
        }
        if earlier.project != *project || earlier.to != from {
            return Err(failure("coord_question_forward_invalid"));
        }
        if severity.rank() < earlier_severity.rank() {
            return Err(failure("coord_question_severity_lowered"));
        }
    }
    let master = || -> Result<Party, EngineError> {
        if row.master() {
            return Ok(Party::Owner);
        }
        master_of(tx, project)?
            .map(Party::Agent)
            .ok_or_else(|| failure("coord_master_absent"))
    };
    match target {
        QuestionFor::Me => Ok(Party::Owner),
        QuestionFor::Master => master(),
        QuestionFor::Lead => match row.parent {
            Some(_) if settings::read(tx, project)?.questions_to == QuestionsTo::Me => {
                Ok(Party::Owner)
            }
            Some(lead) => Ok(Party::Agent(lead)),
            None => master(),
        },
    }
}

/// Stores one message with receipt Sent. Takes the transaction, the message, its routed
/// recipient and the time in ms. Returns the new id.
fn insert(
    tx: &Connection,
    message: &NewMessage,
    to: Party,
    now: i64,
) -> Result<MessageId, EngineError> {
    let (from_kind, from_agent) = party_columns(message.from);
    let (to_kind, to_agent) = party_columns(to);
    let (target, severity) = match message.question {
        Some((target, severity)) => (Some(target.as_str()), Some(severity.as_str())),
        None => (None, None),
    };
    tx.execute(
        "INSERT INTO coord_messages(project,from_kind,from_agent,to_kind,to_agent,kind,q_for,
         q_severity,work,reply_to,body,idem_key,receipt,created,receipt_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'sent',?13,?13)",
        params![
            message.project.as_str(),
            from_kind,
            from_agent,
            to_kind,
            to_agent,
            message.kind.as_str(),
            target,
            severity,
            message.work,
            message.reply_to,
            message.body,
            message.idempotency_key,
            now
        ],
    )
    .map_err(state)?;
    Ok(tx.last_insert_rowid())
}

fn set_receipt(
    tx: &Connection,
    message: MessageId,
    receipt: Receipt,
    now: i64,
) -> Result<(), EngineError> {
    let changed = tx
        .execute(
            "UPDATE coord_messages SET receipt=?2, receipt_at=?3, turn=NULL WHERE id=?1",
            params![message, receipt.as_str(), now],
        )
        .map_err(state)?;
    assert_eq!(changed, 1);
    Ok(())
}

/// Checks that a unit of work belongs to the project. Returns Ok or `coord_work_unknown`.
fn work_in(tx: &Connection, project: &ProjectId, work: i64) -> Result<(), EngineError> {
    let found: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM coord_work WHERE id=?1 AND project=?2",
            params![work, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    found
        .map(|_| ())
        .ok_or_else(|| failure("coord_work_unknown"))
}

/// Checks an idempotency key: 1 to `KEY_BYTES_MAX` bytes of letters, digits, `-`, `_`,
/// `.` and `:`. Returns Ok or `coord_key_invalid`.
fn check_key(key: &str) -> Result<(), EngineError> {
    let fine = !key.is_empty()
        && key.len() <= KEY_BYTES_MAX
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    fine.then_some(())
        .ok_or_else(|| failure("coord_key_invalid"))
}
