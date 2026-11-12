use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::memory::hidden_text;
use crate::protocol::{ProjectId, SessionId};
use crate::storage::versioned::{self, Item, Writer};
use crate::team::events::{self, NotificationKind};
use crate::team::{MessageAuthor, MessageState, append_message, bound_text, now_ms};
use crate::terminal::AgentId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

mod page;
#[cfg(all(test, unix))]
mod tests;

/// The most bytes of one question.
pub const QUESTION_BYTES_MAX: usize = 2048;
/// The most options one question offers.
pub const OPTIONS_MAX: usize = 6;
/// The most bytes of one option, a single line.
pub const OPTION_BYTES_MAX: usize = 200;
/// The most bytes of a recommended answer, a single line.
pub const RECOMMENDED_BYTES_MAX: usize = 200;
/// The most bytes of the context an asker gives with a question.
pub const CONTEXT_BYTES_MAX: usize = 4096;
/// The most bytes of one answer.
pub const ANSWER_BYTES_MAX: usize = 4096;
/// The most bytes of the research kind or change set an engine asker names.
pub const NAME_BYTES_MAX: usize = 64;
/// The most questions of one asker that wait for the owner at once.
pub const PENDING_PER_ASKER_MAX: i64 = 8;
/// The most questions of one project that wait for the owner at once.
pub const PENDING_PER_PROJECT_MAX: i64 = 64;
/// How many questions one project keeps; the oldest settled ones are pruned first.
pub const KEPT_PER_PROJECT_MAX: i64 = 1024;
/// The most questions one page holds.
pub const LISTED_MAX: usize = 50;
/// The most bytes the questions of one page hold as JSON, so a page fits one owner frame.
pub const LISTED_BYTES_MAX: usize = 48 * 1024;
/// The most projects one pending count lists.
pub const COUNTED_PROJECTS_MAX: i64 = 256;
/// The most notes a conversation master is given before one turn.
pub const NOTES_MAX: usize = 16;
/// The label every client shows as the last choice of a single or multi question; picking
/// it opens a free-text box whose words go with the picks or stand alone.
pub const COMMENT_LABEL: &str = "Add a comment";
/// The most bytes of a comment given with picks, so picks and comment fit one answer.
pub const COMMENT_BYTES_MAX: usize = 2048;
/// The fewest options a single or multi question offers.
pub const OPTIONS_MIN: usize = 2;
/// The most questions one producer reply raises, the per-asker pending cap.
pub const ASKS_FROM_ONE_REPLY_MAX: usize = 8;

const QUOTED_BYTES_MAX: usize = 200;
const BUSY_WAIT: Duration = Duration::from_secs(5);
const PICKS_SEPARATOR: &str = "; ";
const COMMENT_PREFIX: &str = "\nComment: ";
const ADDED_COLUMNS: [(&str, &str); 5] = [
    (
        "kind",
        "ALTER TABLE questions_log ADD COLUMN kind TEXT NOT NULL DEFAULT ''",
    ),
    (
        "coord_message",
        "ALTER TABLE questions_log ADD COLUMN coord_message INTEGER",
    ),
    (
        "closed_by",
        "ALTER TABLE questions_log ADD COLUMN closed_by TEXT NOT NULL DEFAULT ''",
    ),
    (
        "relay_key_ms",
        "ALTER TABLE questions_log ADD COLUMN relay_key_ms INTEGER",
    ),
    (
        "reask",
        "ALTER TABLE questions_log ADD COLUMN reask INTEGER NOT NULL DEFAULT 0",
    ),
];
const REASK_COLUMN: &str = "reask";
const REASK_OLD: &str = "UPDATE questions_log SET reask=1 WHERE status='pending' AND asker LIKE 'engine:%' AND options_json='[]'";
const LINK_INDEX: &str = "CREATE UNIQUE INDEX IF NOT EXISTS questions_log_coord ON questions_log(coord_message) WHERE coord_message IS NOT NULL";
const LINKED_ASSUMPTION: &str = "Until you answer, the asker assumes: ";
const COORD_DISMISSED: &str = "The owner dismissed this question. No answer comes.";
const REASK_NOTE: &str = "The owner's questions below were asked as free text before CrazyAgent offered choices, and the owner wants them asked again as choices. For each one, call ask with kind single or multi, 2 to 6 short options in plain words without file or code names, the option you recommend, and replaces set to its number, so the old question leaves the owner's list. Questions: ";
const REASKS_PER_NOTE_MAX: usize = 16;
const COLUMNS: &str = "id,project,asker,question,options_json,recommended,context,status,answer,answered_by,created_ms,settled_ms,kind,closed_by";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS questions_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    asker TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('master','session','agent','engine','terminal')),
    question TEXT NOT NULL,
    options_json TEXT NOT NULL,
    recommended TEXT,
    context TEXT,
    status TEXT NOT NULL CHECK(status IN ('pending','answered','dismissed','answered_in_terminal')),
    answer TEXT,
    answered_by TEXT CHECK(answered_by IN ('owner','chat')),
    message INTEGER,
    turn_floor INTEGER NOT NULL,
    noted_ms INTEGER,
    created_ms INTEGER NOT NULL,
    settled_ms INTEGER,
    kind TEXT NOT NULL DEFAULT '',
    coord_message INTEGER,
    closed_by TEXT NOT NULL DEFAULT '',
    relay_key_ms INTEGER,
    reask INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS questions_log_status ON questions_log(project, status, id);
CREATE INDEX IF NOT EXISTS questions_log_asker ON questions_log(project, asker, status);";

/// The most bytes one question's stored texts hold.
const STORED_BYTES_MAX: usize = QUESTION_BYTES_MAX
    + OPTIONS_MAX * OPTION_BYTES_MAX
    + RECOMMENDED_BYTES_MAX
    + CONTEXT_BYTES_MAX
    + ANSWER_BYTES_MAX
    + 256;

const _: () = assert!(2 * STORED_BYTES_MAX + 1024 <= LISTED_BYTES_MAX);
const _: () = assert!(LISTED_BYTES_MAX + 4096 <= crate::protocol::FRAME_BYTES_MAX);
const _: () = assert!(OPTIONS_MAX <= u8::MAX as usize);
const _: () = assert!(PENDING_PER_ASKER_MAX < PENDING_PER_PROJECT_MAX);
const _: () = assert!(PENDING_PER_PROJECT_MAX < KEPT_PER_PROJECT_MAX);
const _: () = assert!(
    OPTIONS_MAX * (OPTION_BYTES_MAX + PICKS_SEPARATOR.len())
        + COMMENT_PREFIX.len()
        + COMMENT_BYTES_MAX
        <= ANSWER_BYTES_MAX
);
const _: () = assert!(OPTIONS_MIN <= OPTIONS_MAX);
const _: () = assert!(ASKS_FROM_ONE_REPLY_MAX as i64 <= PENDING_PER_ASKER_MAX);
const _: () = assert!(REASKS_PER_NOTE_MAX <= NOTES_MAX);
const _: () = assert!(crate::coord::BODY_BYTES_MAX <= ANSWER_BYTES_MAX);

/// Who asked the owner, apart from the engine's own steps: a team master (live or run
/// through drop files) by its team agent id, a conversation master by its session, a team
/// sub-agent by its team agent id, or a question box a screen watch saw on an agent's own
/// terminal. Stored as `master:<id>`, `master:session:<session>`, `agent:<id>` and
/// `terminal:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asker {
    Master(i64),
    Session(SessionId),
    Agent(i64),
    Terminal(i64),
}

/// The engine steps that raise their own questions, stored as `engine:mapping`,
/// `engine:research:<kind>`, `engine:landing:<change set>`, `engine:planning` and
/// `engine:access:<request>` for an agent's access request (F08).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSource {
    Mapping,
    Research(String),
    Landing(String),
    Planning,
    Access(String),
}

/// Where a question stands. It waits for the owner until answered or dismissed; a box the
/// owner answered on an agent's own screen is answered in the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionStatus {
    Pending,
    Answered,
    Dismissed,
    AnsweredInTerminal,
}

/// Where an answer came from: the owner in a Questions view, or an owner message in the
/// team master's chat that the master named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsweredBy {
    Owner,
    Chat,
}

/// How the owner answers a question: pick one option, pick any options, or write free
/// text. Single and multi questions also take a comment ([`COMMENT_LABEL`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    Single,
    Multi,
    Text,
}

/// Who closed a question when the owner neither answered nor dismissed it in a Questions
/// view (F12, F14): the master withdrew it, it was asked again as a choice question, or the
/// master reported the answer the owner typed in its terminal. Stored as `master`,
/// `replaced` and `relay`, empty for none; a name from a later engine reads as none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosedBy {
    Master,
    Replaced,
    Relay,
}

/// One owner dismissal of many questions at once: the ids dismissed, the ids that were
/// settled or changed since the owner read them, and each id refused with its code.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkDismissal {
    pub settled: Vec<u64>,
    pub stale: Vec<u64>,
    pub refused: Vec<(u64, String)>,
}

/// The owner's answer: picked options by position and an optional comment. A text question
/// takes the comment only; a single question at most one pick; a multi question any
/// distinct picks. A pick, a comment or both must be given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub picks: Vec<u8>,
    pub comment: Option<String>,
}

/// How a settling write ended: recorded, or refused because the question was settled or
/// changed since the writer read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Settlement {
    Settled,
    Stale,
}

/// One question as an asker gives it: the question, how it is answered, `OPTIONS_MIN` to
/// `OPTIONS_MAX` single-line options with one of them recommended for a single or multi
/// question (none for a text question, whose recommended answer is short text) and the
/// context the owner needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub question: String,
    pub kind: QuestionKind,
    pub options: Vec<String>,
    pub recommended: Option<String>,
    pub context: Option<String>,
}

/// One logged question with its outcome and the version an answer or dismissal names: 0
/// while pending, written once when it settles (SS-1 item `question:<project>:<id>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: u64,
    pub project: ProjectId,
    pub asker: String,
    pub question: String,
    pub kind: QuestionKind,
    pub options: Vec<String>,
    pub recommended: Option<String>,
    pub context: Option<String>,
    pub status: QuestionStatus,
    pub version: u64,
    pub answer: Option<String>,
    pub answered_by: Option<AnsweredBy>,
    pub created_ms: u64,
    pub settled_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_by: Option<ClosedBy>,
}

/// One page of a project's questions, pending ones first; `next` goes back as `after`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Question>,
    pub next: Option<u64>,
}

/// How the owner settled one of a conversation master's questions, for its next turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub question: u64,
    pub text: String,
}

#[derive(Clone, Copy)]
enum How<'a> {
    Answer(&'a str, AnsweredBy),
    Dismiss,
    Terminal,
    Withdraw { own: bool },
    Replace,
    Relay { text: &'a str, key: i64, own: bool },
}

impl ClosedBy {
    fn name(closed: Option<Self>) -> &'static str {
        match closed {
            None => "",
            Some(Self::Master) => "master",
            Some(Self::Replaced) => "replaced",
            Some(Self::Relay) => "relay",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Master, Self::Replaced, Self::Relay]
            .into_iter()
            .find(|closed| Self::name(Some(*closed)) == value)
    }
}

impl Asker {
    /// The stored asker name. Input: the asker. Output: `master:<id>`,
    /// `master:session:<session>`, `agent:<id>` or `terminal:<id>`.
    pub fn name(&self) -> String {
        match self {
            Self::Master(id) => format!("master:{id}"),
            Self::Session(session) => format!("master:session:{}", session.as_str()),
            Self::Agent(id) => format!("agent:{id}"),
            Self::Terminal(id) => format!("terminal:{id}"),
        }
    }

    fn source(&self) -> Option<&'static str> {
        match self {
            Self::Master(id) if *id > 0 => Some("master"),
            Self::Session(_) => Some("session"),
            Self::Agent(id) if *id > 0 => Some("agent"),
            Self::Terminal(id) if *id > 0 => Some("terminal"),
            Self::Master(_) | Self::Agent(_) | Self::Terminal(_) => None,
        }
    }
}

impl EngineSource {
    /// The stored asker name, or none when a research kind or change set is empty, longer
    /// than `NAME_BYTES_MAX` or not made of ASCII letters, digits, `-`, `_` and `.`.
    pub fn name(&self) -> Option<String> {
        let named = |name: &str| {
            !name.is_empty()
                && name.len() <= NAME_BYTES_MAX
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        };
        match self {
            Self::Mapping => Some("engine:mapping".into()),
            Self::Planning => Some("engine:planning".into()),
            Self::Research(kind) if named(kind) => Some(format!("engine:research:{kind}")),
            Self::Landing(set) if named(set) => Some(format!("engine:landing:{set}")),
            Self::Access(request) if named(request) => Some(format!("engine:access:{request}")),
            Self::Research(_) | Self::Landing(_) | Self::Access(_) => None,
        }
    }
}

impl QuestionStatus {
    const ALL: [Self; 4] = [
        Self::Pending,
        Self::Answered,
        Self::Dismissed,
        Self::AnsweredInTerminal,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answered => "answered",
            Self::Dismissed => "dismissed",
            Self::AnsweredInTerminal => "answered_in_terminal",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.name() == value)
    }
}

impl AnsweredBy {
    fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Chat => "chat",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Owner, Self::Chat]
            .into_iter()
            .find(|by| by.name() == value)
    }
}

impl QuestionKind {
    /// Returns the stored and wire name of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
            Self::Text => "text",
        }
    }

    /// Reads a stored kind back. An empty name is a row from before kinds: text without
    /// options, single with them. Inputs: the name and the option count. Output: the kind,
    /// or none for a name this module never writes.
    pub fn stored(name: &str, options: usize) -> Option<Self> {
        match name {
            "" if options == 0 => Some(Self::Text),
            "" | "single" => Some(Self::Single),
            "multi" => Some(Self::Multi),
            "text" => Some(Self::Text),
            _ => None,
        }
    }
}

impl Ask {
    /// Checks every bound of a question, never truncating: no hidden characters, plain
    /// text, distinct single-line options, and a recommendation that is one of the options
    /// or short text when there are none. A single or multi question needs `OPTIONS_MIN`
    /// options and its recommended one; a text question takes no options. Input: the ask.
    /// Output: the ask with each text trimmed, or `questions_text_invalid` for hidden
    /// characters and `questions_ask_invalid` for anything else out of bounds.
    pub fn checked(&self) -> Result<Ask, EngineError> {
        let invalid = || failure("questions_ask_invalid");
        let given = [
            Some(&self.question),
            self.recommended.as_ref(),
            self.context.as_ref(),
        ];
        if given
            .into_iter()
            .flatten()
            .chain(&self.options)
            .any(|text| hidden_text(text))
        {
            return Err(failure("questions_text_invalid"));
        }
        if self.options.len() > OPTIONS_MAX {
            return Err(invalid());
        }
        let options = self
            .options
            .iter()
            .map(|option| plain(option, OPTION_BYTES_MAX, false))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)?;
        let mut distinct = options.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() != options.len() {
            return Err(invalid());
        }
        let recommended = match &self.recommended {
            None => None,
            Some(text) => {
                let text = plain(text, RECOMMENDED_BYTES_MAX, false).ok_or_else(invalid)?;
                if !options.is_empty() && !options.contains(&text) {
                    return Err(invalid());
                }
                Some(text)
            }
        };
        let fits = match self.kind {
            QuestionKind::Text => options.is_empty(),
            QuestionKind::Single | QuestionKind::Multi => {
                options.len() >= OPTIONS_MIN && recommended.is_some()
            }
        };
        if !fits {
            return Err(invalid());
        }
        let context = match &self.context {
            None => None,
            Some(text) => Some(plain(text, CONTEXT_BYTES_MAX, true).ok_or_else(invalid)?),
        };
        Ok(Ask {
            question: plain(&self.question, QUESTION_BYTES_MAX, true).ok_or_else(invalid)?,
            kind: self.kind,
            options,
            recommended,
            context,
        })
    }
}

/// Names who asked, as the owner reads it. Input: the stored asker name. Output: the words.
pub fn asker_label(asker: &str) -> String {
    let (kind, rest) = asker.split_once(':').unwrap_or((asker, ""));
    match (kind, rest) {
        ("master", _) => "the master".into(),
        ("agent", id) => format!("agent {id}"),
        ("terminal", id) => format!("the terminal of agent {id}"),
        ("engine", "mapping") => "project mapping".into(),
        ("engine", "planning") => "roadmap planning".into(),
        ("engine", rest) => match rest.split_once(':') {
            Some(("research", kind)) => format!("research ({})", kind.replace('_', " ")),
            Some(("landing", set)) => format!("landing change set {set}"),
            Some(("access", request)) => format!("access request {request}"),
            _ => asker.into(),
        },
        _ => asker.into(),
    }
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

/// Logs a question a master or sub-agent asked through its ask tool, or a box a screen
/// watch saw on an agent's terminal, notifies the owner and rewrites the project's
/// questions page. A sub-agent's question is also told to the project's master, when its
/// conversation has room. Inputs: the engine connection, the project, the asker and the
/// ask. Output: the question id, or `questions_ask_invalid`, `questions_text_invalid`,
/// `questions_asker_full`, `questions_full` or `questions_project_missing`.
pub fn ask(
    db: &Connection,
    project: &ProjectId,
    asker: &Asker,
    ask: &Ask,
) -> Result<u64, EngineError> {
    let source = asker
        .source()
        .ok_or_else(|| failure("questions_ask_invalid"))?;
    let tx = immediate(db)?;
    let id = insert(&tx, project, &asker.name(), source, ask, false, None)?;
    if let Asker::Agent(agent) = asker {
        let text = format!(
            "Agent {agent} asked the owner question {id}, in its own words {}. The owner's answer comes to you too, and CrazyAgent types it into agent {agent}'s terminal.",
            quoted(&ask.question)
        );
        let relayed = append_message(
            &tx,
            project,
            MessageAuthor::Engine,
            &text,
            MessageState::Pending,
            now_ms(),
        )?;
        if relayed.is_err() {
            eprintln!("questions_master_untold project={}", project.as_str());
        }
    }
    tx.commit().map_err(unavailable)?;
    page::refresh(db, project);
    Ok(id)
}

/// Logs a question an engine step raises, as `ask` does. A question the same step already
/// has pending with the same text is not logged twice: its id is returned. Inputs: the engine
/// connection, the project, the step and the ask. Output: as `ask`.
pub fn raise(
    db: &Connection,
    project: &ProjectId,
    source: &EngineSource,
    ask: &Ask,
) -> Result<u64, EngineError> {
    let asker = source
        .name()
        .ok_or_else(|| failure("questions_ask_invalid"))?;
    let tx = immediate(db)?;
    let id = insert(&tx, project, &asker, "engine", ask, true, None)?;
    tx.commit().map_err(unavailable)?;
    page::refresh(db, project);
    Ok(id)
}

/// Records the owner's answer from a Questions view, naming the version the owner read. A
/// team master's or sub-agent's question gets it once as an owner message to the project's
/// master, which the team delivery types once; an engine step's gets it once as an engine
/// message when the conversation has room; a conversation master's waits as a note for its
/// next turn. Inputs: the engine connection, the question id, the version and the answer.
/// Output: Settled, Stale when it was settled or changed since, or `questions_not_found`,
/// `questions_option_invalid`, `questions_answer_invalid`, `questions_text_invalid`,
/// `questions_in_terminal`, or `questions_delivery_full` while the master's conversation is
/// full.
pub fn answer(
    db: &Connection,
    id: u64,
    version: u64,
    answer: &Answer,
) -> Result<Settlement, EngineError> {
    let tx = immediate(db)?;
    let question = found(&tx, id)?;
    if question.status != QuestionStatus::Pending {
        return Ok(Settlement::Stale);
    }
    if is_terminal(&question.asker) {
        return Err(failure("questions_in_terminal"));
    }
    let text = answer_words(&question, answer)?;
    let project = question.project.clone();
    let asker = question.asker.clone();
    let how = How::Answer(&text, AnsweredBy::Owner);
    let settled = settle(&tx, question, how, &owner(), version)?;
    if settled == Settlement::Settled {
        let allowed = crate::sandbox::grants::allows(&answer.picks);
        crate::sandbox::grants::decide(&tx, &project, &asker, allowed, now_ms())?;
    }
    finish(db, tx, &project, settled)
}

/// Dismisses a question the owner will not answer, naming the version the owner read; the
/// asker is told once, as `answer` tells it. Inputs: the engine connection, the question id
/// and the version. Output: Settled, Stale, or `questions_not_found` or
/// `questions_delivery_full`.
pub fn dismiss(db: &Connection, id: u64, version: u64) -> Result<Settlement, EngineError> {
    let tx = immediate(db)?;
    let question = found(&tx, id)?;
    let project = question.project.clone();
    let asker = question.asker.clone();
    let settled = settle(&tx, question, How::Dismiss, &owner(), version)?;
    if settled == Settlement::Settled {
        crate::sandbox::grants::decide(&tx, &project, &asker, false, now_ms())?;
    }
    finish(db, tx, &project, settled)
}

/// Dismisses up to `LISTED_MAX` questions the owner picked at once (F12, DEC-147.8), each
/// as `dismiss` does in its own transaction, so a question whose asker's conversation is
/// full stops only itself. Inputs: the engine connection and each question's id with the
/// version the owner read. Output: what happened to each, or `questions_bulk_invalid` for
/// an empty list or one longer than `LISTED_MAX`.
pub fn dismiss_many(db: &Connection, items: &[(u64, u64)]) -> Result<BulkDismissal, EngineError> {
    if items.is_empty() || items.len() > LISTED_MAX {
        return Err(failure("questions_bulk_invalid"));
    }
    let mut outcome = BulkDismissal::default();
    for &(id, version) in items {
        match dismiss(db, id, version) {
            Ok(Settlement::Settled) => outcome.settled.push(id),
            Ok(Settlement::Stale) => outcome.stale.push(id),
            Err(problem) => outcome.refused.push((id, problem.0)),
        }
    }
    let told = outcome.settled.len() + outcome.stale.len() + outcome.refused.len();
    assert_eq!(told, items.len());
    Ok(outcome)
}

/// Withdraws one pending question for a master that no longer needs it (F12): its own, or
/// one a sub-agent of its project asked, linked team questions included, never an engine
/// step's question or a terminal box. The master gets no message; a sub-agent asker is told
/// once. Inputs: the engine connection, the project, the master as asker, the question id
/// and the version it read. Output: Settled, Stale, or `questions_not_found` for a question
/// it may not withdraw.
pub fn withdraw(
    db: &Connection,
    project: &ProjectId,
    master: &Asker,
    id: u64,
    version: u64,
) -> Result<Settlement, EngineError> {
    assert!(matches!(master, Asker::Master(_) | Asker::Session(_)));
    let tx = immediate(db)?;
    let question = found(&tx, id)?;
    let own = question.asker == master.name();
    if &question.project != project || !(own || question.asker.starts_with("agent:")) {
        return Err(failure("questions_not_found"));
    }
    let writer = asker_writer(&master.name()).unwrap_or_else(owner);
    let settled = settle(&tx, question, How::Withdraw { own }, &writer, version)?;
    finish(db, tx, project, settled)
}

/// Records the owner's answer a live team master saw typed into its own terminal (F12): it
/// is taken only when the owner typed into that terminal after the question was asked, and
/// one owner key time answers at most one question. The text is the master's report of the
/// answer, kept as answered in the master's terminal and never shown as the owner's words.
/// Inputs: the engine connection, the project, the master's team agent id, the question id,
/// the version it read, the answer and when the owner last typed into its terminal. Output:
/// Settled, Stale, or `questions_not_found`, `questions_in_terminal`,
/// `questions_owner_not_typed`, `questions_owner_turn_used`, `questions_answer_invalid` or
/// `questions_text_invalid`.
pub fn relay(
    db: &Connection,
    project: &ProjectId,
    master: i64,
    id: u64,
    version: u64,
    answer: &str,
    owner_key_ms: Option<i64>,
) -> Result<Settlement, EngineError> {
    let agent = AgentId::team(master).map_err(|_| failure("questions_not_found"))?;
    let tx = immediate(db)?;
    let question = found(&tx, id)?;
    if &question.project != project {
        return Err(failure("questions_not_found"));
    }
    if question.status != QuestionStatus::Pending {
        return Ok(Settlement::Stale);
    }
    if is_terminal(&question.asker) {
        return Err(failure("questions_in_terminal"));
    }
    let asked = i64::try_from(question.created_ms).map_err(unavailable)?;
    let Some(key) = owner_key_ms.filter(|at| *at >= asked) else {
        return Err(failure("questions_owner_not_typed"));
    };
    let used: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM questions_log WHERE project=?1 AND relay_key_ms=?2)",
            params![project.as_str(), key],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if used {
        return Err(failure("questions_owner_turn_used"));
    }
    let text = answer_text(answer)?;
    let own = question.asker == Asker::Master(master).name();
    let how = How::Relay {
        text: &text,
        key,
        own,
    };
    let settled = settle(&tx, question, how, &Writer::Agent(agent), version)?;
    finish(db, tx, project, settled)
}

/// Asks a choice question in place of a pending free-text one (F14, DEC-147.8): the old one
/// closes as asked again in the same transaction, then the new one is logged, under the old
/// asker when an engine step asked it, so that step's cap is not crossed, else under the
/// caller. The old one must be a text question of the project asked by the caller or, for a
/// master, by an engine step, and the new one single or multi. Inputs: the engine connection, the
/// project, the caller, the new ask and the old question's id. Output: the new id, or
/// `questions_replace_invalid` and the refusals `ask` names.
pub fn reask(
    db: &Connection,
    project: &ProjectId,
    asker: &Asker,
    ask: &Ask,
    replaces: u64,
) -> Result<u64, EngineError> {
    let source = asker
        .source()
        .ok_or_else(|| failure("questions_ask_invalid"))?;
    let invalid = || failure("questions_replace_invalid");
    if ask.kind == QuestionKind::Text {
        return Err(invalid());
    }
    let tx = immediate(db)?;
    let old = match found(&tx, replaces) {
        Err(problem) if problem.0 == "questions_not_found" => return Err(invalid()),
        read => read?,
    };
    let engine = old.asker.starts_with("engine:") && !matches!(asker, Asker::Agent(_));
    let mine = old.asker == asker.name();
    if &old.project != project
        || old.status != QuestionStatus::Pending
        || old.kind != QuestionKind::Text
        || !(engine || mine)
    {
        return Err(invalid());
    }
    let (name, from) = if engine {
        (old.asker.clone(), "engine")
    } else {
        (asker.name(), source)
    };
    let writer = asker_writer(&asker.name()).unwrap_or_else(owner);
    if settle(&tx, old, How::Replace, &writer, 0)? != Settlement::Settled {
        return Err(invalid());
    }
    let id = insert(&tx, project, &name, from, ask, false, None)?;
    assert!(id != replaces);
    tx.commit().map_err(unavailable)?;
    page::refresh(db, project);
    Ok(id)
}

/// Links one question an agent put to the owner through the team (F12) into the questions
/// log, in the team tick's transaction, so the owner answers it where every other question
/// waits. The text is shown escaped and cut to the question bound, an "Assumption:" part
/// goes to the context, and no notification is made, since the team alert made one by
/// severity. A linked question skips the per-asker cap but not the project's. Inputs: the
/// transaction, the project, the asker, the team message's body and id. Output: the question
/// id, or none while the project is full or when the text cannot be stored.
pub(crate) fn link_coord(
    tx: &Transaction<'_>,
    project: &ProjectId,
    asker: &Asker,
    body: &str,
    message: i64,
) -> Result<Option<u64>, EngineError> {
    let source = asker
        .source()
        .ok_or_else(|| failure("questions_ask_invalid"))?;
    let (question, assumption) = crate::team::questions::split_assumption(body);
    let question = bound_text(&crate::memory::visible_text(&question), QUESTION_BYTES_MAX);
    let context = assumption.map(|kept| {
        bound_text(
            &format!("{LINKED_ASSUMPTION}{}", crate::memory::visible_text(&kept)),
            CONTEXT_BYTES_MAX,
        )
    });
    let ask = Ask {
        question,
        kind: QuestionKind::Text,
        options: Vec::new(),
        recommended: None,
        context,
    };
    match insert(
        tx,
        project,
        &asker.name(),
        source,
        &ask,
        false,
        Some(message),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(problem)
            if matches!(
                problem.0.as_str(),
                "questions_full"
                    | "questions_ask_invalid"
                    | "questions_text_invalid"
                    | "questions_project_missing"
            ) =>
        {
            eprintln!(
                "questions_link_skipped project={} code={}",
                project.as_str(),
                problem.0
            );
            Ok(None)
        }
        Err(problem) => Err(problem),
    }
}

/// Settles the linked row of a team question the team side closed (F12): answered with the
/// owner's reply when one came, else dismissed with its asker as the writer, and told to no
/// one, since the team side carried it. Inputs: the transaction, the team message and the
/// owner's reply, if any. Output: the project whose page changed, or none when no pending
/// row is linked to that message.
pub(crate) fn settle_linked(
    tx: &Transaction<'_>,
    message: i64,
    reply: Option<&str>,
) -> Result<Option<ProjectId>, EngineError> {
    ensure(tx)?;
    let mut rows = select(
        tx,
        &format!("SELECT {COLUMNS} FROM questions_log WHERE coord_message=?1 AND status='pending'"),
        [message],
    )?;
    assert!(rows.len() <= 1);
    let Some(question) = rows.pop() else {
        return Ok(None);
    };
    let project = question.project.clone();
    let reply = reply
        .map(crate::memory::visible_text)
        .and_then(|text| plain(&bound_text(&text, ANSWER_BYTES_MAX), ANSWER_BYTES_MAX, true));
    let settled = match reply.as_deref() {
        Some(text) => settle(
            tx,
            question,
            How::Answer(text, AnsweredBy::Owner),
            &owner(),
            0,
        )?,
        None => {
            let writer = asker_writer(&question.asker).unwrap_or_else(owner);
            settle(tx, question, How::Dismiss, &writer, 0)?
        }
    };
    Ok((settled == Settlement::Settled).then_some(project))
}

/// Sends the master of each project one engine note that lists the old free-text engine
/// questions still waiting (F14, DEC-147.8) and asks it to ask each again as a choice
/// question with `replaces`; a note a full conversation refuses waits for a later tick.
/// Input: the team tick's transaction. Output: how many notes were sent.
pub(crate) fn send_reask_notes(tx: &Transaction<'_>) -> Result<usize, EngineError> {
    ensure(tx)?;
    let due = select(
        tx,
        &format!(
            "SELECT {COLUMNS} FROM questions_log WHERE reask=1 AND status='pending'
             ORDER BY project, id LIMIT ?1"
        ),
        [PENDING_PER_PROJECT_MAX],
    )?;
    let mut sent = 0;
    for listed in due.chunk_by(|one, next| one.project == next.project) {
        let listed = &listed[..listed.len().min(REASKS_PER_NOTE_MAX)];
        let project = &listed[0].project;
        let names: Vec<String> = listed
            .iter()
            .map(|question| format!("{} {}", question.id, quoted(&question.question)))
            .collect();
        let text = format!("{REASK_NOTE}{}.", names.join("; "));
        let appended = append_message(
            tx,
            project,
            MessageAuthor::Engine,
            &text,
            MessageState::Pending,
            now_ms(),
        )?;
        if appended.is_err() {
            continue;
        }
        for question in listed {
            tx.execute(
                "UPDATE questions_log SET reask=2 WHERE id=?1 AND reask=1",
                [column(question.id)],
            )
            .map_err(unavailable)?;
        }
        sent += 1;
    }
    assert!(sent <= due.len());
    Ok(sent)
}

/// Makes the questions log, its later columns and its team link index on an engine
/// database at start, so team reads that join it find it. Input: the engine connection.
/// Output: nothing, or `questions_unavailable`.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    ensure(connection)
}

/// Rewrites a project's questions page after a caller's transaction settled some of its
/// questions. Inputs: the engine connection and the project.
pub(crate) fn refresh_page(db: &Connection, project: &ProjectId) {
    page::refresh(db, project);
}

/// Records an answer the owner gave in a team master's chat. The master names the owner's
/// message and may record only a question it asked itself; the answer is that message's
/// text, never the master's own words, and the message must be an owner turn the engine
/// recorded when the owner typed it in the question's project after the question was
/// asked, never a settle notice or a framed IDE comment. Each owner turn answers at most one
/// question: recording it uses the turn up, so one "Yes" never settles a list of questions
/// the master picked. Nothing is delivered back, since the master read it. Inputs: the
/// engine connection, the master's project, its team agent id, the question id, the
/// version the master read and the owner message id. Output:
/// Settled, Stale, or `questions_not_found` (also for a question it did not ask),
/// `questions_owner_turn_invalid`, `questions_answer_invalid` or `questions_text_invalid`.
pub fn record_from_chat(
    db: &Connection,
    project: &ProjectId,
    master: i64,
    id: u64,
    version: u64,
    owner_turn: i64,
) -> Result<Settlement, EngineError> {
    let agent = AgentId::team(master).map_err(|_| failure("questions_not_found"))?;
    let tx = immediate(db)?;
    let question = found(&tx, id)?;
    if &question.project != project || question.asker != Asker::Master(master).name() {
        return Err(failure("questions_not_found"));
    }
    if question.status != QuestionStatus::Pending {
        return Ok(Settlement::Stale);
    }
    let turn: Option<String> = tx
        .query_row(
            "SELECT m.text FROM team_messages m, team_owner_turns t, questions_log q
             WHERE m.id=?1 AND m.project=?2 AND m.author='owner' AND t.message=m.id
             AND t.project=m.project AND q.id=?3 AND m.id>q.turn_floor",
            params![owner_turn, project.as_str(), column(question.id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let turn = turn.ok_or_else(|| failure("questions_owner_turn_invalid"))?;
    let text = answer_text(&turn)?;
    let how = How::Answer(&text, AnsweredBy::Chat);
    let settled = settle(&tx, question, how, &Writer::Agent(agent), version)?;
    if settled == Settlement::Settled {
        let used = tx
            .execute(
                "DELETE FROM team_owner_turns WHERE message=?1 AND project=?2",
                params![owner_turn, project.as_str()],
            )
            .map_err(unavailable)?;
        assert_eq!(used, 1);
    }
    finish(db, tx, project, settled)
}

/// Records the owner's message of a conversation master's turn as the answer to a question
/// that master's session asked; the text is the engine's copy, and no note comes back.
/// Inputs: the engine connection, the project, the session, the question id, the version
/// the master read and the owner's message text. Output: as `record_from_chat`, without
/// `questions_owner_turn_invalid`.
pub fn record_from_session(
    db: &Connection,
    project: &ProjectId,
    session: &SessionId,
    id: u64,
    version: u64,
    owner_message: &str,
) -> Result<Settlement, EngineError> {
    let tx = immediate(db)?;
    let question = found(&tx, id)?;
    if &question.project != project || question.asker != Asker::Session(session.clone()).name() {
        return Err(failure("questions_not_found"));
    }
    if question.status != QuestionStatus::Pending {
        return Ok(Settlement::Stale);
    }
    let text = answer_text(owner_message)?;
    let how = How::Answer(&text, AnsweredBy::Chat);
    let settled = settle(&tx, question, how, &owner(), version)?;
    if settled == Settlement::Settled {
        let marked = tx
            .execute(
                "UPDATE questions_log SET noted_ms=?2 WHERE id=?1 AND noted_ms IS NULL",
                params![column(id), now_ms()],
            )
            .map_err(unavailable)?;
        assert_eq!(marked, 1);
    }
    finish(db, tx, project, settled)
}

/// Settles a box asked in an agent's terminal once its screen watch saw it leave: answered
/// there by the owner when the owner typed into that terminal since it was asked, otherwise
/// dismissed with that agent as the writer, since no owner answer is known. Nothing is
/// delivered, since the harness has whatever answer it got. Inputs: the engine connection,
/// the question id and when the owner last typed into the asker's terminal, if ever.
/// Output: Settled, Stale, or `questions_not_found` or `questions_not_terminal`.
pub fn terminal_settled(
    db: &Connection,
    id: u64,
    owner_key_ms: Option<i64>,
) -> Result<Settlement, EngineError> {
    let tx = immediate(db)?;
    let question = found(&tx, id)?;
    let Some(agent) = question
        .asker
        .strip_prefix("terminal:")
        .and_then(|agent| agent.parse::<i64>().ok())
        .and_then(|agent| AgentId::team(agent).ok())
    else {
        return Err(failure("questions_not_terminal"));
    };
    let project = question.project.clone();
    let version = question.version;
    let owner_answered = owner_key_ms
        .and_then(|at| u64::try_from(at).ok())
        .is_some_and(|at| at >= question.created_ms);
    let settled = if owner_answered {
        settle(&tx, question, How::Terminal, &owner(), version)?
    } else {
        settle(&tx, question, How::Dismiss, &Writer::Agent(agent), version)?
    };
    finish(db, tx, &project, settled)
}

/// Reads how the owner settled a conversation master's questions that its turns were not
/// yet given, oldest first. Pass their ids to `notes_delivered` once the turn that carries
/// them is accepted. Inputs: the engine connection, the project and the session. Output: at
/// most `NOTES_MAX` notes.
pub fn session_notes(
    db: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Vec<Note>, EngineError> {
    notes_of(db, project, &Asker::Session(session.clone()).name())
}

/// Marks a conversation master's notes as given, so no later turn repeats them. Inputs:
/// the engine connection, the project, the session and at most `NOTES_MAX` question ids
/// from `session_notes`. Output: Ok, also for an id already marked, or
/// `questions_notes_invalid` for more ids.
pub fn notes_delivered(
    db: &Connection,
    project: &ProjectId,
    session: &SessionId,
    ids: &[u64],
) -> Result<(), EngineError> {
    mark_noted(db, project, &Asker::Session(session.clone()).name(), ids)
}

/// Reads how the owner settled a sub-agent's questions that were not yet typed into its
/// terminal, oldest first, worded for the sub-agent itself. Pass their ids to
/// `agent_notes_delivered` once typed. Inputs: the engine connection, the project and the
/// team agent id. Output: at most `NOTES_MAX` notes.
pub fn agent_notes(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Vec<Note>, EngineError> {
    notes_of(db, project, &Asker::Agent(agent).name())
}

/// Marks a sub-agent's notes as typed, so no later tick types them again. Inputs: the
/// engine connection, the project, the team agent id and at most `NOTES_MAX` question ids
/// from `agent_notes`. Output: as `notes_delivered`.
pub fn agent_notes_delivered(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
    ids: &[u64],
) -> Result<(), EngineError> {
    mark_noted(db, project, &Asker::Agent(agent).name(), ids)
}

fn notes_of(db: &Connection, project: &ProjectId, asker: &str) -> Result<Vec<Note>, EngineError> {
    let tx = db.unchecked_transaction().map_err(unavailable)?;
    ensure(&tx)?;
    let settled = select(
        &tx,
        &format!(
            "SELECT {COLUMNS} FROM questions_log WHERE project=?1 AND asker=?2
             AND (status IN ('answered','dismissed') OR (status='answered_in_terminal' AND answer IS NOT NULL))
             AND noted_ms IS NULL ORDER BY id LIMIT ?3"
        ),
        params![project.as_str(), asker, NOTES_MAX as i64],
    )?;
    assert!(settled.len() <= NOTES_MAX);
    Ok(settled
        .iter()
        .map(|question| Note {
            question: question.id,
            text: told(question, question.answer.as_deref(), &mine(question), ""),
        })
        .collect())
}

fn mark_noted(
    db: &Connection,
    project: &ProjectId,
    asker: &str,
    ids: &[u64],
) -> Result<(), EngineError> {
    if ids.len() > NOTES_MAX {
        return Err(failure("questions_notes_invalid"));
    }
    let tx = immediate(db)?;
    ensure(&tx)?;
    let now = now_ms();
    for id in ids {
        tx.execute(
            "UPDATE questions_log SET noted_ms=?4 WHERE id=?1 AND project=?2 AND asker=?3
             AND status!='pending' AND noted_ms IS NULL",
            params![column(*id), project.as_str(), asker, now],
        )
        .map_err(unavailable)?;
    }
    tx.commit().map_err(unavailable)
}

/// Reads the questions of one project that wait for the owner, oldest first. Inputs: the
/// engine connection and the project. Output: at most `PENDING_PER_PROJECT_MAX` questions.
pub fn pending(db: &Connection, project: &ProjectId) -> Result<Vec<Question>, EngineError> {
    let tx = db.unchecked_transaction().map_err(unavailable)?;
    ensure(&tx)?;
    select(
        &tx,
        &format!(
            "SELECT {COLUMNS} FROM questions_log WHERE project=?1 AND status='pending'
             ORDER BY id LIMIT ?2"
        ),
        params![project.as_str(), PENDING_PER_PROJECT_MAX],
    )
}

/// Reads one page of a project's questions: the pending ones oldest first, then the settled
/// ones newest first, at most `LISTED_MAX` and `LISTED_BYTES_MAX` bytes of JSON. Pass `next`
/// back as `after` for the rest; it names the last question shown and its part of the list.
/// Inputs: the engine connection, the project and the `next` of the page before. Output:
/// the page.
pub fn page(db: &Connection, project: &ProjectId, after: Option<u64>) -> Result<Page, EngineError> {
    let tx = db.unchecked_transaction().map_err(unavailable)?;
    ensure(&tx)?;
    let (settled_part, from) = match after {
        None => (false, 0),
        Some(cursor) => (cursor & 1 == 1, column(cursor >> 1)),
    };
    let limit = LISTED_MAX + 1;
    let mut rows = Vec::new();
    if !settled_part {
        rows = select(
            &tx,
            &format!(
                "SELECT {COLUMNS} FROM questions_log WHERE project=?1 AND status='pending' AND id>?2
                 ORDER BY id LIMIT ?3"
            ),
            params![project.as_str(), from, limit as i64],
        )?;
    }
    if rows.len() < limit {
        let below = if settled_part { from } else { i64::MAX };
        rows.extend(select(
            &tx,
            &format!(
                "SELECT {COLUMNS} FROM questions_log WHERE project=?1 AND status!='pending' AND id<?2
                 ORDER BY id DESC LIMIT ?3"
            ),
            params![project.as_str(), below, limit as i64],
        )?);
    }
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut more = false;
    for question in rows {
        let size = serde_json::to_vec(&question).map_err(unavailable)?.len();
        assert!(size <= 2 * STORED_BYTES_MAX + 1024);
        if items.len() == LISTED_MAX || bytes + size > LISTED_BYTES_MAX {
            more = true;
            break;
        }
        bytes += size;
        items.push(question);
    }
    assert!(items.len() <= LISTED_MAX && bytes <= LISTED_BYTES_MAX);
    let next = items.last().filter(|_| more).map(|last| {
        let settled = u64::from(last.status != QuestionStatus::Pending);
        (last.id << 1) | settled
    });
    Ok(Page { items, next })
}

/// Counts the questions waiting for the owner in each project that has any, for the app
/// badge. Input: the engine connection. Output: each project and its count, at most
/// `COUNTED_PROJECTS_MAX`.
pub fn count_pending(db: &Connection) -> Result<Vec<(ProjectId, u32)>, EngineError> {
    let tx = db.unchecked_transaction().map_err(unavailable)?;
    ensure(&tx)?;
    let rows: Vec<(String, i64)> = {
        let mut statement = tx
            .prepare(
                "SELECT project, COUNT(*) FROM questions_log WHERE status='pending'
                 GROUP BY project ORDER BY project LIMIT ?1",
            )
            .map_err(unavailable)?;
        statement
            .query_map([COUNTED_PROJECTS_MAX], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    rows.into_iter()
        .map(|(project, count)| {
            let count = u32::try_from(count).map_err(unavailable)?;
            assert!(i64::from(count) <= PENDING_PER_PROJECT_MAX);
            Ok((ProjectId::new(project).map_err(unavailable)?, count))
        })
        .collect()
}

/// Forgets every question of a removed project inside the removal's transaction, so the
/// badge, notifications and pages never count it again. Inputs: the transaction and the
/// project. Output: Ok, or `questions_unavailable`.
pub fn forget_project(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    ensure(tx)?;
    tx.execute(
        "DELETE FROM questions_log WHERE project=?1",
        [project.as_str()],
    )
    .map_err(unavailable)?;
    Ok(())
}

/// Logs one checked question inside the caller's transaction and notifies the owner; a
/// question linked to a team message skips the per-asker cap and the notice, which its team
/// alert made. Inputs: the transaction, the project, the stored asker name and source, the
/// ask, whether a pending question of the same asker and text is returned instead, and the
/// linked team message, if any. Output: the question id, or the refusals `ask` names.
fn insert(
    tx: &Transaction<'_>,
    project: &ProjectId,
    asker: &str,
    source: &str,
    ask: &Ask,
    once: bool,
    coord: Option<i64>,
) -> Result<u64, EngineError> {
    ensure(tx)?;
    let known: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !known {
        return Err(failure("questions_project_missing"));
    }
    let ask = ask.checked()?;
    if once {
        let same: Option<i64> = tx
            .query_row(
                "SELECT id FROM questions_log WHERE project=?1 AND asker=?2 AND status='pending' AND question=?3",
                params![project.as_str(), asker, ask.question],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        if let Some(same) = same {
            return u64::try_from(same).map_err(unavailable);
        }
    }
    let mine: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM questions_log WHERE project=?1 AND asker=?2 AND status='pending'",
            params![project.as_str(), asker],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if coord.is_none() && mine >= PENDING_PER_ASKER_MAX {
        return Err(failure("questions_asker_full"));
    }
    if pending_count(tx, project)? >= PENDING_PER_PROJECT_MAX {
        return Err(failure("questions_full"));
    }
    let floor: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(id),0) FROM team_messages WHERE project=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let now = now_ms();
    assert!(
        ask.kind != QuestionKind::Text || ask.options.is_empty(),
        "a text question never has options"
    );
    tx.execute(
        "INSERT INTO questions_log(project,asker,source,question,options_json,recommended,context,status,turn_floor,created_ms,kind,coord_message)
         VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,?9,?10,?11)",
        params![
            project.as_str(),
            asker,
            source,
            ask.question,
            serde_json::to_string(&ask.options).map_err(unavailable)?,
            ask.recommended,
            ask.context,
            floor,
            now,
            ask.kind.as_str(),
            coord
        ],
    )
    .map_err(unavailable)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    if coord.is_none() {
        let first = ask.question.lines().next().unwrap_or_default();
        let notice = bound_text(
            &format!("{}: {}", capital(&asker_label(asker)), flat(first)),
            events::ESCALATION_BYTES_MAX,
        );
        events::notify(tx, project, NotificationKind::Question, &notice, now)
            .map_err(unavailable)?;
    }
    prune(tx, project)?;
    u64::try_from(id).map_err(unavailable)
}

/// Commits a settling transaction and rewrites the questions page, or rolls back a stale
/// one. Inputs: the connection, the transaction, the project and how the settling ended.
/// Output: the same settlement.
fn finish(
    db: &Connection,
    tx: Transaction<'_>,
    project: &ProjectId,
    settled: Settlement,
) -> Result<Settlement, EngineError> {
    if settled == Settlement::Settled {
        tx.commit().map_err(unavailable)?;
        page::refresh(db, project);
    }
    Ok(settled)
}

/// Settles one pending question once: the versioned write of its item refuses a writer that
/// read another version, and the asker is told at most once, both in the caller's
/// transaction. A question linked to a team message is told through the team side only: the
/// owner's answer or dismissal as one team reply from the owner, a withdrawal, re-ask or
/// relay by ending the team message, so no asker hears it twice. Inputs: the transaction,
/// the question, how it settles, the writer and the version it read. Output: Settled or
/// Stale, or `questions_delivery_full`.
fn settle(
    tx: &Transaction<'_>,
    question: Question,
    how: How<'_>,
    writer: &Writer,
    version: u64,
) -> Result<Settlement, EngineError> {
    if question.status != QuestionStatus::Pending {
        return Ok(Settlement::Stale);
    }
    let project = question.project.clone();
    match versioned::write_if(tx, &item(&project, question.id)?, version, writer) {
        Ok(written) => assert_eq!(written, 1),
        Err(versioned::Refusal::Stale(_)) => return Ok(Settlement::Stale),
        Err(versioned::Refusal::Failed(problem)) => return Err(problem),
    }
    let (status, answer, by, closed) = match how {
        How::Answer(text, by) => (QuestionStatus::Answered, Some(text), Some(by), None),
        How::Dismiss => (QuestionStatus::Dismissed, None, None, None),
        How::Terminal => (QuestionStatus::AnsweredInTerminal, None, None, None),
        How::Withdraw { .. } => (
            QuestionStatus::Dismissed,
            None,
            None,
            Some(ClosedBy::Master),
        ),
        How::Replace => (
            QuestionStatus::Dismissed,
            None,
            None,
            Some(ClosedBy::Replaced),
        ),
        How::Relay { text, .. } => (
            QuestionStatus::AnsweredInTerminal,
            Some(text),
            None,
            Some(ClosedBy::Relay),
        ),
    };
    let linked = coord_link(tx, question.id)?;
    let by_owner = matches!(how, How::Answer(..) | How::Dismiss);
    let told = match linked {
        Some(coord) if by_owner => tell_coord(tx, coord, answer)?,
        Some(coord) => {
            end_coord(tx, coord)?;
            false
        }
        None => false,
    };
    let message = if told || by == Some(AnsweredBy::Chat) {
        None
    } else if by_owner || matches!(how, How::Terminal) {
        deliver(tx, &question, answer)?
    } else {
        None
    };
    let noted = told
        || matches!(
            how,
            How::Replace | How::Withdraw { own: true } | How::Relay { own: true, .. }
        );
    let key = match how {
        How::Relay { key, .. } => Some(key),
        _ => None,
    };
    let now = now_ms();
    let changed = tx
        .execute(
            "UPDATE questions_log SET status=?2, answer=?3, answered_by=?4, message=?5, settled_ms=?6,
             closed_by=?7, relay_key_ms=?8, noted_ms=CASE WHEN ?9 THEN ?6 ELSE noted_ms END
             WHERE id=?1 AND status='pending'",
            params![
                column(question.id),
                status.name(),
                answer,
                by.map(AnsweredBy::name),
                message,
                now,
                ClosedBy::name(closed),
                key,
                noted
            ],
        )
        .map_err(unavailable)?;
    assert_eq!(changed, 1);
    prune(tx, &project)?;
    Ok(Settlement::Settled)
}

/// Reads the team message a question is linked to (F12). Inputs: the transaction and the
/// question id. Output: the message id, or none for a question asked here.
fn coord_link(tx: &Transaction<'_>, id: u64) -> Result<Option<i64>, EngineError> {
    tx.query_row(
        "SELECT coord_message FROM questions_log WHERE id=?1",
        [column(id)],
        |row| row.get::<_, Option<i64>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(unavailable)
}

/// Says whether a linked team question still waits for an answer. Inputs: the transaction
/// and the message id. Output: true for a receipt of Sent, Delivered, Typed or Read.
fn coord_open(tx: &Transaction<'_>, message: i64) -> Result<bool, EngineError> {
    let receipt: Option<String> = tx
        .query_row(
            "SELECT receipt FROM coord_messages WHERE id=?1",
            [message],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(receipt
        .as_deref()
        .and_then(crate::coord::Receipt::parse)
        .is_some_and(|receipt| {
            matches!(
                receipt,
                crate::coord::Receipt::Sent
                    | crate::coord::Receipt::Delivered
                    | crate::coord::Receipt::Typed
                    | crate::coord::Receipt::Read
            )
        }))
}

/// Tells the asker of a linked team question the owner's answer or dismissal as one team
/// reply from the owner, while the team message still waits. Inputs: the transaction, the
/// message id and the owner's answer, none when dismissed. Output: whether it was told, or
/// `questions_unavailable` when the team reply cannot be written.
fn tell_coord(
    tx: &Transaction<'_>,
    message: i64,
    answer: Option<&str>,
) -> Result<bool, EngineError> {
    if !coord_open(tx, message)? {
        return Ok(false);
    }
    let body = match answer {
        Some(text) => bound_text(text, crate::coord::BODY_BYTES_MAX),
        None => COORD_DISMISSED.to_owned(),
    };
    assert!(!body.is_empty() && body.len() <= crate::coord::BODY_BYTES_MAX);
    crate::coord::reply(tx, crate::coord::Party::Owner, message, body, now_ms())
        .map_err(unavailable)?;
    Ok(true)
}

/// Ends a linked team question nobody answers any more, from any open receipt, so its team
/// alert settles. Inputs: the transaction and the message id. Output: nothing, or
/// `questions_unavailable`.
fn end_coord(tx: &Transaction<'_>, message: i64) -> Result<(), EngineError> {
    crate::coord::withdraw(tx, message, now_ms()).map_err(unavailable)?;
    Ok(())
}

/// The versioned writer for a stored asker that is a team agent: a team master or a
/// sub-agent. Input: the stored asker name. Output: the writer, none for any other asker.
fn asker_writer(asker: &str) -> Option<Writer> {
    let id = asker
        .strip_prefix("master:")
        .filter(|rest| !rest.starts_with("session:"))
        .or_else(|| asker.strip_prefix("agent:"))?;
    let agent = AgentId::team(id.parse::<i64>().ok()?).ok()?;
    Some(Writer::Agent(agent))
}

/// Queues the one team message that tells an asker how the owner settled its question: an
/// owner message to the project's master for a team master's or sub-agent's question,
/// refused while the conversation is full since the asker waits for it, and an engine
/// message for an engine step's, dropped while it is full since nothing waits for it. A
/// terminal box or a conversation master's question gets none. Inputs: the transaction,
/// the question and the answer, none when dismissed. Output: the message id, if any, or
/// `questions_delivery_full`.
fn deliver(
    tx: &Transaction<'_>,
    question: &Question,
    answer: Option<&str>,
) -> Result<Option<i64>, EngineError> {
    let (kind, rest) = question.asker.split_once(':').unwrap_or_default();
    let (author, text, required) = match (kind, rest) {
        ("master", rest) if !rest.starts_with("session:") => (
            MessageAuthor::Owner,
            told(question, answer, &mine(question), ""),
            true,
        ),
        ("agent", agent) => (
            MessageAuthor::Owner,
            told(
                question,
                answer,
                &format!("agent {agent}'s question {}", question.id),
                &format!(
                    "\nCrazyAgent types this into agent {agent}'s terminal itself, now while it runs or once its work resumes, so do not send it to agent {agent} yourself."
                ),
            ),
            true,
        ),
        ("engine", _) => (
            MessageAuthor::Engine,
            told(
                question,
                answer,
                &format!(
                    "the {} question {} ({})",
                    asker_label(&question.asker),
                    question.id,
                    quoted(&question.question)
                ),
                "",
            ),
            false,
        ),
        _ => return Ok(None),
    };
    let appended = append_message(
        tx,
        &question.project,
        author,
        &text,
        MessageState::Pending,
        now_ms(),
    )?;
    match appended {
        Ok(message) => Ok(Some(message)),
        Err(_) if required => Err(failure("questions_delivery_full")),
        Err(_) => {
            eprintln!(
                "questions_engine_note_dropped project={}",
                question.project.as_str()
            );
            Ok(None)
        }
    }
}

/// Words how the owner settled a question for the one who waits on it. Inputs: the
/// question, the answer, none when dismissed, the words that name the question, such as
/// `your question 4 ("Ship?")`, and a closing line, if any. Output: the text.
fn told(question: &Question, answer: Option<&str>, named: &str, tail: &str) -> String {
    assert!(named.contains(&question.id.to_string()));
    match (question.closed_by, answer) {
        (Some(ClosedBy::Relay), Some(answer)) => format!(
            "The owner answered {named} in the master's terminal. The master reports the answer as:\n{answer}{tail}"
        ),
        (Some(ClosedBy::Master), _) => {
            format!("The master withdrew {named}. No answer comes.{tail}")
        }
        (_, Some(answer)) => format!("The owner answered {named}:\n{answer}{tail}"),
        (_, None) => format!("The owner dismissed {named}. No answer comes.{tail}"),
    }
}

/// Names a question for the one who asked it, with its words quoted.
fn mine(question: &Question) -> String {
    format!(
        "your question {} ({})",
        question.id,
        quoted(&question.question)
    )
}

/// Quotes an asker's words on one line, at most `QUOTED_BYTES_MAX` bytes, as a JSON string,
/// so quote marks inside cannot end the quote and words cannot pass as the engine's or the
/// owner's. Input: the words. Output: the quoted text.
fn quoted(text: &str) -> String {
    serde_json::to_string(&bound_text(&flat(text), QUOTED_BYTES_MAX))
        .expect("a string always encodes as JSON")
}

/// Drops the oldest settled questions of a project beyond `KEPT_PER_PROJECT_MAX` kept
/// questions, with their versioned items; a pending question is never dropped.
fn prune(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    let keep = KEPT_PER_PROJECT_MAX - pending_count(tx, project)?;
    assert!(keep >= KEPT_PER_PROJECT_MAX - PENDING_PER_PROJECT_MAX);
    let dropped: Vec<i64> = {
        let mut statement = tx
            .prepare(
                "SELECT id FROM questions_log WHERE project=?1 AND status!='pending'
                 ORDER BY id DESC LIMIT -1 OFFSET ?2",
            )
            .map_err(unavailable)?;
        statement
            .query_map(params![project.as_str(), keep], |row| row.get(0))
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    for id in dropped {
        let id = u64::try_from(id).map_err(unavailable)?;
        versioned::forget(tx, &item(project, id)?)?;
        tx.execute("DELETE FROM questions_log WHERE id=?1", [column(id)])
            .map_err(unavailable)?;
    }
    Ok(())
}

fn pending_count(tx: &Transaction<'_>, project: &ProjectId) -> Result<i64, EngineError> {
    tx.query_row(
        "SELECT COUNT(*) FROM questions_log WHERE project=?1 AND status='pending'",
        [project.as_str()],
        |row| row.get(0),
    )
    .map_err(unavailable)
}

/// Reads one question by id with its current version. Inputs: the transaction and the id.
/// Output: the question, or `questions_not_found`.
fn found(tx: &Transaction<'_>, id: u64) -> Result<Question, EngineError> {
    ensure(tx)?;
    let Ok(id) = i64::try_from(id) else {
        return Err(failure("questions_not_found"));
    };
    let mut rows = select(
        tx,
        &format!("SELECT {COLUMNS} FROM questions_log WHERE id=?1"),
        [id],
    )?;
    assert!(rows.len() <= 1);
    rows.pop().ok_or_else(|| failure("questions_not_found"))
}

/// Reads the questions a query selects, its columns in `COLUMNS` order, each with its
/// versioned-write version. A pending question always holds version 0: only settling it
/// writes its item. A kind name from a later engine reads like a row from before kinds.
/// Inputs: the transaction, the query and its parameters. Output: the questions, or
/// `questions_unavailable` for a row this module never writes.
fn select(
    tx: &Transaction<'_>,
    query: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<Question>, EngineError> {
    type Row = (
        i64,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        i64,
        Option<i64>,
        String,
        String,
    );
    let rows: Vec<Row> = {
        let mut statement = tx.prepare(query).map_err(unavailable)?;
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
                    row.get(13)?,
                ))
            })
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut questions = Vec::with_capacity(rows.len());
    for (
        id,
        project,
        asker,
        question,
        options,
        recommended,
        context,
        status,
        answer,
        by,
        created,
        settled,
        kind,
        closed_by,
    ) in rows
    {
        let id = u64::try_from(id).map_err(unavailable)?;
        let project = ProjectId::new(project).map_err(unavailable)?;
        let status = QuestionStatus::parse(&status).ok_or_else(|| unavailable(()))?;
        let version = versioned::current(tx, &item(&project, id)?)?.version;
        assert!(
            (status == QuestionStatus::Pending) == (version == 0),
            "a pending question always holds version 0"
        );
        let answered_by = match by {
            None => None,
            Some(by) => Some(AnsweredBy::parse(&by).ok_or_else(|| unavailable(()))?),
        };
        let settled_ms = match settled {
            None => None,
            Some(ms) => Some(u64::try_from(ms).map_err(unavailable)?),
        };
        let options: Vec<String> = serde_json::from_str(&options).map_err(unavailable)?;
        let kind = QuestionKind::stored(&kind, options.len())
            .or_else(|| QuestionKind::stored("", options.len()))
            .ok_or_else(|| unavailable(()))?;
        assert!(
            kind != QuestionKind::Text || options.is_empty(),
            "a text question never has options"
        );
        questions.push(Question {
            id,
            project,
            asker,
            question,
            kind,
            options,
            recommended,
            context,
            status,
            version,
            answer,
            answered_by,
            created_ms: u64::try_from(created).map_err(unavailable)?,
            settled_ms,
            closed_by: ClosedBy::parse(&closed_by),
        });
    }
    Ok(questions)
}

/// Names the versioned item of one owner question. Inputs: the project and question id.
/// Output: `question:<project>:<id>`.
fn item(project: &ProjectId, id: u64) -> Result<Item, EngineError> {
    Item::new(&format!("question:{}:{id}", project.as_str()))
}

/// Renders the owner's answer as the asker reads it: picked options in the question's order
/// joined by "; ", then "Comment: " and the comment, or the comment alone. Inputs: the
/// question and the answer. Output: at most `ANSWER_BYTES_MAX` bytes, or
/// `questions_option_invalid`, `questions_answer_invalid` or `questions_text_invalid`.
fn answer_words(question: &Question, answer: &Answer) -> Result<String, EngineError> {
    if answer.picks.len() > OPTIONS_MAX {
        return Err(failure("questions_option_invalid"));
    }
    let mut picks = answer.picks.clone();
    picks.sort_unstable();
    picks.dedup();
    let allowed = match question.kind {
        QuestionKind::Text => 0,
        QuestionKind::Single => 1,
        QuestionKind::Multi => question.options.len(),
    };
    if picks.len() != answer.picks.len() || picks.len() > allowed {
        return Err(failure("questions_option_invalid"));
    }
    let chosen = picks
        .iter()
        .map(|at| question.options.get(usize::from(*at)).cloned())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| failure("questions_option_invalid"))?;
    let limit = if chosen.is_empty() {
        ANSWER_BYTES_MAX
    } else {
        COMMENT_BYTES_MAX
    };
    let comment = answer
        .comment
        .as_deref()
        .map(|text| bounded_answer(text, limit))
        .transpose()?;
    let words = match (chosen.is_empty(), comment) {
        (true, None) => return Err(failure("questions_answer_invalid")),
        (true, Some(comment)) => comment,
        (false, None) => chosen.join(PICKS_SEPARATOR),
        (false, Some(comment)) => {
            format!("{}{COMMENT_PREFIX}{comment}", chosen.join(PICKS_SEPARATOR))
        }
    };
    assert!(!words.trim().is_empty());
    assert!(words.len() <= ANSWER_BYTES_MAX);
    Ok(words)
}

/// Checks an answer the owner gave, never truncating it. Input: the text. Output: the
/// trimmed answer, or `questions_text_invalid` for hidden characters and
/// `questions_answer_invalid` when it is blank or longer than `ANSWER_BYTES_MAX`.
fn answer_text(text: &str) -> Result<String, EngineError> {
    bounded_answer(text, ANSWER_BYTES_MAX)
}

/// Checks an owner text against a bound, never truncating. Inputs: the text and the bound.
/// Output: the trimmed text, or `questions_text_invalid` for hidden characters and
/// `questions_answer_invalid` when blank or too long.
fn bounded_answer(text: &str, max: usize) -> Result<String, EngineError> {
    assert!(max > 0);
    assert!(max <= ANSWER_BYTES_MAX);
    if hidden_text(text) {
        return Err(failure("questions_text_invalid"));
    }
    plain(text, max, true).ok_or_else(|| failure("questions_answer_invalid"))
}

fn owner() -> Writer {
    Writer::Owner {
        force_after_conflict: false,
    }
}

fn is_terminal(asker: &str) -> bool {
    asker.starts_with("terminal:")
}

/// Makes the questions log when it is missing, adds each later column to a log made before
/// it and the team link index. Adding the re-ask column marks every pending engine question
/// without options as due to be asked again as a choice (F14). Input: the connection or
/// transaction. Output: nothing, or `questions_unavailable`.
fn ensure(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let present: Vec<String> = {
        let mut statement = db
            .prepare("SELECT name FROM pragma_table_info('questions_log')")
            .map_err(unavailable)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    assert!(present.iter().any(|name| name == "id"));
    for (name, alter) in ADDED_COLUMNS {
        if present.iter().any(|column| column == name) {
            continue;
        }
        db.execute_batch(alter).map_err(unavailable)?;
        if name == REASK_COLUMN {
            db.execute_batch(REASK_OLD).map_err(unavailable)?;
        }
    }
    db.execute_batch(LINK_INDEX).map_err(unavailable)
}

/// Reads the questions a producer agent returned in its `{"questions": [...]}` block: each
/// item checked as an ask and dropped when invalid, at most `ASKS_FROM_ONE_REPLY_MAX`, every
/// one given `context`. Inputs: the parsed block and the context line. Output: the asks.
pub fn asks_from_json(value: &serde_json::Value, context: &str) -> Vec<Ask> {
    let items = value
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut asks = Vec::new();
    for item in items.iter().take(ASKS_FROM_ONE_REPLY_MAX * 2) {
        if asks.len() == ASKS_FROM_ONE_REPLY_MAX {
            break;
        }
        if let Some(ask) = ask_from_item(item, context) {
            asks.push(ask);
        }
    }
    assert!(asks.len() <= ASKS_FROM_ONE_REPLY_MAX);
    assert!(asks.len() <= items.len());
    asks
}

/// Reads one question item as an ask, or none when a field is missing, of the wrong type
/// or out of bounds. Inputs: the item and the context line. Output: the checked ask.
fn ask_from_item(item: &serde_json::Value, context: &str) -> Option<Ask> {
    let text = |key: &str| {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let options: Vec<String> = match item.get("options") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(values)) if values.len() <= OPTIONS_MAX => values
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<_>>()?,
        Some(_) => return None,
    };
    let named = text("kind").filter(|name| !name.is_empty())?;
    let kind = QuestionKind::stored(&named, options.len())?;
    let ask = Ask {
        question: text("question")?,
        kind,
        options,
        recommended: text("recommended"),
        context: Some(context.to_owned()),
    }
    .checked()
    .ok()?;
    assert!(ask.options.len() <= OPTIONS_MAX);
    assert!(ask.kind == kind);
    Some(ask)
}

fn immediate(db: &Connection) -> Result<Transaction<'_>, EngineError> {
    Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)
}

/// Checks one plain text: trimmed, not empty, at most `max` bytes, no control characters
/// but, where `lines` allows, line breaks and tabs. Output: the trimmed text, or none.
fn plain(text: &str, max: usize, lines: bool) -> Option<String> {
    let text = text.trim();
    let allowed = |c: char| !c.is_control() || (lines && (c == '\n' || c == '\t'));
    (!text.is_empty() && text.len() <= max && text.chars().all(allowed)).then(|| text.to_owned())
}

/// Joins a text's words with single spaces, for one-line quotes.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Starts a label with a capital letter for the start of a notice.
fn capital(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Converts an id the engine wrote back into its SQLite column; an id past `i64::MAX`
/// names no row.
fn column(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unavailable<E>(_: E) -> EngineError {
    failure("questions_unavailable")
}
