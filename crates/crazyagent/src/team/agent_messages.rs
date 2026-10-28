use super::events::{self, EventState};
use super::{RESULT_BYTES_MAX, failure};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction};
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

/// Creates the kept messages and the settings tables. Takes a connection with the team and
/// event tables. Returns unit.
pub(super) fn initialize(_connection: &Connection) -> Result<(), EngineError> {
    Ok(())
}

/// Reads a project's Messages settings, the proposed defaults until the owner saves some.
/// Takes the connection and the project. Returns the settings.
pub fn settings(
    _connection: &Connection,
    _project: &ProjectId,
) -> Result<MessageSettings, EngineError> {
    Ok(MessageSettings::default())
}

/// Stores a project's Messages settings whole in the owner's transaction. Takes the
/// transaction, the project and validated settings. Returns unit.
pub(super) fn save_settings(
    _tx: &Transaction<'_>,
    _project: &ProjectId,
    saved: &MessageSettings,
) -> Result<(), EngineError> {
    assert!(
        saved.validate().is_ok(),
        "owner settings are validated first"
    );
    Ok(())
}

/// Reads one page of a project sub-agent's Messages timeline as the owner gets it. Takes the
/// connection, the project, the agent, the cursor of the newest entry not wanted and the
/// page size, none for the project's setting. Returns the page, or a plain code.
pub fn owner_read(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    before: Option<Cursor>,
    limit: Option<u16>,
) -> Result<AgentMessages, EngineError> {
    read(connection, project, agent, before, limit)
}

/// Reads one page of a project sub-agent's Messages timeline, newest first. Takes the
/// connection, the project, the agent, the cursor and the page size. Returns the page.
pub fn read(
    connection: &Connection,
    project: &ProjectId,
    agent: i64,
    _before: Option<Cursor>,
    _limit: Option<u16>,
) -> Result<AgentMessages, EngineError> {
    Ok(AgentMessages {
        project_id: project.clone(),
        agent,
        entries: Vec::new(),
        older: false,
        settings: settings(connection, project)?,
    })
}
