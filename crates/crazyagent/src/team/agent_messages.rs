use super::events::{self, EventKind, EventState};
use super::{
    RESULT_BYTES_MAX, STATUS_REPLY_BYTES_MAX, STATUS_TEXT_CAPS, bound_text, cut_text, encoded_len,
    error, failure, make_room,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

pub const PAGE_MAX: u16 = 200;
pub const KEPT_CHOICES: [u32; 3] = [500, 2000, 5000];
pub const PAGE_CHOICES: [u16; 3] = [25, 50, 100];
pub const SAID_BYTES_MAX: usize = events::SEND_BYTES_MAX;
pub const ENTRY_TEXT_BYTES_MAX: usize = 4096;
pub const RESULT_PREVIEW_BYTES_MAX: usize = 2048;

const _: () = assert!(PAGE_CHOICES[PAGE_CHOICES.len() - 1] <= PAGE_MAX);
const _: () = assert!(RESULT_PREVIEW_BYTES_MAX < RESULT_BYTES_MAX);
const _: () = assert!(SAID_BYTES_MAX <= ENTRY_TEXT_BYTES_MAX);

/// How a finished or failed task's result shows in an agent's Messages timeline: a preview
/// of at most `RESULT_PREVIEW_BYTES_MAX` bytes the client opens on its first line, or the
/// whole result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultsShown {
    #[default]
    FirstLine,
    Full,
}

impl ResultsShown {
    fn name(self) -> &'static str {
        match self {
            Self::FirstLine => "first_line",
            Self::Full => "full",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::FirstLine, Self::Full]
            .into_iter()
            .find(|shown| shown.name() == value)
    }
}

/// A project's owner settings for the agents' Messages timelines: whether the master's
/// messages and answers to its sub-agents are kept, how many kept messages the project
/// holds, whether progress events and the owner's own tasks and prompts show, how task
/// results show and how many entries one page holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageSettings {
    pub keep_master_messages: bool,
    pub kept_per_project: u32,
    pub progress_shown: bool,
    pub results: ResultsShown,
    pub owner_items_shown: bool,
    pub page_size: u16,
}

impl Default for MessageSettings {
    fn default() -> Self {
        Self {
            keep_master_messages: true,
            kept_per_project: 2000,
            progress_shown: true,
            results: ResultsShown::FirstLine,
            owner_items_shown: true,
            page_size: 50,
        }
    }
}

impl MessageSettings {
    /// Checks the settings name only the listed choices. Takes the settings. Returns unit,
    /// or `team_message_settings_invalid`.
    pub fn validate(&self) -> Result<(), EngineError> {
        (KEPT_CHOICES.contains(&self.kept_per_project) && PAGE_CHOICES.contains(&self.page_size))
            .then_some(())
            .ok_or_else(|| failure("team_message_settings_invalid"))
    }
}

/// What the master, the owner or the engine said straight to a sub-agent once its terminal
/// took it: a master's `message` or `answer`, a prompt the owner sent, or a notice the engine
/// typed, such as which of a harness's teammates it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    Message,
    Answer,
    Prompt,
    Notice,
}

impl Said {
    fn author(self) -> Speaker {
        match self {
            Self::Message | Self::Answer => Speaker::Master,
            Self::Prompt => Speaker::Owner,
            Self::Notice => Speaker::Engine,
        }
    }

    fn kind(self) -> EntryKind {
        match self {
            Self::Message | Self::Notice => EntryKind::Message,
            Self::Answer => EntryKind::Answer,
            Self::Prompt => EntryKind::Prompt,
        }
    }
}

/// Who an entry of a Messages timeline comes from, as the engine knows it, never as the
/// text says: the master, the owner, the sub-agent or the engine itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    Master,
    Owner,
    Agent,
    Engine,
}

impl Speaker {
    fn name(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Owner => "owner",
            Self::Agent => "agent",
            Self::Engine => super::graph::ENGINE_PARTY,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [Self::Master, Self::Owner, Self::Agent, Self::Engine]
            .into_iter()
            .find(|speaker| speaker.name() == value)
    }
}

/// What one timeline entry is: a task or a wake handed to the agent, a message, answer or
/// prompt typed into it, or one of its events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Task,
    Wake,
    Message,
    Answer,
    Prompt,
    Question,
    Progress,
    ChangeSetReady,
    Finished,
    Failed,
}

impl EntryKind {
    fn name(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Wake => "wake",
            Self::Message => "message",
            Self::Answer => "answer",
            Self::Prompt => "prompt",
            Self::Question => "question",
            Self::Progress => "progress",
            Self::ChangeSetReady => "change_set_ready",
            Self::Finished => "finished",
            Self::Failed => "failed",
        }
    }

    fn said(value: &str) -> Option<Self> {
        [Self::Message, Self::Answer, Self::Prompt]
            .into_iter()
            .find(|kind| kind.name() == value)
    }

    fn event(kind: EventKind) -> Self {
        match kind {
            EventKind::Question => Self::Question,
            EventKind::Finished => Self::Finished,
            EventKind::Failed => Self::Failed,
            EventKind::ChangeSetReady => Self::ChangeSetReady,
            EventKind::Progress => Self::Progress,
        }
    }
}

/// Which store a timeline entry comes from; with the time and the row id it orders entries
/// of one millisecond the same way on every page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Task,
    Message,
    Event,
}

/// Where a page of a timeline ends: the next page holds the entries before it, so rows that
/// arrive between two reads never move an entry onto another page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub at_ms: i64,
    pub source: Source,
    pub id: i64,
}

/// One entry of an agent's Messages timeline. `text` is none for a master's message the
/// project kept no text of; `result` is a finished task's result or a failed task's problem,
/// `result_more` says the stored one is longer; `state` is an event's way to the master.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessage {
    pub at_ms: i64,
    pub source: Source,
    pub id: i64,
    pub from: Speaker,
    pub kind: EntryKind,
    pub text: Option<String>,
    pub task: Option<i64>,
    pub state: Option<EventState>,
    pub result: Option<String>,
    pub result_more: bool,
}

impl AgentMessage {
    /// Names where this entry sits, for the page after it. Takes the entry. Returns its cursor.
    pub fn cursor(&self) -> Cursor {
        Cursor {
            at_ms: self.at_ms,
            source: self.source,
            id: self.id,
        }
    }
}

/// One page of one sub-agent's Messages timeline, newest first, with whether older entries
/// remain and the project's settings it was read under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessages {
    pub project_id: ProjectId,
    pub agent: i64,
    pub entries: Vec<AgentMessage>,
    pub older: bool,
    pub settings: MessageSettings,
}

/// The kept messages table's columns: an entry is said by the master, the owner or the
/// engine, as a message, an answer or a prompt. Takes nothing. Returns the column list.
fn message_columns() -> String {
    let engine = super::graph::ENGINE_PARTY;
    format!(
        "id INTEGER PRIMARY KEY AUTOINCREMENT,
         project TEXT NOT NULL,
         agent INTEGER NOT NULL,
         author TEXT NOT NULL CHECK(author IN ('master','owner','{engine}')),
         kind TEXT NOT NULL CHECK(kind IN ('message','answer','prompt')),
         text TEXT,
         created_ms INTEGER NOT NULL"
    )
}

/// Rebuilds a kept messages table made before engine notices (B19R3-H1), whose author check
/// allows only the master and the owner: a new table takes every row with its id and the
/// id sequence, then replaces the old one, all in one savepoint, so a failure leaves the old
/// table whole. A table that allows the engine, or none, is left alone. Takes the
/// connection. Returns unit.
fn widen_authors(connection: &Connection) -> Result<(), EngineError> {
    let sql: Option<String> = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='team_agent_messages'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let engine = super::graph::ENGINE_PARTY;
    let Some(sql) = sql else {
        return Ok(());
    };
    if sql.contains(&format!("'{engine}'")) {
        return Ok(());
    }
    let rebuilt = connection.execute_batch(&format!(
        "SAVEPOINT widen_authors;
         CREATE TABLE team_agent_messages_widened ({columns});
         INSERT INTO team_agent_messages_widened(id,project,agent,author,kind,text,created_ms)
             SELECT id,project,agent,author,kind,text,created_ms FROM team_agent_messages;
         DELETE FROM sqlite_sequence WHERE name='team_agent_messages_widened';
         INSERT INTO sqlite_sequence(name,seq)
             SELECT 'team_agent_messages_widened',seq FROM sqlite_sequence
             WHERE name='team_agent_messages';
         DROP TABLE team_agent_messages;
         ALTER TABLE team_agent_messages_widened RENAME TO team_agent_messages;
         RELEASE widen_authors;",
        columns = message_columns()
    ));
    if let Err(failure) = rebuilt {
        let _ = connection.execute_batch("ROLLBACK TO widen_authors; RELEASE widen_authors;");
        return Err(error(failure));
    }
    Ok(())
}

/// Creates the kept messages and the settings tables and the indexes that read one agent's
/// tasks, pinned tasks, events and messages by time, after widening an older kept messages
/// table's authors. Takes a connection with the team and event tables. Returns unit.
pub(super) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    widen_authors(connection)?;
    connection
        .execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS team_agent_messages ({columns});
            CREATE INDEX IF NOT EXISTS team_agent_messages_agent ON team_agent_messages(project, agent, created_ms);
            CREATE INDEX IF NOT EXISTS team_events_agent ON team_events(project, agent, created_ms);
            CREATE INDEX IF NOT EXISTS team_tasks_agent ON team_tasks(project, agent, created_ms);
            CREATE INDEX IF NOT EXISTS team_task_pins_agent ON team_task_pins(project, agent);
            CREATE TABLE IF NOT EXISTS team_message_settings (
                project TEXT PRIMARY KEY,
                keep_master INTEGER NOT NULL,
                kept INTEGER NOT NULL,
                progress INTEGER NOT NULL,
                results TEXT NOT NULL CHECK(results IN ('first_line','full')),
                owner_items INTEGER NOT NULL,
                page_size INTEGER NOT NULL
            );",
            columns = message_columns()
        ))
        .map_err(error)
}

/// Reads a project's Messages settings, the proposed defaults until the owner saves some.
/// Takes the connection and the project. Returns the settings, or
/// `team_message_settings_invalid` for a stored value outside the choices.
pub fn settings(
    connection: &Connection,
    project: &ProjectId,
) -> Result<MessageSettings, EngineError> {
    type Row = (bool, i64, bool, String, bool, i64);
    let row: Option<Row> = connection
        .query_row(
            "SELECT keep_master,kept,progress,results,owner_items,page_size
             FROM team_message_settings WHERE project=?1",
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
        .map_err(error)?;
    let Some((keep, kept, progress, results, owner, page)) = row else {
        return Ok(MessageSettings::default());
    };
    let invalid = || failure("team_message_settings_invalid");
    let settings = MessageSettings {
        keep_master_messages: keep,
        kept_per_project: u32::try_from(kept).map_err(|_| invalid())?,
        progress_shown: progress,
        results: ResultsShown::parse(&results).ok_or_else(invalid)?,
        owner_items_shown: owner,
        page_size: u16::try_from(page).map_err(|_| invalid())?,
    };
    settings.validate()?;
    Ok(settings)
}

/// Stores a project's Messages settings whole and reads them back in the owner's
/// transaction, so the stored settings are the ones saved. Takes the transaction, the
/// project and validated settings. Returns unit.
pub(super) fn save_settings(
    tx: &Transaction<'_>,
    project: &ProjectId,
    saved: &MessageSettings,
) -> Result<(), EngineError> {
    assert!(
        saved.validate().is_ok(),
        "owner settings are validated first"
    );
    tx.execute(
        "INSERT INTO team_message_settings(project,keep_master,kept,progress,results,owner_items,page_size)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(project) DO UPDATE SET keep_master=excluded.keep_master, kept=excluded.kept,
         progress=excluded.progress, results=excluded.results, owner_items=excluded.owner_items,
         page_size=excluded.page_size",
        params![
            project.as_str(),
            saved.keep_master_messages,
            saved.kept_per_project,
            saved.progress_shown,
            saved.results.name(),
            saved.owner_items_shown,
            saved.page_size
        ],
    )
    .map_err(error)?;
    assert_eq!(&settings(tx, project)?, saved);
    Ok(())
}

/// Keeps what the master, the owner or the engine said to a sub-agent once its terminal took
/// it: the text made plain, or no text for a master's message or answer when the project
/// keeps none. The project's oldest kept messages make room first, down to its kept count.
/// Takes the transaction, the project, the agent, what was said, the text and the time.
/// Returns the row id.
pub(crate) fn record(
    tx: &Transaction<'_>,
    project: &ProjectId,
    agent: i64,
    said: Said,
    text: &str,
    now: i64,
) -> Result<i64, EngineError> {
    assert!(agent > 0);
    let settings = settings(tx, project)?;
    let kept = said.author() != Speaker::Master || settings.keep_master_messages;
    let text = kept.then(|| bound_text(&events::plain_agent_text(text), SAID_BYTES_MAX));
    make_room(
        tx,
        "team_agent_messages",
        "1=1",
        project,
        i64::from(settings.kept_per_project),
    )?;
    tx.execute(
        "INSERT INTO team_agent_messages(project,agent,author,kind,text,created_ms) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            project.as_str(),
            agent,
            said.author().name(),
            said.kind().name(),
            text,
            now
        ],
    )
    .map_err(error)?;
    let id = tx.last_insert_rowid();
    assert!(id > 0);
    Ok(id)
}

/// Reads one page of a project sub-agent's Messages timeline as the owner gets it: `read`,
/// fitted to one owner reply by `fit`. Takes the connection, the project, the agent, the
/// cursor of the newest entry not wanted and the page size, none for the project's setting.
/// Returns the page, or a plain code.
pub fn owner_read(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    before: Option<Cursor>,
    limit: Option<u16>,
) -> Result<AgentMessages, EngineError> {
    fit(read(connection, project, agent, before, limit)?)
}

/// Reads one page of a project sub-agent's Messages timeline, newest first: the tasks it
/// took or was woken with, what the master and the owner said to it and its events, merged
/// by time. A finished or failed event carries its task's result or problem, so a task end
/// shows once. Every text is made plain and cut to `ENTRY_TEXT_BYTES_MAX` bytes; the speaker
/// comes from where the row is stored. Takes the connection, the project, the agent, the
/// cursor and the page size, none for the project's setting. Returns the page, or
/// `team_agent_not_found` or `team_messages_limit_invalid`.
pub fn read(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    before: Option<Cursor>,
    limit: Option<u16>,
) -> Result<AgentMessages, EngineError> {
    let known: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_agents WHERE id=?1 AND project=?2)",
            params![agent, project.as_str()],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !known {
        return Err(failure("team_agent_not_found"));
    }
    let settings = settings(connection, project)?;
    let limit = limit.unwrap_or(settings.page_size);
    if limit == 0 || limit > PAGE_MAX {
        return Err(failure("team_messages_limit_invalid"));
    }
    let page = Page {
        connection,
        project,
        agent,
        before,
        take: i64::from(limit) + 1,
        settings,
    };
    let mut entries = page.tasks()?;
    entries.extend(page.said()?);
    entries.extend(page.events()?);
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.cursor()));
    let older = entries.len() > usize::from(limit);
    entries.truncate(usize::from(limit));
    assert!(
        before.is_none_or(|before| entries.iter().all(|entry| entry.cursor() < before)),
        "a page holds only entries before its cursor"
    );
    Ok(AgentMessages {
        project_id: project.clone(),
        agent,
        entries,
        older,
        settings,
    })
}

/// One timeline read: each store gives at most `take` of its newest rows before the cursor,
/// so the merged newest `take - 1` are exact and one more says older entries remain.
struct Page<'a> {
    connection: &'a Connection,
    project: &'a ProjectId,
    agent: i64,
    before: Option<Cursor>,
    take: i64,
    settings: MessageSettings,
}

impl Page<'_> {
    /// Gives the time and row id bound a store's rows stay under for the cursor: rows of the
    /// cursor's millisecond count only from stores ordered before the cursor's, and in its own
    /// store only below its id. Takes the store. Returns the time and id bound.
    fn bound(&self, source: Source) -> (i64, i64) {
        match self.before {
            None => (i64::MAX, i64::MAX),
            Some(cursor) if source < cursor.source => (cursor.at_ms, i64::MAX),
            Some(cursor) if source == cursor.source => (cursor.at_ms, cursor.id),
            Some(cursor) => (cursor.at_ms, i64::MIN),
        }
    }

    /// Reads the agent's newest tasks before the cursor in two seeks: the tasks it took, each
    /// marked when it is also pinned to it, and the other tasks pinned to it to wake it, so
    /// each half reads one agent's rows by its own index and no task shows twice. Takes the
    /// page. Returns at most `take` entries, newest first.
    fn tasks(&self) -> Result<Vec<AgentMessage>, EngineError> {
        let (at, id) = self.bound(Source::Task);
        let mut statement = self
            .connection
            .prepare(
                "SELECT t.id AS id, t.source, t.prompt, t.created_ms AS created_ms,
                 EXISTS(SELECT 1 FROM team_task_pins p WHERE p.task=t.id AND p.agent=?2)
                 FROM team_tasks t
                 WHERE t.project=?1 AND t.agent=?2
                 AND (?3 OR t.source<>'owner')
                 AND (t.created_ms<?4 OR (t.created_ms=?4 AND t.id<?5))
                 UNION ALL
                 SELECT t.id, t.source, t.prompt, t.created_ms, 1
                 FROM team_task_pins p JOIN team_tasks t ON t.id=p.task
                 WHERE p.project=?1 AND p.agent=?2 AND t.project=?1 AND t.agent IS NOT ?2
                 AND (?3 OR t.source<>'owner')
                 AND (t.created_ms<?4 OR (t.created_ms=?4 AND t.id<?5))
                 ORDER BY created_ms DESC, id DESC LIMIT ?6",
            )
            .map_err(error)?;
        let rows = statement
            .query_map(
                params![
                    self.project.as_str(),
                    self.agent,
                    self.settings.owner_items_shown,
                    at,
                    id,
                    self.take
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, bool>(4)?,
                    ))
                },
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|(id, source, prompt, at_ms, pinned)| {
                let from = match source.as_str() {
                    "master" => Speaker::Master,
                    "owner" => Speaker::Owner,
                    _ => return Err(failure("team_task_source_invalid")),
                };
                Ok(AgentMessage {
                    at_ms,
                    source: Source::Task,
                    id,
                    from,
                    kind: if pinned {
                        EntryKind::Wake
                    } else {
                        EntryKind::Task
                    },
                    text: Some(entry_text(&prompt)),
                    task: Some(id),
                    state: None,
                    result: None,
                    result_more: false,
                })
            })
            .collect()
    }

    fn said(&self) -> Result<Vec<AgentMessage>, EngineError> {
        let (at, id) = self.bound(Source::Message);
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, author, kind, text, created_ms FROM team_agent_messages
                 WHERE project=?1 AND agent=?2 AND (?3 OR author<>'owner')
                 AND (created_ms<?4 OR (created_ms=?4 AND id<?5))
                 ORDER BY created_ms DESC, id DESC LIMIT ?6",
            )
            .map_err(error)?;
        let rows = statement
            .query_map(
                params![
                    self.project.as_str(),
                    self.agent,
                    self.settings.owner_items_shown,
                    at,
                    id,
                    self.take
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|(id, author, kind, text, at_ms)| {
                let from = Speaker::parse(&author)
                    .filter(|from| {
                        matches!(from, Speaker::Master | Speaker::Owner | Speaker::Engine)
                    })
                    .ok_or_else(|| failure("team_agent_message_invalid"))?;
                let kind =
                    EntryKind::said(&kind).ok_or_else(|| failure("team_agent_message_invalid"))?;
                Ok(AgentMessage {
                    at_ms,
                    source: Source::Message,
                    id,
                    from,
                    kind,
                    text: text.as_deref().map(entry_text),
                    task: None,
                    state: None,
                    result: None,
                    result_more: false,
                })
            })
            .collect()
    }

    fn events(&self) -> Result<Vec<AgentMessage>, EngineError> {
        let (at, id) = self.bound(Source::Event);
        let mut statement = self
            .connection
            .prepare(
                "SELECT e.id, e.kind, e.text, e.task, e.state, e.created_ms,
                 CASE e.kind WHEN 'finished' THEN t.result WHEN 'failed' THEN t.error END
                 FROM team_events e LEFT JOIN team_tasks t ON t.id=e.task AND t.project=e.project
                 WHERE e.project=?1 AND e.agent=?2 AND (?3 OR e.kind<>'progress')
                 AND (e.created_ms<?4 OR (e.created_ms=?4 AND e.id<?5))
                 ORDER BY e.created_ms DESC, e.id DESC LIMIT ?6",
            )
            .map_err(error)?;
        let rows = statement
            .query_map(
                params![
                    self.project.as_str(),
                    self.agent,
                    self.settings.progress_shown,
                    at,
                    id,
                    self.take
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|(id, kind, text, task, state, at_ms, ended)| {
                let invalid = || failure("team_event_invalid");
                let kind = EventKind::parse(&kind).ok_or_else(invalid)?;
                let state = EventState::parse(&state).ok_or_else(invalid)?;
                let (result, result_more) = match ended {
                    Some(ended) => {
                        let (shown, more) = self.result(&ended);
                        (Some(shown), more)
                    }
                    None => (None, false),
                };
                Ok(AgentMessage {
                    at_ms,
                    source: Source::Event,
                    id,
                    from: match kind {
                        EventKind::Question | EventKind::Finished | EventKind::Failed => {
                            Speaker::Agent
                        }
                        EventKind::ChangeSetReady | EventKind::Progress => Speaker::Engine,
                    },
                    kind: EntryKind::event(kind),
                    text: Some(entry_text(&text)),
                    task,
                    state: Some(state),
                    result,
                    result_more,
                })
            })
            .collect()
    }

    /// Shows a task's result or problem as the project's setting says: a preview of at most
    /// `RESULT_PREVIEW_BYTES_MAX` bytes, or the whole stored text. Takes the stored text.
    /// Returns the plain text shown and whether the stored one is longer.
    fn result(&self, stored: &str) -> (String, bool) {
        let plain = events::plain_agent_text(stored);
        let max = match self.settings.results {
            ResultsShown::FirstLine => RESULT_PREVIEW_BYTES_MAX,
            ResultsShown::Full => RESULT_BYTES_MAX,
        };
        let shown = bound_text(&plain, max);
        let more = shown.len() < plain.len();
        (shown, more)
    }
}

/// Makes one entry's text plain and cuts it to `ENTRY_TEXT_BYTES_MAX` bytes, saying how much
/// was left out. Takes the stored text. Returns the text shown.
fn entry_text(text: &str) -> String {
    cut_text(&events::plain_agent_text(text), ENTRY_TEXT_BYTES_MAX)
}

/// Fits a timeline page to one owner reply: every text and result is cut to one shared cap,
/// halved until the page fits `STATUS_REPLY_BYTES_MAX`; when the shortest cap still does not
/// fit, the oldest entries go and the page says older entries remain, so the next page
/// starts after the last one shown. Takes the page. Returns it, or `team_messages_bounds`
/// when even one entry at the shortest cap does not fit.
fn fit(page: AgentMessages) -> Result<AgentMessages, EngineError> {
    if encoded_len(&page)? <= STATUS_REPLY_BYTES_MAX {
        return Ok(page);
    }
    let cut_all = |page: &mut AgentMessages, cap: usize| {
        for entry in &mut page.entries {
            for text in entry.text.iter_mut().chain(entry.result.iter_mut()) {
                *text = cut_text(text, cap);
            }
        }
    };
    for cap in STATUS_TEXT_CAPS {
        let mut cut = page.clone();
        cut_all(&mut cut, cap);
        if encoded_len(&cut)? <= STATUS_REPLY_BYTES_MAX {
            assert_eq!(cut.entries.len(), page.entries.len());
            return Ok(cut);
        }
    }
    let mut cut = page;
    cut_all(&mut cut, STATUS_TEXT_CAPS[STATUS_TEXT_CAPS.len() - 1]);
    while cut.entries.len() > 1 {
        cut.entries.pop();
        cut.older = true;
        if encoded_len(&cut)? <= STATUS_REPLY_BYTES_MAX {
            return Ok(cut);
        }
    }
    Err(failure("team_messages_bounds"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scale_tests;
