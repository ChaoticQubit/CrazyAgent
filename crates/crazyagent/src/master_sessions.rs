use crate::engine::EngineError;
use crate::master_session::{MasterBackend, MasterSessionPhase};
use crate::protocol::{Cursor, ProjectId, SessionId};
use crate::team::master::SessionConversation;
use crate::team::names;
use crate::team::{AgentRole, SwitchMode};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The most sessions one list read returns, newest activity first.
pub const SESSIONS_LISTED_MAX: usize = 50;
/// The most characters a session's first or last line keeps in the list.
pub const LINE_CHARS_MAX: usize = 80;
/// The most bytes of one stored message of a master conversation; longer text is cut with
/// "..." at the end.
pub const MESSAGE_BYTES_MAX: usize = 8192;
/// The most messages one page of a session's conversation carries.
pub const MESSAGES_PAGE_MAX: u8 = 64;
/// The most bytes one page of a session's conversation serializes to, so it fits a frame.
const MESSAGES_PAGE_BYTES_MAX: usize = crate::master_session::PAGE_BYTES_MAX;
/// The one store of every master conversation: one row per message, whatever harness ran
/// the master and whether the engine ran the turn itself or the master's own app ran it in
/// the live terminal. `seq` orders a session's rows; `speaker` is who said it, with `switch`
/// for the divider where another master took over; `mode` and `harness` say which master
/// the row belongs to, or for a divider which one took over; `text` is absent for a
/// terminal turn whose typed words were not read. `source` and `source_key` name the turn
/// or record line a row came from, so writing one twice keeps one row.
const MESSAGES: &str = "CREATE TABLE IF NOT EXISTS session_messages (
    session TEXT NOT NULL CHECK(length(session) BETWEEN 1 AND 128),
    seq INTEGER NOT NULL CHECK(seq > 0),
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    speaker TEXT NOT NULL CHECK(speaker IN ('owner','engine','master','switch')),
    mode TEXT NOT NULL CHECK(mode IN ('one_turn','live')),
    harness TEXT CHECK(harness IS NULL OR length(harness) BETWEEN 1 AND 64),
    text TEXT CHECK(text IS NULL OR length(CAST(text AS BLOB)) BETWEEN 1 AND 8192),
    at_ms INTEGER NOT NULL CHECK(at_ms >= 0),
    source TEXT NOT NULL CHECK(source IN ('turn','terminal','record','counter','switch','backfill')),
    source_key TEXT NOT NULL CHECK(length(source_key) BETWEEN 1 AND 192),
    PRIMARY KEY(session,seq),
    UNIQUE(session,source,source_key)
)";
const _: () = assert!(MESSAGE_BYTES_MAX == 8192);
/// The most stored messages a summary, a handoff or a history reads: the newest ones.
const MESSAGES_READ_MAX: usize = 2048;
/// The most sessions one engine start moves into the store.
#[cfg(unix)]
const BACKFILL_SESSIONS_MAX: usize = 4096;
/// The most terminal turns an older engine only counted that the store keeps as rows.
const COUNTED_TURNS_MAX: u64 = 1024;
/// The `source` of a divider the backfill placed where a live conversation began after
/// one-turn turns that no handoff carried over: it settles nothing.
const BACKFILL: &str = "backfill";
/// The `source` of a divider written when a master took a conversation over.
const SWITCH: &str = "switch";
/// The `source` of the rows of an engine-run turn.
const TURN: &str = "turn";
/// The `source` of the rows of a turn run in the live terminal.
const TERMINAL: &str = "terminal";
/// The `source` of rows read once from a harness record an older engine kept.
const RECORD: &str = "record";
/// The `source` of rows that stand for terminal turns an older engine only counted.
const COUNTER: &str = "counter";
/// What a typed message holds when the engine itself typed it to carry a conversation
/// over, so it is never stored as something the owner said.
const SEED_MARKS: [&str; 5] = [
    "switched this conversation's master from",
    "Summary of this session's conversation so far",
    "Summary of the newest turns of this session's conversation",
    "Your earlier harness conversation in this session",
    "The newest part of your earlier harness conversation in this session",
];
const _: () = assert!(MESSAGES_PAGE_BYTES_MAX < crate::protocol::FRAME_BYTES_MAX);
/// The most agent names one listed session carries.
pub const LISTED_AGENTS_MAX: usize = 6;
/// The most bytes the encoded session list may take, under the owner frame bound.
pub const LIST_BYTES_MAX: usize = 49152;
/// The most bytes the engine-built state section of a master's context may take.
pub const STATE_BYTES_MAX: usize = 3072;
/// The most bytes the transcript summary that seeds a fresh harness conversation may take.
pub const SUMMARY_BYTES_MAX: usize = 8192;
/// The most rows of each kind the state section lists.
pub const STATE_ROWS_MAX: usize = 12;
/// The most bytes of earlier turns a master harness switch hands to the new harness.
pub const HANDOFF_BYTES_MAX: usize = 8192;
/// The most bytes of the seed a fresh master conversation starts with: two summaries and
/// the session state, which also holds a switch handoff with its open tasks.
pub const SEED_BYTES_MAX: usize = 2 * SUMMARY_BYTES_MAX + STATE_BYTES_MAX + 4;
/// The line a switch handoff ends its turns with when older turns did not fit.
pub const HANDOFF_OMITTED: &str = "(Older turns were left out to fit.)";
/// The most bytes one turn takes in a switch handoff, so the newest turn always fits.
pub const HANDOFF_TURN_BYTES_MAX: usize = HANDOFF_BYTES_MAX / 4;
/// The line a handed-over turn shows where its earlier replies did not fit.
pub const TURN_OMITTED: &str = "(Earlier replies in this turn were left out.)";
/// The fewest bytes of room an older turn is cut into rather than left out.
const HANDOFF_TURN_MIN_BYTES: usize = 160;
/// The longest title a handoff part has, naming the conversation it comes from.
const HANDOFF_TITLE_BYTES_MAX: usize = 128;

#[cfg(unix)]
const MARKS: &str = "CREATE TABLE IF NOT EXISTS session_marks (
    session TEXT PRIMARY KEY CHECK(length(session) BETWEEN 1 AND 128),
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    started_ms INTEGER NOT NULL CHECK(started_ms >= 0),
    active_ms INTEGER NOT NULL CHECK(active_ms >= started_ms)
)";

#[cfg(unix)]
const OWNER_TURNS: &str = "CREATE TABLE session_owner_turns (
    session TEXT PRIMARY KEY CHECK(length(session) BETWEEN 1 AND 128),
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    turns INTEGER NOT NULL DEFAULT 1 CHECK(turns >= 0),
    last_line TEXT CHECK(last_line IS NULL OR length(last_line) BETWEEN 1 AND 80)
)";

/// Gives a table an older engine made the newest line of each session's terminal turns.
#[cfg(unix)]
const OWNER_TURNS_LINED: &str = "ALTER TABLE session_owner_turns ADD COLUMN last_line TEXT CHECK(last_line IS NULL OR length(last_line) BETWEEN 1 AND 80);";

const _: () = assert!(LINE_CHARS_MAX == 80);

/// Gives a table an older engine made its turn count: a row of a session with no engine
/// turn counts the one terminal turn it proves, while a row of a session with engine turns
/// may be the first creation's backfill of a kept conversation, so it counts none.
#[cfg(unix)]
const OWNER_TURNS_COUNTED: &str =
    "ALTER TABLE session_owner_turns ADD COLUMN turns INTEGER NOT NULL DEFAULT 1 CHECK(turns >= 0);
     UPDATE session_owner_turns SET turns=0
     WHERE EXISTS(SELECT 1 FROM master_turns t WHERE t.session=session_owner_turns.session);";

/// The SQL condition that the master session `s` holds an owner turn: a stored message the
/// owner or the engine opened a turn with, in either master mode, or a turn row the engine
/// keeps as run state.
const HAS_OWNER_TURN: &str = "(EXISTS(SELECT 1 FROM master_turns t WHERE t.session=s.id) OR EXISTS(SELECT 1 FROM session_messages g WHERE g.session=s.id AND g.speaker IN ('owner','engine')))";
/// The same condition for a database that has no message store yet.
const HAS_TURN_ROW: &str = "EXISTS(SELECT 1 FROM master_turns t WHERE t.session=s.id)";

/// The SQL condition that the master session `s` holds an owner turn, as this database can
/// answer it. Takes the connection. Returns the condition.
fn owner_turn_condition(connection: &Connection) -> Result<&'static str, EngineError> {
    Ok(if table_exists(connection, "session_messages")? {
        HAS_OWNER_TURN
    } else {
        HAS_TURN_ROW
    })
}

/// One master session as the Sessions list shows it. `created` and `last_activity` are
/// engine revisions; `started_ms` and `active_ms` are wall-clock times, absent for sessions
/// older than the marks. `turns`, `first_line` and `last_line` come from the session's
/// stored messages, whatever master ran them: the turns the owner or the engine opened,
/// the first line the owner wrote and the newest line the owner or a master wrote.
/// `agents` names the session's own sub-agents, newest first;
/// `results_waiting` counts finished sub-agent results queued for this session's master.
/// `mode` and `harness` name the master that ran the session's newest stored message, or
/// took the conversation over last; both are absent for a session with no stored message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterSessionSummary {
    pub session_id: SessionId,
    pub phase: MasterSessionPhase,
    pub created: Cursor,
    pub last_activity: Cursor,
    pub started_ms: Option<u64>,
    pub active_ms: Option<u64>,
    pub turns: u64,
    pub first_line: Option<String>,
    pub last_line: Option<String>,
    pub agents: Vec<String>,
    pub results_waiting: u64,
    pub mode: Option<MasterMode>,
    pub harness: Option<String>,
}

/// Who said one message of a master conversation: the owner, the engine on its own
/// behalf, the master, or `Switch` for the divider where another master took over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSpeaker {
    Owner,
    Engine,
    Master,
    Switch,
}

impl MessageSpeaker {
    /// The stored name. Takes the speaker. Returns the name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Engine => "engine",
            Self::Master => "master",
            Self::Switch => "switch",
        }
    }

    /// The speaker a stored name means. Takes the name. Returns None for any other text.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Owner, Self::Engine, Self::Master, Self::Switch]
            .into_iter()
            .find(|speaker| speaker.name() == name)
    }
}

/// How a master runs: the engine runs each turn itself (`OneTurn`), or the master's own app
/// runs in the live terminal (`Live`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterMode {
    OneTurn,
    Live,
}

impl MasterMode {
    /// The stored name. Takes the mode. Returns the name.
    pub fn name(self) -> &'static str {
        match self {
            Self::OneTurn => "one_turn",
            Self::Live => "live",
        }
    }

    /// The mode a stored name means. Takes the name. Returns None for any other text.
    pub fn named(name: &str) -> Option<Self> {
        [Self::OneTurn, Self::Live]
            .into_iter()
            .find(|mode| mode.name() == name)
    }
}

/// One message of a master conversation as the store keeps it and every view reads it.
/// `seq` orders a session's messages from 1. `harness` is the catalog id of a live master's
/// harness, or the backend name of a one-turn master (`rcli`, `codex`, `claude`). `text` is
/// absent for a terminal turn whose typed words were not read; for a `switch` it is the
/// divider line. `at_ms` is the wall-clock time the row was stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMessage {
    pub seq: u64,
    pub speaker: MessageSpeaker,
    pub mode: MasterMode,
    pub harness: Option<String>,
    pub text: Option<String>,
    pub at_ms: u64,
}

/// One page of a session's conversation: `messages` oldest first, ending just before the
/// `before` the read named, or at the session's newest message. `next` is the `before` of
/// the page of older messages, absent once the page holds the session's first message.
/// `count` is every stored message of the session, dividers included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMessagePage {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub messages: Vec<SessionMessage>,
    pub next: Option<u64>,
    pub count: u64,
}

/// How the live master runs a project's live session. `Resume` continues the harness
/// conversation the session kept, by its id, in the home of `agent` where `record` keeps
/// it. `Fresh` starts a new harness conversation whose first input is `seed`, the
/// engine-built summary of the session's earlier turns and its state, and shows `note` in
/// the master view so history is never dropped silently. `New` starts a session that has
/// no earlier turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResumePlan {
    Resume {
        agent: i64,
        conversation: String,
        record: PathBuf,
    },
    Fresh {
        seed: String,
        note: String,
    },
    New,
}

/// The note a resumed session shows when its harness conversation starts fresh.
pub const FRESH_NOTE: &str = "This session continues in a new harness conversation, started with a summary of its earlier turns.";
/// The note a resumed session shows when its earlier harness conversation can no longer be read.
pub const LOST_NOTE: &str = "This session continues in a new harness conversation. Its earlier harness conversation could not be read, so only the session's state was passed on.";

/// Creates the tables of session wall-clock marks and of sessions with an owner turn when
/// the engine database opens. The first creation counts every session that kept a harness
/// conversation and ran no engine turn as having one terminal turn, so sessions used only
/// in the live terminal stay listed; a table an older engine made without the turn count
/// gains it as `OWNER_TURNS_COUNTED` says. A new table, or one that gains the newest line,
/// fills it once by `fill_lines`. Takes the engine connection. Returns any SQLite failure,
/// with the table left as it was.
#[cfg(unix)]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    initialize_owner_turns(connection)?;
    connection.execute_batch(MESSAGES).map_err(error)?;
    backfill(connection)
}

/// Creates the marks table and the table of sessions with an owner turn as older engines
/// kept them. Takes the engine connection. Returns any SQLite failure.
#[cfg(unix)]
fn initialize_owner_turns(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(MARKS).map_err(error)?;
    if table_exists(connection, "session_owner_turns")? {
        if !owner_turns_column(connection, "turns")? {
            owner_turns_change(connection, |connection| {
                connection.execute_batch(OWNER_TURNS_COUNTED).map_err(error)
            })?;
        }
        if !owner_turns_column(connection, "last_line")? {
            owner_turns_change(connection, |connection| {
                connection.execute_batch(OWNER_TURNS_LINED).map_err(error)?;
                fill_lines(connection)
            })?;
        }
        return Ok(());
    }
    let backfill = if table_exists(connection, "team_master_conversations")? {
        "INSERT OR IGNORE INTO session_owner_turns(session,project)
         SELECT c.session,c.project FROM team_master_conversations c
         WHERE length(c.session) BETWEEN 1 AND 128 AND length(c.project) BETWEEN 1 AND 128
         AND NOT EXISTS(SELECT 1 FROM master_turns t WHERE t.session=c.session);"
    } else {
        ""
    };
    owner_turns_change(connection, |connection| {
        connection
            .execute_batch(&format!("{OWNER_TURNS}; {backfill}"))
            .map_err(error)?;
        fill_lines(connection)
    })
}

#[cfg(unix)]
fn owner_turns_column(connection: &Connection, name: &str) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('session_owner_turns') WHERE name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Applies a change to the table of sessions with an owner turn all or nothing. Takes the
/// engine connection and the change. Returns any failure, rolled back.
#[cfg(unix)]
fn owner_turns_change(
    connection: &Connection,
    change: impl FnOnce(&Connection) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    connection
        .execute_batch("SAVEPOINT session_owner_turns")
        .map_err(error)?;
    let applied = change(connection).and_then(|()| {
        connection
            .execute_batch("RELEASE session_owner_turns")
            .map_err(error)
    });
    if applied.is_err() {
        let _ = connection
            .execute_batch("ROLLBACK TO session_owner_turns; RELEASE session_owner_turns;");
    }
    applied
}

/// Fills the newest line of the sessions with terminal turns and no line yet from the last
/// message of the harness conversation each kept, newest sessions first and at most
/// `SESSIONS_LISTED_MAX`, so a session an older engine recorded shows a line once. A record
/// that cannot be read leaves its line empty. Takes the engine connection. Returns any
/// SQLite failure.
#[cfg(unix)]
fn fill_lines(connection: &Connection) -> Result<(), EngineError> {
    if !table_exists(connection, "team_master_conversations")? {
        return Ok(());
    }
    let mut statement = connection
        .prepare(
            "SELECT o.session,c.harness,c.record FROM session_owner_turns o
             JOIN team_master_conversations c ON c.session=o.session
             JOIN master_sessions s ON s.id=o.session
             WHERE o.turns > 0 AND o.last_line IS NULL AND length(c.harness) <= 64
             ORDER BY s.version DESC LIMIT ?1",
        )
        .map_err(error)?;
    let kept = statement
        .query_map(
            [i64::try_from(SESSIONS_LISTED_MAX).map_err(error)?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    assert!(kept.len() <= SESSIONS_LISTED_MAX);
    for (session, harness, record) in kept {
        let messages = crate::harnesses::transcript::conversation_messages(
            &harness,
            std::path::Path::new(&record),
        );
        let Some(line) = messages
            .last()
            .and_then(|(_, text)| clip(text, LINE_CHARS_MAX))
        else {
            continue;
        };
        connection
            .execute(
                "UPDATE session_owner_turns SET last_line=?1 WHERE session=?2",
                params![line, session],
            )
            .map_err(error)?;
    }
    Ok(())
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(error)
}

/// Stores the start of a turn a project's master session runs in its live terminal as an
/// owner message of the session's one conversation, so the session is listed, New session
/// may leave it and every view shows the turn. Its typed words are not known yet; the
/// turn's end reads them. A session whose messages another project holds is left as it
/// is. Takes the engine database, the project, the session and the harness in the
/// terminal. Returns Ok, also when the session id cannot be one.
pub(crate) fn mark_owner_turn(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
    harness: &str,
) -> Result<(), EngineError> {
    if session.is_empty() || session.len() > 128 {
        return Ok(());
    }
    connection.execute_batch(MESSAGES).map_err(error)?;
    if held_elsewhere(connection, project, session)? {
        return Ok(());
    }
    let next: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM session_messages WHERE session=?1",
            [session],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert!(next > 0);
    let added = append(
        connection,
        project.as_str(),
        session,
        &Draft {
            speaker: MessageSpeaker::Owner,
            mode: MasterMode::Live,
            harness: Some(harness),
            text: None,
            source: TERMINAL,
            key: &format!("t{next}"),
        },
    )?;
    assert!(added);
    Ok(())
}

/// Stores the end of a turn a project's master session ran in its live terminal: the
/// reply as the master's message after the turn's owner message, and that message's typed
/// words when the record the session kept shows them. A turn whose start was not seen is
/// stored whole. A session whose messages another project holds is left as it is. Takes
/// the engine database, the project, the session, the harness in the terminal and the
/// reply. Returns Ok, also when the reply shows no words or the session id cannot be one.
pub(crate) fn mark_owner_reply(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
    harness: &str,
    reply: &str,
) -> Result<(), EngineError> {
    if session.is_empty() || session.len() > 128 {
        return Ok(());
    }
    connection.execute_batch(MESSAGES).map_err(error)?;
    if held_elsewhere(connection, project, session)? {
        return Ok(());
    }
    let words = clip(reply, LINE_CHARS_MAX).is_some();
    let mut turn = open_turn(connection, session)?;
    if turn.is_none() && words {
        mark_owner_turn(connection, project, session, harness)?;
        turn = open_turn(connection, session)?;
        assert!(turn.is_some());
    }
    let Some((seq, key)) = turn else {
        return Ok(());
    };
    fill_typed(connection, project, session, seq)?;
    if words {
        append(
            connection,
            project.as_str(),
            session,
            &Draft {
                speaker: MessageSpeaker::Master,
                mode: MasterMode::Live,
                harness: Some(harness),
                text: Some(reply),
                source: TERMINAL,
                key: &format!("{key}/reply"),
            },
        )?;
    }
    Ok(())
}

/// Tells whether a session's stored messages belong to another project. Takes the
/// connection, project and session. Returns the answer.
fn held_elsewhere(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM session_messages WHERE session=?1 AND project<>?2)",
            params![session, project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)
}

/// The live terminal turn a session's newest stored message opened and no reply closed
/// yet. Takes the connection and session. Returns its seq and key, or None when the
/// newest message is anything else.
fn open_turn(connection: &Connection, session: &str) -> Result<Option<(i64, String)>, EngineError> {
    let newest: Option<(i64, String, String, String, String)> = connection
        .query_row(
            "SELECT seq,source_key,speaker,mode,source FROM session_messages
             WHERE session=?1 ORDER BY seq DESC LIMIT 1",
            [session],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(error)?;
    Ok(newest.and_then(|(seq, key, speaker, mode, source)| {
        (speaker == MessageSpeaker::Owner.name()
            && mode == MasterMode::Live.name()
            && source == TERMINAL)
            .then_some((seq, key))
    }))
}

/// Gives a live terminal turn's owner message its typed words from the record the session
/// kept: the record's newest typed input, unless the engine typed it to carry a
/// conversation over or it is the previous turn's, which a record not written yet still
/// shows. Takes the connection, project, session and the message's seq. Returns any SQLite
/// failure; a record that cannot be read leaves the message without text.
fn fill_typed(
    connection: &Connection,
    project: &ProjectId,
    session: &str,
    seq: i64,
) -> Result<(), EngineError> {
    use crate::harnesses::transcript::{Speaker, conversation_messages};
    if !table_exists(connection, "team_master_conversations")? {
        return Ok(());
    }
    let Some(kept) = crate::team::master::session_conversation(connection, project, session)?
        .filter(|kept| kept.resume.id != crate::team::master::REFUSED_CONVERSATION)
    else {
        return Ok(());
    };
    let typed = conversation_messages(&kept.harness, &kept.resume.record)
        .into_iter()
        .rev()
        .find_map(|(speaker, text)| matches!(speaker, Speaker::Input).then_some(text))
        .filter(|text| !text.is_empty() && !seeded(text))
        .map(|text| marked(&text, MESSAGE_BYTES_MAX));
    let Some(typed) = typed else {
        return Ok(());
    };
    let previous: Option<String> = connection
        .query_row(
            "SELECT text FROM session_messages
             WHERE session=?1 AND seq<?2 AND speaker='owner' AND mode='live' AND text IS NOT NULL
             ORDER BY seq DESC LIMIT 1",
            params![session, seq],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    if previous.as_deref() == Some(typed.as_str()) {
        return Ok(());
    }
    let filled = connection
        .execute(
            "UPDATE session_messages SET text=?1 WHERE session=?2 AND seq=?3 AND text IS NULL",
            params![typed, session, seq],
        )
        .map_err(error)?;
    assert!(filled <= 1);
    Ok(())
}

/// Tells whether a master session holds an owner turn. Takes the connection and session.
/// Returns false for a session with none, or one that does not exist.
pub(crate) fn has_owner_turn(
    connection: &Connection,
    session: &SessionId,
) -> Result<bool, EngineError> {
    connection
        .query_row(
            &format!(
                "SELECT {} FROM master_sessions s WHERE s.id=?1",
                owner_turn_condition(connection)?
            ),
            [session.as_str()],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map(|found| found.unwrap_or(false))
        .map_err(error)
}

/// Migration part that gives team tasks and team agents their owning session column.
/// Tables not created yet are left to `team::initialize`, which makes the column.
/// Takes the upgrade transaction. Returns Ok once both tables have the column.
#[cfg(unix)]
pub(crate) fn add_ownership_columns(tx: &Transaction<'_>) -> Result<(), EngineError> {
    for table in ["team_tasks", "team_agents"] {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !exists {
            continue;
        }
        let present: bool = tx
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='session')"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !present {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN session TEXT"))
                .map_err(error)?;
        }
    }
    Ok(())
}

/// The live master session of a project, the one whose phase is `configured`.
/// Takes the connection and project. Returns its id, or None when no session is live or
/// the database has no master sessions table yet.
pub fn live_session(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Option<String>, EngineError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='master_sessions')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !exists {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT CASE WHEN length(CAST(id AS BLOB))<=128 THEN id END FROM master_sessions WHERE project=?1 AND phase='configured'",
            [project.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(error)
}

/// Records that a project's live session was used now: its start time on first use and
/// its last activity time. Takes the transaction, project and time. Returns Ok, also
/// when no session is live.
pub(crate) fn touch(
    tx: &Transaction<'_>,
    project: &ProjectId,
    now: u64,
) -> Result<(), EngineError> {
    let Some(session) = live_session(tx, project)? else {
        return Ok(());
    };
    let now = i64::try_from(now).map_err(error)?;
    tx.execute(
        "INSERT INTO session_marks(session,project,started_ms,active_ms) VALUES (?1,?2,?3,?3)
         ON CONFLICT(session) DO UPDATE SET active_ms=MAX(active_ms,excluded.active_ms)",
        params![session, project.as_str(), now],
    )
    .map_err(error)?;
    Ok(())
}

/// Counts the finished sub-agent results queued for one session's master: tasks the
/// session owns that ended and were not yet delivered to a master.
/// Takes the connection and session. Returns the count.
pub fn results_waiting(connection: &Connection, session: &SessionId) -> Result<u64, EngineError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM team_tasks WHERE session=?1 AND source='master' AND delivered=0 AND status IN ('done','failed','cancelled')",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    u64::try_from(count).map_err(error)
}

/// Lists a project's master sessions that hold an owner turn: the live one first, then
/// newest activity first, at most `SESSIONS_LISTED_MAX`, within `LIST_BYTES_MAX` once
/// encoded. A session with no owner turn, the live one included, is not listed.
/// Takes the connection and project. Returns the summaries.
pub(crate) fn list(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<MasterSessionSummary>, EngineError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT CASE WHEN length(CAST(s.id AS BLOB))<=128 THEN s.id END,s.created,s.version,
                    CASE WHEN length(s.phase)<=10 THEN s.phase END,
                    MAX(s.version,COALESCE((SELECT MAX(t.input) FROM master_turns t WHERE t.session=s.id),0)) AS active,
                    m.started_ms,m.active_ms
             FROM master_sessions s LEFT JOIN session_marks m ON m.session=s.id
             WHERE s.project=?1 AND {}
             ORDER BY s.phase='configured' DESC, active DESC, s.created DESC LIMIT ?2",
            owner_turn_condition(connection)?
        ))
        .map_err(error)?;
    type Row = (
        Option<String>,
        i64,
        i64,
        Option<String>,
        i64,
        Option<i64>,
        Option<i64>,
    );
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                i64::try_from(SESSIONS_LISTED_MAX).map_err(error)?
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<Row>, _>>()
        .map_err(error)?;
    let mut sessions = Vec::with_capacity(rows.len());
    let mut bytes = 2usize;
    for (id, created, version, phase, active, started_ms, active_ms) in rows {
        let session_id =
            SessionId::new(id.ok_or_else(|| error("master_session_record"))?).map_err(error)?;
        let phase = match phase.as_deref() {
            Some("configured") => MasterSessionPhase::Configured,
            Some("parked") => MasterSessionPhase::Parked,
            Some("closed") => MasterSessionPhase::Closed,
            _ => return Err(error("master_session_record")),
        };
        let created = u64::try_from(created).map_err(error)?;
        let version = u64::try_from(version).map_err(error)?;
        let active = u64::try_from(active).map_err(error)?;
        if created == 0 || version < created || active < version {
            return Err(error("master_session_record"));
        }
        let (turns, first_line, last_line) = listed_conversation(connection, &session_id)?;
        let (mode, harness) = newest_runner(connection, &session_id)?.unzip();
        let summary = MasterSessionSummary {
            phase,
            created: Cursor::new(created),
            last_activity: Cursor::new(active),
            started_ms: started_ms.map(u64::try_from).transpose().map_err(error)?,
            active_ms: active_ms.map(u64::try_from).transpose().map_err(error)?,
            turns,
            first_line,
            last_line,
            agents: agent_names(connection, project, &session_id)?,
            results_waiting: results_waiting(connection, &session_id)?,
            mode,
            harness: harness.flatten(),
            session_id,
        };
        let size = serde_json::to_vec(&summary).map_err(error)?.len() + 1;
        if bytes + size > LIST_BYTES_MAX {
            break;
        }
        bytes += size;
        sessions.push(summary);
    }
    assert!(sessions.len() <= SESSIONS_LISTED_MAX);
    assert!(serde_json::to_vec(&sessions).map_err(error)?.len() <= LIST_BYTES_MAX);
    Ok(sessions)
}

/// What the Sessions list shows of a session's stored conversation, whatever master ran
/// it: how many turns the owner or the engine opened, the first line the owner wrote and
/// the newest line the owner or a master wrote, both clipped for the list. Takes the
/// connection and session. Returns the three; 0 and no lines for a session with nothing
/// stored or a database without the store.
fn listed_conversation(
    connection: &Connection,
    session: &SessionId,
) -> Result<(u64, Option<String>, Option<String>), EngineError> {
    if !table_exists(connection, "session_messages")? {
        return Ok((0, None, None));
    }
    let turns: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM session_messages
             WHERE session=?1 AND speaker IN ('owner','engine')",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    let line = |sql: &str| -> Result<Option<String>, EngineError> {
        let text: Option<Option<String>> = connection
            .query_row(sql, [session.as_str()], |row| row.get(0))
            .optional()
            .map_err(error)?;
        Ok(text.flatten().and_then(|text| clip(&text, LINE_CHARS_MAX)))
    };
    let first = line(
        "SELECT CASE WHEN length(CAST(text AS BLOB))<=8192 THEN text END FROM session_messages
         WHERE session=?1 AND speaker='owner' AND text IS NOT NULL ORDER BY seq LIMIT 1",
    )?;
    let last = line(
        "SELECT CASE WHEN length(CAST(text AS BLOB))<=8192 THEN text END FROM session_messages
         WHERE session=?1 AND speaker IN ('owner','master') AND text IS NOT NULL
         ORDER BY seq DESC LIMIT 1",
    )?;
    let turns = u64::try_from(turns).map_err(error)?;
    assert!(turns > 0 || first.is_none());
    Ok((turns, first, last))
}

/// One agent a session owns, as its master's context and the list name it.
struct OwnAgent {
    label: String,
    harness: String,
    status: String,
    result: Option<String>,
}

/// Reads the sub-agents a session owns, newest first. Takes the connection, project,
/// session and the most to read. Returns each agent's label, harness, status and the
/// result of its newest finished task.
fn own_agents(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    limit: usize,
) -> Result<Vec<OwnAgent>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT a.id,CASE WHEN length(a.harness)<=64 THEN a.harness END,CASE WHEN length(a.status)<=16 THEN a.status END,
                    (SELECT CASE WHEN length(CAST(t.result AS BLOB))<=65536 THEN t.result END FROM team_tasks t
                     WHERE t.agent=a.id AND t.result IS NOT NULL ORDER BY t.finished_ms DESC, t.id DESC LIMIT 1)
             FROM team_agents a WHERE a.project=?1 AND a.session=?2 AND a.role='sub'
             ORDER BY a.id DESC LIMIT ?3",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![
                project.as_str(),
                session.as_str(),
                i64::try_from(limit).map_err(error)?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut agents = Vec::with_capacity(rows.len());
    for (id, harness, status, result) in rows {
        let name = names::name(connection, id)?;
        agents.push(OwnAgent {
            label: names::label(AgentRole::Sub, id, name.as_ref()),
            harness: harness.ok_or_else(|| error("team_agent_record"))?,
            status: status.ok_or_else(|| error("team_agent_record"))?,
            result,
        });
    }
    assert!(agents.len() <= limit);
    Ok(agents)
}

fn agent_names(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Vec<String>, EngineError> {
    Ok(own_agents(connection, project, session, LISTED_AGENTS_MAX)?
        .into_iter()
        .filter_map(|agent| clip(&agent.label, 48))
        .collect())
}

/// The engine-built state section of a session's master context: its own agents with
/// harness, state and last result, the project's open tasks, messages waiting for the
/// master and pending change sets. Project-level context is built elsewhere and is the
/// same for every session. Takes the connection, project and session. Returns the
/// section within `STATE_BYTES_MAX`, or None when there is nothing to list.
pub fn session_state(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Option<String>, EngineError> {
    let mut sections = Vec::new();
    let agents = own_agents(connection, project, session, STATE_ROWS_MAX)?;
    if !agents.is_empty() {
        let mut text =
            String::from("Your agents in this session (message or delegate to them by name):");
        for agent in &agents {
            text.push_str("\n- ");
            text.push_str(&names::line(&agent.label, &agent.harness, &agent.status));
            if let Some(result) = agent.result.as_deref().and_then(|result| flat(result, 160)) {
                text.push_str(". Last result: ");
                text.push_str(&result);
            }
        }
        sections.push(text);
    }
    let tasks = rows(
        connection,
        "SELECT 'Task '||id||' ('||harness||', '||status||'): '||CASE WHEN length(CAST(prompt AS BLOB))<=65536 THEN prompt END
         FROM team_tasks WHERE project=?1 AND status IN ('queued','needs_pool','running') ORDER BY id LIMIT ?2",
        project,
    )?;
    if !tasks.is_empty() {
        sections.push(listed("Open tasks:", &tasks));
    }
    let messages = rows(
        connection,
        "SELECT author||': '||CASE WHEN length(CAST(text AS BLOB))<=65536 THEN text END
         FROM team_messages WHERE project=?1 AND state IN ('pending','sent') AND author<>'master' ORDER BY id LIMIT ?2",
        project,
    )?;
    if !messages.is_empty() {
        sections.push(listed("Waiting questions and messages:", &messages));
    }
    let landing: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='landing_change_sets')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    let found = if landing {
        crate::landing::owner_list(connection, project)?
    } else {
        Vec::new()
    };
    let changes: Vec<String> = found
        .into_iter()
        .filter(|change| {
            matches!(
                change.state,
                crate::landing::ChangeState::Pending
                    | crate::landing::ChangeState::Conflict
                    | crate::landing::ChangeState::Updating
            )
        })
        .take(STATE_ROWS_MAX)
        .map(|change| crate::landing::open_set_line(&change))
        .collect();
    if !changes.is_empty() {
        sections.push(listed("Open change sets:", &changes));
    }
    if sections.is_empty() {
        return Ok(None);
    }
    let mut text = String::from("Session state from the engine:\n\n");
    text.push_str(&sections.join("\n\n"));
    let text = fit(text, STATE_BYTES_MAX);
    assert!(text.len() <= STATE_BYTES_MAX);
    Ok(Some(text))
}

fn rows(
    connection: &Connection,
    sql: &str,
    project: &ProjectId,
) -> Result<Vec<String>, EngineError> {
    let mut statement = connection.prepare(sql).map_err(error)?;
    let found = statement
        .query_map(
            params![
                project.as_str(),
                i64::try_from(STATE_ROWS_MAX).map_err(error)?
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    Ok(found
        .into_iter()
        .flatten()
        .filter_map(|line| clip(&line, 200))
        .collect())
}

fn listed(header: &str, lines: &[String]) -> String {
    let mut text = String::from(header);
    for line in lines {
        text.push_str("\n- ");
        text.push_str(line);
    }
    text
}

/// A session's stored messages, after what older engines kept of it is moved into the
/// store, so every reader sees the one conversation. It writes on the first call for an
/// old session. Takes the connection, project and session. Returns the messages, oldest
/// first.
fn conversation(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
) -> Result<Vec<Stored>, EngineError> {
    absorb(connection, project.as_str(), session.as_str())?;
    stored(connection, session.as_str())
}

/// Tells whether a stored message is the divider of a switch the owner set to Start fresh.
fn fresh_switch(stored: &Stored) -> bool {
    !said(stored) && stored.source == SWITCH && stored.key.starts_with("fresh:")
}

/// The messages a handoff or a summary may carry: those after the newest switch the owner
/// set to Start fresh, which left the earlier conversation behind. Takes the messages.
/// Returns the part that is carried.
fn carried(messages: &[Stored]) -> &[Stored] {
    let cut = messages.iter().rposition(fresh_switch);
    &messages[cut.map_or(0, |index| index + 1)..]
}

/// Tells whether a stored message was said in a live terminal, by any harness; the rows
/// that only stand for turns an older engine counted are not.
fn terminal_said(stored: &&Stored) -> bool {
    said(stored) && stored.message.mode == MasterMode::Live && stored.source != COUNTER
}

/// Reads the turns the one-turn master ran as one entry each, oldest first: the owner's
/// message or the engine's line, then the reply or a no-reply line, each flattened and
/// clipped. Takes the stored messages. Returns the entries of turns with text.
fn turn_entries(messages: &[Stored]) -> Vec<String> {
    let one_turn = |stored: &&Stored| said(stored) && stored.message.mode == MasterMode::OneTurn;
    let replies: std::collections::HashMap<&str, &str> = messages
        .iter()
        .filter(one_turn)
        .filter(|stored| stored.message.speaker == MessageSpeaker::Master)
        .filter_map(|stored| {
            Some((
                stored.key.strip_suffix("/reply")?,
                stored.message.text.as_deref()?,
            ))
        })
        .collect();
    let mut entries: Vec<String> = Vec::new();
    for stored in messages.iter().filter(one_turn) {
        let text = stored.message.text.as_deref();
        let asked = match stored.message.speaker {
            MessageSpeaker::Owner => text
                .and_then(|text| flat(text, 600))
                .map(|text| format!("Owner: {text}")),
            MessageSpeaker::Engine => text
                .and_then(|line| flat(line, 300))
                .map(|line| format!("Engine: {line}")),
            MessageSpeaker::Master | MessageSpeaker::Switch => None,
        };
        let Some(mut entry) = asked else {
            continue;
        };
        match replies
            .get(stored.key.as_str())
            .and_then(|reply| flat(reply, 600))
        {
            Some(reply) => {
                entry.push_str("\nYou: ");
                entry.push_str(&reply);
            }
            None => entry.push_str("\n(No reply was received.)"),
        }
        entries.push(entry);
    }
    assert!(entries.len() <= messages.len());
    entries
}

/// Keeps the newest entries of a summary that fit `SUMMARY_BYTES_MAX`, oldest first under
/// `header`, or under `cut` when older entries had to go. Takes the two headers and the
/// entries oldest first. Returns the summary, or None without entries.
fn newest(header: &str, cut: &str, entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut kept = Vec::new();
    let mut used = header.len().max(cut.len());
    for entry in entries.iter().rev() {
        if used + entry.len() + 2 > SUMMARY_BYTES_MAX {
            break;
        }
        used += entry.len() + 2;
        kept.push(entry.as_str());
    }
    kept.reverse();
    let mut text = String::from(if kept.len() < entries.len() {
        cut
    } else {
        header
    });
    for entry in kept {
        text.push_str("\n\n");
        text.push_str(entry);
    }
    let text = fit(text, SUMMARY_BYTES_MAX);
    assert!(text.len() <= SUMMARY_BYTES_MAX);
    Some(text)
}

/// Summarizes what was said in a session's live terminal, whatever harness ran there: what
/// was typed into the master and its replies, oldest first, newest kept when it must be
/// cut. Takes the stored messages. Returns the summary within `SUMMARY_BYTES_MAX`, or None
/// when no terminal message with text is stored.
fn live_summary(messages: &[Stored]) -> Option<String> {
    let entries: Vec<String> = messages
        .iter()
        .filter(terminal_said)
        .filter_map(|stored| {
            let said = flat(stored.message.text.as_deref()?, 600)?;
            Some(match stored.message.speaker {
                MessageSpeaker::Master => format!("You: {said}"),
                _ => format!("Typed to you: {said}"),
            })
        })
        .collect();
    newest(
        "Your earlier harness conversation in this session, oldest first:",
        "The newest part of your earlier harness conversation in this session, oldest first:",
        &entries,
    )
}

/// The engine hook the live master calls before it starts a project's live session. It
/// continues the conversation the session kept when it can: the same harness, a kept account
/// `continues` accepts for the one about to start, the session's master stopped with its
/// worktree, a resumable id and the record a plain file; else as `fresh_harness` says. Takes
/// the connection, project, session, launch harness and `continues`. Returns the plan.
pub fn resume_harness(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    harness: &str,
    continues: &dyn Fn(&str) -> bool,
) -> Result<ResumePlan, EngineError> {
    let kept = crate::team::master::session_conversation(connection, project, session.as_str())?;
    if let Some(kept) = &kept
        && crate::harnesses::same(&kept.harness, harness)
        && continues(&kept.connection)
        && crate::harnesses::interactive::resumable(&kept.resume.id)
        && std::fs::symlink_metadata(&kept.resume.record).is_ok_and(|meta| meta.is_file())
        && stopped_master(connection, project, kept.agent)?
    {
        return Ok(ResumePlan::Resume {
            agent: kept.agent,
            conversation: kept.resume.id.clone(),
            record: kept.resume.record.clone(),
        });
    }
    fresh_harness(connection, project, session, kept.as_ref())
}

/// How a session's master starts when its kept harness conversation cannot continue: a
/// session with earlier turns starts a fresh harness conversation seeded with the summary
/// of the turns its one-turn master ran, of what was said in its live terminal and of its
/// state, all read from the session's stored messages, with a note saying so; a session
/// without starts new. A session that kept a conversation of which no message is stored
/// says the conversation could not be read. Takes the connection, project, session and
/// the kept conversation. Returns `Fresh` or `New`.
pub fn fresh_harness(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    kept: Option<&SessionConversation>,
) -> Result<ResumePlan, EngineError> {
    let messages = conversation(connection, project, session)?;
    let messages = carried(&messages);
    let mut parts = Vec::new();
    parts.extend(newest(
        "Summary of this session's conversation so far, oldest first:",
        "Summary of the newest turns of this session's conversation, oldest first:",
        &turn_entries(messages),
    ));
    let read = live_summary(messages);
    let lost = kept.is_some() && read.is_none();
    parts.extend(read);
    if parts.is_empty() && !lost {
        return Ok(ResumePlan::New);
    }
    let note = if parts.is_empty() {
        LOST_NOTE
    } else {
        FRESH_NOTE
    };
    if let Some(state) = session_state(connection, project, session)? {
        parts.push(state);
    }
    if parts.is_empty() {
        parts.push(String::from(
            "This session's earlier conversation could not be read.",
        ));
    }
    let seed = parts.join("\n\n");
    assert!(seed.len() <= SEED_BYTES_MAX);
    Ok(ResumePlan::Fresh {
        seed,
        note: note.into(),
    })
}

/// Tells which master a session's conversation comes from when the live harness about to
/// start takes it over: any other master, a live terminal on another harness or the
/// one-turn master on any backend, decided from the session's stored messages by `owed`,
/// so no pair of harnesses has a rule of its own. A switch is seen once: its stored divider
/// settles it, even before the new harness says anything. A session with nothing stored of
/// a live terminal still counts the conversation it kept on another harness. Aliases of
/// one harness count as one. Takes the connection, project, session and the harness about
/// to start. Returns the earlier master as `ran_on` names it, or None when unchanged.
pub fn switched_from(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    harness: &str,
) -> Result<Option<String>, EngineError> {
    let messages = conversation(connection, project, session)?;
    let next = Runner {
        mode: MasterMode::Live,
        harness,
    };
    if let Some(from) = owed(&messages, next) {
        return Ok(Some(from));
    }
    let terminal = messages
        .iter()
        .any(|stored| stored.message.mode == MasterMode::Live);
    if terminal || !table_exists(connection, "team_master_conversations")? {
        return Ok(None);
    }
    let kept = crate::team::master::session_conversation(connection, project, session.as_str())?;
    Ok(kept
        .map(|kept| kept.harness)
        .filter(|earlier| !crate::harnesses::same(earlier, harness)))
}

/// Tells whether a session's master starts new because its last switch, to this harness
/// under Start fresh, is not yet followed by a conversation the new harness reported: the
/// kept conversation, if any, is still another harness's, which the owner chose not to
/// carry over (HSL-1). Takes the connection, project, session and the harness about to
/// start. Returns whether to start new.
pub fn fresh_after_switch(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    harness: &str,
) -> Result<bool, EngineError> {
    let switch: Option<(Option<String>, String)> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(harness AS BLOB))<=64 THEN harness END, mode
             FROM team_session_switches WHERE session=?1 AND project=?2",
            params![session.as_str(), project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((Some(to), mode)) = switch else {
        return Ok(false);
    };
    if !crate::harnesses::same(&to, harness) || SwitchMode::named(&mode) != Some(SwitchMode::Fresh)
    {
        return Ok(false);
    }
    let kept = crate::team::master::session_conversation(connection, project, session.as_str())?;
    Ok(kept.is_none_or(|kept| !crate::harnesses::same(&kept.harness, harness)))
}

/// The display name of a harness: its catalog name, or the stored name the catalog does
/// not know. Takes the stored name. Returns the name.
pub fn harness_label(harness: &str) -> &str {
    crate::harnesses::spec(harness).map_or(harness, |spec| spec.name)
}

/// The note a session's conversation shows where its master harness switched, read as a
/// divider in the conversation: what the new harness was given under the owner's setting.
/// Takes the new harness's display name and the mode. Returns the note.
pub fn switched_note(to: &str, mode: SwitchMode) -> String {
    match mode {
        SwitchMode::Carry => format!(
            "Switched to {to}. The conversation continues: {to} got its earlier turns, newest first, and the open tasks."
        ),
        SwitchMode::Fresh => format!(
            "Switched to {to}. Started fresh: {to} did not get the earlier conversation, as the On harness switch setting asks."
        ),
    }
}

/// Builds the Carry over handoff the master that takes a conversation over reads first: a
/// line naming the switch, then one part per kind of earlier conversation, what was said
/// in the live terminal first and the turns the one-turn master ran second, each newest
/// first within its share of `HANDOFF_BYTES_MAX`, then the open tasks. Both parts come
/// from the session's stored messages, whatever harness wrote them. Takes the connection,
/// project, session and the earlier master's display name. Returns the handoff.
pub fn handoff(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    from: &str,
) -> Result<String, EngineError> {
    let messages = conversation(connection, project, session)?;
    let messages = carried(&messages);
    let mut parts: Vec<(String, Vec<String>)> = Vec::new();
    let terminal = live_turns(messages);
    if !terminal.is_empty() {
        let label = messages
            .iter()
            .rev()
            .filter(terminal_said)
            .find_map(|stored| stored.message.harness.as_deref())
            .map_or("live terminal", harness_label);
        parts.push((
            fit(
                format!("From the {label} conversation, newest first:"),
                HANDOFF_TITLE_BYTES_MAX,
            ),
            terminal,
        ));
    }
    let engine = turn_entries(messages);
    if !engine.is_empty() {
        parts.push((
            "From CrazyAgent's master conversation, newest first:".to_owned(),
            engine,
        ));
    }
    let header = if parts.is_empty() {
        format!(
            "The owner switched this conversation's master from {from} to you. Its earlier turns could not be read."
        )
    } else {
        format!(
            "The owner switched this conversation's master from {from} to you. Its earlier turns, newest first:"
        )
    };
    let mut text = fit(header, HANDOFF_BYTES_MAX / 4);
    let mut left = HANDOFF_BYTES_MAX - text.len() - 2 * parts.len();
    for (index, (title, entries)) in parts.iter().enumerate() {
        let share = match parts.get(index + 1) {
            Some((next_title, next)) => {
                (left / 2).max(left.saturating_sub(part_bytes(next_title, next)))
            }
            None => left,
        };
        let part = handoff_part(title, entries, share);
        left -= part.len();
        text.push_str("\n\n");
        text.push_str(&part);
    }
    assert!(text.len() <= HANDOFF_BYTES_MAX);
    if let Some(state) = session_state(connection, project, session)? {
        text.push_str("\n\n");
        text.push_str(&state);
    }
    assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    Ok(text)
}

/// The room a handoff part needs to hand every entry over whole, its omitted line included.
/// Takes the part's title and entries. Returns the size.
fn part_bytes(title: &str, entries: &[String]) -> usize {
    let room = title.len() + 2 + HANDOFF_OMITTED.len();
    entries.iter().fold(room, |bytes, entry| {
        bytes + 2 + entry.len().min(HANDOFF_TURN_BYTES_MAX)
    })
}

/// One part of a switch handoff within `room` bytes: its title, then its entries newest
/// first, each cut to `HANDOFF_TURN_BYTES_MAX` so the newest always fits, the one crossing
/// the room cut to what is left, every cut ending in "...", and `HANDOFF_OMITTED` when any
/// were cut or left out. Takes the title, entries oldest first and the room. Returns it.
fn handoff_part(title: &str, entries: &[String], room: usize) -> String {
    let budget = room - HANDOFF_OMITTED.len() - 2;
    let mut text = title.to_owned();
    let mut given = 0;
    let mut cut = false;
    for entry in entries.iter().rev() {
        let entry = marked(entry, HANDOFF_TURN_BYTES_MAX);
        let left = budget.saturating_sub(text.len() + 2);
        let whole = entry.len() <= left;
        if !whole && left < HANDOFF_TURN_MIN_BYTES {
            break;
        }
        text.push_str("\n\n");
        text.push_str(&marked(&entry, left));
        given += 1;
        if !whole {
            cut = true;
            break;
        }
    }
    assert!(given > 0, "the newest turn of a part is always handed over");
    if cut || given < entries.len() {
        text.push_str("\n\n");
        text.push_str(HANDOFF_OMITTED);
    }
    assert!(text.len() <= room);
    text
}

/// Reads what was said in a session's live terminal as one entry per turn, oldest first:
/// what was typed to the master with the replies that followed it. Takes the stored
/// messages. Returns the entries of turns with text.
fn live_turns(messages: &[Stored]) -> Vec<String> {
    let mut turns: Vec<(Option<String>, Vec<String>)> = Vec::new();
    for stored in messages.iter().filter(terminal_said) {
        let said = stored
            .message
            .text
            .as_deref()
            .and_then(|text| flat(text, 600));
        match (stored.message.speaker, said, turns.last_mut()) {
            (MessageSpeaker::Master, Some(said), Some((_, replies))) => replies.push(said),
            (MessageSpeaker::Master, Some(said), None) => turns.push((None, vec![said])),
            (MessageSpeaker::Master, None, _) => {}
            (_, said, _) => turns.push((said, Vec::new())),
        }
    }
    turns
        .iter()
        .filter(|(typed, replies)| typed.is_some() || !replies.is_empty())
        .map(|(typed, replies)| bounded_turn(typed.as_deref(), replies))
        .collect()
}

/// One handed-over turn within `HANDOFF_TURN_BYTES_MAX`: what was typed, then its last
/// replies that fit, with `TURN_OMITTED` where earlier replies were left out and the last
/// reply cut with "..." when even it does not fit. Takes the typed input, if any, and the
/// replies oldest first. Returns the turn's lines.
fn bounded_turn(input: Option<&str>, replies: &[String]) -> String {
    let head = input.map(|input| {
        marked(
            &format!("Typed to you: {input}"),
            HANDOFF_TURN_BYTES_MAX / 2,
        )
    });
    let room = HANDOFF_TURN_BYTES_MAX
        - head.as_ref().map_or(0, |head| head.len() + 1)
        - TURN_OMITTED.len()
        - 1;
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0;
    for reply in replies.iter().rev() {
        let line = format!("You: {reply}");
        if used + line.len() + 1 > room {
            if kept.is_empty() {
                kept.push(marked(&line, room - 1));
            }
            break;
        }
        used += line.len() + 1;
        kept.push(line);
    }
    let mut lines: Vec<String> = head.into_iter().collect();
    if kept.len() < replies.len() {
        lines.push(TURN_OMITTED.to_owned());
    }
    lines.extend(kept.into_iter().rev());
    let text = lines.join("\n");
    assert!(text.len() <= HANDOFF_TURN_BYTES_MAX);
    text
}

/// Tells whether a team agent is a stopped master of the project with its worktree still
/// recorded, so it can start again in the home that keeps its harness conversations.
/// Takes the connection, project and agent. Returns the answer.
fn stopped_master(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<bool, EngineError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_agents a JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.id=?1 AND a.project=?2 AND a.role='master' AND a.status IN ('stopped','failed'))",
            params![agent, project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)
}

/// The first line of a text with visible words, made one line by `flat`.
/// Takes the text and bound. Returns None when no line has visible words.
pub fn clip(text: &str, max: usize) -> Option<String> {
    text.lines().find_map(|line| flat(line, max))
}

/// A whole text on one line, every control character and run of white space made one
/// space, so no terminal escape reaches a screen, cut to at most `max` characters with
/// "..." marking a cut. Takes the text and bound. Returns None when nothing visible is left.
pub fn flat(text: &str, max: usize) -> Option<String> {
    assert!(max > 3);
    let visible: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let joined = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    if joined.chars().count() <= max {
        return Some(joined);
    }
    let mut kept: String = joined.chars().take(max - 3).collect();
    kept.truncate(kept.trim_end().len());
    kept.push_str("...");
    assert!(kept.chars().count() <= max);
    assert!(!kept.chars().any(char::is_control));
    Some(kept)
}

/// Cuts a text to at most `max` bytes on a character boundary, ending a cut with "..." as
/// `flat` marks one, so a handed-over turn never loses its end silently. Takes the text and
/// the bound, over 3 when the text must be cut. Returns the text, whole when it fits.
fn marked(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    assert!(max > 3);
    let mut kept = fit(text.to_owned(), max - 3);
    kept.push_str("...");
    assert!(kept.len() <= max);
    kept
}

/// Cuts a text to at most `max` bytes on a character boundary. Takes the text and bound.
/// Returns the text, whole when it fits.
fn fit(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    assert!(text.len() <= max);
    text
}

/// The master that ran a session's newest stored message, or took its conversation over
/// last. Takes the connection and session. Returns its mode and harness, or None for a
/// session with no stored message; `session_message_record` for a mode no writer stores.
fn newest_runner(
    connection: &Connection,
    session: &SessionId,
) -> Result<Option<(MasterMode, Option<String>)>, EngineError> {
    if !table_exists(connection, "session_messages")? {
        return Ok(None);
    }
    let row: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT mode,CASE WHEN length(CAST(harness AS BLOB))<=64 THEN harness END
             FROM session_messages WHERE session=?1 ORDER BY seq DESC LIMIT 1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    row.map(|(mode, harness)| {
        let mode = MasterMode::named(&mode).ok_or_else(|| error("session_message_record"))?;
        Ok((mode, harness))
    })
    .transpose()
}

/// Checks the bounds of a conversation page read. Takes the `before` cursor and the
/// limit. Returns `master_messages_page_limit` or `master_messages_cursor_bounds`.
pub(crate) fn validate_messages_page(before: Option<u64>, limit: u8) -> Result<(), EngineError> {
    if !(1..=MESSAGES_PAGE_MAX).contains(&limit) {
        return Err(error("master_messages_page_limit"));
    }
    if before.is_some_and(|before| before == 0 || before > i64::MAX.unsigned_abs()) {
        return Err(error("master_messages_cursor_bounds"));
    }
    Ok(())
}

/// Reads one page of a session's conversation from the store: the newest messages before
/// `before`, or the session's newest when it is absent, at most `limit` and as many as one
/// reply holds, oldest first. A lone message too large for a reply has its text cut. Takes
/// the connection, project, session, cursor and limit. Returns the page; a database
/// without the store gives an empty one.
pub(crate) fn page(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    before: Option<u64>,
    limit: u8,
) -> Result<SessionMessagePage, EngineError> {
    validate_messages_page(before, limit)?;
    let mut page = SessionMessagePage {
        project_id: project.clone(),
        session_id: session.clone(),
        messages: Vec::new(),
        next: None,
        count: 0,
    };
    if !table_exists(connection, "session_messages")? {
        return Ok(page);
    }
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM session_messages WHERE session=?1 AND project=?2",
            params![session.as_str(), project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    page.count = u64::try_from(count).map_err(error)?;
    let mut older = false;
    for (index, message) in page_rows(connection, project, session, before, limit)?
        .into_iter()
        .enumerate()
    {
        if index == usize::from(limit) {
            older = true;
            break;
        }
        let previous = page.next.replace(message.seq);
        page.messages.push(message);
        if page_fits(&page)? {
            continue;
        }
        if page.messages.len() > 1 {
            page.messages.pop();
            page.next = previous;
            older = true;
            break;
        }
        cut_to_fit(&mut page)?;
    }
    if !older {
        page.next = None;
    }
    page.messages.reverse();
    assert!(page.messages.len() <= usize::from(limit));
    assert!(page_fits(&page)?);
    Ok(page)
}

/// Reads the stored messages a page is built from, newest first, one more than `limit` so
/// the page knows whether older ones exist. Takes the connection, project, session, cursor
/// and limit. Returns them, or `session_message_record` for a row no writer stores.
fn page_rows(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    before: Option<u64>,
    limit: u8,
) -> Result<Vec<SessionMessage>, EngineError> {
    let before = i64::try_from(before.unwrap_or(i64::MAX.unsigned_abs())).map_err(error)?;
    let mut statement = connection
        .prepare(
            "SELECT seq,speaker,mode,
                    CASE WHEN length(CAST(harness AS BLOB))<=64 THEN harness END,
                    CASE WHEN length(CAST(text AS BLOB))<=8192 THEN text END,at_ms
             FROM session_messages WHERE session=?1 AND project=?2 AND seq<?3
             ORDER BY seq DESC LIMIT ?4",
        )
        .map_err(error)?;
    type Row = (i64, String, String, Option<String>, Option<String>, i64);
    let rows = statement
        .query_map(
            params![
                session.as_str(),
                project.as_str(),
                before,
                i64::from(limit) + 1
            ],
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
        .collect::<Result<Vec<Row>, _>>()
        .map_err(error)?;
    assert!(rows.len() <= usize::from(limit) + 1);
    let record = || error("session_message_record");
    rows.into_iter()
        .map(|(seq, speaker, mode, harness, text, at_ms)| {
            Ok(SessionMessage {
                seq: u64::try_from(seq).map_err(error)?,
                speaker: MessageSpeaker::named(&speaker).ok_or_else(record)?,
                mode: MasterMode::named(&mode).ok_or_else(record)?,
                harness,
                text,
                at_ms: u64::try_from(at_ms).map_err(error)?,
            })
        })
        .collect()
}

/// Tells whether a conversation page serializes inside its byte bound. Takes the page.
/// Returns the answer, or the serializer's failure.
fn page_fits(page: &SessionMessagePage) -> Result<bool, EngineError> {
    Ok(serde_json::to_vec(page).map_err(error)?.len() <= MESSAGES_PAGE_BYTES_MAX)
}

/// Halves the text of a page's only message, marked as cut, until the page fits one
/// reply; text made of characters that serialize long can need it. Takes the page. Returns
/// the serializer's failure.
fn cut_to_fit(page: &mut SessionMessagePage) -> Result<(), EngineError> {
    assert!(page.messages.len() == 1);
    for _ in 0..16 {
        if page_fits(page)? {
            return Ok(());
        }
        let message = &mut page.messages[0];
        let text = message.text.take().unwrap_or_default();
        let keep = text.len() / 2;
        message.text = (keep > 3).then(|| marked(&text, keep));
    }
    assert!(page_fits(page)?);
    Ok(())
}

/// The master about to run a session's next turn: its mode, and the catalog id of its
/// harness when live or its backend name when one-turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Runner<'a> {
    pub mode: MasterMode,
    pub harness: &'a str,
}

/// A stored message with the kind of writer it came from and that writer's key for it.
struct Stored {
    message: SessionMessage,
    source: String,
    key: String,
}

/// A message about to be stored.
struct Draft<'a> {
    speaker: MessageSpeaker,
    mode: MasterMode,
    harness: Option<&'a str>,
    text: Option<&'a str>,
    source: &'a str,
    key: &'a str,
}

/// Appends one message to a session's conversation, after its newest row. A message whose
/// source and key the session already holds is left as it is, so every writer may repeat
/// itself. A database a bare fixture made gains the table on first use. Takes the
/// connection, project, session and message. Returns whether a row was added; a session,
/// project or key outside the stored bounds adds none.
fn append(
    connection: &Connection,
    project: &str,
    session: &str,
    draft: &Draft<'_>,
) -> Result<bool, EngineError> {
    let bounded = |text: &str, max: usize| !text.is_empty() && text.len() <= max;
    if !bounded(session, 128) || !bounded(project, 128) || !bounded(draft.key, 192) {
        return Ok(false);
    }
    connection.execute_batch(MESSAGES).map_err(error)?;
    let text = draft
        .text
        .filter(|text| !text.is_empty())
        .map(|text| marked(text, MESSAGE_BYTES_MAX));
    let harness = draft.harness.filter(|harness| bounded(*harness, 64));
    let added = connection
        .execute(
            "INSERT INTO session_messages(session,seq,project,speaker,mode,harness,text,at_ms,source,source_key)
             SELECT ?1,COALESCE(MAX(seq),0)+1,?2,?3,?4,?5,?6,?7,?8,?9 FROM session_messages WHERE session=?1
             ON CONFLICT(session,source,source_key) DO NOTHING",
            params![
                session,
                project,
                draft.speaker.name(),
                draft.mode.name(),
                harness,
                text,
                crate::team::now_ms().max(0),
                draft.source,
                draft.key
            ],
        )
        .map_err(error)?;
    assert!(added <= 1);
    Ok(added == 1)
}

/// Reads a session's newest stored messages, oldest first, at most `MESSAGES_READ_MAX`.
/// It writes nothing, so a read-only connection may call it. Takes the connection and
/// session. Returns them, none for a database without the store, or
/// `session_message_record` for a row no writer of this engine makes.
fn stored(connection: &Connection, session: &str) -> Result<Vec<Stored>, EngineError> {
    if !table_exists(connection, "session_messages")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT seq,speaker,mode,harness,text,at_ms,source,source_key FROM (
                SELECT seq,speaker,mode,
                       CASE WHEN length(CAST(harness AS BLOB))<=64 THEN harness END AS harness,
                       CASE WHEN length(CAST(text AS BLOB))<=8192 THEN text END AS text,
                       at_ms,source,source_key
                FROM session_messages WHERE session=?1 ORDER BY seq DESC LIMIT ?2
             ) ORDER BY seq",
        )
        .map_err(error)?;
    type Row = (
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        String,
        String,
    );
    let rows = statement
        .query_map(
            params![session, i64::try_from(MESSAGES_READ_MAX).map_err(error)?],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<Result<Vec<Row>, _>>()
        .map_err(error)?;
    let mut messages = Vec::with_capacity(rows.len());
    for (seq, speaker, mode, harness, text, at_ms, source, key) in rows {
        let record = || error("session_message_record");
        messages.push(Stored {
            message: SessionMessage {
                seq: u64::try_from(seq).map_err(error)?,
                speaker: MessageSpeaker::named(&speaker).ok_or_else(record)?,
                mode: MasterMode::named(&mode).ok_or_else(record)?,
                harness,
                text,
                at_ms: u64::try_from(at_ms).map_err(error)?,
            },
            source,
            key,
        });
    }
    assert!(messages.len() <= MESSAGES_READ_MAX);
    assert!(
        messages
            .windows(2)
            .all(|pair| pair[0].message.seq < pair[1].message.seq)
    );
    Ok(messages)
}

/// Tells whether a message is part of the conversation itself, not a divider.
fn said(stored: &Stored) -> bool {
    stored.message.speaker != MessageSpeaker::Switch
}

/// Tells whether a typed message is one the engine typed to carry a conversation over.
fn seeded(text: &str) -> bool {
    SEED_MARKS.iter().any(|mark| text.contains(mark))
}

/// The name of the master a row belongs to, as a handoff and a divider say it: a live
/// master's harness as stored, or the one-turn master with its backend. Takes the mode and
/// the stored harness. Returns the name; `harness_label` leaves a one-turn name as it is.
fn ran_on(mode: MasterMode, harness: Option<&str>) -> String {
    match (mode, harness) {
        (MasterMode::Live, Some(harness)) => harness.to_owned(),
        (MasterMode::Live, None) => "the live terminal".to_owned(),
        (MasterMode::OneTurn, Some("rcli")) => "the one-turn master (RCLI)".to_owned(),
        (MasterMode::OneTurn, Some("codex")) => "the one-turn master (Codex)".to_owned(),
        (MasterMode::OneTurn, Some("claude")) => "the one-turn master (Claude)".to_owned(),
        (MasterMode::OneTurn, _) => "the one-turn master".to_owned(),
    }
}

/// Decides from a session's stored messages whether the master about to run its next turn
/// takes the conversation over from another one, whatever harness or mode either is. The
/// newest divider a switch wrote settles what came before it; a divider the backfill
/// placed settles nothing. Messages after the settled point that another master ran are
/// owed to `next`, and a settled switch to another master is a switch away from it even
/// when it said nothing since.
/// Takes the messages, oldest first, and the next master. Returns the name of the master
/// the conversation comes from, as `ran_on` gives it, or None when `next` continues its own.
fn owed(messages: &[Stored], next: Runner<'_>) -> Option<String> {
    let foreign = |stored: &Stored| {
        stored.message.mode != next.mode
            || stored
                .message
                .harness
                .as_deref()
                .is_some_and(|harness| !crate::harnesses::same(harness, next.harness))
    };
    let from = |stored: &Stored| ran_on(stored.message.mode, stored.message.harness.as_deref());
    let settled = messages
        .iter()
        .rposition(|stored| !said(stored) && stored.source != BACKFILL);
    let open = &messages[settled.map_or(0, |index| index + 1)..];
    if let Some(newest) = open
        .iter()
        .rev()
        .find(|stored| said(stored) && foreign(stored))
    {
        return Some(from(newest));
    }
    let switch = &messages[settled?];
    assert!(!said(switch));
    foreign(switch).then(|| from(switch))
}

/// What an engine-run turn asked its master: the owner's message, absent once it is no
/// longer kept, or the line of a request the engine made itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked<'a> {
    Owner(Option<&'a str>),
    Engine(&'a str),
}

/// Stores what an engine-run turn asked. Takes the connection, project, session, backend
/// name, turn id and what was asked. Returns whether the row is new.
fn append_asked(
    connection: &Connection,
    project: &str,
    session: &str,
    backend: Option<&str>,
    turn: &str,
    asked: &Asked<'_>,
) -> Result<bool, EngineError> {
    let (speaker, text) = match asked {
        Asked::Owner(text) => (MessageSpeaker::Owner, *text),
        Asked::Engine(line) => (MessageSpeaker::Engine, Some(*line)),
    };
    append(
        connection,
        project,
        session,
        &Draft {
            speaker,
            mode: MasterMode::OneTurn,
            harness: backend,
            text,
            source: TURN,
            key: turn,
        },
    )
}

/// Stores the reply of an engine-run turn. Takes the connection, project, session, backend
/// name, turn id and reply. Returns whether the row is new.
fn append_reply(
    connection: &Connection,
    project: &str,
    session: &str,
    backend: Option<&str>,
    turn: &str,
    reply: &str,
) -> Result<bool, EngineError> {
    append(
        connection,
        project,
        session,
        &Draft {
            speaker: MessageSpeaker::Master,
            mode: MasterMode::OneTurn,
            harness: backend,
            text: Some(reply),
            source: TURN,
            key: &format!("{turn}/reply"),
        },
    )
}

/// Stores the divider where a master took a session's conversation over, which settles
/// what came before it: handed over under Carry over, or left behind as Start fresh asks.
/// Takes the connection, project, session, the mode and harness of the master that took
/// over and the owner's setting. Returns any SQLite failure.
pub(crate) fn note_switch(
    connection: &Connection,
    project: &str,
    session: &str,
    mode: MasterMode,
    harness: &str,
    carried: SwitchMode,
) -> Result<(), EngineError> {
    connection.execute_batch(MESSAGES).map_err(error)?;
    let next: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM session_messages WHERE session=?1",
            [session],
            |row| row.get(0),
        )
        .map_err(error)?;
    assert!(next > 0);
    let to = ran_on(mode, Some(harness));
    append(
        connection,
        project,
        session,
        &Draft {
            speaker: MessageSpeaker::Switch,
            mode,
            harness: Some(harness),
            text: Some(&switched_note(harness_label(&to), carried)),
            source: SWITCH,
            key: &format!("{}:{next}", carried.name()),
        },
    )?;
    Ok(())
}

/// The stored name of the backend a one-turn master runs on. Takes the backend. Returns
/// the name a session row keeps for it.
fn backend_name(backend: MasterBackend) -> &'static str {
    match backend {
        MasterBackend::Rcli => "rcli",
        MasterBackend::Codex => "codex",
        MasterBackend::Claude => "claude",
    }
}

/// The owner's On harness switch setting for a project, Carry over where a database has no
/// such setting. Takes the connection and project. Returns the mode.
fn switch_setting(connection: &Connection, project: &ProjectId) -> Result<SwitchMode, EngineError> {
    if !table_exists(connection, "team_master_switch")? {
        return Ok(SwitchMode::default());
    }
    crate::team::on_harness_switch(connection, project)
}

/// Stores what a turn the one-turn master is about to run asked, in the session's one
/// conversation. When the conversation so far belongs to another master, a live terminal
/// or another backend, the divider of that switch is stored first under the owner's On
/// harness switch setting. Takes the connection, project, session, the master's backend,
/// the turn id and what was asked. Returns any SQLite failure.
pub(crate) fn one_turn_asked(
    connection: &Connection,
    project: &ProjectId,
    session: &SessionId,
    backend: MasterBackend,
    turn: &str,
    asked: &Asked<'_>,
) -> Result<(), EngineError> {
    let backend = backend_name(backend);
    let next = Runner {
        mode: MasterMode::OneTurn,
        harness: backend,
    };
    let (project_name, session_name) = (project.as_str(), session.as_str());
    if owed(&stored(connection, session_name)?, next).is_some() {
        let carried = switch_setting(connection, project)?;
        note_switch(
            connection,
            project_name,
            session_name,
            MasterMode::OneTurn,
            backend,
            carried,
        )?;
    }
    append_asked(
        connection,
        project_name,
        session_name,
        Some(backend),
        turn,
        asked,
    )?;
    Ok(())
}

/// Stores the reply of a turn the one-turn master ran, after what the turn asked. A turn
/// whose asked row the store does not hold, queued before the store existed, has all the
/// session's engine-run turns moved in instead, this one with its reply. Takes the
/// connection, session, turn id and reply. Returns any SQLite failure.
pub(crate) fn one_turn_reply(
    connection: &Connection,
    session: &SessionId,
    turn: &str,
    reply: &str,
) -> Result<(), EngineError> {
    connection.execute_batch(MESSAGES).map_err(error)?;
    let asked: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT project,harness FROM session_messages
             WHERE session=?1 AND source='turn' AND source_key=?2",
            params![session.as_str(), turn],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    if let Some((project, backend)) = asked {
        append_reply(
            connection,
            &project,
            session.as_str(),
            backend.as_deref(),
            turn,
            reply,
        )?;
        return Ok(());
    }
    if !table_exists(connection, "master_sessions")? {
        return Ok(());
    }
    let project: Option<String> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(project AS BLOB))<=128 THEN project END
             FROM master_sessions WHERE id=?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?
        .flatten();
    if let Some(project) = project {
        absorb_turns(connection, &project, session.as_str())?;
    }
    Ok(())
}

/// One exchange of a session's live terminal as the one-turn master's history carries it:
/// its lines, and the id of the engine-run turn it came after, absent when it came before
/// every engine-run turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveExchange {
    pub after_turn: Option<String>,
    pub entry: String,
}

/// A live terminal exchange being read from the store: the turn it follows, its harness,
/// what was typed and the newest reply.
struct Exchange {
    after_turn: Option<String>,
    harness: Option<String>,
    typed: Option<String>,
    reply: Option<String>,
}

impl Exchange {
    /// The exchange as history lines, each on one line and clipped. Takes the exchange.
    /// Returns None for one with neither typed words nor a reply kept.
    fn entry(self) -> Option<LiveExchange> {
        let label = self.harness.as_deref().map_or("live", harness_label);
        let typed = self.typed.as_deref().and_then(|text| flat(text, 600));
        let reply = self.reply.as_deref().and_then(|text| flat(text, 600));
        if typed.is_none() && reply.is_none() {
            return None;
        }
        let typed = typed.unwrap_or_else(|| "(The typed words were not kept.)".to_owned());
        let entry = match reply {
            Some(reply) => format!(
                "Owner, in the {label} terminal: {typed}
You, in the {label} terminal: {reply}"
            ),
            None => format!(
                "Owner, in the {label} terminal: {typed}
(No reply was kept.)"
            ),
        };
        Some(LiveExchange {
            after_turn: self.after_turn,
            entry,
        })
    }
}

/// Reads what was said in a session's live terminal for the one-turn master's history,
/// oldest first, each exchange with the engine-run turn it followed. Exchanges before a
/// switch the owner set to Start fresh are left out, as that setting asks. Takes the connection and session. Returns the exchanges.
pub(crate) fn live_exchanges(
    connection: &Connection,
    session: &SessionId,
) -> Result<Vec<LiveExchange>, EngineError> {
    let mut exchanges = Vec::new();
    let mut open: Option<Exchange> = None;
    let mut after_turn: Option<String> = None;
    for stored in stored(connection, session.as_str())? {
        let message = stored.message;
        if message.speaker == MessageSpeaker::Master && message.mode == MasterMode::Live {
            let exchange = open.get_or_insert_with(|| Exchange {
                after_turn: after_turn.clone(),
                harness: message.harness.clone(),
                typed: None,
                reply: None,
            });
            if message.text.is_some() {
                exchange.reply = message.text;
            }
            continue;
        }
        exchanges.extend(open.take().and_then(Exchange::entry));
        match (message.speaker, message.mode) {
            (MessageSpeaker::Switch, _) => {
                if stored.source == SWITCH && stored.key.starts_with("fresh:") {
                    exchanges.clear();
                }
            }
            (_, MasterMode::OneTurn) => {
                let turn = stored.key.strip_suffix("/reply").unwrap_or(&stored.key);
                after_turn = Some(turn.to_owned());
            }
            (_, MasterMode::Live) => {
                open = Some(Exchange {
                    after_turn: after_turn.clone(),
                    harness: message.harness,
                    typed: message.text,
                    reply: None,
                });
            }
        }
    }
    exchanges.extend(open.take().and_then(Exchange::entry));
    assert!(exchanges.len() <= MESSAGES_READ_MAX);
    Ok(exchanges)
}

/// The backend a one-turn master of a session runs on, as its session row names it. Takes
/// the connection and session. Returns the name, or None without a session row.
fn session_backend(connection: &Connection, session: &str) -> Result<Option<String>, EngineError> {
    if !table_exists(connection, "master_sessions")? {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT CASE WHEN length(CAST(body AS BLOB))<=16384 AND json_valid(body)
                         THEN json_extract(body,'$.selection.backend') END
             FROM master_sessions WHERE id=?1",
            [session],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|backend| backend.flatten().filter(|backend| backend.len() <= 64))
        .map_err(error)
}

/// What an older engine kept of a session's live terminal conversation: the harness and
/// the messages of the record it kept, the terminal turns it only counted with the newest
/// line it kept of them, and the switch it recorded.
struct OldLive {
    harness: Option<String>,
    spoken: Vec<(MessageSpeaker, String)>,
    counted: u64,
    line: Option<String>,
    switch: Option<(String, SwitchMode)>,
}

/// Reads what an older engine kept of a session's live terminal conversation. Messages the
/// engine typed to carry a conversation over are left out. Takes the connection, project
/// and session. Returns it; a record that cannot be read gives no messages.
fn old_live(connection: &Connection, project: &str, session: &str) -> Result<OldLive, EngineError> {
    use crate::harnesses::transcript::{Speaker, conversation_messages};
    let kept = if table_exists(connection, "team_master_conversations")? {
        let project = ProjectId::new(project).map_err(error)?;
        crate::team::master::session_conversation(connection, &project, session)?
    } else {
        None
    };
    let spoken = kept
        .as_ref()
        .filter(|kept| kept.resume.id != crate::team::master::REFUSED_CONVERSATION)
        .map(|kept| conversation_messages(&kept.harness, &kept.resume.record))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(speaker, text)| match speaker {
            Speaker::Input if seeded(&text) => None,
            Speaker::Input => Some((MessageSpeaker::Owner, text)),
            Speaker::Agent => Some((MessageSpeaker::Master, text)),
        })
        .collect();
    let (counted, line) = counted_turns(connection, session)?;
    Ok(OldLive {
        harness: kept.map(|kept| kept.harness),
        spoken,
        counted,
        line,
        switch: recorded_switch(connection, project, session)?,
    })
}

/// The terminal turns an older engine counted for a session and the newest line it kept
/// of them. Takes the connection and session. Returns 0 and None without such a row.
fn counted_turns(
    connection: &Connection,
    session: &str,
) -> Result<(u64, Option<String>), EngineError> {
    if !table_exists(connection, "session_owner_turns")? {
        return Ok((0, None));
    }
    let row: Option<(i64, Option<String>)> = connection
        .query_row(
            "SELECT turns,CASE WHEN length(CAST(last_line AS BLOB))<=1024 THEN last_line END
             FROM session_owner_turns WHERE session=?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let (turns, line) = row.unwrap_or((0, None));
    Ok((u64::try_from(turns).map_err(error)?, line))
}

/// The switch an older engine recorded for a session: the harness that took over and the
/// owner's mode. Takes the connection, project and session. Returns None without one.
fn recorded_switch(
    connection: &Connection,
    project: &str,
    session: &str,
) -> Result<Option<(String, SwitchMode)>, EngineError> {
    if !table_exists(connection, "team_session_switches")? {
        return Ok(None);
    }
    let row: Option<(Option<String>, String)> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(harness AS BLOB)) BETWEEN 1 AND 64 THEN harness END,mode
             FROM team_session_switches WHERE session=?1 AND project=?2",
            params![session, project],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    Ok(row.and_then(|(harness, mode)| Some((harness?, SwitchMode::named(&mode)?))))
}

/// Moves what older engines kept of one session into the store, each kind once: its
/// engine-run turns when the store holds none, then its terminal conversation when the
/// store holds none. Where a terminal conversation follows engine-run turns the store gets
/// a divider, and a recorded switch no divider stands for is stored after the
/// conversation. Old tables are only read. Takes the connection, project and session.
/// Returns any SQLite failure.
fn absorb(connection: &Connection, project: &str, session: &str) -> Result<(), EngineError> {
    let before = stored(connection, session)?;
    let holds = |mode: MasterMode| {
        before
            .iter()
            .any(|stored| said(stored) && stored.message.mode == mode)
    };
    let divided = before.iter().any(|stored| !said(stored));
    let turns = holds(MasterMode::OneTurn) || absorb_turns(connection, project, session)?;
    if holds(MasterMode::Live) {
        return Ok(());
    }
    let old = old_live(connection, project, session)?;
    let live = !old.spoken.is_empty() || old.counted > 0;
    let mut placed = divided;
    if live && turns && !divided {
        placed = absorb_boundary(connection, project, session, &old)?;
    }
    absorb_live(connection, project, session, &old)?;
    if let Some((to, mode)) = old.switch.filter(|_| !placed && (turns || live)) {
        note_switch(connection, project, session, MasterMode::Live, &to, mode)?;
    }
    Ok(())
}

/// Moves a session's engine-run turns into the store, oldest first: what was asked, the
/// owner's message or the engine's line, then the reply when one came. A database without
/// the owner conversation still gets the turns, without the owner's words. Takes the
/// connection, project and session. Returns whether the session has such turns.
fn absorb_turns(
    connection: &Connection,
    project: &str,
    session: &str,
) -> Result<bool, EngineError> {
    if !table_exists(connection, "master_turns")? {
        return Ok(false);
    }
    let backend = session_backend(connection, session)?;
    let (owner, joined) = if table_exists(connection, "conversation_entries")? {
        (
            "CASE WHEN length(CAST(c.text AS BLOB))<=8192 THEN c.text END",
            "LEFT JOIN conversation_entries c
                  ON c.event_cursor=t.input AND c.kind='message' AND c.project=?2",
        )
    } else {
        ("CASE WHEN ?2 IS NULL THEN NULL END", "")
    };
    let mut statement = connection
        .prepare(&format!(
            "SELECT CASE WHEN length(CAST(t.id AS BLOB))<=128 THEN t.id END,{owner},
                    CASE WHEN length(CAST(t.body AS BLOB))<=16384 AND json_valid(t.body) THEN json_extract(t.body,'$.engine_request.line') END,
                    CASE WHEN length(CAST(t.body AS BLOB))<=16384 AND json_valid(t.body) THEN json_extract(t.body,'$.reply.text') END
             FROM master_turns t {joined}
             WHERE t.session=?1 ORDER BY t.ordinal LIMIT 1024"
        ))
        .map_err(error)?;
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let turns = statement
        .query_map(params![session, project], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<Row>, _>>()
        .map_err(error)?;
    assert!(turns.len() <= 1024);
    for (id, owner, line, reply) in &turns {
        let Some(id) = id else {
            continue;
        };
        let asked = match line {
            Some(line) => Asked::Engine(line),
            None => Asked::Owner(owner.as_deref()),
        };
        append_asked(connection, project, session, backend.as_deref(), id, &asked)?;
        if let Some(reply) = reply {
            append_reply(connection, project, session, backend.as_deref(), id, reply)?;
        }
    }
    Ok(!turns.is_empty())
}

/// Stores the divider between a session's engine-run turns and the terminal conversation
/// an older engine kept after them: the session's recorded switch when it names that
/// conversation's harness, else a divider that settles nothing, so the next live start
/// still hands the earlier turns over. Takes the connection, project, session and what was
/// kept. Returns whether the recorded switch was stored.
fn absorb_boundary(
    connection: &Connection,
    project: &str,
    session: &str,
    old: &OldLive,
) -> Result<bool, EngineError> {
    let harness = old.harness.as_deref();
    let settled = old
        .switch
        .as_ref()
        .filter(|(to, _)| harness.is_none_or(|harness| crate::harnesses::same(to, harness)));
    if let Some((to, mode)) = settled {
        note_switch(connection, project, session, MasterMode::Live, to, *mode)?;
        return Ok(true);
    }
    let label = harness.map_or("the live terminal", harness_label);
    let added = append(
        connection,
        project,
        session,
        &Draft {
            speaker: MessageSpeaker::Switch,
            mode: MasterMode::Live,
            harness,
            text: Some(&format!(
                "Switched to {label}. The earlier turns were not handed over to it."
            )),
            source: BACKFILL,
            key: "boundary",
        },
    )?;
    assert!(added);
    Ok(false)
}

/// Stores the terminal conversation an older engine kept: a row without text for each
/// counted turn the record no longer shows, the newest kept line when no record reads,
/// then the record's messages. Takes the connection, project, session and what was kept.
/// Returns any SQLite failure.
fn absorb_live(
    connection: &Connection,
    project: &str,
    session: &str,
    old: &OldLive,
) -> Result<(), EngineError> {
    let harness = old.harness.as_deref();
    let live = |speaker: MessageSpeaker, text: Option<&str>, source: &str, key: &str| {
        append(
            connection,
            project,
            session,
            &Draft {
                speaker,
                mode: MasterMode::Live,
                harness,
                text,
                source,
                key,
            },
        )
    };
    let typed = old
        .spoken
        .iter()
        .filter(|(speaker, _)| *speaker == MessageSpeaker::Owner)
        .count();
    let unread = old
        .counted
        .saturating_sub(u64::try_from(typed).map_err(error)?)
        .min(COUNTED_TURNS_MAX);
    for turn in 1..=unread {
        live(MessageSpeaker::Owner, None, COUNTER, &turn.to_string())?;
    }
    if old.spoken.is_empty() && old.counted > 0 && old.line.is_some() {
        live(MessageSpeaker::Master, old.line.as_deref(), COUNTER, "line")?;
    }
    for (index, (speaker, text)) in old.spoken.iter().enumerate() {
        live(*speaker, Some(text.as_str()), RECORD, &index.to_string())?;
    }
    Ok(())
}

/// Moves every session older engines wrote into the store when the engine database opens,
/// each all or nothing. A session the store already holds rows of is skipped, so the work
/// is done once; one with nothing to move is looked at again at the next start, which
/// costs a few reads. A session that cannot be moved is left for the next start and named
/// on standard error, so one bad row never stops the engine. Takes the engine connection.
/// Returns a failure to list the sessions.
#[cfg(unix)]
fn backfill(connection: &Connection) -> Result<(), EngineError> {
    if !table_exists(connection, "master_sessions")? {
        return Ok(());
    }
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN length(CAST(s.id AS BLOB))<=128 THEN s.id END,
                    CASE WHEN length(CAST(s.project AS BLOB))<=128 THEN s.project END
             FROM master_sessions s
             WHERE NOT EXISTS(SELECT 1 FROM session_messages m WHERE m.session=s.id)
             ORDER BY s.created LIMIT ?1",
        )
        .map_err(error)?;
    type Row = (Option<String>, Option<String>);
    let sessions = statement
        .query_map(
            [i64::try_from(BACKFILL_SESSIONS_MAX).map_err(error)?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?
        .collect::<Result<Vec<Row>, _>>()
        .map_err(error)?;
    assert!(sessions.len() <= BACKFILL_SESSIONS_MAX);
    for (session, project) in sessions {
        let (Some(session), Some(project)) = (session, project) else {
            continue;
        };
        if ProjectId::new(project.as_str()).is_err() {
            continue;
        }
        let moved = owner_turns_change(connection, |connection| {
            absorb(connection, &project, &session)
        });
        if let Err(cause) = moved {
            eprintln!("session_messages_not_backfilled cause={}", cause.0);
        }
    }
    Ok(())
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    #[cfg(unix)]
    use super::{
        HANDOFF_BYTES_MAX, HANDOFF_OMITTED, HANDOFF_TURN_BYTES_MAX, LOST_NOTE, ResumePlan,
        STATE_BYTES_MAX, TURN_OMITTED, handoff, resume_harness, switched_from,
    };
    use super::{clip, flat};
    #[cfg(unix)]
    use crate::harnesses::terminal_session::Resume;
    #[cfg(unix)]
    use crate::protocol::{ProjectId, SessionId};
    #[cfg(unix)]
    use crate::team::master::{SessionConversation, keep_conversation};
    #[cfg(unix)]
    use std::time::Duration;

    /// A switch handoff gives the new harness the session's earlier turns newest first, as
    /// many as fit its byte bound with a line saying older ones were left out, then the
    /// session's open tasks; and the switch is seen only for another harness, never for an
    /// alias of the one the conversation ran on.
    #[cfg(unix)]
    #[test]
    fn a_switch_handoff_is_newest_first_bounded_and_lists_open_tasks() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-switch").unwrap();
        let id = "0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let mut lines = String::new();
        for turn in 1..=30 {
            let words = format!("turn {turn:02} ").repeat(60);
            lines.push_str(
                &serde_json::json!({"type": "user", "message": {"content": words}}).to_string(),
            );
            lines.push('\n');
            lines.push_str(
                &serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": format!("answer {turn:02}")}], "stop_reason": "end_turn"}})
                    .to_string(),
            );
            lines.push('\n');
        }
        std::fs::write(&record, lines).unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, session.as_str(), &kept).unwrap();
        connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms)
                 VALUES (?1,'owner','codex',0,'Write the release notes','queued',1)",
                [project.as_str()],
            )
            .unwrap();

        assert_eq!(
            switched_from(&connection, &project, &session, "claude").unwrap(),
            None,
            "an alias of the conversation's harness is no switch"
        );
        assert_eq!(
            switched_from(&connection, &project, &session, "codex").unwrap(),
            Some("claude-code".to_owned())
        );

        let text = handoff(&connection, &project, &session, "Claude Code").unwrap();
        assert!(
            text.starts_with(
                "The owner switched this conversation's master from Claude Code to you. Its earlier turns, newest first:"
            ),
            "{text}"
        );
        let newest = text
            .find("answer 30")
            .expect("the newest turn is handed over");
        let older = text
            .find("answer 29")
            .expect("the next newest turn follows");
        assert!(newest < older, "newest first: {text}");
        assert!(!text.contains("answer 01"), "the oldest turn did not fit");
        let turns_end = text.find(HANDOFF_OMITTED).expect("the cut is said");
        assert!(turns_end <= HANDOFF_BYTES_MAX);
        assert!(
            text.contains("Open tasks:\n- Task 1 (codex, queued): Write the release notes"),
            "{text}"
        );
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    /// A tool-heavy newest turn, one typed input followed by many replies that together
    /// exceed the whole handoff, is cut to fit, never dropped: the new harness gets that
    /// turn's input and its last replies, a line saying earlier replies were left out, and
    /// the older turns after it.
    #[cfg(unix)]
    #[test]
    fn a_long_newest_turn_is_cut_to_fit_the_handoff_not_dropped() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-switch").unwrap();
        let id = "1f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let user = |text: &str| {
            serde_json::json!({"type": "user", "message": {"content": text}}).to_string() + "\n"
        };
        let assistant = |text: &str| {
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}], "stop_reason": "end_turn"}})
                .to_string()
                + "\n"
        };
        let mut lines = String::new();
        for turn in 1..=3 {
            lines.push_str(&user(&format!("older {turn:02}")));
            lines.push_str(&assistant(&format!("answer {turn:02}")));
        }
        lines.push_str(&user("implement the export"));
        for reply in 1..=25 {
            lines.push_str(&assistant(&format!("reply {reply:02} ").repeat(44)));
        }
        std::fs::write(&record, lines).unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, session.as_str(), &kept).unwrap();

        let text = handoff(&connection, &project, &session, "Claude Code").unwrap();
        let newest = text
            .find("Typed to you: implement the export")
            .expect("the newest turn's input is handed over");
        let cut = text.find(TURN_OMITTED).expect("the cut replies are said");
        let last = text
            .find("You: reply 25 reply 25")
            .expect("the newest turn's last reply is handed over");
        let older = text
            .find("Typed to you: older 03\nYou: answer 03")
            .expect("the older turns follow");
        assert!(newest < cut && cut < last && last < older, "{text}");
        assert!(
            !text.contains("reply 01"),
            "the earliest replies did not fit"
        );
        assert!(
            text.contains("Typed to you: older 01\nYou: answer 01"),
            "every older turn fits: {text}"
        );
        assert!(!text.contains(HANDOFF_OMITTED), "{text}");
        let turn = &text[newest..older];
        assert!(turn.len() <= HANDOFF_TURN_BYTES_MAX + 2, "{}", turn.len());
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    /// A session with turns in both the kept harness conversation and the engine's master
    /// conversation hands over each in its own part, newest first, so the newest engine
    /// turns are never crowded out by a long harness conversation.
    #[cfg(unix)]
    #[test]
    fn a_switch_handoff_keeps_a_part_for_each_conversation_newest_first() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-parts").unwrap();
        for turn in 1..=3_i64 {
            let body = serde_json::json!({
                "engine_request": {"line": format!("Check the build {turn:02}")},
                "reply": {"text": format!("Build reply {turn:02}")},
            });
            connection
                .execute(
                    "INSERT INTO master_turns(session,ordinal,id,input,phase,body) VALUES (?1,?2,?3,?4,'succeeded',?5)",
                    rusqlite::params![
                        session.as_str(),
                        turn,
                        format!("turn-{turn}"),
                        1000 + turn,
                        body.to_string()
                    ],
                )
                .unwrap();
        }
        let id = "2f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let mut lines = String::new();
        for turn in 1..=20 {
            let words = format!("turn {turn:02} ").repeat(60);
            lines.push_str(
                &serde_json::json!({"type": "user", "message": {"content": words}}).to_string(),
            );
            lines.push('\n');
            lines.push_str(
                &serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": format!("answer {turn:02}")}], "stop_reason": "end_turn"}})
                    .to_string(),
            );
            lines.push('\n');
        }
        std::fs::write(&record, lines).unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, session.as_str(), &kept).unwrap();

        let text = handoff(&connection, &project, &session, "Claude Code").unwrap();
        let terminal = text
            .find("From the Claude Code conversation, newest first:")
            .expect("the harness conversation has its part");
        let newest_record = text
            .find("answer 20")
            .expect("its newest turn is handed over");
        let older_record = text.find("answer 19").expect("then the one before");
        let engine = text
            .find("From CrazyAgent's master conversation, newest first:")
            .expect("the engine conversation has its part");
        let newest_engine = text
            .find("Engine: Check the build 03\nYou: Build reply 03")
            .expect("the newest engine turn is handed over");
        let oldest_engine = text
            .find("Engine: Check the build 01\nYou: Build reply 01")
            .expect("every engine turn fits");
        assert!(
            terminal < newest_record
                && newest_record < older_record
                && older_record < engine
                && engine < newest_engine
                && newest_engine < oldest_engine,
            "{text}"
        );
        assert!(
            text.contains(HANDOFF_OMITTED),
            "the harness part was cut: {text}"
        );
        assert!(!text.contains("answer 01"), "{text}");
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    /// A switch is seen once: after the session's master switched to Codex, a later start on
    /// Codex is no switch even before Codex reported a conversation of its own, and a start
    /// on Claude Code again is a switch back from Codex.
    #[cfg(unix)]
    #[test]
    fn a_switch_is_seen_once_even_before_the_new_harness_reports_its_conversation() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-once").unwrap();
        let id = "3f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        std::fs::write(&record, "").unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, session.as_str(), &kept).unwrap();
        assert_eq!(
            switched_from(&connection, &project, &session, "codex").unwrap(),
            Some("claude-code".to_owned())
        );
        crate::team::record_switch(
            &connection,
            &project,
            session.as_str(),
            "codex",
            crate::team::SwitchMode::Carry,
        )
        .unwrap();
        assert_eq!(
            switched_from(&connection, &project, &session, "codex").unwrap(),
            None,
            "the switch to Codex was applied already"
        );
        assert_eq!(
            switched_from(&connection, &project, &session, "claude-code").unwrap(),
            Some("codex".to_owned())
        );
    }

    /// An engine turn of multi-byte text longer than one handed-over turn is cut with a
    /// visible "..." where its reply was cut, never silently.
    #[cfg(unix)]
    #[test]
    fn a_multi_byte_engine_turn_cut_to_fit_shows_where_it_was_cut() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        let project = ProjectId::new("switch-project").unwrap();
        let session = SessionId::new("s-wide").unwrap();
        let body = serde_json::json!({
            "engine_request": {"line": "検".repeat(300)},
            "reply": {"text": "答".repeat(600)},
        });
        connection
            .execute(
                "INSERT INTO master_turns(session,ordinal,id,input,phase,body) VALUES (?1,1,'turn-1',1001,'succeeded',?2)",
                rusqlite::params![session.as_str(), body.to_string()],
            )
            .unwrap();
        let text = handoff(&connection, &project, &session, "Codex").unwrap();
        let turn = text
            .find(&format!("Engine: {}", "検".repeat(300)))
            .expect("the engine turn is handed over");
        let reply = text[turn..].find("\nYou: 答").expect("its reply starts") + turn;
        assert!(text[reply..].contains("答..."), "the cut is shown: {text}");
        assert!(!text.contains(&"答".repeat(600)), "{text}");
        assert!(text.len() <= HANDOFF_BYTES_MAX + STATE_BYTES_MAX + 2);
    }

    #[test]
    fn listed_lines_hold_no_control_characters_and_stay_bounded() {
        assert_eq!(
            flat("Ship \u{1b}[31mnow\u{7}\u{0} please", 80).as_deref(),
            Some("Ship [31mnow please")
        );
        assert_eq!(
            clip("\n  \u{1b}\u{8}\n First line \r\nSecond", 80).as_deref(),
            Some("First line")
        );
        assert_eq!(flat("\u{1b}\u{7} \t", 80), None);
        let long = flat(&"word ".repeat(40), 12).unwrap();
        assert_eq!(long, "word word...");
        assert!(long.chars().count() <= 12);
    }

    /// A FIFO left at a kept harness record, as a tools-on master could make in its own
    /// home, is never opened for a blocking read: the session's start returns at once with a
    /// fresh conversation and the note that the earlier one could not be read.
    #[cfg(unix)]
    #[test]
    fn a_fifo_at_a_kept_record_falls_back_at_once_with_the_lost_note() {
        let folder = tempfile::tempdir().unwrap();
        let database = folder.path().join("engine.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        let id = "0f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let made = crate::spawn_gate::status(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&record)
                .env_clear()
                .stdin(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(made.success());
        let project = ProjectId::new("fifo-project").unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        keep_conversation(&connection, &project, "s-fifo", &kept).unwrap();
        drop(connection);
        let (sent, got) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let connection = rusqlite::Connection::open(&database).unwrap();
            let plan = resume_harness(
                &connection,
                &project,
                &SessionId::new("s-fifo").unwrap(),
                "claude-code",
                &|_| true,
            );
            let _ = sent.send(plan.map_err(|problem| problem.0));
        });
        let plan = got
            .recv_timeout(Duration::from_secs(10))
            .expect("a FIFO at the kept record must not block the session's start")
            .unwrap();
        let ResumePlan::Fresh { note, .. } = &plan else {
            panic!("a FIFO is no record to resume: {plan:?}");
        };
        assert_eq!(note, LOST_NOTE);
    }

    /// A conversation kept while the master ran on one pool account resumes on the account the
    /// schedule picks when the caller accepts the kept account for it (DEC-091, N-D14), and
    /// starts fresh when it does not (ND14-L1); the caller is asked about the kept account.
    /// Another harness never resumes it.
    #[test]
    fn a_kept_conversation_resumes_on_another_pool_account() {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        let project = ProjectId::new("pool-project").unwrap();
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms)
                 VALUES (?1,'master','claude',1,'account-a','stopped',1,1)",
                [project.as_str()],
            )
            .unwrap();
        let agent = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
                 VALUES (?1,?2,'/repo','/repo/.crazyagent/worktrees/agent-1','/repo/.git')",
                rusqlite::params![agent, project.as_str()],
            )
            .unwrap();
        let id = "5b0e2c1a-7d3f-4e8b-9a6c-2f1e0d9c8b7a";
        let record = folder.path().join(format!("{id}.jsonl"));
        std::fs::write(&record, "{}\n").unwrap();
        let kept = SessionConversation {
            agent,
            harness: "claude-code".into(),
            connection: "account-a".into(),
            resume: Resume {
                id: id.into(),
                record: record.clone(),
            },
        };
        keep_conversation(&connection, &project, "s-pool", &kept).unwrap();
        let session = SessionId::new("s-pool").unwrap();

        let subscription = |kept: &str| kept == "account-a";
        let plan = resume_harness(
            &connection,
            &project,
            &session,
            "claude-code",
            &subscription,
        )
        .unwrap();
        assert_eq!(
            plan,
            ResumePlan::Resume {
                agent,
                conversation: id.into(),
                record: record.clone(),
            },
            "a master started on another subscription continues the conversation kept on account-a"
        );

        let plan =
            resume_harness(&connection, &project, &session, "claude-code", &|_| false).unwrap();
        assert!(
            matches!(plan, ResumePlan::Fresh { .. }),
            "an account that cannot take the kept history starts fresh: {plan:?}"
        );

        let plan = resume_harness(&connection, &project, &session, "codex", &|_| true).unwrap();
        assert!(
            !matches!(plan, ResumePlan::Resume { .. }),
            "another harness never resumes the kept conversation: {plan:?}"
        );
    }
    /// A connection holding only the conversation store, with the given rows of one
    /// session: speaker, mode, harness and text, stored in that order from seq 1.
    fn stored_messages(
        project: &str,
        session: &str,
        rows: &[(&str, &str, Option<&str>, Option<&str>)],
    ) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch(super::MESSAGES).unwrap();
        for (index, (speaker, mode, harness, text)) in rows.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO session_messages(session,seq,project,speaker,mode,harness,text,at_ms,source,source_key)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'turn',?9)",
                    rusqlite::params![
                        session,
                        i64::try_from(index).unwrap() + 1,
                        project,
                        speaker,
                        mode,
                        harness,
                        text,
                        1_000 + i64::try_from(index).unwrap(),
                        index.to_string()
                    ],
                )
                .unwrap();
        }
        connection
    }

    /// A conversation page holds the session's newest messages oldest first, whatever
    /// master said them, and its cursor walks back to the first message: an engine-run
    /// turn, the divider where the live terminal took over, and a terminal turn whose typed
    /// words were not read all come back as rows of the one store.
    #[test]
    fn a_message_page_reads_newest_first_and_walks_back_to_the_first_message() {
        let project = crate::protocol::ProjectId::new("page-project").unwrap();
        let session = crate::protocol::SessionId::new("page-session").unwrap();
        let connection = stored_messages(
            "page-project",
            "page-session",
            &[
                ("owner", "one_turn", Some("claude"), Some("plan the launch")),
                ("master", "one_turn", Some("claude"), Some("launch planned")),
                (
                    "switch",
                    "live",
                    Some("claude-code"),
                    Some("Switched to Claude Code."),
                ),
                ("owner", "live", Some("claude-code"), None),
                (
                    "master",
                    "live",
                    Some("claude-code"),
                    Some("launch shipped"),
                ),
            ],
        );

        let newest = super::page(&connection, &project, &session, None, 2).unwrap();
        let middle = super::page(&connection, &project, &session, newest.next, 2).unwrap();
        let oldest = super::page(&connection, &project, &session, middle.next, 2).unwrap();

        let seqs = |page: &super::SessionMessagePage| {
            page.messages
                .iter()
                .map(|message| message.seq)
                .collect::<Vec<_>>()
        };
        assert_eq!(seqs(&newest), [4, 5]);
        assert_eq!(newest.next, Some(4));
        assert_eq!(newest.count, 5);
        assert_eq!(seqs(&middle), [2, 3]);
        assert_eq!(middle.next, Some(2));
        assert_eq!(seqs(&oldest), [1]);
        assert_eq!(oldest.next, None);
        assert_eq!(
            newest.messages[0],
            super::SessionMessage {
                seq: 4,
                speaker: super::MessageSpeaker::Owner,
                mode: super::MasterMode::Live,
                harness: Some("claude-code".into()),
                text: None,
                at_ms: 1_003,
            }
        );
        assert_eq!(
            middle.messages[1],
            super::SessionMessage {
                seq: 3,
                speaker: super::MessageSpeaker::Switch,
                mode: super::MasterMode::Live,
                harness: Some("claude-code".into()),
                text: Some("Switched to Claude Code.".into()),
                at_ms: 1_002,
            }
        );
        assert_eq!(oldest.messages[0].text.as_deref(), Some("plan the launch"));
    }

    /// A page stops before the message that would push it past one reply and names the
    /// cursor of the rest, so a session of long messages is read in several pages and none
    /// is dropped.
    #[test]
    fn a_message_page_of_long_messages_stops_inside_one_reply() {
        let project = crate::protocol::ProjectId::new("page-project").unwrap();
        let session = crate::protocol::SessionId::new("page-session").unwrap();
        let long = "\"".repeat(super::MESSAGE_BYTES_MAX);
        let connection = stored_messages(
            "page-project",
            "page-session",
            &[
                ("owner", "one_turn", Some("rcli"), Some(long.as_str())),
                ("master", "one_turn", Some("rcli"), Some(long.as_str())),
                ("owner", "one_turn", Some("rcli"), Some(long.as_str())),
            ],
        );

        let page = super::page(&connection, &project, &session, None, 64).unwrap();

        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].seq, 3);
        assert_eq!(page.messages[0].text.as_deref(), Some(long.as_str()));
        assert_eq!(page.next, Some(3));
        assert_eq!(page.count, 3);
        assert!(serde_json::to_vec(&page).unwrap().len() <= super::MESSAGES_PAGE_BYTES_MAX);
    }

    /// One message whose text serializes larger than a reply is cut and marked as cut, so
    /// the session still opens.
    #[test]
    fn a_lone_message_too_large_for_a_reply_is_cut_not_refused() {
        let project = crate::protocol::ProjectId::new("page-project").unwrap();
        let session = crate::protocol::SessionId::new("page-session").unwrap();
        let long = "\u{1}".repeat(super::MESSAGE_BYTES_MAX);
        let connection = stored_messages(
            "page-project",
            "page-session",
            &[("master", "live", Some("codex"), Some(long.as_str()))],
        );

        let page = super::page(&connection, &project, &session, None, 64).unwrap();

        let text = page.messages[0].text.as_deref().unwrap();
        assert_eq!(page.messages.len(), 1);
        assert!(text.len() < long.len(), "{}", text.len());
        assert!(text.ends_with("..."), "{}", text.len());
        assert_eq!(page.next, None);
        assert!(serde_json::to_vec(&page).unwrap().len() <= super::MESSAGES_PAGE_BYTES_MAX);
    }

    /// A page read names its bounds: no page of zero or of more than the page maximum, and
    /// no cursor before the first message.
    #[test]
    fn a_message_page_refuses_a_limit_or_cursor_out_of_bounds() {
        let project = crate::protocol::ProjectId::new("page-project").unwrap();
        let session = crate::protocol::SessionId::new("page-session").unwrap();
        let connection = stored_messages("page-project", "page-session", &[]);
        let refused = |before, limit| {
            super::page(&connection, &project, &session, before, limit)
                .unwrap_err()
                .0
        };

        assert_eq!(refused(None, 0), "master_messages_page_limit");
        assert_eq!(
            refused(None, super::MESSAGES_PAGE_MAX + 1),
            "master_messages_page_limit"
        );
        assert_eq!(refused(Some(0), 1), "master_messages_cursor_bounds");
        assert_eq!(refused(Some(u64::MAX), 1), "master_messages_cursor_bounds");
    }

    /// A session's messages are read only under the project that holds them, and a database
    /// an older engine made, with no store yet, gives an empty page.
    #[test]
    fn a_message_page_holds_only_the_named_project_and_is_empty_without_the_store() {
        let session = crate::protocol::SessionId::new("page-session").unwrap();
        let other = crate::protocol::ProjectId::new("other-project").unwrap();
        let connection = stored_messages(
            "page-project",
            "page-session",
            &[("owner", "live", Some("codex"), Some("hello"))],
        );
        let bare = rusqlite::Connection::open_in_memory().unwrap();

        let foreign = super::page(&connection, &other, &session, None, 8).unwrap();
        let unstored = super::page(&bare, &other, &session, None, 8).unwrap();

        assert!(foreign.messages.is_empty());
        assert_eq!(foreign.count, 0);
        assert!(unstored.messages.is_empty());
        assert_eq!(unstored.next, None);
    }
    /// An engine database with every schema, in a folder that also holds harness records.
    #[cfg(unix)]
    fn store_fixture() -> (tempfile::TempDir, rusqlite::Connection) {
        let folder = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(folder.path().join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        crate::team::initialize(&connection).unwrap();
        (folder, connection)
    }

    /// Stores a session row of project `store-project` the way an older engine did, with
    /// the backend its one-turn master ran on.
    #[cfg(unix)]
    fn old_session(connection: &rusqlite::Connection, session: &str, backend: &str) {
        let body = serde_json::json!({"selection": {"backend": backend}});
        connection
            .execute(
                "INSERT INTO master_sessions(id,project,created,version,phase,body)
                 VALUES (?1,'store-project',1,1,'configured',?2)",
                rusqlite::params![session, body.to_string()],
            )
            .unwrap();
    }

    /// Stores one engine-run turn the way an older engine did: its turn row with the given
    /// body, and the owner's message, when one is given, in the owner conversation.
    #[cfg(unix)]
    fn old_turn(
        connection: &rusqlite::Connection,
        session: &str,
        ordinal: i64,
        owner: Option<&str>,
        body: serde_json::Value,
    ) {
        connection
            .execute(
                "INSERT INTO master_turns(session,ordinal,id,input,phase,body)
                 VALUES (?1,?2,?3,?4,'succeeded',?5)",
                rusqlite::params![
                    session,
                    ordinal,
                    format!("turn-{ordinal}"),
                    1000 + ordinal,
                    body.to_string()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO conversation_entries SELECT 'store-project',?1,'message',0,?2 WHERE ?2 IS NOT NULL",
                rusqlite::params![1000 + ordinal, owner],
            )
            .unwrap();
    }

    /// Keeps a Claude Code conversation for a session the way an older engine did, with a
    /// record of the given typed inputs and replies.
    #[cfg(unix)]
    fn old_record(
        folder: &tempfile::TempDir,
        connection: &rusqlite::Connection,
        session: &str,
        exchanges: &[(&str, &str)],
    ) {
        let id = "4f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f";
        let record = folder.path().join(format!("{id}.jsonl"));
        let lines: String = exchanges
            .iter()
            .map(|(typed, reply)| {
                format!(
                    "{}\n{}\n",
                    serde_json::json!({"type": "user", "message": {"content": typed}}),
                    serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": reply}], "stop_reason": "end_turn"}})
                )
            })
            .collect();
        std::fs::write(&record, lines).unwrap();
        let kept = SessionConversation {
            agent: 1,
            harness: "claude-code".into(),
            connection: "local-a".into(),
            resume: Resume {
                id: id.into(),
                record,
            },
        };
        let project = ProjectId::new("store-project").unwrap();
        keep_conversation(connection, &project, session, &kept).unwrap();
    }

    /// A session's stored messages as lines: speaker, mode, the kind of writer, the text.
    fn shown(connection: &rusqlite::Connection, session: &str) -> Vec<String> {
        super::stored(connection, session)
            .unwrap()
            .into_iter()
            .map(|stored| {
                format!(
                    "{}/{}/{}: {}",
                    stored.message.speaker.name(),
                    stored.message.mode.name(),
                    stored.source,
                    stored.message.text.as_deref().unwrap_or("-")
                )
            })
            .collect()
    }

    /// One stored message for the switch rule: who said it, the master it belongs to and
    /// the kind of writer.
    fn message(
        seq: u64,
        speaker: super::MessageSpeaker,
        mode: super::MasterMode,
        harness: &str,
        source: &str,
    ) -> super::Stored {
        super::Stored {
            message: super::SessionMessage {
                seq,
                speaker,
                mode,
                harness: Some(harness.into()),
                text: Some("text".into()),
                at_ms: 1,
            },
            source: source.into(),
            key: seq.to_string(),
        }
    }

    /// OB-6: turns the one-turn master ran on RCLI are owed to a Claude Code terminal that
    /// starts next, and the handoff names where they come from. No harness has its own
    /// rule: the same messages are owed to a Codex terminal.
    #[test]
    fn one_turn_rcli_turns_are_owed_to_the_live_harness_that_starts_next() {
        use super::{MasterMode, MessageSpeaker, Runner};
        let messages = [
            message(
                1,
                MessageSpeaker::Owner,
                MasterMode::OneTurn,
                "rcli",
                "turn",
            ),
            message(
                2,
                MessageSpeaker::Master,
                MasterMode::OneTurn,
                "rcli",
                "turn",
            ),
        ];
        let live = |harness| Runner {
            mode: MasterMode::Live,
            harness,
        };

        assert_eq!(
            super::owed(&messages, live("claude-code")).as_deref(),
            Some("the one-turn master (RCLI)")
        );
        assert_eq!(
            super::owed(&messages, live("codex")).as_deref(),
            Some("the one-turn master (RCLI)")
        );
    }

    /// OB-6, the other direction: what was said in a Claude Code terminal is owed to the
    /// one-turn master on RCLI that runs the next turn, and a Codex terminal's conversation
    /// to a one-turn master on Claude.
    #[test]
    fn a_live_conversation_is_owed_to_the_one_turn_master_that_runs_next() {
        use super::{MasterMode, MessageSpeaker, Runner};
        let claude_code = [
            message(
                1,
                MessageSpeaker::Owner,
                MasterMode::Live,
                "claude-code",
                "terminal",
            ),
            message(
                2,
                MessageSpeaker::Master,
                MasterMode::Live,
                "claude-code",
                "terminal",
            ),
        ];
        let codex = [message(
            1,
            MessageSpeaker::Owner,
            MasterMode::Live,
            "codex",
            "terminal",
        )];
        let one_turn = |harness| Runner {
            mode: MasterMode::OneTurn,
            harness,
        };

        assert_eq!(
            super::owed(&claude_code, one_turn("rcli")).as_deref(),
            Some("claude-code")
        );
        assert_eq!(
            super::owed(&codex, one_turn("claude")).as_deref(),
            Some("codex")
        );
    }

    /// A master that continues its own conversation owes nothing: the same one-turn
    /// backend, the same live harness under either of its names, or a session with no
    /// message yet.
    #[test]
    fn a_master_that_continues_its_own_conversation_is_no_switch() {
        use super::{MasterMode, MessageSpeaker, Runner};
        let one_turn = [message(
            1,
            MessageSpeaker::Owner,
            MasterMode::OneTurn,
            "rcli",
            "turn",
        )];
        let live = [message(
            1,
            MessageSpeaker::Owner,
            MasterMode::Live,
            "claude-code",
            "terminal",
        )];

        assert_eq!(
            super::owed(
                &one_turn,
                Runner {
                    mode: MasterMode::OneTurn,
                    harness: "rcli"
                }
            ),
            None
        );
        assert_eq!(
            super::owed(
                &live,
                Runner {
                    mode: MasterMode::Live,
                    harness: "claude"
                }
            ),
            None
        );
        assert_eq!(
            super::owed(
                &[],
                Runner {
                    mode: MasterMode::Live,
                    harness: "codex"
                }
            ),
            None
        );
    }

    /// A switch is settled once: after the divider a switch wrote, the master it names owes
    /// nothing, turns it ran since are owed to whichever other master comes next, and going
    /// back before it ran a turn is a switch from it too.
    #[test]
    fn a_stored_switch_settles_what_came_before_it() {
        use super::{MasterMode, MessageSpeaker, Runner};
        let mut messages = vec![
            message(
                1,
                MessageSpeaker::Owner,
                MasterMode::OneTurn,
                "rcli",
                "turn",
            ),
            message(
                2,
                MessageSpeaker::Switch,
                MasterMode::Live,
                "claude-code",
                "switch",
            ),
        ];
        let live = Runner {
            mode: MasterMode::Live,
            harness: "claude-code",
        };
        let one_turn = Runner {
            mode: MasterMode::OneTurn,
            harness: "rcli",
        };

        assert_eq!(super::owed(&messages, live), None);
        assert_eq!(
            super::owed(&messages, one_turn).as_deref(),
            Some("claude-code")
        );
        messages.push(message(
            3,
            MessageSpeaker::Owner,
            MasterMode::Live,
            "claude-code",
            "terminal",
        ));
        assert_eq!(super::owed(&messages, live), None);
        assert_eq!(
            super::owed(&messages, one_turn).as_deref(),
            Some("claude-code")
        );
    }

    /// A message stored twice under the same turn keeps one row, rows keep the order they
    /// were first stored in, and text past the row bound is cut and marked as cut.
    #[test]
    fn a_message_stored_twice_keeps_one_row_and_long_text_is_cut() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let long = "a".repeat(super::MESSAGE_BYTES_MAX + 1);
        let asked = super::Asked::Owner(Some("Plan it"));

        let first = super::append_asked(&connection, "p", "s", Some("rcli"), "turn-1", &asked);
        let again = super::append_asked(&connection, "p", "s", Some("rcli"), "turn-1", &asked);
        super::append_reply(&connection, "p", "s", Some("rcli"), "turn-1", &long).unwrap();
        super::append_reply(&connection, "p", "s", Some("rcli"), "turn-1", "late").unwrap();

        assert_eq!((first.unwrap(), again.unwrap()), (true, false));
        let stored = super::stored(&connection, "s").unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].message.text.as_deref(), Some("Plan it"));
        assert_eq!(stored[1].message.seq, 2);
        let reply = stored[1].message.text.as_deref().unwrap();
        assert_eq!(reply.len(), super::MESSAGE_BYTES_MAX);
        assert!(reply.ends_with("a..."), "{}", &reply[reply.len() - 8..]);
    }

    /// OB-6, the owner's session: one-turn turns, then a Claude Code terminal conversation
    /// that began with no handoff. The engine start moves both into the one store, with the
    /// owner's words, the engine's own request and a turn without a reply, and puts a
    /// divider between them that settles nothing, so the next Claude Code start still owes
    /// the one-turn turns to it.
    #[cfg(unix)]
    #[test]
    fn a_live_conversation_begun_after_one_turn_turns_without_a_handoff_still_owes_them() {
        let (folder, connection) = store_fixture();
        old_session(&connection, "s-oneday", "claude");
        old_turn(
            &connection,
            "s-oneday",
            1,
            Some("Plan the course page"),
            serde_json::json!({"reply": {"text": "Planned it"}}),
        );
        old_turn(
            &connection,
            "s-oneday",
            2,
            None,
            serde_json::json!({"engine_request": {"line": "Agent 3 finished"}, "reply": {"text": "Noted"}}),
        );
        old_turn(
            &connection,
            "s-oneday",
            3,
            Some("Add the pricing table"),
            serde_json::json!({}),
        );
        old_record(
            &folder,
            &connection,
            "s-oneday",
            &[("Fix the footer", "Footer fixed")],
        );

        super::initialize(&connection).unwrap();

        assert_eq!(
            shown(&connection, "s-oneday"),
            [
                "owner/one_turn/turn: Plan the course page",
                "master/one_turn/turn: Planned it",
                "engine/one_turn/turn: Agent 3 finished",
                "master/one_turn/turn: Noted",
                "owner/one_turn/turn: Add the pricing table",
                "switch/live/backfill: Switched to Claude Code. The earlier turns were not handed over to it.",
                "owner/live/record: Fix the footer",
                "master/live/record: Footer fixed",
            ]
        );
        let messages = super::stored(&connection, "s-oneday").unwrap();
        assert_eq!(
            super::owed(
                &messages,
                super::Runner {
                    mode: super::MasterMode::Live,
                    harness: "claude-code"
                }
            )
            .as_deref(),
            Some("the one-turn master (Claude)")
        );
    }

    /// The move into the store is done once: a second engine start, and a second look at
    /// the same session, add no row and change none.
    #[cfg(unix)]
    #[test]
    fn a_second_engine_start_stores_no_message_twice() {
        let (folder, connection) = store_fixture();
        old_session(&connection, "s-twice", "codex");
        old_turn(
            &connection,
            "s-twice",
            1,
            Some("Ship it"),
            serde_json::json!({"reply": {"text": "Shipped"}}),
        );
        old_record(
            &folder,
            &connection,
            "s-twice",
            &[("Check the logs", "Logs are clean")],
        );
        super::initialize(&connection).unwrap();
        let first = shown(&connection, "s-twice");

        super::initialize(&connection).unwrap();
        super::absorb(&connection, "store-project", "s-twice").unwrap();

        assert_eq!(first.len(), 5);
        assert_eq!(shown(&connection, "s-twice"), first);
    }

    /// A switch an older engine recorded for the harness of the kept conversation is the
    /// divider between the engine-run turns and that conversation, and it settles them: the
    /// harness owes nothing more, while the one-turn master that runs next is owed the
    /// terminal conversation.
    #[cfg(unix)]
    #[test]
    fn a_recorded_switch_becomes_the_divider_and_settles_the_earlier_turns() {
        let (folder, connection) = store_fixture();
        old_session(&connection, "s-settled", "rcli");
        old_turn(
            &connection,
            "s-settled",
            1,
            Some("Draft the email"),
            serde_json::json!({"reply": {"text": "Drafted"}}),
        );
        old_record(&folder, &connection, "s-settled", &[("Send it", "Sent")]);
        connection
            .execute(
                "INSERT INTO team_session_switches(session,project,harness,mode,created_ms)
                 VALUES ('s-settled','store-project','claude','carry',1)",
                [],
            )
            .unwrap();

        super::initialize(&connection).unwrap();

        let divider = format!(
            "switch/live/switch: {}",
            super::switched_note("Claude Code", crate::team::SwitchMode::Carry)
        );
        assert_eq!(
            shown(&connection, "s-settled"),
            [
                "owner/one_turn/turn: Draft the email",
                "master/one_turn/turn: Drafted",
                divider.as_str(),
                "owner/live/record: Send it",
                "master/live/record: Sent",
            ]
        );
        let messages = super::stored(&connection, "s-settled").unwrap();
        let next = |mode, harness| super::owed(&messages, super::Runner { mode, harness });
        assert_eq!(next(super::MasterMode::Live, "claude-code"), None);
        assert_eq!(
            next(super::MasterMode::OneTurn, "rcli").as_deref(),
            Some("claude-code")
        );
    }

    /// A kept record that can no longer be read loses no turn from the session's count: the
    /// terminal turns an older engine counted are stored as owner rows without text, with
    /// the newest line it kept as the master's.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_record_keeps_the_counted_turns_as_rows_without_text() {
        let (folder, connection) = store_fixture();
        old_session(&connection, "s-lost", "claude");
        old_record(&folder, &connection, "s-lost", &[("Deploy", "Deployed")]);
        std::fs::remove_file(
            folder
                .path()
                .join("4f8c2c1e-5a7b-4c3d-9e2f-1a2b3c4d5e6f.jsonl"),
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO session_owner_turns(session,project,turns,last_line)
                 VALUES ('s-lost','store-project',3,'Deployed')",
                [],
            )
            .unwrap();

        super::initialize(&connection).unwrap();

        assert_eq!(
            shown(&connection, "s-lost"),
            [
                "owner/live/counter: -",
                "owner/live/counter: -",
                "owner/live/counter: -",
                "master/live/counter: Deployed",
            ]
        );
    }

    /// A record shows only its newest part, so turns an older engine counted beyond it are
    /// kept as owner rows without text before the record's messages, and the session's
    /// count of turns does not drop.
    #[cfg(unix)]
    #[test]
    fn turns_counted_beyond_the_record_are_kept_as_rows_without_text() {
        let (folder, connection) = store_fixture();
        old_session(&connection, "s-counted", "claude");
        old_record(
            &folder,
            &connection,
            "s-counted",
            &[("Rename it", "Renamed")],
        );
        connection
            .execute(
                "INSERT INTO session_owner_turns(session,project,turns,last_line)
                 VALUES ('s-counted','store-project',3,'Renamed')",
                [],
            )
            .unwrap();

        super::initialize(&connection).unwrap();

        assert_eq!(
            shown(&connection, "s-counted"),
            [
                "owner/live/counter: -",
                "owner/live/counter: -",
                "owner/live/record: Rename it",
                "master/live/record: Renamed",
            ]
        );
    }

    /// A handoff the engine typed into a terminal to carry a conversation over is not
    /// stored as something the owner said; the master's answer to it and the owner's own
    /// messages are.
    #[cfg(unix)]
    #[test]
    fn a_handoff_the_engine_typed_is_not_stored_as_the_owner_s_words() {
        let (folder, connection) = store_fixture();
        old_session(&connection, "s-seeded", "claude");
        old_record(
            &folder,
            &connection,
            "s-seeded",
            &[
                (
                    "The owner switched this conversation's master from Codex to you. Its earlier turns, newest first:\n\nOwner: hello",
                    "Understood",
                ),
                ("Carry on", "Carrying on"),
            ],
        );

        super::initialize(&connection).unwrap();

        assert_eq!(
            shown(&connection, "s-seeded"),
            [
                "master/live/record: Understood",
                "owner/live/record: Carry on",
                "master/live/record: Carrying on",
            ]
        );
    }
    /// OB-6, Claude Code to RCLI: the first turn the one-turn master runs after a live
    /// terminal conversation stores the divider of that switch before what the turn asked,
    /// once, and its reply follows the turn; a second turn adds no second divider.
    #[test]
    fn a_one_turn_turn_after_a_live_conversation_stores_the_switch_once() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let project = crate::protocol::ProjectId::new("p").unwrap();
        let session = crate::protocol::SessionId::new("s").unwrap();
        let live = |speaker: super::MessageSpeaker, text: &str, key: &str| {
            super::append(
                &connection,
                "p",
                "s",
                &super::Draft {
                    speaker,
                    mode: super::MasterMode::Live,
                    harness: Some("claude-code"),
                    text: Some(text),
                    source: "terminal",
                    key,
                },
            )
            .unwrap()
        };
        live(super::MessageSpeaker::Owner, "Fix the footer", "1");
        live(super::MessageSpeaker::Master, "Footer fixed", "1/reply");
        let rcli = crate::master_session::MasterBackend::Rcli;
        let asked = |turn: &str, text: &str| {
            super::one_turn_asked(
                &connection,
                &project,
                &session,
                rcli,
                turn,
                &super::Asked::Owner(Some(text)),
            )
            .unwrap()
        };

        asked("turn-1", "And the header?");
        super::one_turn_reply(&connection, &session, "turn-1", "Header fixed").unwrap();
        super::one_turn_reply(&connection, &session, "turn-1", "Header fixed").unwrap();
        asked("turn-2", "Thanks");

        let divider = format!(
            "switch/one_turn/switch: {}",
            super::switched_note("the one-turn master (RCLI)", crate::team::SwitchMode::Carry)
        );
        assert_eq!(
            shown(&connection, "s"),
            [
                "owner/live/terminal: Fix the footer",
                "master/live/terminal: Footer fixed",
                divider.as_str(),
                "owner/one_turn/turn: And the header?",
                "master/one_turn/turn: Header fixed",
                "owner/one_turn/turn: Thanks",
            ]
        );
    }

    /// The one-turn master's history gets what was said in the live terminal, each exchange
    /// placed after the engine-run turn it followed, with the newest reply of a turn of
    /// many replies and a plain line where the typed words were not kept; a terminal turn
    /// with nothing kept is left out.
    #[test]
    fn live_exchanges_are_read_for_the_one_turn_history_in_their_place() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let session = crate::protocol::SessionId::new("s").unwrap();
        let live = |speaker: super::MessageSpeaker, text: Option<&str>, key: &str| {
            super::append(
                &connection,
                "p",
                "s",
                &super::Draft {
                    speaker,
                    mode: super::MasterMode::Live,
                    harness: Some("codex"),
                    text,
                    source: "terminal",
                    key,
                },
            )
            .unwrap()
        };
        live(super::MessageSpeaker::Owner, Some("Before any turn"), "1");
        live(
            super::MessageSpeaker::Master,
            Some("Early answer"),
            "1/reply",
        );
        let asked = super::Asked::Owner(Some("Plan it"));
        super::append_asked(&connection, "p", "s", Some("rcli"), "turn-1", &asked).unwrap();
        super::append_reply(&connection, "p", "s", Some("rcli"), "turn-1", "Planned").unwrap();
        live(super::MessageSpeaker::Owner, None, "2");
        live(super::MessageSpeaker::Master, Some("Working"), "2/a");
        live(super::MessageSpeaker::Master, Some("Done"), "2/b");
        live(super::MessageSpeaker::Owner, None, "3");

        let exchanges = super::live_exchanges(&connection, &session).unwrap();

        assert_eq!(
            exchanges,
            [
                super::LiveExchange {
                    after_turn: None,
                    entry: "Owner, in the Codex terminal: Before any turn\nYou, in the Codex terminal: Early answer".into(),
                },
                super::LiveExchange {
                    after_turn: Some("turn-1".into()),
                    entry: "Owner, in the Codex terminal: (The typed words were not kept.)\nYou, in the Codex terminal: Done".into(),
                },
            ]
        );
    }

    /// Start fresh is kept for the one-turn master too: what was said in the live terminal
    /// before a switch the owner set to Start fresh is not in its history, and what is said
    /// there afterwards is.
    #[test]
    fn a_fresh_switch_to_the_one_turn_master_leaves_the_earlier_terminal_out() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let session = crate::protocol::SessionId::new("s").unwrap();
        let live = |text: &str, key: &str| {
            super::append(
                &connection,
                "p",
                "s",
                &super::Draft {
                    speaker: super::MessageSpeaker::Owner,
                    mode: super::MasterMode::Live,
                    harness: Some("codex"),
                    text: Some(text),
                    source: "terminal",
                    key,
                },
            )
            .unwrap()
        };
        live("Left behind", "1");
        super::note_switch(
            &connection,
            "p",
            "s",
            super::MasterMode::OneTurn,
            "rcli",
            crate::team::SwitchMode::Fresh,
        )
        .unwrap();
        live("Said later", "2");

        let exchanges = super::live_exchanges(&connection, &session).unwrap();

        assert_eq!(exchanges.len(), 1);
        assert_eq!(
            exchanges[0].entry,
            "Owner, in the Codex terminal: Said later\n(No reply was kept.)"
        );
    }
    /// A turn run in the live terminal is stored like any other master's: the owner's
    /// message when it starts, the master's reply when it ends, under the harness that ran
    /// in the terminal, one pair per turn.
    #[test]
    fn a_live_terminal_turn_is_stored_as_an_owner_message_then_the_reply() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let project = crate::protocol::ProjectId::new("p").unwrap();

        super::mark_owner_turn(&connection, &project, "s", "codex").unwrap();
        super::mark_owner_reply(&connection, &project, "s", "codex", "Done.").unwrap();
        super::mark_owner_turn(&connection, &project, "s", "codex").unwrap();
        super::mark_owner_reply(&connection, &project, "s", "codex", "Done again.").unwrap();

        assert_eq!(
            shown(&connection, "s"),
            [
                "owner/live/terminal: -",
                "master/live/terminal: Done.",
                "owner/live/terminal: -",
                "master/live/terminal: Done again.",
            ]
        );
        let stored = super::stored(&connection, "s").unwrap();
        assert_eq!(stored[3].message.harness.as_deref(), Some("codex"));
    }

    /// A reply whose turn start the engine did not see still stores the whole turn, and a
    /// reply that shows no words stores nothing on its own.
    #[test]
    fn a_reply_without_a_seen_start_stores_the_turn_and_an_empty_one_stores_nothing() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let project = crate::protocol::ProjectId::new("p").unwrap();

        super::mark_owner_reply(&connection, &project, "s", "claude-code", " \n ").unwrap();
        let after_empty = shown(&connection, "s");
        super::mark_owner_reply(&connection, &project, "s", "claude-code", "Shipped").unwrap();

        assert!(after_empty.is_empty(), "{after_empty:?}");
        assert_eq!(
            shown(&connection, "s"),
            ["owner/live/terminal: -", "master/live/terminal: Shipped"]
        );
    }

    /// A session whose messages another project holds takes no terminal turn of this one.
    #[test]
    fn a_terminal_turn_never_lands_in_another_project_s_session() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let holder = crate::protocol::ProjectId::new("holder").unwrap();
        let other = crate::protocol::ProjectId::new("other").unwrap();
        super::mark_owner_turn(&connection, &holder, "s", "codex").unwrap();

        super::mark_owner_turn(&connection, &other, "s", "codex").unwrap();
        super::mark_owner_reply(&connection, &other, "s", "codex", "Not here").unwrap();

        assert_eq!(shown(&connection, "s"), ["owner/live/terminal: -"]);
    }

    /// The words the owner typed in the terminal are read from the record the session kept
    /// when the turn ends. A record that still shows the previous turn's input gives the
    /// next turn no words, so nothing is stored twice as said.
    #[cfg(unix)]
    #[test]
    fn the_typed_words_of_a_terminal_turn_come_from_the_kept_record() {
        let (folder, connection) = store_fixture();
        let project = ProjectId::new("store-project").unwrap();
        old_record(
            &folder,
            &connection,
            "s-typed",
            &[("Fix the footer", "Footer fixed")],
        );

        super::mark_owner_turn(&connection, &project, "s-typed", "claude-code").unwrap();
        super::mark_owner_reply(
            &connection,
            &project,
            "s-typed",
            "claude-code",
            "Footer fixed",
        )
        .unwrap();
        super::mark_owner_turn(&connection, &project, "s-typed", "claude-code").unwrap();
        super::mark_owner_reply(&connection, &project, "s-typed", "claude-code", "And more")
            .unwrap();

        assert_eq!(
            shown(&connection, "s-typed"),
            [
                "owner/live/terminal: Fix the footer",
                "master/live/terminal: Footer fixed",
                "owner/live/terminal: -",
                "master/live/terminal: And more",
            ]
        );
    }
    /// OB-6, the owner's session end to end: one-turn turns, then a Claude Code terminal
    /// conversation that began with no handoff. The next Claude Code start is a switch from
    /// the one-turn master, its handoff carries the one-turn turns beside what was said in
    /// the terminal, and once that start is recorded the turns are owed no more.
    #[cfg(unix)]
    #[test]
    fn the_next_live_start_seeds_the_one_turn_turns_a_live_conversation_never_got() {
        let (folder, connection) = store_fixture();
        let project = ProjectId::new("store-project").unwrap();
        let session = SessionId::new("s-oneday").unwrap();
        old_session(&connection, "s-oneday", "claude");
        old_turn(
            &connection,
            "s-oneday",
            1,
            Some("Plan the course page"),
            serde_json::json!({"reply": {"text": "Planned it"}}),
        );
        old_record(
            &folder,
            &connection,
            "s-oneday",
            &[("Fix the footer", "Footer fixed")],
        );

        let from = switched_from(&connection, &project, &session, "claude-code").unwrap();
        let text = handoff(
            &connection,
            &project,
            &session,
            super::harness_label(from.as_deref().unwrap()),
        )
        .unwrap();
        crate::team::record_switch(
            &connection,
            &project,
            "s-oneday",
            "claude-code",
            crate::team::SwitchMode::Carry,
        )
        .unwrap();
        let again = switched_from(&connection, &project, &session, "claude-code").unwrap();

        assert_eq!(from.as_deref(), Some("the one-turn master (Claude)"));
        assert!(
            text.starts_with(
                "The owner switched this conversation's master from the one-turn master (Claude) to you. Its earlier turns, newest first:"
            ),
            "{text}"
        );
        assert!(
            text.contains("From CrazyAgent's master conversation, newest first:\n\nOwner: Plan the course page\nYou: Planned it"),
            "{text}"
        );
        assert!(
            text.contains("From the Claude Code conversation, newest first:\n\nTyped to you: Fix the footer\nYou: Footer fixed"),
            "{text}"
        );
        assert_eq!(again, None, "the handoff is given once");
    }

    /// OB-6, RCLI to Claude Code on this engine: turns the one-turn master ran on RCLI are a
    /// switch for the Claude Code terminal that starts next, and the handoff carries them.
    #[cfg(unix)]
    #[test]
    fn a_claude_code_start_after_rcli_turns_is_a_switch_that_carries_them() {
        let (_folder, connection) = store_fixture();
        let project = crate::protocol::ProjectId::new("p").unwrap();
        let session = crate::protocol::SessionId::new("s").unwrap();
        let rcli = crate::master_session::MasterBackend::Rcli;
        let asked = super::Asked::Owner(Some("Name the release"));
        super::one_turn_asked(&connection, &project, &session, rcli, "turn-1", &asked).unwrap();
        super::one_turn_reply(&connection, &session, "turn-1", "Call it Aurora").unwrap();

        let from = super::switched_from(&connection, &project, &session, "claude-code").unwrap();
        let text = super::handoff(&connection, &project, &session, "RCLI").unwrap();

        assert_eq!(from.as_deref(), Some("the one-turn master (RCLI)"));
        assert!(
            text.contains("From CrazyAgent's master conversation, newest first:\n\nOwner: Name the release\nYou: Call it Aurora"),
            "{text}"
        );
    }

    /// Start fresh leaves the earlier conversation behind for good: after a switch the
    /// owner set to Start fresh, a later handoff carries only what was said since.
    #[cfg(unix)]
    #[test]
    fn a_handoff_after_a_fresh_switch_carries_only_what_was_said_since() {
        let (_folder, connection) = store_fixture();
        let project = crate::protocol::ProjectId::new("p").unwrap();
        let session = crate::protocol::SessionId::new("s").unwrap();
        let rcli = crate::master_session::MasterBackend::Rcli;
        let asked = super::Asked::Owner(Some("Left behind"));
        super::one_turn_asked(&connection, &project, &session, rcli, "turn-1", &asked).unwrap();
        super::note_switch(
            &connection,
            "p",
            "s",
            super::MasterMode::Live,
            "codex",
            crate::team::SwitchMode::Fresh,
        )
        .unwrap();
        super::mark_owner_reply(&connection, &project, "s", "codex", "Said since").unwrap();

        let text = super::handoff(&connection, &project, &session, "Codex").unwrap();

        assert!(text.contains("You: Said since"), "{text}");
        assert!(!text.contains("Left behind"), "{text}");
    }
    /// The Sessions list reads one store for every master: a session whose turns ran on the
    /// one-turn master and then in a Codex terminal counts them all, shows the owner's first
    /// line and the newest line whoever wrote it, and names the master that ran last.
    #[cfg(unix)]
    #[test]
    fn the_session_list_counts_and_quotes_every_master_s_messages() {
        let (_folder, connection) = store_fixture();
        let project = ProjectId::new("store-project").unwrap();
        let session = SessionId::new("s-listed").unwrap();
        old_session(&connection, "s-listed", "rcli");
        let rcli = crate::master_session::MasterBackend::Rcli;
        let asked = super::Asked::Owner(Some("Name the release\nand date it"));
        super::one_turn_asked(&connection, &project, &session, rcli, "turn-1", &asked).unwrap();
        super::one_turn_reply(&connection, &session, "turn-1", "Call it Aurora").unwrap();
        super::mark_owner_turn(&connection, &project, "s-listed", "codex").unwrap();
        super::mark_owner_reply(&connection, &project, "s-listed", "codex", "Dated for May")
            .unwrap();

        let listed = super::list(&connection, &project).unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].turns, 2);
        assert_eq!(listed[0].first_line.as_deref(), Some("Name the release"));
        assert_eq!(listed[0].last_line.as_deref(), Some("Dated for May"));
        assert_eq!(listed[0].mode, Some(super::MasterMode::Live));
        assert_eq!(listed[0].harness.as_deref(), Some("codex"));
    }
}
