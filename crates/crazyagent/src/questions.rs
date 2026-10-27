use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use crate::storage::versioned::{self, Item, Writer};
use crate::team::events::{self, NotificationKind};
use crate::team::{MessageAuthor, MessageState, append_message, bound_text};
use crate::terminal::AgentId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

mod page;
#[cfg(all(test, unix))]
mod tests;

/// The most bytes of one question.
pub const QUESTION_BYTES_MAX: usize = 2000;
/// The most options one question offers.
pub const OPTIONS_MAX: usize = 4;
/// The most bytes of one option, a single line.
pub const OPTION_BYTES_MAX: usize = 200;
/// The most bytes of the answer the asker recommends.
pub const RECOMMENDED_BYTES_MAX: usize = 500;
/// The most bytes of the context the asker gives with a question.
pub const CONTEXT_BYTES_MAX: usize = 2000;
/// The most bytes of one answer.
pub const ANSWER_BYTES_MAX: usize = 2000;
/// The most bytes of a question a master's terminal shows, a single line.
pub const TERMINAL_BYTES_MAX: usize = 1000;
/// The most questions of one project that are open: waiting for the owner, or answered
/// and not yet taken by the engine step that asked. A new question waits for room.
pub const OPEN_PER_PROJECT_MAX: i64 = 32;
/// How many settled questions of one project are kept, newest first; older ones are pruned.
pub const KEPT_PER_PROJECT_MAX: i64 = 256;
/// The most questions one list returns.
pub const LISTED_MAX: usize = 50;
/// The most bytes the questions of one list hold as JSON, so a list fits one owner frame.
pub const LISTED_BYTES_MAX: usize = 48 * 1024;

const QUOTED_BYTES_MAX: usize = 200;
const BUSY_WAIT: Duration = Duration::from_secs(5);
const COLUMNS: &str = "id,project,source,agent,question,options,recommended,context,state,answer,answered_by,asked_ms,resolved_ms";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS owner_questions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('master','mapping','research','landing','terminal')),
    agent INTEGER,
    question TEXT NOT NULL,
    options TEXT NOT NULL,
    recommended TEXT,
    context TEXT,
    state TEXT NOT NULL CHECK(state IN ('pending','answered','dismissed')),
    answer TEXT,
    answered_by TEXT CHECK(answered_by IN ('owner','master','terminal')),
    handed INTEGER NOT NULL CHECK(handed IN (0,1)),
    message INTEGER,
    asked_ms INTEGER NOT NULL,
    resolved_ms INTEGER
);
CREATE INDEX IF NOT EXISTS owner_questions_open ON owner_questions(project, state, handed, id);";

const _: () = assert!(
    2 * (QUESTION_BYTES_MAX
        + OPTIONS_MAX * OPTION_BYTES_MAX
        + RECOMMENDED_BYTES_MAX
        + CONTEXT_BYTES_MAX
        + ANSWER_BYTES_MAX)
        + 1024
        <= LISTED_BYTES_MAX
);
const _: () = assert!(LISTED_BYTES_MAX + 4096 <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(TERMINAL_BYTES_MAX <= QUESTION_BYTES_MAX);
const _: () = assert!(QUESTION_BYTES_MAX <= events::ESCALATION_BYTES_MAX);

/// Who asked an owner question: a project's master through its ask tool, an engine step
/// (mapping "Open questions", research, a landing conflict), or a master's own terminal
/// question box the engine saw on its screen. On the owner wire each is its snake_case name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Master,
    Mapping,
    Research,
    Landing,
    Terminal,
}

/// The engine steps that raise their own owner questions and take each answer once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Mapping,
    Research,
    Landing,
}

/// Where an owner question stands: waiting for the owner until answered or dismissed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Answered,
    Dismissed,
}

/// Who settled an answered question: the owner in a Questions view, the master recording
/// what the owner told it in chat, or the owner on the master's own terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsweredBy {
    Owner,
    Master,
    Terminal,
}

/// One question for the owner as an asker gives it: the question, up to `OPTIONS_MAX`
/// single-line options, the answer the asker recommends and the context the owner needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub question: String,
    pub options: Vec<String>,
    pub recommended: Option<String>,
    pub context: Option<String>,
}

/// One logged owner question with its outcome and its versioned-write version: 0 while it
/// is pending, which every answer or dismissal names as the version it read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: i64,
    pub project_id: ProjectId,
    pub source: Source,
    pub agent: Option<i64>,
    pub question: String,
    pub options: Vec<String>,
    pub recommended: Option<String>,
    pub context: Option<String>,
    pub state: State,
    pub answer: Option<String>,
    pub answered_by: Option<AnsweredBy>,
    pub asked_ms: i64,
    pub resolved_ms: Option<i64>,
    pub version: u64,
}

/// Why an answer or dismissal did not land: a plain code, or the question was already
/// settled or changed since the writer read it, with its current state to re-read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Code(&'static str),
    Stale(Box<Question>),
}

/// One settled question an engine step takes: its id and the owner's answer, none when
/// the owner dismissed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    pub id: i64,
    pub answer: Option<String>,
}

enum Settle<'a> {
    Answer(&'a str, AnsweredBy),
    Dismiss,
}

impl Source {
    const ALL: [Self; 5] = [
        Self::Master,
        Self::Mapping,
        Self::Research,
        Self::Landing,
        Self::Terminal,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Mapping => "mapping",
            Self::Research => "research",
            Self::Landing => "landing",
            Self::Terminal => "terminal",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.name() == value)
    }

    /// Names who asked, as the owner reads it. Input: the source. Output: the words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Master => "the master",
            Self::Mapping => "project mapping",
            Self::Research => "research",
            Self::Landing => "landing",
            Self::Terminal => "the master, asked in terminal",
        }
    }
}

impl Step {
    fn source(self) -> Source {
        match self {
            Self::Mapping => Source::Mapping,
            Self::Research => Source::Research,
            Self::Landing => Source::Landing,
        }
    }
}

impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answered => "answered",
            Self::Dismissed => "dismissed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Pending, Self::Answered, Self::Dismissed]
            .into_iter()
            .find(|state| state.name() == value)
    }
}

impl AnsweredBy {
    fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Master => "master",
            Self::Terminal => "terminal",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Owner, Self::Master, Self::Terminal]
            .into_iter()
            .find(|by| by.name() == value)
    }
}

impl Ask {
    /// Checks every bound of a question: plain text, no control characters but line breaks
    /// and tabs, single-line distinct options. Input: the ask. Output: the ask with each
    /// text trimmed, or none when any part is empty, too long or not plain.
    pub fn checked(&self) -> Option<Ask> {
        if self.options.len() > OPTIONS_MAX {
            return None;
        }
        let options = self
            .options
            .iter()
            .map(|option| plain(option, OPTION_BYTES_MAX, false))
            .collect::<Option<Vec<_>>>()?;
        let mut distinct = options.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() != options.len() {
            return None;
        }
        let optional = |text: &Option<String>, max: usize| match text {
            None => Some(None),
            Some(text) => plain(text, max, true).map(Some),
        };
        Some(Ask {
            question: plain(&self.question, QUESTION_BYTES_MAX, true)?,
            options,
            recommended: optional(&self.recommended, RECOMMENDED_BYTES_MAX)?,
            context: optional(&self.context, CONTEXT_BYTES_MAX)?,
        })
    }
}

/// Checks an answer: plain text of at most `ANSWER_BYTES_MAX` bytes. Input: the text.
/// Output: the trimmed answer, or none.
pub fn answer_checked(text: &str) -> Option<String> {
    plain(text, ANSWER_BYTES_MAX, true)
}

/// Opens the engine database in its root, waiting up to five seconds behind another
/// writer. Input: the absolute engine root holding `engine.db`. Output: the connection, or
/// `questions_unavailable`.
pub fn open(root: &Path) -> Result<Connection, EngineError> {
    if !root.is_absolute() {
        return Err(unavailable(()));
    }
    let db = Connection::open(root.join("engine.db")).map_err(unavailable)?;
    db.busy_timeout(BUSY_WAIT).map_err(unavailable)?;
    Ok(db)
}

/// Logs a question a project's master asked through its ask tool, notifies the owner and
/// rewrites the project's questions page. Its answer or dismissal reaches the master once.
/// Inputs: the engine connection, the project, the master's team agent id, the ask and
/// the time. Output: the question id, or `question_invalid` or `questions_full`.
pub fn ask(
    connection: &Connection,
    project: &ProjectId,
    master: i64,
    ask: &Ask,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    let tx = immediate(connection)?;
    let id = insert(&tx, project, Source::Master, Some(master), ask, now)?;
    if id.is_ok() {
        tx.commit().map_err(unavailable)?;
    }
    Ok(id)
}

/// Logs a question an engine step raises, notifies the owner and rewrites the project's
/// questions page, inside the step's own transaction. The step reads the outcome once
/// through `take`. Inputs: the step's transaction, the project, the step, the ask and the
/// time. Output: the question id, or `question_invalid` or `questions_full`.
pub fn raise(
    tx: &Transaction<'_>,
    project: &ProjectId,
    step: Step,
    ask: &Ask,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    insert(tx, project, step.source(), None, ask, now)
}

/// Logs a question box the engine saw on a master's own terminal, shown as asked in
/// terminal. It is never delivered: the owner answers on that screen. Inputs: the engine
/// connection, the project, the master's team agent id, the question line and the time.
/// Output: the question id, or `question_invalid` or `questions_full`.
pub fn asked_in_terminal(
    connection: &Connection,
    project: &ProjectId,
    master: i64,
    text: &str,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    let Some(question) = plain(text, TERMINAL_BYTES_MAX, false) else {
        return Ok(Err("question_invalid"));
    };
    let ask = Ask {
        question,
        options: Vec::new(),
        recommended: None,
        context: None,
    };
    let tx = immediate(connection)?;
    let id = insert(&tx, project, Source::Terminal, Some(master), &ask, now)?;
    if id.is_ok() {
        tx.commit().map_err(unavailable)?;
    }
    Ok(id)
}

/// Marks a question asked in terminal as answered there, with no answer text and no
/// delivery. Inputs: the engine connection, the question id and the time. Output: Ok,
/// `question_missing`, `question_not_terminal`, or the settled question when it already was.
pub fn answered_in_terminal(
    connection: &Connection,
    id: i64,
    now: i64,
) -> Result<Result<(), Refusal>, EngineError> {
    let tx = immediate(connection)?;
    ensure(&tx)?;
    let project: Option<String> = tx
        .query_row(
            "SELECT project FROM owner_questions WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let Some(project) = project else {
        return Ok(Err(Refusal::Code("question_missing")));
    };
    let project = ProjectId::new(project).map_err(unavailable)?;
    let Some(question) = load(&tx, &project, id)? else {
        return Ok(Err(Refusal::Code("question_missing")));
    };
    if question.source != Source::Terminal {
        return Ok(Err(Refusal::Code("question_not_terminal")));
    }
    let owner = Writer::Owner {
        force_after_conflict: false,
    };
    let version = question.version;
    let outcome = settle(
        &tx,
        question,
        Settle::Answer("", AnsweredBy::Terminal),
        &owner,
        version,
        now,
    )?;
    finish(tx, outcome)
}

/// Lists the questions asked in terminal by one master that still wait, oldest first, so
/// a restarted watch adopts a box still on screen instead of logging it twice. Inputs:
/// the engine connection, the project and the master's team agent id. Output: each
/// question's id and line, at most `OPEN_PER_PROJECT_MAX`.
pub fn terminal_pending(
    connection: &Connection,
    project: &ProjectId,
    master: i64,
) -> Result<Vec<(i64, String)>, EngineError> {
    let tx = connection.unchecked_transaction().map_err(unavailable)?;
    ensure(&tx)?;
    let mut statement = tx
        .prepare_cached(
            "SELECT id,question FROM owner_questions
             WHERE project=?1 AND source='terminal' AND agent=?2 AND state='pending'
             ORDER BY id LIMIT ?3",
        )
        .map_err(unavailable)?;
    statement
        .query_map(
            params![project.as_str(), master, OPEN_PER_PROJECT_MAX],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?
        .collect::<Result<Vec<(i64, String)>, _>>()
        .map_err(unavailable)
}

/// Records the owner's answer from a Questions view. A question the master asked gets the
/// answer as one owner message, which the team delivery types into the master once.
/// Inputs: the engine connection, the project, the question id, the version the owner
/// read, the answer and the time. Output: Ok, `question_invalid`, `question_missing`,
/// `question_in_terminal` for a box the owner answers on the master's screen,
/// `question_delivery_full` while the master's conversation is full, or the current question
/// when it was settled or changed since the owner read it.
pub fn answer(
    connection: &Connection,
    project: &ProjectId,
    id: i64,
    version: u64,
    text: &str,
    now: i64,
) -> Result<Result<(), Refusal>, EngineError> {
    let Some(text) = answer_checked(text) else {
        return Ok(Err(Refusal::Code("question_invalid")));
    };
    let owner = Writer::Owner {
        force_after_conflict: false,
    };
    resolve(
        connection,
        project,
        id,
        Settle::Answer(&text, AnsweredBy::Owner),
        &owner,
        version,
        now,
    )
}

/// Dismisses a question the owner will not answer. A question the master asked tells the
/// master once that no answer comes. Inputs: the engine connection, the project, the
/// question id, the version the owner read and the time. Output: as `answer`.
pub fn dismiss(
    connection: &Connection,
    project: &ProjectId,
    id: i64,
    version: u64,
    now: i64,
) -> Result<Result<(), Refusal>, EngineError> {
    let owner = Writer::Owner {
        force_after_conflict: false,
    };
    resolve(
        connection,
        project,
        id,
        Settle::Dismiss,
        &owner,
        version,
        now,
    )
}

/// Records an answer the owner gave the master in chat, which the master reports through
/// its ask tool; it is shown as recorded by the master and is never delivered back to it.
/// The master saw the question pending, so it names version 0. Inputs: the engine
/// connection, the project, the master's team agent id, the question id, the answer and
/// the time. Output: as `answer`.
pub fn record(
    connection: &Connection,
    project: &ProjectId,
    master: i64,
    id: i64,
    text: &str,
    now: i64,
) -> Result<Result<(), Refusal>, EngineError> {
    let (Some(text), Ok(agent)) = (answer_checked(text), AgentId::team(master)) else {
        return Ok(Err(Refusal::Code("question_invalid")));
    };
    resolve(
        connection,
        project,
        id,
        Settle::Answer(&text, AnsweredBy::Master),
        &Writer::Agent(agent),
        0,
        now,
    )
}

/// Takes, once, every settled question one engine step raised in a project and has not
/// taken yet, oldest first, inside the step's transaction so the step acts on each
/// outcome exactly once. Inputs: the step's transaction, the project and the step.
/// Output: each question's id and answer, none for a dismissed one.
pub fn take(
    tx: &Transaction<'_>,
    project: &ProjectId,
    step: Step,
) -> Result<Vec<Taken>, EngineError> {
    ensure(tx)?;
    let taken = {
        let mut statement = tx
            .prepare_cached(
                "SELECT id,answer FROM owner_questions
                 WHERE project=?1 AND source=?2 AND state!='pending' AND handed=0
                 ORDER BY id LIMIT ?3",
            )
            .map_err(unavailable)?;
        statement
            .query_map(
                params![project.as_str(), step.source().name(), OPEN_PER_PROJECT_MAX],
                |row| {
                    Ok(Taken {
                        id: row.get(0)?,
                        answer: row.get(1)?,
                    })
                },
            )
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?
    };
    for item in &taken {
        let changed = tx
            .execute(
                "UPDATE owner_questions SET handed=1 WHERE id=?1 AND handed=0",
                [item.id],
            )
            .map_err(unavailable)?;
        assert_eq!(changed, 1);
    }
    if !taken.is_empty() {
        prune(tx, project)?;
    }
    Ok(taken)
}

/// Lists one project's questions newest first: only pending ones, or all, those older than
/// `before` when given, at most `LISTED_MAX` and `LISTED_BYTES_MAX` bytes of JSON. Inputs:
/// the engine connection, the project, the filter and the paging id. Output: the questions.
pub fn list(
    connection: &Connection,
    project: &ProjectId,
    pending_only: bool,
    before: Option<i64>,
) -> Result<Vec<Question>, EngineError> {
    let tx = connection.unchecked_transaction().map_err(unavailable)?;
    ensure(&tx)?;
    let questions = select(
        &tx,
        &format!(
            "SELECT {COLUMNS} FROM owner_questions
             WHERE project=?1 AND (?2=0 OR state='pending') AND (?3 IS NULL OR id<?3)
             ORDER BY id DESC LIMIT ?4"
        ),
        params![project.as_str(), pending_only, before, LISTED_MAX as i64],
    )?;
    let mut listed = Vec::new();
    let mut bytes = 0;
    for question in questions {
        bytes += serde_json::to_vec(&question).map_err(unavailable)?.len();
        if bytes > LISTED_BYTES_MAX {
            break;
        }
        listed.push(question);
    }
    assert!(listed.len() <= LISTED_MAX);
    Ok(listed)
}

fn insert(
    tx: &Transaction<'_>,
    project: &ProjectId,
    source: Source,
    agent: Option<i64>,
    ask: &Ask,
    now: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    ensure(tx)?;
    let Some(ask) = ask.checked() else {
        return Ok(Err("question_invalid"));
    };
    let open: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM owner_questions WHERE project=?1 AND (state='pending' OR handed=0)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if open >= OPEN_PER_PROJECT_MAX {
        return Ok(Err("questions_full"));
    }
    let options = serde_json::to_string(&ask.options).map_err(unavailable)?;
    tx.execute(
        "INSERT INTO owner_questions(project,source,agent,question,options,recommended,context,state,handed,asked_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',0,?8)",
        params![
            project.as_str(),
            source.name(),
            agent,
            ask.question,
            options,
            ask.recommended,
            ask.context,
            now
        ],
    )
    .map_err(unavailable)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    let notice = bound_text(
        &format!(
            "Question {id} from {}: {}",
            source.label(),
            flat(&ask.question)
        ),
        events::ESCALATION_BYTES_MAX,
    );
    events::notify(tx, project, NotificationKind::Escalation, &notice, now).map_err(unavailable)?;
    page::write(tx, project)?;
    Ok(Ok(id))
}

fn resolve(
    connection: &Connection,
    project: &ProjectId,
    id: i64,
    how: Settle<'_>,
    writer: &Writer,
    version: u64,
    now: i64,
) -> Result<Result<(), Refusal>, EngineError> {
    let tx = immediate(connection)?;
    ensure(&tx)?;
    let Some(question) = load(&tx, project, id)? else {
        return Ok(Err(Refusal::Code("question_missing")));
    };
    let on_screen = matches!(how, Settle::Answer(_, AnsweredBy::Owner));
    if on_screen && question.source == Source::Terminal && question.state == State::Pending {
        return Ok(Err(Refusal::Code("question_in_terminal")));
    }
    let outcome = settle(&tx, question, how, writer, version, now)?;
    finish(tx, outcome)
}

fn finish(
    tx: Transaction<'_>,
    outcome: Result<(), Refusal>,
) -> Result<Result<(), Refusal>, EngineError> {
    if outcome.is_ok() {
        tx.commit().map_err(unavailable)?;
    }
    Ok(outcome)
}

/// Settles one pending question once: the versioned write of its item refuses a writer
/// that read another version, a master's question gets one owner message for the team
/// delivery, and the page is rewritten. Inputs: the transaction, the question, how it is
/// settled, the writer, the version it read and the time. Output: Ok, or the refusal.
fn settle(
    tx: &Transaction<'_>,
    question: Question,
    how: Settle<'_>,
    writer: &Writer,
    version: u64,
    now: i64,
) -> Result<Result<(), Refusal>, EngineError> {
    if question.state != State::Pending {
        return Ok(Err(Refusal::Stale(Box::new(question))));
    }
    let project = question.project_id.clone();
    match versioned::write_if(tx, &item(&project, question.id)?, version, writer) {
        Ok(written) => assert_eq!(written, 1),
        Err(versioned::Refusal::Stale(_)) => return Ok(Err(Refusal::Stale(Box::new(question)))),
        Err(versioned::Refusal::Failed(problem)) => return Err(problem),
    }
    let (state, answer, by) = match how {
        Settle::Answer(text, by) => (
            State::Answered,
            Some(text).filter(|text| !text.is_empty()),
            Some(by),
        ),
        Settle::Dismiss => (State::Dismissed, None, None),
    };
    let told = matches!(by, Some(AnsweredBy::Master));
    let (handed, message) = match question.source {
        Source::Master if !told => {
            let quoted = bound_text(&flat(&question.question), QUOTED_BYTES_MAX);
            let text = match &answer {
                Some(answer) => format!(
                    "The owner answered your question {} (\"{quoted}\"):\n{answer}",
                    question.id
                ),
                None => format!(
                    "The owner dismissed your question {} (\"{quoted}\") without an answer. Do not wait for it.",
                    question.id
                ),
            };
            let appended = append_message(
                tx,
                &project,
                MessageAuthor::Owner,
                &text,
                MessageState::Pending,
                now,
            )
            .map_err(unavailable)?;
            match appended {
                Ok(message) => (true, Some(message)),
                Err(_) => return Ok(Err(Refusal::Code("question_delivery_full"))),
            }
        }
        Source::Master | Source::Terminal => (true, None),
        Source::Mapping | Source::Research | Source::Landing => (false, None),
    };
    let changed = tx
        .execute(
            "UPDATE owner_questions SET state=?2, answer=?3, answered_by=?4, handed=?5, message=?6, resolved_ms=?7
             WHERE id=?1 AND state='pending'",
            params![
                question.id,
                state.name(),
                answer,
                by.map(AnsweredBy::name),
                handed,
                message,
                now
            ],
        )
        .map_err(unavailable)?;
    assert_eq!(changed, 1);
    prune(tx, &project)?;
    page::write(tx, &project)?;
    Ok(Ok(()))
}

/// Drops the oldest settled questions of a project beyond `KEPT_PER_PROJECT_MAX`, with
/// their versioned items. One an engine step has not taken yet is never dropped.
fn prune(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    let dropped: Vec<i64> = {
        let mut statement = tx
            .prepare_cached(
                "SELECT id FROM owner_questions WHERE project=?1 AND state!='pending' AND handed=1
                 ORDER BY id DESC LIMIT -1 OFFSET ?2",
            )
            .map_err(unavailable)?;
        statement
            .query_map(params![project.as_str(), KEPT_PER_PROJECT_MAX], |row| {
                row.get(0)
            })
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    for id in dropped {
        versioned::forget(tx, &item(project, id)?)?;
        tx.execute("DELETE FROM owner_questions WHERE id=?1", [id])
            .map_err(unavailable)?;
    }
    Ok(())
}

/// Reads one question of a project with its current version. A pending question always
/// holds version 0: only settling it writes its item.
fn load(
    tx: &Transaction<'_>,
    project: &ProjectId,
    id: i64,
) -> Result<Option<Question>, EngineError> {
    let mut found = select(
        tx,
        &format!("SELECT {COLUMNS} FROM owner_questions WHERE project=?1 AND id=?2"),
        params![project.as_str(), id],
    )?;
    assert!(found.len() <= 1);
    Ok(found.pop())
}

/// Reads the questions a query selects, oldest columns first as `COLUMNS` names them, each
/// with its versioned-write version. Inputs: the transaction, the query and its parameters.
/// Output: the questions, or `questions_unavailable` for a row this module never writes.
fn select(
    tx: &Transaction<'_>,
    query: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<Question>, EngineError> {
    type Row = (
        i64,
        String,
        String,
        Option<i64>,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        i64,
        Option<i64>,
    );
    let rows: Vec<Row> = {
        let mut statement = tx.prepare_cached(query).map_err(unavailable)?;
        statement
            .query_map(parameters, |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                ))
            })
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut questions = Vec::with_capacity(rows.len());
    for row in rows {
        let (id, project, source, agent, question, options, recommended, context, state) = (
            row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8,
        );
        let (answer, answered_by, asked_ms, resolved_ms) = (row.9, row.10, row.11, row.12);
        let project_id = ProjectId::new(project).map_err(unavailable)?;
        let state = State::parse(&state).ok_or_else(|| unavailable(()))?;
        let version = versioned::current(tx, &item(&project_id, id)?)?.version;
        assert!(
            (state == State::Pending) == (version == 0),
            "a pending question always holds version 0"
        );
        questions.push(Question {
            id,
            project_id,
            source: Source::parse(&source).ok_or_else(|| unavailable(()))?,
            agent,
            question,
            options: serde_json::from_str(&options).map_err(unavailable)?,
            recommended,
            context,
            state,
            answer,
            answered_by: match answered_by {
                None => None,
                Some(by) => Some(AnsweredBy::parse(&by).ok_or_else(|| unavailable(()))?),
            },
            asked_ms,
            resolved_ms,
            version,
        });
    }
    Ok(questions)
}

/// Names the versioned item of one owner question. Inputs: the project and question id.
/// Output: `question:<project>:<id>`.
fn item(project: &ProjectId, id: i64) -> Result<Item, EngineError> {
    Item::new(&format!("question:{}:{id}", project.as_str()))
}

fn ensure(tx: &Transaction<'_>) -> Result<(), EngineError> {
    tx.execute_batch(SCHEMA).map_err(unavailable)
}

fn immediate(connection: &Connection) -> Result<Transaction<'_>, EngineError> {
    Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(unavailable)
}

/// Checks one plain text: trimmed, not empty, at most `max` bytes, no control characters
/// but, where `lines` allows, line breaks and tabs. Output: the trimmed text, or none.
fn plain(text: &str, max: usize, lines: bool) -> Option<String> {
    let text = text.trim();
    let allowed = |c: char| !c.is_control() || (lines && (c == '\n' || c == '\t'));
    (!text.is_empty() && text.len() <= max && text.chars().all(allowed)).then(|| text.to_owned())
}

/// Joins a text's words with single spaces, for one-line notices and quotes.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unavailable<E>(_: E) -> EngineError {
    failure("questions_unavailable")
}
