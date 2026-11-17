use crate::cleanup_settings::CleanupSettings;
use crate::engine::EngineError;
use crate::harnesses::update::UpdateSettings;
use crate::instruction_files::{AppSettings, Settings};
use crate::master_frame_limit::MasterFrameSettings;
use crate::master_step_limit::MasterStepSettings;
use crate::master_time_limit::MasterTimeSettings;
use crate::protocol::{Command, Outcome, ProjectId};
use crate::provider_retry::ProviderRetry;
use crate::quit_settings::QuitSettings;
use crate::roadmap_status::Refresh;
use crate::sandbox_settings::SandboxSettings;
use crate::state_log::fence::Fence;
use crate::state_log::replay::{self, Clock, Logged};
use crate::state_log::{Aggregate, Appender, BODY_BYTES_MAX, Chunk, Draft, SNAPSHOT_CHUNK};
#[cfg(any(unix, test))]
use crate::storage::opening::Opening;
use rusqlite::types::Value as Column;
use rusqlite::{Connection, Transaction, TransactionBehavior, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
mod tests;

/// The kind of the event an owner sandbox settings save appends (v2.5 L10).
pub const KIND_SANDBOX: &str = "SandboxSettingsSaved";
/// The kind of the event an owner finished-agents cleanup save appends.
pub const KIND_CLEANUP: &str = "AgentCleanupSaved";
/// The kind of the event an owner agents-on-quit save appends.
pub const KIND_QUIT: &str = "AgentQuitSaved";
/// The kind of the event an owner provider retry save appends.
pub const KIND_RETRY: &str = "ProviderRetrySaved";
/// The kind of the event a harness update settings save appends.
pub const KIND_HARNESS_UPDATE: &str = "HarnessUpdateSaved";
/// The kind of the event an app-wide instruction settings save appends.
pub const KIND_INSTRUCTION_APP: &str = "InstructionAppSaved";
/// The kind of the event a project's instruction settings save appends.
pub const KIND_INSTRUCTION: &str = "InstructionProjectSaved";
/// The kind of the event a project's roadmap refresh save appends.
pub const KIND_ROADMAP: &str = "RoadmapRefreshSaved";
/// The kind of the event an owner master step limit save appends.
pub const KIND_STEPS: &str = "MasterStepsSaved";
/// The kind of the event an owner master time limit save appends.
pub const KIND_TIME: &str = "MasterTimeSaved";
/// The kind of the event an owner master frame limit save appends.
pub const KIND_FRAMES: &str = "MasterFramesSaved";
/// Every settings kind, in [`TABLES`] order.
pub const KINDS: [&str; 11] = [
    KIND_SANDBOX,
    KIND_CLEANUP,
    KIND_QUIT,
    KIND_RETRY,
    KIND_HARNESS_UPDATE,
    KIND_INSTRUCTION_APP,
    KIND_INSTRUCTION,
    KIND_ROADMAP,
    KIND_STEPS,
    KIND_TIME,
    KIND_FRAMES,
];
/// The schema every settings event is written and read at.
const SCHEMA: u16 = 1;
/// Every settings table in snapshot order, each with its columns in key order.
const TABLES: [(&str, &str); 11] = [
    (
        "sandbox_settings",
        "singleton,linux_sockets,linux_supervisor,linux_strict",
    ),
    ("agent_cleanup_settings", "singleton,mode,days"),
    (
        "agent_quit_settings",
        "singleton,on_quit,left_running,hours",
    ),
    ("provider_retry_settings", "singleton,choice"),
    (
        "harness_update_settings",
        "id,mode,check_when,online,install",
    ),
    ("agent_instruction_app_settings", "singleton,edit_with_ai"),
    (
        "agent_instruction_settings",
        "project,mode,transcripts,edits,repeated,edit_with_ai",
    ),
    ("roadmap_status_settings", "project,refresh"),
    ("configured_master_steps", "singleton,step_limit"),
    ("configured_master_time", "singleton,time_limit"),
    ("configured_master_frames", "singleton,frame_limit"),
];
/// The two per-project settings tables a project removal clears.
const PROJECT_TABLES: [&str; 2] = ["agent_instruction_settings", "roadmap_status_settings"];
/// The most rows one table may bring to a snapshot run: one per project at most for the
/// per-project tables, one for the rest.
const SNAPSHOT_ROWS_MAX: usize = 65_536;
/// The bytes of a chunk body kept for its envelope (aggregate, table, index) around rows.
const CHUNK_ENVELOPE_BYTES: usize = 1_024;
/// The widest settings table, in columns.
const COLUMNS_MAX: usize = 6;

const _: () = assert!(KINDS.len() == TABLES.len());

/// An app-wide setting saved: the setting the table's one row stores. Each setting is an
/// enum, a bounded number or a switch, and refuses any field more, so no text rides in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved<T> {
    pub(crate) settings: T,
}

/// A project's setting saved: the project, the key of its row, and the setting the row
/// stores, an enum, bounded number or switch set that refuses any field more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSaved<T> {
    pub(crate) project: ProjectId,
    pub(crate) settings: T,
}

/// One settings event, one kind per settings table. Each carries the typed setting its row
/// stores and, for a project setting, the project id; never a secret or free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Sandbox(Saved<SandboxSettings>),
    Cleanup(Saved<CleanupSettings>),
    Quit(Saved<QuitSettings>),
    Retry(Saved<ProviderRetry>),
    HarnessUpdate(Saved<UpdateSettings>),
    InstructionApp(Saved<AppSettings>),
    Instruction(ProjectSaved<Settings>),
    Roadmap(ProjectSaved<Refresh>),
    Steps(Saved<MasterStepSettings>),
    Time(Saved<MasterTimeSettings>),
    Frames(Saved<MasterFrameSettings>),
}

impl Event {
    /// The event's kind in the log. Output: the kind.
    fn kind(&self) -> &'static str {
        match self {
            Self::Sandbox(_) => KIND_SANDBOX,
            Self::Cleanup(_) => KIND_CLEANUP,
            Self::Quit(_) => KIND_QUIT,
            Self::Retry(_) => KIND_RETRY,
            Self::HarnessUpdate(_) => KIND_HARNESS_UPDATE,
            Self::InstructionApp(_) => KIND_INSTRUCTION_APP,
            Self::Instruction(_) => KIND_INSTRUCTION,
            Self::Roadmap(_) => KIND_ROADMAP,
            Self::Steps(_) => KIND_STEPS,
            Self::Time(_) => KIND_TIME,
            Self::Frames(_) => KIND_FRAMES,
        }
    }

    /// The event's body in the log, its fields as JSON. Output: the bytes.
    fn body(&self) -> Result<Vec<u8>, EngineError> {
        match self {
            Self::Sandbox(event) => serde_json::to_vec(event),
            Self::Cleanup(event) => serde_json::to_vec(event),
            Self::Quit(event) => serde_json::to_vec(event),
            Self::Retry(event) => serde_json::to_vec(event),
            Self::HarnessUpdate(event) => serde_json::to_vec(event),
            Self::InstructionApp(event) => serde_json::to_vec(event),
            Self::Instruction(event) => serde_json::to_vec(event),
            Self::Roadmap(event) => serde_json::to_vec(event),
            Self::Steps(event) => serde_json::to_vec(event),
            Self::Time(event) => serde_json::to_vec(event),
            Self::Frames(event) => serde_json::to_vec(event),
        }
        .map_err(error)
    }
}

/// Writes one settings event: the projection applies it in the appender's transaction, then
/// the log appends it, so an event is logged only when its row was written. Each kind has
/// its own draft naming its kind constant. A refusal leaves the transaction for the caller
/// to roll back. Inputs: the appender, the writer's clock and the event. Output: the event's
/// `seq`, or the first refusal.
pub fn record(
    appender: &mut Appender<'_, '_>,
    at_ms: u64,
    event: &Event,
) -> Result<u64, EngineError> {
    let body = event.body()?;
    appender.project(|tx| apply(tx, event))?;
    let aggregate = Aggregate::Settings;
    let seq = match event {
        Event::Sandbox(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_SANDBOX,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Cleanup(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_CLEANUP,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Quit(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_QUIT,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Retry(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_RETRY,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::HarnessUpdate(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_HARNESS_UPDATE,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::InstructionApp(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_INSTRUCTION_APP,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Instruction(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_INSTRUCTION,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Roadmap(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_ROADMAP,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Steps(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_STEPS,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Time(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_TIME,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
        Event::Frames(_) => appender.append(&Draft {
            aggregate,
            kind: KIND_FRAMES,
            schema: SCHEMA,
            at_ms,
            body: &body,
        }),
    }?;
    assert!(seq > 0, "a logged event has a seq");
    assert!(
        body.len() <= BODY_BYTES_MAX,
        "the log bounds what it appended"
    );
    Ok(seq)
}

/// Applies one settings event to its table, for the live writers, the test seeders and
/// replay alike: the bare upsert of the row, its words the setting's stored names. It runs
/// no check and no other effect, so replay never judges an old event by today's rules.
/// Inputs: the connection and the event. Output: Ok, or the store's refusal of the row.
pub fn apply(db: &Connection, event: &Event) -> Result<(), EngineError> {
    let count = match event {
        Event::Sandbox(row) => db.execute(
            "INSERT INTO sandbox_settings(singleton,linux_sockets,linux_supervisor,linux_strict) VALUES(1,?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET linux_sockets=excluded.linux_sockets, linux_supervisor=excluded.linux_supervisor, linux_strict=excluded.linux_strict",
            params![
                word(row.settings.linux_sockets)?,
                word(row.settings.linux_supervisor)?,
                word(row.settings.linux_strict)?
            ],
        ),
        Event::Cleanup(row) => db.execute(
            "INSERT INTO agent_cleanup_settings(singleton,mode,days) VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET mode=excluded.mode, days=excluded.days",
            params![word(row.settings.mode)?, row.settings.days],
        ),
        Event::Quit(row) => db.execute(
            "INSERT INTO agent_quit_settings(singleton,on_quit,left_running,hours) VALUES(1,?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET on_quit=excluded.on_quit, left_running=excluded.left_running, hours=excluded.hours",
            params![
                word(row.settings.on_quit)?,
                word(row.settings.left_running)?,
                row.settings.hours
            ],
        ),
        Event::Retry(row) => db.execute(
            "INSERT INTO provider_retry_settings(singleton,choice) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET choice=excluded.choice",
            [word(row.settings)?],
        ),
        Event::HarnessUpdate(row) => db.execute(
            "INSERT INTO harness_update_settings(id,mode,check_when,online,install) VALUES(1,?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET mode=excluded.mode, check_when=excluded.check_when, online=excluded.online, install=excluded.install",
            params![
                word(row.settings.mode)?,
                word(row.settings.when)?,
                word(row.settings.online)?,
                word(row.settings.install)?
            ],
        ),
        Event::InstructionApp(row) => db.execute(
            "INSERT INTO agent_instruction_app_settings(singleton, edit_with_ai) VALUES (1, ?1) ON CONFLICT(singleton) DO UPDATE SET edit_with_ai=excluded.edit_with_ai",
            params![i64::from(row.settings.edit_with_ai)],
        ),
        Event::Instruction(row) => db.execute(
            "INSERT INTO agent_instruction_settings(project, mode, transcripts, edits, repeated, edit_with_ai) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(project) DO UPDATE SET mode=excluded.mode, transcripts=excluded.transcripts, edits=excluded.edits, repeated=excluded.repeated, edit_with_ai=excluded.edit_with_ai",
            params![
                row.project.as_str(),
                row.settings.mode.as_str(),
                i64::from(row.settings.transcripts),
                row.settings.edits.as_str(),
                row.settings.repeated.as_str(),
                i64::from(row.settings.edit_with_ai)
            ],
        ),
        Event::Roadmap(row) => db.execute(
            "INSERT INTO roadmap_status_settings(project,refresh) VALUES(?1,?2) ON CONFLICT(project) DO UPDATE SET refresh=excluded.refresh",
            params![row.project.as_str(), row.settings.name()],
        ),
        Event::Steps(row) => db.execute(
            "INSERT INTO configured_master_steps(singleton,step_limit) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET step_limit=excluded.step_limit",
            [word(row.settings.limit)?],
        ),
        Event::Time(row) => db.execute(
            "INSERT INTO configured_master_time(singleton,time_limit) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET time_limit=excluded.time_limit",
            [word(row.settings.limit)?],
        ),
        Event::Frames(row) => db.execute(
            "INSERT INTO configured_master_frames(singleton,frame_limit) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET frame_limit=excluded.frame_limit",
            [word(row.settings.limit)?],
        ),
    }
    .map_err(error)?;
    assert_eq!(count, 1, "a settings event writes one keyed row");
    Ok(())
}

/// Reads one logged settings event: one of the settings kinds at schema 1, its body exactly
/// that kind's fields with known words. Input: the logged event. Output: the event, or
/// `settings_log_event_invalid`.
pub fn decode(logged: &Logged) -> Result<Event, EngineError> {
    let invalid = || error("settings_log_event_invalid");
    if logged.aggregate != Aggregate::Settings || logged.schema != SCHEMA {
        return Err(invalid());
    }
    let body = logged.body.as_slice();
    let event = match logged.kind.as_str() {
        KIND_SANDBOX => serde_json::from_slice(body).map(Event::Sandbox),
        KIND_CLEANUP => serde_json::from_slice(body).map(Event::Cleanup),
        KIND_QUIT => serde_json::from_slice(body).map(Event::Quit),
        KIND_RETRY => serde_json::from_slice(body).map(Event::Retry),
        KIND_HARNESS_UPDATE => serde_json::from_slice(body).map(Event::HarnessUpdate),
        KIND_INSTRUCTION_APP => serde_json::from_slice(body).map(Event::InstructionApp),
        KIND_INSTRUCTION => serde_json::from_slice(body).map(Event::Instruction),
        KIND_ROADMAP => serde_json::from_slice(body).map(Event::Roadmap),
        KIND_STEPS => serde_json::from_slice(body).map(Event::Steps),
        KIND_TIME => serde_json::from_slice(body).map(Event::Time),
        KIND_FRAMES => serde_json::from_slice(body).map(Event::Frames),
        _ => return Err(invalid()),
    }
    .map_err(|_| invalid())?;
    assert_eq!(event.kind(), logged.kind);
    Ok(event)
}

/// Rebuilds every settings table from the log: clears them, then replays every settings
/// event in order. A snapshot run holds every row of all of them, so its first chunk clears
/// them again; a chunk opens a run unless it follows another chunk at the previous `seq`. A
/// project removal deletes its project's per-project settings at its place in the log, since
/// a snapshot run before it can still hold them (v2.5 L10 Q4). The tables must exist (the
/// open and step 14 make them). Input: the transaction. Output: the last settings `seq` read,
/// or the first refusal.
pub fn rebuild(tx: &Transaction<'_>) -> Result<u64, EngineError> {
    let mut removals = crate::worktrees::log::removals(tx)?.into_iter().peekable();
    clear(tx)?;
    let mut chunk_seq = 0;
    let last = replay::replay(tx, Aggregate::Settings, 0, |logged| {
        while let Some((_, project)) = removals.next_if(|(seq, _)| *seq < logged.seq) {
            forget_rows(tx, &project)?;
        }
        if logged.kind != SNAPSHOT_CHUNK {
            return apply(tx, &decode(logged)?);
        }
        if chunk_seq == 0 || logged.seq != chunk_seq + 1 {
            clear(tx)?;
        }
        chunk_seq = logged.seq;
        restore(tx, &replay::chunk(logged)?)
    })?;
    for (_, project) in removals {
        forget_rows(tx, &project)?;
    }
    assert!(
        last >= chunk_seq,
        "replay reads past every chunk it applied"
    );
    Ok(last)
}

/// Appends a snapshot run of every settings table as it stands, each in chunks that fit one
/// event body and at least one chunk each, so replay of a run always clears and fills all of
/// them. Inputs: the appender and the writer's clock. Output: the `seq` of the run's closing
/// event, or `settings_log_snapshot_rows`, `settings_log_row_too_large` or the log's
/// refusal.
pub fn snapshot(appender: &mut Appender<'_, '_>, at_ms: u64) -> Result<u64, EngineError> {
    let mut chunks = Vec::new();
    for (table, columns) in TABLES {
        let rows = appender.project(|tx| table_rows(tx, table, columns))?;
        chunks.extend(pack(table, rows)?);
    }
    assert!(chunks.len() >= TABLES.len(), "one chunk or more per table");
    assert_eq!(chunks[0].table, TABLES[0].0);
    appender.snapshot(Aggregate::Settings, at_ms, chunks)
}

/// Makes every settings table that is missing. The engine's open calls it after its schema
/// steps, so every store holds all of them, and step 14 calls it before its run. Input: the
/// connection. Output: Ok, or the first table's refusal.
#[cfg(any(unix, test))]
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    crate::sandbox_settings::initialize(db)?;
    crate::cleanup_settings::initialize(db)?;
    crate::quit_settings::initialize(db)?;
    crate::provider_retry::initialize(db)?;
    crate::harnesses::update::initialize(db)?;
    crate::instruction_files::initialize(db).map_err(error)?;
    crate::roadmap_status::initialize(db)?;
    crate::master_step_limit::initialize(db)?;
    crate::master_time_limit::initialize(db)?;
    crate::master_frame_limit::initialize(db)?;
    Ok(())
}

/// The first snapshot run of every settings table, for the schema step that puts settings
/// on the log (step 14): rows written before the log reach replay through it, through the
/// open's proof. The step runs before the open makes the tables, so it makes any one an older
/// engine never made first. A store without the `state` table has no log to write, and one with
/// no settings row needs no run, so it moves no revision. Inputs: the step's transaction and
/// the open's proof. Output: whether a run was appended, or the first refusal.
#[cfg(any(unix, test))]
pub(crate) fn first_run(tx: &Transaction<'_>, opening: Opening) -> Result<bool, EngineError> {
    let stated: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='state')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !stated {
        return Ok(false);
    }
    initialize(tx)?;
    let mut rows = 0;
    for (table, _) in TABLES {
        let count: i64 = tx
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(error)?;
        rows += count;
    }
    if rows == 0 {
        return Ok(false);
    }
    let at_ms = opening.at_ms();
    let mut appender = Appender::opening(tx, opening)?;
    let done = snapshot(&mut appender, at_ms)?;
    assert!(done > 0, "a run closes at a seq");
    assert!(rows > 0);
    Ok(true)
}

/// Writes one owner settings save inside the owner request, at its signed next revision
/// (v2.5 L10): each check that can refuse the save runs first, so a refused save moves no
/// revision and logs nothing; then the event is applied and logged at next. Inputs: the
/// request's transaction, the command and the revision the request read. Output: `Applied`,
/// the check's rejection, a check's refusal or `settings_log_command` for another command.
pub(crate) fn owner(
    tx: &Transaction<'_>,
    command: &Command,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let event = match command {
        Command::OwnerSandbox { settings, version } => {
            if let Some(rejected) = crate::sandbox_settings::admit(tx, *version)? {
                return Ok(rejected);
            }
            Event::Sandbox(Saved {
                settings: *settings,
            })
        }
        Command::OwnerAgentCleanup { settings } => {
            crate::cleanup_settings::check(settings)?;
            Event::Cleanup(Saved {
                settings: *settings,
            })
        }
        Command::OwnerAgentQuit { settings } => {
            if let Some(rejected) = crate::quit_settings::check(settings) {
                return Ok(rejected);
            }
            Event::Quit(Saved {
                settings: *settings,
            })
        }
        Command::OwnerProviderRetry { choice } => Event::Retry(Saved { settings: *choice }),
        Command::OwnerMasterSteps { settings } => Event::Steps(Saved {
            settings: *settings,
        }),
        Command::OwnerMasterTime { settings } => Event::Time(Saved {
            settings: *settings,
        }),
        Command::OwnerMasterFrames { settings } => Event::Frames(Saved {
            settings: *settings,
        }),
        _ => return Err(error("settings_log_command")),
    };
    let next = revision
        .checked_add(1)
        .ok_or_else(|| error("revision_exhausted"))?;
    record_at(tx, next, &event)?;
    assert!(!tx.is_autocommit(), "the save is part of the request");
    Ok(Outcome::Applied)
}

/// Saves a project's instruction settings for the owner transport, in its transaction:
/// makes the table when missing, then applies and logs the save behind the store's engine
/// host. Inputs: the transaction, the project and the settings. Output: Ok, or
/// `instruction_settings_unavailable`, which the caller rolls back.
pub(crate) fn instruction(
    tx: &Transaction<'_>,
    project: &ProjectId,
    settings: Settings,
) -> Result<(), EngineError> {
    const CODE: &str = "instruction_settings_unavailable";
    crate::instruction_files::initialize(tx).map_err(coded(CODE))?;
    let event = Event::Instruction(ProjectSaved {
        project: project.clone(),
        settings,
    });
    record_own(tx, &event).map_err(coded(CODE))?;
    Ok(())
}

/// Saves the app-wide instruction settings for the owner transport, in its transaction:
/// makes the table when missing, then applies and logs the save behind the store's engine
/// host. Inputs: the transaction and the settings. Output: Ok, or
/// `instruction_settings_save_unavailable`, which the caller rolls back.
pub(crate) fn instruction_app(
    tx: &Transaction<'_>,
    settings: AppSettings,
) -> Result<(), EngineError> {
    const CODE: &str = "instruction_settings_save_unavailable";
    crate::instruction_files::initialize(tx).map_err(coded(CODE))?;
    record_own(tx, &Event::InstructionApp(Saved { settings })).map_err(coded(CODE))?;
    Ok(())
}

/// Saves when a project's roadmap status is refreshed, in its own transaction: applies and
/// logs the save behind the store's engine host, then settles the project's status row the
/// way the choice needs (`roadmap_status::settle`), a live effect replay does not repeat.
/// Inputs: the engine connection outside any transaction, the project and the choice.
/// Output: Ok, or `roadmap_status_unavailable` with nothing changed.
pub(crate) fn roadmap_refresh(
    db: &Connection,
    project: &ProjectId,
    refresh: Refresh,
) -> Result<(), EngineError> {
    const CODE: &str = "roadmap_status_unavailable";
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(coded(CODE))?;
    let event = Event::Roadmap(ProjectSaved {
        project: project.clone(),
        settings: refresh,
    });
    record_own(&tx, &event).map_err(coded(CODE))?;
    crate::roadmap_status::settle(&tx, project, refresh)?;
    tx.commit().map_err(coded(CODE))?;
    assert!(db.is_autocommit(), "the save committed");
    Ok(())
}

/// Saves the owner's harness update settings in their own transaction: applies and logs the
/// save behind the store's engine host. Inputs: the engine connection outside any
/// transaction and the settings. Output: Ok, or `harness_update_settings_unreadable` with
/// nothing changed.
pub(crate) fn harness_update(db: &Connection, settings: UpdateSettings) -> Result<(), EngineError> {
    const CODE: &str = "harness_update_settings_unreadable";
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(coded(CODE))?;
    record_own(&tx, &Event::HarnessUpdate(Saved { settings })).map_err(coded(CODE))?;
    tx.commit().map_err(coded(CODE))?;
    assert!(db.is_autocommit(), "the save committed");
    Ok(())
}

/// Writes one settings event for an engine writer in its own transaction: `own` moves the
/// `state` revision once behind the store's engine host and writes its `events` row, then
/// the event is applied and logged at it; before an owner runtime starts the store it is
/// refused (v2.5 L6 Q9). Inputs: the transaction and the event. Output: the revision it
/// moved to, or the first refusal, which the caller rolls back.
pub(crate) fn record_own(tx: &Transaction<'_>, event: &Event) -> Result<u64, EngineError> {
    let fence = crate::worktrees::log::store_fence(tx)?;
    let mut appender = Appender::own(tx, fence)?;
    let seq = record(&mut appender, Clock::System.now_ms(), event)?;
    let revision = appender.revision();
    assert!(seq > 0, "a recorded event has a seq");
    assert!(revision > 0, "own moved the revision");
    Ok(revision)
}

/// Writes one settings event inside an owner request whose next revision is already signed:
/// moves `state` from one before it to it, then applies and logs the event there through
/// `owner`, so the request's own `events` row at next covers the change. Inputs: the
/// request's transaction, its next revision and the event. Output: Ok,
/// `state_log_revision_stale` when `state` is not one before next, or the first refusal.
fn record_at(tx: &Transaction<'_>, next: u64, event: &Event) -> Result<(), EngineError> {
    let stale = || error("state_log_revision_stale");
    let before = next.checked_sub(1).ok_or_else(stale)?;
    let moved = tx
        .execute(
            "UPDATE state SET revision=?1 WHERE id=1 AND revision=?2",
            params![
                i64::try_from(next).map_err(error)?,
                i64::try_from(before).map_err(error)?
            ],
        )
        .map_err(error)?;
    if moved != 1 {
        return Err(stale());
    }
    let fence = crate::worktrees::log::store_fence(tx)?;
    let mut appender = Appender::owner(tx, next, fence)?;
    let seq = record(&mut appender, Clock::System.now_ms(), event)?;
    assert!(seq > 0, "a recorded event has a seq");
    assert_eq!(appender.revision(), next);
    Ok(())
}

/// Tombstones, for a project removal or a demo clear, every logged instruction and roadmap
/// settings event of the project, so its id leaves the log's settings events; snapshot runs
/// keep their rows until compaction (L1b) rewrites them. The caller checkpoints the WAL
/// after its commit. Inputs: the transaction, the project, the removal's own `seq` (none for
/// a clear, which logs no removal) and the fence. Output: how many events were tombstoned,
/// or the redaction's refusal.
pub(crate) fn forget_project(
    tx: &Transaction<'_>,
    project: &str,
    keep: Option<u64>,
    fence: &Fence,
) -> Result<usize, EngineError> {
    if let Some(seq) = keep {
        assert!(seq > 0, "the removal is logged before its redaction");
    }
    let kinds = [KIND_INSTRUCTION, KIND_ROADMAP];
    let redacted = replay::redact_matching(
        tx,
        &[Aggregate::Settings],
        Some(&kinds),
        "project",
        project,
        keep.unwrap_or(0),
        fence,
    )?;
    assert!(!tx.is_autocommit(), "the redaction is part of the removal");
    Ok(redacted)
}

/// Every row of one settings table in key order, each as a JSON array of its columns, a
/// text as a string and an integer as a number. Inputs: the connection, the table and its
/// columns. Output: the rows, or `settings_log_snapshot_rows` past [`SNAPSHOT_ROWS_MAX`] or
/// for a column that is neither.
fn table_rows(db: &Connection, table: &str, columns: &str) -> Result<Vec<Value>, EngineError> {
    let width = columns.split(',').count();
    assert!((2..=COLUMNS_MAX).contains(&width));
    let mut statement = db
        .prepare(&format!(
            "SELECT {columns} FROM {table} ORDER BY {columns} LIMIT ?1"
        ))
        .map_err(error)?;
    let limit = i64::try_from(SNAPSHOT_ROWS_MAX + 1).map_err(error)?;
    let stored = statement
        .query_map([limit], |row| {
            (0..width)
                .map(|column| row.get::<_, Column>(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    if stored.len() > SNAPSHOT_ROWS_MAX {
        return Err(error("settings_log_snapshot_rows"));
    }
    let rows = stored
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|column| match column {
                    Column::Integer(number) => Ok(Value::from(number)),
                    Column::Text(text) => Ok(Value::String(text)),
                    _ => Err(error("settings_log_snapshot_rows")),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        rows.iter()
            .all(|row| row.as_array().is_some_and(|row| row.len() == width))
    );
    Ok(rows)
}

/// Splits one table's rows, in order, into chunks whose rows fit one event body with room
/// for the chunk's envelope; a table with no rows is one empty chunk. Inputs: the table and
/// its rows. Output: the chunks, or `settings_log_row_too_large`.
fn pack(table: &str, rows: Vec<Value>) -> Result<Vec<Chunk>, EngineError> {
    let budget = BODY_BYTES_MAX - CHUNK_ENVELOPE_BYTES;
    let total = rows.len();
    let mut chunks = vec![Chunk {
        table: table.into(),
        rows: Vec::new(),
    }];
    let mut used = 0;
    for row in rows {
        let bytes = serde_json::to_vec(&row).map_err(error)?.len() + 1;
        if bytes > budget {
            return Err(error("settings_log_row_too_large"));
        }
        if used + bytes > budget {
            chunks.push(Chunk {
                table: table.into(),
                rows: Vec::new(),
            });
            used = 0;
        }
        used += bytes;
        if let Some(chunk) = chunks.last_mut() {
            chunk.rows.push(row);
        }
    }
    assert_eq!(
        chunks.iter().map(|chunk| chunk.rows.len()).sum::<usize>(),
        total
    );
    assert!(chunks.iter().all(|chunk| chunk.table == table));
    Ok(chunks)
}

/// Inserts one snapshot chunk's rows into its table. Inputs: the connection and the chunk.
/// Output: Ok, or `settings_log_chunk_invalid` for an unknown table, a row that is not that
/// table's columns as strings and integers, or a row the table refuses.
fn restore(db: &Connection, chunk: &Chunk) -> Result<(), EngineError> {
    let invalid = || error("settings_log_chunk_invalid");
    let (table, columns) = TABLES
        .into_iter()
        .find(|(table, _)| *table == chunk.table)
        .ok_or_else(invalid)?;
    if chunk.rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(invalid());
    }
    let width = columns.split(',').count();
    let marks = (1..=width)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>();
    let mut statement = db
        .prepare(&format!(
            "INSERT INTO {table}({columns}) VALUES ({})",
            marks.join(",")
        ))
        .map_err(error)?;
    for row in &chunk.rows {
        let values = row
            .as_array()
            .filter(|values| values.len() == width)
            .ok_or_else(invalid)?;
        let stored = values
            .iter()
            .map(|value| match value {
                Value::String(text) => Ok(Column::Text(text.clone())),
                Value::Number(number) => number.as_i64().map(Column::Integer).ok_or_else(invalid),
                _ => Err(invalid()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        statement
            .execute(params_from_iter(stored))
            .map_err(|_| invalid())?;
    }
    assert!((2..=COLUMNS_MAX).contains(&width));
    assert!(chunk.rows.len() <= SNAPSHOT_ROWS_MAX);
    Ok(())
}

/// Deletes every row of every settings table. Input: the connection. Output: Ok or the
/// store's refusal.
fn clear(db: &Connection) -> Result<(), EngineError> {
    for (table, _) in TABLES {
        db.execute(&format!("DELETE FROM {table}"), [])
            .map_err(error)?;
    }
    Ok(())
}

/// Deletes one project's per-project settings rows, as its removal's projection does.
/// Inputs: the connection and the project. Output: Ok or the store's refusal.
fn forget_rows(db: &Connection, project: &str) -> Result<(), EngineError> {
    for table in PROJECT_TABLES {
        db.execute(&format!("DELETE FROM {table} WHERE project=?1"), [project])
            .map_err(error)?;
    }
    Ok(())
}

/// The stored word of one setting value, as its serde name. Input: the value. Output: the
/// word, or `settings_log_word` for a value that is not one word.
fn word(value: impl Serialize) -> Result<String, EngineError> {
    match serde_json::to_value(value).map_err(error)? {
        Value::String(word) => Ok(word),
        _ => Err(error("settings_log_word")),
    }
}

/// An engine error from any failure. Input: the failure. Output: the error.
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// A mapping of any failure to one writer's client code. Input: the code. Output: the
/// mapping.
fn coded<E>(code: &'static str) -> impl Fn(E) -> EngineError {
    move |_| error(code)
}
