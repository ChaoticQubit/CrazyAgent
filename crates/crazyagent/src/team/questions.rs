use super::events::{self, NotificationKind};
use super::{
    AgentRole, AgentStatus, Outcome, STATUS_REPLY_BYTES_MAX, STATUS_TEXT_CAPS, cut_text,
    encoded_len, error, failure, rejected,
};
use crate::coord::{self, MessageKind, Party, QuestionFor, Receipt, Severity, Waiting};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::terminal::AgentId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// The most open owner questions one inbox read lists; the others are counted in `more`.
pub const QUESTIONS_SHOWN_MAX: usize = 32;
/// The most agents waiting on the owner in their own terminal one inbox read lists.
pub const WAITING_SHOWN_MAX: usize = 32;
/// The most new questions and the most new terminal waits one tick alerts the owner about.
pub const ALERTS_PER_TICK_MAX: usize = 16;
/// The most bytes of one alert's text in the owner's notifications.
pub const ALERT_TEXT_BYTES_MAX: usize = 240;
/// The label that starts the line where an asker states what it assumes until answered.
pub const ASSUMPTION_LABEL: &str = "assumption:";
/// The name a question meant for the owner shows for whom it is meant.
pub const OWNER_LABEL: &str = "Me";

const QUESTION: &str = "question";
const WAITING: &str = "waiting";
const OWNER: &str = "owner";
const AGENT: &str = "agent";
const REASK_DUE: &str = "FROM questions_log WHERE reask=1 AND status='pending'";
const OPEN: [Receipt; 4] = [
    Receipt::Sent,
    Receipt::Delivered,
    Receipt::Typed,
    Receipt::Read,
];

const _: () = assert!(QUESTIONS_SHOWN_MAX <= 64 && WAITING_SHOWN_MAX <= 64);
const _: () = assert!(ALERTS_PER_TICK_MAX <= 64 && ALERT_TEXT_BYTES_MAX <= 1024);

/// One open question an agent put to the owner, as the Questions inbox shows it: who asked,
/// how urgent it is, whom the asker meant it for and that party's name, the question, what
/// the asker assumes until the owner answers when it said so, when it was asked and the
/// asker's terminal while the asker runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerQuestion {
    pub id: i64,
    pub agent: i64,
    pub label: String,
    pub severity: Severity,
    pub asked_for: QuestionFor,
    pub for_label: String,
    pub text: String,
    pub assumption: Option<String>,
    pub created_ms: i64,
    pub terminal: Option<AgentId>,
}

/// One running agent that waits on its user in its own terminal (Q14): a permission prompt
/// or a question it asked there, which only that terminal can answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingAgent {
    pub agent: i64,
    pub label: String,
    pub waiting: Waiting,
    pub since_ms: i64,
    pub terminal: Option<AgentId>,
}

/// One project's Questions inbox for the owner: the open questions put to the owner, the
/// most urgent first and then the oldest, how many more are open than it lists, and the
/// running agents that wait on the owner in their own terminal, the longest waiting first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbox {
    pub project_id: ProjectId,
    pub questions: Vec<OwnerQuestion>,
    pub more: u32,
    pub waiting: Vec<WaitingAgent>,
}

/// Who asked or waits, as the owner sees it: the label every client shows, whether the
/// agent runs and its terminal while it does.
struct Asker {
    label: String,
    live: bool,
    terminal: Option<AgentId>,
}

/// Creates the table that remembers which questions and terminal waits already reached the
/// owner, and which asker an urgent question paused. Takes the engine connection. Returns
/// any SQLite failure.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS team_owner_alerts (
                source TEXT NOT NULL,
                ref INTEGER NOT NULL,
                since INTEGER NOT NULL,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                notification INTEGER,
                paused INTEGER,
                settled INTEGER,
                PRIMARY KEY(source, ref, since)
            );
            CREATE INDEX IF NOT EXISTS team_owner_alerts_paused ON team_owner_alerts(agent, paused);",
        )
        .map_err(error)?;
    crate::questions::initialize(connection)
}

/// Reads one project's Questions inbox for the owner, bounded to one owner reply: texts are
/// cut to a shared cap, halved until the inbox fits, then the least urgent questions move
/// into `more`. Takes the connection and the project. Returns the inbox, or
/// `coord_questions_bounds` when even an empty list does not fit.
pub fn inbox(connection: &Connection, project: &ProjectId) -> Result<Inbox, EngineError> {
    let open = open_receipts();
    let filter = format!(
        "FROM coord_messages WHERE project=?1 AND kind='{}' AND to_kind='{OWNER}'
           AND from_kind='{AGENT}' AND from_agent IS NOT NULL AND receipt IN ({open})
           AND NOT EXISTS (SELECT 1 FROM questions_log q WHERE q.coord_message=coord_messages.id)",
        MessageKind::Question.as_str()
    );
    let total: i64 = connection
        .query_row(
            &format!("SELECT COUNT(*) {filter}"),
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    type QuestionRow = (i64, i64, Option<String>, Option<String>, String, i64);
    let rows: Vec<QuestionRow> = connection
        .prepare(&format!(
            "SELECT id, from_agent, q_for, q_severity, body, created {filter}
             ORDER BY CASE q_severity WHEN '{}' THEN 0 WHEN '{}' THEN 1 ELSE 2 END, id
             LIMIT ?2",
            Severity::High.as_str(),
            Severity::Normal.as_str()
        ))
        .map_err(error)?
        .query_map(
            params![project.as_str(), QUESTIONS_SHOWN_MAX as i64],
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
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let mut questions = Vec::with_capacity(rows.len());
    for (id, agent, asked_for, severity, body, created) in rows {
        let asked_for = asked_for
            .as_deref()
            .and_then(QuestionFor::parse)
            .ok_or_else(|| failure("coord_question_invalid"))?;
        let severity = severity
            .as_deref()
            .and_then(Severity::parse)
            .ok_or_else(|| failure("coord_question_invalid"))?;
        let for_label = meant_for(connection, project, agent, asked_for)?;
        let asker = asker(connection, project, agent)?;
        let (text, assumption) = split_assumption(&body);
        questions.push(OwnerQuestion {
            id,
            agent,
            label: asker.label,
            severity,
            asked_for,
            for_label,
            text,
            assumption,
            created_ms: created,
            terminal: asker.terminal,
        });
    }
    assert!(questions.len() <= QUESTIONS_SHOWN_MAX);
    let shown = i64::try_from(questions.len()).expect("the shown questions are bounded");
    let more = u32::try_from(total.saturating_sub(shown).max(0)).unwrap_or(u32::MAX);
    fit(Inbox {
        project_id: project.clone(),
        questions,
        more,
        waiting: waiting_agents(connection, project)?,
    })
}

/// Answers one open question an agent put to the owner, as the owner (Q14b), then settles
/// what the question raised: the asker an urgent question paused waits no longer and its
/// notification counts as seen. Takes the owner transaction, the project, the question's
/// message id, the answer and the time. Returns Applied, or `coord_question_missing` for a
/// message that is not a question to the owner in this project and `coord_question_closed`
/// for one already answered or ended, before any row is written.
pub(super) fn answer(
    tx: &Transaction<'_>,
    project: &ProjectId,
    message: i64,
    text: &str,
    now: i64,
) -> Result<Outcome, EngineError> {
    let receipt: Option<String> = tx
        .query_row(
            "SELECT receipt FROM coord_messages WHERE id=?1 AND project=?2 AND kind=?3 AND to_kind=?4",
            params![
                message,
                project.as_str(),
                MessageKind::Question.as_str(),
                OWNER
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(receipt) = receipt else {
        return Ok(rejected("coord_question_missing"));
    };
    let receipt = Receipt::parse(&receipt).ok_or_else(|| failure("coord_receipt_unknown"))?;
    if !OPEN.contains(&receipt) {
        return Ok(rejected("coord_question_closed"));
    }
    coord::reply(tx, Party::Owner, message, text.to_owned(), now)?;
    settle(tx, now)?;
    Ok(Outcome::Applied)
}

/// Brings the owner's alerts up to date once per team tick (Q14, Q14b). A new question to
/// the owner alerts by severity: High notifies, which the app shows at once, and pauses a
/// running asker that waits on nothing else, which turns its node amber; Normal notifies
/// for the badge only; Low waits in the inbox. A new terminal wait of a running agent
/// notifies. Ended questions and waits clear the pause and mark their notification seen.
/// Takes the engine connection and the time. Returns any SQLite failure.
pub fn tick(connection: &mut Connection, now: i64) -> Result<(), EngineError> {
    let due: bool = connection
        .query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 {}) OR EXISTS (SELECT 1 {}) OR EXISTS (SELECT 1 {}) OR EXISTS (SELECT 1 {})
                 OR EXISTS (SELECT 1 {}) OR EXISTS (SELECT 1 {}) OR EXISTS (SELECT 1 {})",
                new_questions(),
                new_waits(),
                ended_questions(),
                ended_waits(),
                unlinked_questions(),
                ended_links(),
                REASK_DUE
            ),
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !due {
        return Ok(());
    }
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    alert_questions(&tx, now)?;
    let mut changed = link_questions(&tx)?;
    alert_waits(&tx, now)?;
    settle(&tx, now)?;
    changed.extend(settle_links(&tx)?);
    crate::questions::send_reask_notes(&tx)?;
    tx.commit().map_err(error)?;
    changed.sort_unstable_by(|one, other| one.as_str().cmp(other.as_str()));
    changed.dedup();
    for project in &changed {
        crate::questions::refresh_page(&*connection, project);
    }
    Ok(())
}

/// Links each open question an agent put to the owner through the team into the questions
/// log (F12), at most `ALERTS_PER_TICK_MAX` a tick: only one whose alert is on record and
/// open, so its end always reaches `settle_links`, in a project with room. The asker is
/// the master or a sub-agent by its role. Takes the tick's transaction. Returns the
/// projects whose questions page changed.
fn link_questions(tx: &Transaction<'_>) -> Result<Vec<ProjectId>, EngineError> {
    let rows: Vec<(i64, String, i64, String)> = tx
        .prepare(&format!(
            "SELECT m.id, m.project, m.from_agent, m.body {} ORDER BY m.id LIMIT {ALERTS_PER_TICK_MAX}",
            unlinked_questions()
        ))
        .map_err(error)?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(rows.len() <= ALERTS_PER_TICK_MAX);
    let mut changed = Vec::with_capacity(rows.len());
    for (message, project, agent, body) in rows {
        let project = ProjectId::new(project).map_err(|_| failure("coord_project_invalid"))?;
        let role: Option<String> = tx
            .query_row(
                "SELECT role FROM team_agents WHERE id=?1 AND project=?2",
                params![agent, project.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        let master = matches!(
            role.as_deref().and_then(AgentRole::parse),
            Some(AgentRole::Master)
        );
        let asker = if master {
            crate::questions::Asker::Master(agent)
        } else {
            crate::questions::Asker::Agent(agent)
        };
        if crate::questions::link_coord(tx, &project, &asker, &body, message)?.is_some() {
            changed.push(project);
        }
    }
    Ok(changed)
}

/// Settles each pending linked question whose team message ended or is gone (F12), at most
/// `ALERTS_PER_TICK_MAX` a tick: answered with the owner's latest team reply to it, else
/// dismissed. Takes the tick's transaction. Returns the projects whose page changed.
fn settle_links(tx: &Transaction<'_>) -> Result<Vec<ProjectId>, EngineError> {
    let messages: Vec<i64> = tx
        .prepare(&format!(
            "SELECT q.coord_message {} ORDER BY q.id LIMIT {ALERTS_PER_TICK_MAX}",
            ended_links()
        ))
        .map_err(error)?
        .query_map([], |row| row.get(0))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(messages.len() <= ALERTS_PER_TICK_MAX);
    let mut changed = Vec::with_capacity(messages.len());
    for message in messages {
        let reply: Option<String> = tx
            .query_row(
                &format!(
                    "SELECT body FROM coord_messages WHERE reply_to=?1 AND kind='{}' AND from_kind='{OWNER}'
                     ORDER BY id DESC LIMIT 1",
                    MessageKind::Reply.as_str()
                ),
                [message],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        if let Some(project) = crate::questions::settle_linked(tx, message, reply.as_deref())? {
            changed.push(project);
        }
    }
    Ok(changed)
}

/// Splits a question's body into the question and what the asker assumes until the owner
/// answers, stated on the last line that starts with "Assumption:" and the lines after it
/// (Q14b, Low). Takes the body. Returns the question and the assumption, if any; a body
/// that is only an assumption stays whole as the question.
pub fn split_assumption(body: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = body.lines().collect();
    let labelled = |line: &&str| {
        line.trim_start()
            .get(..ASSUMPTION_LABEL.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(ASSUMPTION_LABEL))
    };
    let Some(at) = lines.iter().rposition(labelled) else {
        return (body.trim().to_owned(), None);
    };
    let question = lines[..at].join("\n").trim().to_owned();
    if question.is_empty() {
        return (body.trim().to_owned(), None);
    }
    let first = lines[at].trim_start()[ASSUMPTION_LABEL.len()..].trim();
    let rest = lines[at + 1..].join("\n");
    let assumption = [first, rest.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(question.len() <= body.len());
    (question, (!assumption.is_empty()).then_some(assumption))
}

fn open_receipts() -> String {
    OPEN.map(|receipt| format!("'{}'", receipt.as_str()))
        .join(",")
}

fn new_questions() -> String {
    format!(
        "FROM coord_messages m WHERE m.id > (SELECT COALESCE(MAX(a.ref), 0) FROM team_owner_alerts a WHERE a.source='{QUESTION}')
           AND m.kind='{}' AND m.to_kind='{OWNER}' AND m.from_kind='{AGENT}' AND m.from_agent IS NOT NULL",
        MessageKind::Question.as_str()
    )
}

fn new_waits() -> String {
    format!(
        "FROM coord_activity c JOIN team_agents t ON t.id=c.agent
         WHERE c.waiting IS NOT NULL AND c.waiting_since IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM team_owner_alerts a WHERE a.source='{WAITING}' AND a.ref=c.agent AND a.since=c.waiting_since)
           AND NOT EXISTS (SELECT 1 FROM team_owner_alerts a WHERE a.source='{QUESTION}' AND a.agent=c.agent AND a.paused=c.waiting_since)"
    )
}

fn ended_questions() -> String {
    format!(
        "FROM team_owner_alerts a WHERE a.source='{QUESTION}' AND a.settled IS NULL
           AND NOT EXISTS (SELECT 1 FROM coord_messages m WHERE m.id=a.ref AND m.receipt IN ({}))",
        open_receipts()
    )
}

fn unlinked_questions() -> String {
    format!(
        "FROM coord_messages m WHERE m.kind='{}' AND m.to_kind='{OWNER}' AND m.from_kind='{AGENT}'
           AND m.from_agent IS NOT NULL AND m.receipt IN ({})
           AND EXISTS (SELECT 1 FROM team_owner_alerts a WHERE a.source='{QUESTION}' AND a.ref=m.id AND a.settled IS NULL)
           AND NOT EXISTS (SELECT 1 FROM questions_log q WHERE q.coord_message=m.id)
           AND (SELECT COUNT(*) FROM questions_log q WHERE q.project=m.project AND q.status='pending') < {}",
        MessageKind::Question.as_str(),
        open_receipts(),
        crate::questions::PENDING_PER_PROJECT_MAX
    )
}

fn ended_links() -> String {
    format!(
        "FROM questions_log q WHERE q.status='pending' AND q.coord_message IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM coord_messages m WHERE m.id=q.coord_message AND m.receipt IN ({}))",
        open_receipts()
    )
}

fn ended_waits() -> String {
    format!(
        "FROM team_owner_alerts a WHERE a.source='{WAITING}'
           AND NOT EXISTS (SELECT 1 FROM coord_activity c WHERE c.agent=a.ref AND c.waiting IS NOT NULL AND c.waiting_since=a.since)"
    )
}

fn alert_questions(tx: &Transaction<'_>, now: i64) -> Result<(), EngineError> {
    type AlertRow = (i64, String, i64, Option<String>, String, String, i64);
    let rows: Vec<AlertRow> = tx
        .prepare(&format!(
            "SELECT m.id, m.project, m.from_agent, m.q_severity, m.body, m.receipt, m.created {}
             ORDER BY m.id LIMIT {ALERTS_PER_TICK_MAX}",
            new_questions()
        ))
        .map_err(error)?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(rows.len() <= ALERTS_PER_TICK_MAX);
    for (id, project, agent, severity, body, receipt, created) in rows {
        let project = ProjectId::new(project).map_err(|_| failure("coord_project_invalid"))?;
        let receipt = Receipt::parse(&receipt).ok_or_else(|| failure("coord_receipt_unknown"))?;
        let severity = severity
            .as_deref()
            .and_then(Severity::parse)
            .ok_or_else(|| failure("coord_question_invalid"))?;
        let mut notification = None;
        let mut paused = None;
        let open = OPEN.contains(&receipt);
        if open {
            let asker = asker(tx, &project, agent)?;
            let (question, _) = split_assumption(&body);
            match severity {
                Severity::High => {
                    let text = alert_text(&format!("{} asks you, urgent: {question}", asker.label));
                    notification = Some(events::notify(
                        tx,
                        &project,
                        NotificationKind::UrgentQuestion,
                        &text,
                        now,
                    )?);
                    if asker.live && waiting_now(tx, agent)?.is_none() {
                        coord::set_waiting(tx, agent, Some(Waiting::Question), now)?;
                        paused = Some(now);
                    }
                }
                Severity::Normal => {
                    let text = alert_text(&format!("{} asks you: {question}", asker.label));
                    notification = Some(events::notify(
                        tx,
                        &project,
                        NotificationKind::Question,
                        &text,
                        now,
                    )?);
                }
                Severity::Low => {}
            }
        }
        tx.execute(
            "INSERT INTO team_owner_alerts(source,ref,since,project,agent,notification,paused,settled)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                QUESTION,
                id,
                created,
                project.as_str(),
                agent,
                notification,
                paused,
                (!open).then_some(now)
            ],
        )
        .map_err(error)?;
    }
    Ok(())
}

fn alert_waits(tx: &Transaction<'_>, now: i64) -> Result<(), EngineError> {
    let rows: Vec<(i64, String, i64, String)> = tx
        .prepare(&format!(
            "SELECT c.agent, c.waiting, c.waiting_since, t.project {}
             ORDER BY c.waiting_since, c.agent LIMIT {ALERTS_PER_TICK_MAX}",
            new_waits()
        ))
        .map_err(error)?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    assert!(rows.len() <= ALERTS_PER_TICK_MAX);
    for (agent, waiting, since, project) in rows {
        let project = ProjectId::new(project).map_err(|_| failure("coord_project_invalid"))?;
        let waiting = Waiting::parse(&waiting).ok_or_else(|| failure("coord_waiting_unknown"))?;
        let asker = asker(tx, &project, agent)?;
        let notification = if asker.live {
            let text = alert_text(&format!(
                "{} is waiting on you in its terminal: {}.",
                asker.label,
                waiting_words(waiting)
            ));
            Some(events::notify(
                tx,
                &project,
                NotificationKind::WaitingOnYou,
                &text,
                now,
            )?)
        } else {
            None
        };
        tx.execute(
            "INSERT INTO team_owner_alerts(source,ref,since,project,agent,notification,paused,settled)
             VALUES (?1,?2,?3,?4,?2,?5,NULL,NULL)",
            params![WAITING, agent, since, project.as_str(), notification],
        )
        .map_err(error)?;
    }
    Ok(())
}

fn settle(tx: &Transaction<'_>, now: i64) -> Result<(), EngineError> {
    let questions: Vec<(i64, i64, Option<i64>, Option<i64>)> = tx
        .prepare(&format!(
            "SELECT a.ref, a.agent, a.paused, a.notification {}",
            ended_questions()
        ))
        .map_err(error)?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    for (message, agent, paused, notification) in questions {
        if let Some(paused) = paused
            && waiting_now(tx, agent)? == Some((Waiting::Question, paused))
        {
            coord::set_waiting(tx, agent, None, now)?;
        }
        seen(tx, notification, now)?;
        let changed = tx
            .execute(
                "UPDATE team_owner_alerts SET settled=?3 WHERE source=?1 AND ref=?2 AND settled IS NULL",
                params![QUESTION, message, now],
            )
            .map_err(error)?;
        assert_eq!(changed, 1);
    }
    let waits: Vec<(i64, i64, Option<i64>)> = tx
        .prepare(&format!(
            "SELECT a.ref, a.since, a.notification {}",
            ended_waits()
        ))
        .map_err(error)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    for (agent, since, notification) in waits {
        seen(tx, notification, now)?;
        tx.execute(
            "DELETE FROM team_owner_alerts WHERE source=?1 AND ref=?2 AND since=?3",
            params![WAITING, agent, since],
        )
        .map_err(error)?;
    }
    tx.execute(
        "DELETE FROM team_owner_alerts WHERE source=?1 AND settled IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM coord_messages m WHERE m.id=team_owner_alerts.ref)",
        [QUESTION],
    )
    .map_err(error)?;
    Ok(())
}

fn seen(tx: &Transaction<'_>, notification: Option<i64>, now: i64) -> Result<(), EngineError> {
    if let Some(id) = notification {
        tx.execute(
            "UPDATE team_notifications SET seen_ms=?2 WHERE id=?1 AND seen_ms IS NULL",
            params![id, now],
        )
        .map_err(error)?;
    }
    Ok(())
}

fn waiting_now(connection: &Connection, agent: i64) -> Result<Option<(Waiting, i64)>, EngineError> {
    let row: Option<(Option<String>, Option<i64>)> = connection
        .query_row(
            "SELECT waiting, waiting_since FROM coord_activity WHERE agent=?1",
            [agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    Ok(match row {
        Some((Some(waiting), Some(since))) => Some((
            Waiting::parse(&waiting).ok_or_else(|| failure("coord_waiting_unknown"))?,
            since,
        )),
        _ => None,
    })
}

/// The name of whom an asker meant a question for, as the owner reads it: Me for the owner,
/// the master's label for the master, and for its lead the label of the asker's lead, the
/// master when the asker has none. Takes the connection, the project, the asker and the
/// tag. Returns the name.
fn meant_for(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    asked_for: QuestionFor,
) -> Result<String, EngineError> {
    let lead = match asked_for {
        QuestionFor::Me => return Ok(OWNER_LABEL.into()),
        QuestionFor::Master => None,
        QuestionFor::Lead => connection
            .query_row(
                "SELECT parent FROM team_agents WHERE id=?1 AND project=?2",
                params![agent, project.as_str()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(error)?
            .flatten(),
    };
    match lead {
        Some(lead) => Ok(asker(connection, project, lead)?.label),
        None => Ok(AgentRole::Master.agent_label(agent)),
    }
}

fn asker(connection: &Connection, project: &ProjectId, agent: i64) -> Result<Asker, EngineError> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT role, status FROM team_agents WHERE id=?1 AND project=?2",
            params![agent, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((role, status)) = row else {
        return Ok(Asker {
            label: AgentRole::Sub.agent_label(agent),
            live: false,
            terminal: None,
        });
    };
    let role = AgentRole::parse(&role).ok_or_else(|| failure("team_agent_role_unknown"))?;
    let live = AgentStatus::parse(&status).is_some_and(AgentStatus::live);
    let label = match role {
        AgentRole::Master => role.agent_label(agent),
        AgentRole::Sub | AgentRole::Teammate => events::label(connection, agent)?,
    };
    Ok(Asker {
        label,
        live,
        terminal: live.then(|| AgentId::team(agent).ok()).flatten(),
    })
}

fn waiting_agents(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<WaitingAgent>, EngineError> {
    let rows: Vec<(i64, String, i64)> = connection
        .prepare(&format!(
            "SELECT c.agent, c.waiting, c.waiting_since FROM coord_activity c JOIN team_agents t ON t.id=c.agent
             WHERE t.project=?1 AND c.waiting IS NOT NULL AND c.waiting_since IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM team_owner_alerts a WHERE a.source='{QUESTION}' AND a.agent=c.agent AND a.paused=c.waiting_since)
             ORDER BY c.waiting_since, c.agent LIMIT ?2"
        ))
        .map_err(error)?
        .query_map(
            params![project.as_str(), WAITING_SHOWN_MAX as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let mut waiting = Vec::with_capacity(rows.len());
    for (agent, kind, since) in rows {
        let kind = Waiting::parse(&kind).ok_or_else(|| failure("coord_waiting_unknown"))?;
        let asker = asker(connection, project, agent)?;
        if !asker.live {
            continue;
        }
        waiting.push(WaitingAgent {
            agent,
            label: asker.label,
            waiting: kind,
            since_ms: since,
            terminal: asker.terminal,
        });
    }
    assert!(waiting.len() <= WAITING_SHOWN_MAX);
    Ok(waiting)
}

fn fit(mut inbox: Inbox) -> Result<Inbox, EngineError> {
    let fits = |inbox: &Inbox| -> Result<bool, EngineError> {
        Ok(encoded_len(inbox)? <= STATUS_REPLY_BYTES_MAX)
    };
    if fits(&inbox)? {
        return Ok(inbox);
    }
    let whole: Vec<(String, Option<String>)> = inbox
        .questions
        .iter()
        .map(|question| (question.text.clone(), question.assumption.clone()))
        .collect();
    for cap in STATUS_TEXT_CAPS {
        for (question, (text, assumption)) in inbox.questions.iter_mut().zip(&whole) {
            question.text = cut_text(text, cap);
            question.assumption = assumption.as_deref().map(|kept| cut_text(kept, cap));
        }
        if fits(&inbox)? {
            return Ok(inbox);
        }
    }
    while !fits(&inbox)? {
        if inbox.questions.pop().is_none() {
            return Err(failure("coord_questions_bounds"));
        }
        inbox.more = inbox.more.saturating_add(1);
    }
    Ok(inbox)
}

fn alert_text(text: &str) -> String {
    events::one_line(text, ALERT_TEXT_BYTES_MAX)
}

fn waiting_words(waiting: Waiting) -> &'static str {
    match waiting {
        Waiting::Permission => "a permission prompt",
        Waiting::Question => "a question",
    }
}

#[cfg(all(test, unix))]
#[path = "questions/tests.rs"]
mod tests;
