#[cfg(target_os = "linux")]
mod display_linux;
pub mod hold;
#[cfg(target_os = "linux")]
mod hold_linux;
#[cfg(target_os = "macos")]
mod hold_macos;
#[cfg(windows)]
mod hold_windows;
pub mod journal;
#[cfg(any(windows, test))]
pub(crate) mod lid_windows;
#[cfg(target_os = "linux")]
mod low_power_linux;
pub mod power;
pub mod power_linux;
pub mod power_macos;
#[cfg(any(test, target_os = "linux"))]
mod power_saver;
#[cfg(windows)]
mod power_windows;
pub mod settings;
pub mod state;
pub mod work;

#[cfg(test)]
mod tests;

pub use hold::{Held, Hold, HoldError, REASON, platform_hold};
pub use settings::{
    BatteryFloor, Cap, DisplayAwake, Grace, HeatEnd, HeatNotice, KeepAwakeSettings, LidDisplay,
    LidRule, LowPower, Mode,
};
pub use state::{
    Decision, EndReason, Heat, Inputs, LidSupport, LowPowerSupport, PowerSource, Reason,
    Sensors, Want, decide, heat_notice, lid_status, low_power_wanted,
};

use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::paths::Platform;
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How often the keeper evaluates when nothing changed.
pub const TICK_MS: i64 = 5000;

/// How often the keeper looks for a changed work count or an owner change.
const LOOK_MS: i64 = 1000;

/// The longest the engine stop waits for the lid line to release its lease.
pub const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// The longest manual timer, and the furthest Extend may move one from now: 24 hours.
pub const MINUTES_MAX: u16 = 1440;

const MINUTE_MS: i64 = 60 * 1000;
const STATE_BYTES_MAX: usize = 4096;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS keep_awake_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    dirty INTEGER NOT NULL CHECK(dirty IN (0,1)),
    state TEXT NOT NULL
)";

/// One owner change to Keep awake: start a manual timer of 1 to 1440 minutes, move a
/// running timer later, end the hold now, save the settings, a macOS lid helper command:
/// Turn on lid-closed hold, or Remove lid helper (QUESTION 35i, 35j, 36a), or Put the lid
/// setting back now on Windows (DEC-167 addendum 32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum KeepAwakeAction {
    Start { minutes: u16 },
    Extend { minutes: u16 },
    End,
    Set { settings: KeepAwakeSettings },
    TurnOnLidHold,
    RemoveLidHelper,
    PutLidBack,
}

impl KeepAwakeAction {
    /// Checks the bounds a change carries by itself. Input: the change. Output: Ok, or
    /// `keep_awake_minutes_invalid` for minutes outside 1 to 1440.
    pub fn validate(&self) -> Result<(), EngineError> {
        let minutes = match self {
            Self::Start { minutes } | Self::Extend { minutes } => *minutes,
            Self::End
            | Self::Set { .. }
            | Self::TurnOnLidHold
            | Self::RemoveLidHelper
            | Self::PutLidBack => return Ok(()),
        };
        if !(1..=MINUTES_MAX).contains(&minutes) {
            return Err(error("keep_awake_minutes_invalid"));
        }
        Ok(())
    }
}

/// The owner's sentence for a refused Keep awake change, the same words in the window and in
/// the TUI. Input: the refusal code. Output: the sentence, or none for any other code.
pub fn refusal_words(code: &str) -> Option<&'static str> {
    match code {
        "keep_awake_off" => Some("Keep awake is Off. Pick Auto or Manual first."),
        "keep_awake_no_timer" => Some("The timer already ended. Start a new one."),
        "keep_awake_not_holding" => Some("Nothing is held right now."),
        "keep_awake_minutes_invalid" => Some("Enter whole minutes from 1 to 1440."),
        "keep_awake_no_lid_helper" => Some("Lid-closed hold is set up only on macOS."),
        "keep_awake_no_lid_setting" => Some("Put the lid setting back is only on Windows."),
        "keep_awake_lid_held" => Some("The lid setting goes back when this hold ends."),
        _ => None,
    }
}

/// What the owner sees of Keep awake: the settings, the last decision (with the reason the
/// last hold ended until a new one starts), whether the lid is covered, what the platform
/// itself says of the lid whatever the rule (QUESTION 36c: Settings offers Turn on and
/// Remove lid helper from it), the platform and the running work count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepAwakeView {
    pub settings: KeepAwakeSettings,
    pub state: Decision,
    pub lid_support: LidSupport,
    pub lid_platform: LidSupport,
    pub platform: Platform,
    pub work: u32,
}

/// The run state the engine keeps between evaluations, gone at every engine start: the
/// manual deadline, when the running hold began, when work was last seen ending, whether
/// Auto stands down until the running work ends, the last readings, decision and count,
/// and the last lid command the owner gave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    manual_until_ms: Option<i64>,
    hold_since_ms: Option<i64>,
    work_last_ms: Option<i64>,
    stood_down: bool,
    sensors: Sensors,
    decision: Decision,
    work: u32,
    #[serde(default)]
    lid_order: Option<power::LidOrder>,
}

impl Stored {
    fn fresh() -> Self {
        Self {
            manual_until_ms: None,
            hold_since_ms: None,
            work_last_ms: None,
            stood_down: false,
            sensors: Sensors::unread(),
            decision: Decision::NONE,
            work: 0,
            lid_order: None,
        }
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Creates the settings and run-state tables when they are missing. Input: the engine
/// database. Output: Ok, or the database error.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    settings::initialize(connection)?;
    connection.execute_batch(TABLE).map_err(error)
}

/// Forgets the run state of an earlier engine, so a started engine holds nothing until it
/// evaluates: no timer, no hold, no end reason. Settings stay. Input: the engine database.
/// Output: Ok, or the database error.
pub(crate) fn engine_started(connection: &Connection) -> Result<(), EngineError> {
    let removed = connection
        .execute("DELETE FROM keep_awake_state", [])
        .map_err(error)?;
    assert!(removed <= 1);
    Ok(())
}

/// Loads the run state and whether an owner change waits for the keeper. Input: the engine
/// database. Output: the stored state, a fresh one when none is stored, or
/// `keep_awake_state_invalid` for a row this engine cannot read.
fn load_state(connection: &Connection) -> Result<(Stored, bool), EngineError> {
    let row: Option<(Option<String>, bool)> = connection
        .query_row(
            "SELECT CASE WHEN length(CAST(state AS BLOB))<=?1 THEN state END, dirty \
             FROM keep_awake_state WHERE singleton=1",
            [STATE_BYTES_MAX as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((text, dirty)) = row else {
        return Ok((Stored::fresh(), false));
    };
    let invalid = || error("keep_awake_state_invalid");
    let stored: Stored = serde_json::from_str(&text.ok_or_else(invalid)?).map_err(|_| invalid())?;
    assert!(stored.sensors.battery_pct.is_none_or(|pct| pct <= 100));
    Ok((stored, dirty))
}

/// Writes the run state over the stored one. Inputs: the engine database, the state, and
/// whether an owner change waits for the keeper. Output: Ok, or the database error.
fn save_state(connection: &Connection, stored: &Stored, dirty: bool) -> Result<(), EngineError> {
    let text = serde_json::to_string(stored).map_err(error)?;
    assert!(!text.is_empty());
    if text.len() > STATE_BYTES_MAX {
        return Err(error("keep_awake_state_invalid"));
    }
    let written = connection
        .execute(
            "INSERT INTO keep_awake_state(singleton,dirty,state) VALUES(1,?1,?2) \
             ON CONFLICT(singleton) DO UPDATE SET dirty=excluded.dirty, state=excluded.state",
            rusqlite::params![dirty, text],
        )
        .map_err(error)?;
    assert_eq!(written, 1);
    Ok(())
}

/// Tells whether an owner change waits for the keeper. Input: the engine database. Output:
/// true when the stored state is marked changed by the owner.
fn dirty(connection: &Connection) -> Result<bool, EngineError> {
    let dirty: Option<bool> = connection
        .query_row(
            "SELECT dirty FROM keep_awake_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(dirty.unwrap_or(false))
}

/// Moves the run state one evaluation on: notes when work ended, decides, and keeps the
/// anchors in step with the decision (the hold start, a spent timer, the stand-down after
/// the cap, and the end reason shown until a new hold starts). Inputs: the settings, the
/// state, the work count and the time. Output: none; the state carries the decision.
fn advance(settings: &KeepAwakeSettings, stored: &mut Stored, work: u32, now_ms: i64) {
    assert!(now_ms > 0);
    assert!(work <= work::WORK_MAX);
    if work == 0 {
        if stored.work > 0 && !stored.stood_down {
            stored.work_last_ms = Some(now_ms);
        }
        stored.stood_down = false;
    }
    let inputs = Inputs {
        now_ms,
        work,
        manual_until_ms: stored.manual_until_ms,
        hold_since_ms: stored.hold_since_ms,
        work_last_ms: stored.work_last_ms,
        stood_down: stored.stood_down,
        sensors: stored.sensors.clone(),
    };
    let decision = decide(settings, &inputs);
    let holding = decision.want != Want::Nothing;
    if stored.manual_until_ms.is_some_and(|until| now_ms >= until) {
        stored.manual_until_ms = None;
    }
    if matches!(decision.ended, Some(EndReason::Cap { .. })) {
        stored.manual_until_ms = None;
        stored.work_last_ms = None;
        stored.stood_down = work > 0;
    }
    stored.hold_since_ms = holding.then(|| stored.hold_since_ms.unwrap_or(now_ms));
    let ended = if holding {
        None
    } else {
        decision.ended.or(stored.decision.ended)
    };
    stored.decision = Decision { ended, ..decision };
    stored.work = work;
    assert_eq!(holding, stored.hold_since_ms.is_some());
    assert!(!holding || stored.decision.ended.is_none());
}

/// Applies one owner change to the settings and the run state in memory. A lid command
/// waits in the run state, numbered, until the keeper hands it to the lid line. Inputs:
/// the change, the settings, the state and the time. Output: none when it applied, or the
/// plain code it is refused with: `keep_awake_off`, `keep_awake_no_timer`,
/// `keep_awake_minutes_invalid`, `keep_awake_not_holding`, `keep_awake_no_lid_helper` off
/// macOS, `keep_awake_no_lid_setting` off Windows, or `keep_awake_lid_held` while a hold
/// covers the lid (QUESTION 22:58 Q3).
fn change(
    action: &KeepAwakeAction,
    settings: &mut KeepAwakeSettings,
    stored: &mut Stored,
    now_ms: i64,
) -> Option<&'static str> {
    match *action {
        KeepAwakeAction::Start { minutes } => {
            if settings.mode == Mode::Off {
                return Some("keep_awake_off");
            }
            stored.manual_until_ms = Some(now_ms + i64::from(minutes) * MINUTE_MS);
            stored.stood_down = false;
        }
        KeepAwakeAction::Extend { minutes } => {
            let Some(until) = stored.manual_until_ms.filter(|until| now_ms < *until) else {
                return Some("keep_awake_no_timer");
            };
            let moved = until + i64::from(minutes) * MINUTE_MS;
            if moved - now_ms > i64::from(MINUTES_MAX) * MINUTE_MS {
                return Some("keep_awake_minutes_invalid");
            }
            stored.manual_until_ms = Some(moved);
        }
        KeepAwakeAction::End => {
            if stored.decision.want == Want::Nothing {
                return Some("keep_awake_not_holding");
            }
            stored.manual_until_ms = None;
            stored.work_last_ms = None;
            stored.stood_down = true;
            stored.decision.ended = Some(EndReason::Manual);
        }
        KeepAwakeAction::Set { settings: chosen } => {
            if chosen.mode != settings.mode {
                stored.stood_down = false;
                stored.decision.ended = None;
            }
            if chosen.mode == Mode::Off {
                stored.manual_until_ms = None;
            }
            *settings = chosen;
        }
        KeepAwakeAction::TurnOnLidHold | KeepAwakeAction::RemoveLidHelper => {
            if !cfg!(target_os = "macos") {
                return Some("keep_awake_no_lid_helper");
            }
            let command = match *action {
                KeepAwakeAction::TurnOnLidHold => power::LidCommand::TurnOn,
                _ => power::LidCommand::Remove,
            };
            lid_order(stored, command);
        }
        KeepAwakeAction::PutLidBack => {
            if !cfg!(windows) {
                return Some("keep_awake_no_lid_setting");
            }
            if matches!(stored.decision.want, Want::IdleAndLid { .. }) {
                return Some("keep_awake_lid_held");
            }
            lid_order(stored, power::LidCommand::PutBack);
        }
    }
    None
}

/// Numbers one owner lid command and leaves it in the run state, where the keeper finds
/// it and hands it to the lid line once. Inputs: the run state and the command.
fn lid_order(stored: &mut Stored, command: power::LidCommand) {
    let number = stored
        .lid_order
        .map_or(1, |order| order.number.wrapping_add(1).max(1));
    stored.lid_order = Some(power::LidOrder { command, number });
    assert!(number > 0);
    assert_eq!(stored.lid_order.map(|order| order.command), Some(command));
}

/// Builds the owner's view from the settings and the run state.
fn view_of(settings: KeepAwakeSettings, stored: &Stored) -> KeepAwakeView {
    KeepAwakeView {
        settings,
        state: stored.decision,
        lid_support: lid_status(&settings, &stored.sensors),
        lid_platform: stored.sensors.lid_support.clone(),
        platform: Platform::CURRENT,
        work: stored.work,
    }
}

/// Evaluates Keep awake now: counts the running work, decides, and stores the run state
/// when it changed, all in one write transaction of its own, so an owner change made on
/// another connection is never overwritten. Inputs: the engine database outside any
/// transaction, the time, and new platform readings when the caller has some. Output: the
/// view after this evaluation.
pub fn evaluate(
    connection: &Connection,
    now_ms: i64,
    sensors: Option<&Sensors>,
) -> Result<KeepAwakeView, EngineError> {
    evaluated(connection, now_ms, sensors).map(|(view, _)| view)
}

/// `evaluate`, with the last lid command the owner gave, which only the keeper reads.
/// Inputs as `evaluate`. Output: the view and the last lid command, if any.
fn evaluated(
    connection: &Connection,
    now_ms: i64,
    sensors: Option<&Sensors>,
) -> Result<(KeepAwakeView, Option<power::LidOrder>), EngineError> {
    assert!(now_ms > 0);
    assert!(connection.is_autocommit());
    let tx =
        Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(error)?;
    let settings = settings::load(&tx)?;
    let (mut stored, dirty) = load_state(&tx)?;
    let before = stored.clone();
    if let Some(sensors) = sensors {
        stored.sensors = sensors.clone();
    }
    let work = work::count(&tx)?;
    advance(&settings, &mut stored, work, now_ms);
    if dirty || stored != before {
        save_state(&tx, &stored, false)?;
        noted(&tx, &settings, &before, &stored, now_ms);
    }
    tx.commit().map_err(error)?;
    assert!(stored.lid_order.is_none_or(|order| order.number > 0));
    Ok((view_of(settings, &stored), stored.lid_order))
}

/// Applies one owner change (DEC-167): a timer start, a timer move, End, or new settings,
/// then evaluates at once so the next read shows it, and marks the state for the keeper.
/// Inputs: the open transaction, the change and the time. Output: `Applied`, a rejection
/// with its plain code, or `keep_awake_minutes_invalid` for minutes out of bounds.
pub(crate) fn apply(
    connection: &Connection,
    action: &KeepAwakeAction,
    now_ms: i64,
) -> Result<Outcome, EngineError> {
    assert!(now_ms > 0);
    action.validate()?;
    let mut settings = settings::load(connection)?;
    let (mut stored, _) = load_state(connection)?;
    let before = stored.clone();
    if let Some(code) = change(action, &mut settings, &mut stored, now_ms) {
        return Ok(Outcome::Rejected { code: code.into() });
    }
    if matches!(action, KeepAwakeAction::Set { .. }) {
        settings::save(connection, &settings)?;
    }
    let work = work::count(connection)?;
    advance(&settings, &mut stored, work, now_ms);
    save_state(connection, &stored, true)?;
    noted(connection, &settings, &before, &stored, now_ms);
    Ok(Outcome::Applied)
}

/// Reads the owner's view without evaluating: the settings and the last stored run state.
/// Input: the engine database, writable or read-only. Output: the view.
pub fn view(connection: &Connection) -> Result<KeepAwakeView, EngineError> {
    let settings = settings::load(connection)?;
    let (stored, _) = load_state(connection)?;
    Ok(view_of(settings, &stored))
}

/// The owner read of Keep awake. Input: the open transaction. Output: the view as owner
/// data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::KeepAwake {
        view: view(connection)?,
    })
}

/// Records that the platform hold could not be kept: the decision becomes no hold with the
/// end reason `HoldLost`, any timer is gone, Auto stands down until the running work
/// finishes, so a hold that cannot be taken is not tried again every few seconds, and the
/// owner gets the notice. Inputs: the engine database outside any transaction and the
/// time. Output: the view after the change.
fn hold_failed(connection: &Connection, now_ms: i64) -> Result<KeepAwakeView, EngineError> {
    assert!(connection.is_autocommit());
    assert!(now_ms > 0);
    let tx =
        Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(error)?;
    let settings = settings::load(&tx)?;
    let (mut stored, dirty) = load_state(&tx)?;
    let before = stored.clone();
    stored.manual_until_ms = None;
    stored.work_last_ms = None;
    stored.hold_since_ms = None;
    stored.stood_down = stored.work > 0;
    stored.decision = Decision {
        ended: Some(EndReason::HoldLost),
        ..Decision::NONE
    };
    save_state(&tx, &stored, dirty)?;
    noted(&tx, &settings, &before, &stored, now_ms);
    tx.commit().map_err(error)?;
    assert_eq!(stored.decision.want, Want::Nothing);
    Ok(view_of(settings, &stored))
}

/// Undoes one persistent change an earlier engine recorded: on Windows the lid close
/// action a hold changed (DEC-167 addendum 32); every other kind is refused and stays
/// recorded. Input: the entry. Output: Ok, or why it is not undone.
fn undo(entry: &journal::Entry) -> Result<(), EngineError> {
    assert!(!entry.kind.is_empty());
    #[cfg(windows)]
    if entry.kind == lid_windows::KIND {
        return lid_windows::undo_with(&mut lid_windows::Windows, entry);
    }
    Err(error(format!(
        "keep_awake_journal_kind_unknown: {}",
        entry.kind
    )))
}

/// What the engine start's undo left (DEC-167 addendum 10): the kinds of the recorded
/// changes it could not undo, none when every one was undone, or a journal it could not
/// read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leftover {
    Kinds(Vec<String>),
    Unreadable,
}

/// Uninstall's lid step (QUESTION 37b): on macOS, Remove lid helper on this Mac's lid line,
/// behind the same gate as the owner's command; elsewhere there is no lid helper. Output:
/// what could not be removed, in words, or none.
pub fn remove_lid_helper() -> Option<String> {
    #[cfg(target_os = "macos")]
    return hold_macos::remove_lid_helper(crate::team::now_ms().max(1));
    #[cfg(not(target_os = "macos"))]
    None
}

/// Undoes at engine start every persistent change an earlier engine left behind (DEC-167)
/// and says what it could not: the kinds of the entries still recorded, or a journal it
/// could not open or read, which never counts as empty. A failure is logged by its code.
/// Input: the engine root. Output: what was left.
pub(crate) fn reconcile(root: &Path) -> Leftover {
    let mut found = match journal::Journal::open(root) {
        Ok(found) => found,
        Err(problem) => {
            eprintln!("keep awake journal: {}", short(&problem.0));
            return Leftover::Unreadable;
        }
    };
    if let Err(problem) = found.reconcile(undo) {
        eprintln!("keep awake journal: {}", short(&problem.0));
    }
    let kinds: Vec<String> = found
        .entries()
        .iter()
        .map(|entry| entry.kind.clone())
        .collect();
    assert!(kinds.len() <= journal::ENTRIES_MAX);
    Leftover::Kinds(kinds)
}

/// What `crazyagent keep-awake restore` says when a recorded change could not be put back.
pub const RESTORE_LEFT: &str = "Keep awake could not put back every setting it changed. Run \
    this again, or set them back by hand; on Windows the lid action is in Power Options.";
/// What `crazyagent keep-awake restore` says while an engine holds the journal.
pub const RESTORE_ENGINE_RUNNING: &str = "The engine is running and puts these settings back \
    itself. Stop it, then try again.";
/// What `crazyagent keep-awake restore` says when its list of changes could not be read.
pub const RESTORE_UNREADABLE: &str = "Keep awake could not read its list of settings to put \
    back. Check them by hand; on Windows the lid action is in Power Options.";

const LOCK_TRIES: u32 = 50;
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(100);
const _: () = assert!(LOCK_TRIES > 0);

/// Takes the journal lock for the engine's whole life (QUESTION 01:16 Q3), waiting at
/// most `LOCK_TRIES` times `LOCK_WAIT` for a `crazyagent keep-awake restore` that holds it.
/// Input: the engine root. Output: the lock, or none, logged, when it was not taken.
pub(crate) fn journal_lock(root: &Path) -> Option<journal::JournalLock> {
    assert!(!root.as_os_str().is_empty());
    for _ in 0..LOCK_TRIES {
        match journal::JournalLock::try_take(root) {
            Ok(Some(lock)) => return Some(lock),
            Ok(None) => std::thread::sleep(LOCK_WAIT),
            Err(problem) => {
                eprintln!("keep awake journal: {}", short(&problem.0));
                return None;
            }
        }
    }
    eprintln!("keep awake journal: another process holds its lock");
    None
}

/// `crazyagent keep-awake restore` (plan Task 14, for an uninstaller): runs the engine
/// start's undo now under the journal lock, and refuses while an engine holds it. A data
/// folder no engine made has nothing to put back and is not made (rv-ds241 KA T14 r1 nit
/// 1). Input: the engine root. Output: Ok when nothing is left to put back, else the
/// owner's sentence.
pub fn restore(root: &Path) -> Result<(), &'static str> {
    assert!(!root.as_os_str().is_empty());
    let missing = std::fs::symlink_metadata(root)
        .is_err_and(|problem| problem.kind() == std::io::ErrorKind::NotFound);
    if missing {
        return Ok(());
    }
    let lock = match journal::JournalLock::try_take(root) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Err(RESTORE_ENGINE_RUNNING),
        Err(problem) => {
            eprintln!("keep awake journal: {}", short(&problem.0));
            return Err(RESTORE_UNREADABLE);
        }
    };
    let said = match reconcile(root) {
        Leftover::Kinds(kinds) if kinds.is_empty() => Ok(()),
        Leftover::Kinds(_) => Err(RESTORE_LEFT),
        Leftover::Unreadable => Err(RESTORE_UNREADABLE),
    };
    drop(lock);
    assert!(said.is_ok() || !said.unwrap_err().is_empty());
    said
}

/// Cuts a logged failure to at most 300 characters. Input: the text. Output: its start.
fn short(text: &str) -> String {
    text.chars().take(300).collect()
}

/// Writes the notices one change earns (DEC-167 addendum 10): a hold that was in place and
/// is gone says how it ended, and heat that turned serious while the hold stays says so.
/// Inputs: the connection inside the change's transaction, the settings, the run state
/// before and after the change, and the time. Output: none; a notice that cannot be
/// written is logged and never fails the change.
fn noted(
    connection: &Connection,
    settings: &KeepAwakeSettings,
    before: &Stored,
    after: &Stored,
    now_ms: i64,
) {
    assert!(!connection.is_autocommit());
    assert!(now_ms > 0);
    match (before.hold_since_ms, after.hold_since_ms) {
        (Some(since), None) => {
            if let Some(ended) = after.decision.ended {
                crate::notifications::keep_awake::hold_ended(connection, ended, since, now_ms);
            }
        }
        (_, Some(since)) => {
            let same_hold = before.hold_since_ms == Some(since);
            let hot_before = same_hold && heat_notice(settings, &before.sensors);
            if heat_notice(settings, &after.sensors) && !hot_before {
                let heat_end = settings.heat_end;
                crate::notifications::keep_awake::running_hot(connection, heat_end, since, now_ms);
            }
        }
        (None, None) => {}
    }
}

/// The engine's Keep awake pacer: looks once a second for a changed work count, an owner
/// change or a new platform reading and evaluates at once when it finds one, and every
/// `TICK_MS` otherwise. After every evaluation it brings the platform hold, when it has
/// one, in step with the decision. With a reader it asks a sensor thread for a reading
/// every `power::READ_MS`; with a lid line there too, it hands the line what the lid
/// wants with each ask, and at once when that changes, each owner lid command once, and
/// notes a lid helper journal the line cannot read for the notification center. With a
/// power line, it hands the line the low power ask the same way (Task 16).
#[derive(Debug, Default)]
pub struct Keeper {
    looked_ms: Option<i64>,
    evaluated_ms: Option<i64>,
    work: Option<u32>,
    hold: Option<Box<dyn Hold + Send>>,
    lost: bool,
    sensing: Option<power::Sensing>,
    lid_line: bool,
    power_line: bool,
    lid_ask: power::LidAsk,
    low_power_under: Option<LowPower>,
    lid_order_sent: u32,
    lid_journal: Option<bool>,
    reading: Option<power::PowerReading>,
    battery_unread: bool,
    battery_unread_twice: bool,
    lid_refused: bool,
    leftover: Option<Leftover>,
    journal: Option<journal::JournalLock>,
}

impl Keeper {
    /// Builds a keeper that takes and drops a platform hold. Input: the hold. Output: the
    /// keeper; a default keeper decides the same but holds nothing.
    pub fn with_hold(hold: Box<dyn Hold + Send>) -> Self {
        Self {
            hold: Some(hold),
            ..Self::default()
        }
    }

    /// Gives the keeper platform readings: starts the sensor thread around a reader, which
    /// the keeper then asks every `power::READ_MS`. Input: the reader. Output: the keeper;
    /// when the thread cannot start that is logged and the keeper reads nothing, as one
    /// built without a reader does.
    pub fn with_reader(self, reader: power::Reader) -> Self {
        self.with_sensors(reader, None)
    }

    /// Gives the keeper platform readings and, where the platform has one, the lid line,
    /// both run on the sensor thread. A keeper with a lid line leaves the closed lid to
    /// it, so a platform hold that takes only idle sleep is no refused lid. Inputs: the
    /// reader and the lid line. Output: the keeper; when the thread cannot start that is
    /// logged and the keeper reads nothing and holds no lid.
    pub fn with_sensors(self, reader: power::Reader, lid: Option<power::LidReader>) -> Self {
        self.with_lines(reader, lid, None)
    }

    /// Gives the keeper platform readings, the lid line and the power line, where the
    /// platform has them, all run on the sensor thread. The power line holds the platform's
    /// power saver while the keeper asks for low power mode (Task 16). Inputs: the reader
    /// and both lines. Output: the keeper; when the thread cannot start that is logged and
    /// the keeper reads nothing and holds neither line.
    pub fn with_lines(
        mut self,
        reader: power::Reader,
        lid: Option<power::LidReader>,
        saver: Option<power::PowerLine>,
    ) -> Self {
        assert!(self.sensing.is_none());
        assert!(!self.lid_line);
        assert!(!self.power_line);
        let (has_lid, has_saver) = (lid.is_some(), saver.is_some());
        match power::Sensing::start_lines(reader, lid, saver) {
            Ok(sensing) => {
                self.sensing = Some(sensing);
                self.lid_line = has_lid;
                self.power_line = has_saver;
            }
            Err(problem) => eprintln!("keep awake: no sensor thread: {problem}"),
        }
        assert!(self.sensing.is_some() || !self.lid_line);
        assert!(self.sensing.is_some() || !self.power_line);
        self
    }

    /// Gives the keeper what the engine start's undo left, which its next step writes to
    /// the restore state the notification center reads. Input: what was left. Output: the
    /// keeper.
    pub fn with_leftover(mut self, leftover: Leftover) -> Self {
        assert!(self.leftover.is_none());
        self.leftover = Some(leftover);
        self
    }

    /// Keeps the journal lock the engine start took for the keeper's whole life (QUESTION
    /// 01:16 Q3), so `crazyagent keep-awake restore` refuses while this engine runs; on
    /// Windows the lid line holds it instead, until the sensor thread ends. Input: the
    /// lock, none when it was not taken or the lid line holds it. Output: the keeper.
    pub fn with_journal(mut self, lock: Option<journal::JournalLock>) -> Self {
        assert!(self.journal.is_none());
        self.journal = lock;
        self
    }

    /// Writes what the engine start's undo left, once; a write that fails is logged and
    /// tried again at the next step, and never fails this one. Input: the engine database
    /// outside any transaction.
    fn hand_on(&mut self, connection: &Connection) {
        let Some(leftover) = self.leftover.as_ref() else {
            return;
        };
        match crate::notifications::keep_awake::left_over(connection, leftover) {
            Ok(()) => self.leftover = None,
            Err(problem) => eprintln!("keep awake restore state: {}", short(&problem.0)),
        }
    }

    /// The readings `decide` looks at, from the last platform reading. Output: none
    /// before the first reading, else the sensors, with a lid the system refused shown as
    /// not available.
    fn sensors(&self) -> Option<Sensors> {
        let reading = self.reading.as_ref()?;
        Some(power::sensors(
            reading,
            self.battery_unread_twice,
            self.lid_refused,
        ))
    }

    /// Takes the reading the sensor thread sent back, if one came, and asks for the next
    /// when it is due. A battery with no charge counts as unread twice when the reading
    /// before it was unread too. Input: the time. Output: the new sensors, or none when
    /// no reading came back.
    fn sense(&mut self, now_ms: i64) -> Option<Sensors> {
        let reading = self.sensing.as_mut()?.step(now_ms, self.lid_ask)?;
        let unread = reading.source == PowerSource::Battery && reading.battery_pct.is_none();
        self.battery_unread_twice = unread && self.battery_unread;
        self.battery_unread = unread;
        self.reading = Some(reading);
        assert!(!self.battery_unread_twice || self.battery_unread);
        self.sensors()
    }

    /// Drops the platform hold, as the engine stops or a step fails, and tells the lid
    /// line at the next ask that the lid is not wanted.
    pub fn release(&mut self) {
        if let Some(hold) = self.hold.as_mut() {
            hold.release();
        }
        self.lost = false;
        self.lid_ask.wanted = false;
        self.lid_ask.sleep_display = false;
        self.lid_ask.low_power = false;
        self.low_power_under = None;
    }

    /// Drops every hold as the engine stops (QUESTION 36d): the platform hold at once, and
    /// the lid lease or the power saver hold through one last ask the sensor thread answers
    /// within `STOP_WAIT`, or the lease runs out and the bus connection closes by itself.
    pub fn stop(&mut self) {
        self.release();
        assert!(!self.lid_ask.wanted);
        assert!(!self.lid_ask.low_power);
        if !self.lid_line && !self.power_line {
            return;
        }
        if let Some(sensing) = self.sensing.as_mut() {
            sensing.finish(self.lid_ask, STOP_WAIT);
        }
    }

    /// Hands the lid line the owner's last lid command once, on its next ask. Input: the
    /// last lid command in the run state, if any.
    fn hand_lid_order(&mut self, order: Option<power::LidOrder>) {
        let Some(order) = order.filter(|order| order.number != self.lid_order_sent) else {
            return;
        };
        assert!(order.number > 0);
        match self.sensing.as_mut() {
            Some(sensing) => sensing.order(order.command),
            None => eprintln!("keep awake: no sensor thread, the lid command is dropped"),
        }
        self.lid_order_sent = order.number;
    }

    /// Writes the restore row while the lid line says the helper cannot read its journal,
    /// and clears it once the helper holds the lid, the one support that shows its journal
    /// reads; any other support, a rule Off or a daemon not enabled among them, leaves the
    /// row as it is. On Windows the lid line reads its own journal at every ask, so any
    /// other support clears the row (DEC-167 addendum 32). A write that fails is logged and
    /// tried again after the next reading. Input: the engine database outside any
    /// transaction.
    fn note_lid_journal(&mut self, connection: &Connection) {
        let Some(reading) = self.reading.as_ref().filter(|_| self.lid_line) else {
            return;
        };
        let unreadable = match reading.lid_support {
            LidSupport::SleepNotRestored => true,
            LidSupport::Held => false,
            _ if cfg!(windows) => false,
            _ => return,
        };
        if self.lid_journal == Some(unreadable) {
            return;
        }
        match crate::notifications::keep_awake::lid_journal(connection, unreadable) {
            Ok(()) => self.lid_journal = Some(unreadable),
            Err(problem) => eprintln!("keep awake lid journal notice: {}", short(&problem.0)),
        }
    }

    /// Brings the platform hold in step with the wanted state. A hold found lost is logged
    /// and taken again at once; one lost twice in a row, or one that cannot be taken, is
    /// logged and counts as failed. A hold that came back without the lid it was asked for
    /// marks the lid as refused for the rest of this engine run, unless a lid line holds
    /// the lid, which reports its own refusals in its lid support. Input: the wanted state.
    /// Output: false when the hold is wanted and not in place.
    fn keep(&mut self, want: &Want) -> bool {
        let Some(hold) = self.hold.as_mut() else {
            return true;
        };
        let first = hold.apply(want);
        let again = self.lost;
        self.lost = first == Err(HoldError::Lost);
        let held = match first {
            Err(HoldError::Lost) if !again => {
                eprintln!("keep awake: the hold was lost, taking it again");
                hold.apply(want)
            }
            other => other,
        };
        match held {
            Ok(held) => {
                self.lid_refused |=
                    !self.lid_line && matches!(want, Want::IdleAndLid { .. }) && !held.lid;
                true
            }
            Err(problem) => {
                eprintln!("keep awake: no hold: {problem}");
                false
            }
        }
    }

    /// Runs one step. A step that fails lets the platform hold go, so an engine that can
    /// no longer check its decision fails toward sleep; the next good step evaluates at
    /// once and takes the hold again when one is still wanted. Inputs: the engine database
    /// and the time. Output: the view when this step evaluated, none when nothing was due,
    /// or the first failure.
    pub fn tick(
        &mut self,
        connection: &Connection,
        now_ms: i64,
    ) -> Result<Option<KeepAwakeView>, EngineError> {
        assert!(now_ms > 0);
        let stepped = self.step(connection, now_ms);
        if stepped.is_err() {
            self.release();
            self.evaluated_ms = None;
        }
        assert!(stepped.is_ok() || self.evaluated_ms.is_none());
        stepped
    }

    /// The work of one step: looks, takes a platform reading that came back, evaluates
    /// when something changed or `TICK_MS` passed, and brings the hold in step. Inputs and
    /// output as `tick`.
    fn step(
        &mut self,
        connection: &Connection,
        now_ms: i64,
    ) -> Result<Option<KeepAwakeView>, EngineError> {
        assert!(now_ms > 0);
        let since =
            |at: Option<i64>, span: i64| at.is_some_and(|at| (0..span).contains(&(now_ms - at)));
        if since(self.looked_ms, LOOK_MS) {
            return Ok(None);
        }
        self.looked_ms = Some(now_ms);
        self.hand_on(connection);
        let work = work::count(connection)?;
        let sensed = self.sense(now_ms);
        if sensed.is_some() {
            self.note_lid_journal(connection);
        }
        let settled = since(self.evaluated_ms, TICK_MS) && self.work == Some(work);
        if settled && sensed.is_none() && !dirty(connection)? {
            return Ok(None);
        }
        let (mut view, lid_order) = evaluated(connection, now_ms, sensed.as_ref())?;
        let refused = self.lid_refused;
        let mut kept = self.keep(&view.state.want);
        if kept
            && self.lid_refused
            && !refused
            && let Some(sensors) = self.sensors()
        {
            view = evaluate(connection, now_ms, Some(&sensors))?;
            assert!(!matches!(view.state.want, Want::IdleAndLid { .. }));
            kept = self.keep(&view.state.want);
        }
        let view = if kept {
            view
        } else {
            self.release();
            hold_failed(connection, now_ms)?
        };
        self.evaluated_ms = Some(now_ms);
        self.work = Some(view.work);
        self.hand_lid_order(lid_order);
        let wanted = matches!(view.state.want, Want::IdleAndLid { .. });
        let holding = view.state.want != Want::Nothing;
        let low_power = self.sensors().is_some_and(|sensors| {
            low_power_wanted(&view.settings, &sensors, holding, self.low_power_under)
        });
        self.low_power_under = low_power.then_some(view.settings.low_power);
        self.lid_ask = power::LidAsk {
            rule_on: view.settings.lid != LidRule::Off,
            wanted,
            sleep_display: wanted && view.settings.lid_display == LidDisplay::Sleep,
            low_power,
        };
        assert!(self.lid_ask.rule_on || !self.lid_ask.wanted);
        assert!(self.lid_ask.wanted || !self.lid_ask.sleep_display);
        assert!(holding || !self.lid_ask.low_power);
        assert_eq!(self.low_power_under.is_some(), self.lid_ask.low_power);
        assert!(view.work <= work::WORK_MAX);
        Ok(Some(view))
    }
}
