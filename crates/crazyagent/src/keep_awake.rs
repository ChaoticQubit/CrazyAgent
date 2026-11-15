pub mod hold;
#[cfg(target_os = "linux")]
mod hold_linux;
#[cfg(target_os = "macos")]
mod hold_macos;
#[cfg(windows)]
mod hold_windows;
pub mod journal;
pub mod power;
pub mod power_linux;
pub mod power_macos;
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
    LidRule, Mode,
};
pub use state::{
    Decision, EndReason, Heat, Inputs, LidSupport, PowerSource, Reason, Sensors, Want, decide,
    heat_notice, lid_status,
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
/// running timer later, end the hold now, or save the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum KeepAwakeAction {
    Start { minutes: u16 },
    Extend { minutes: u16 },
    End,
    Set { settings: KeepAwakeSettings },
}

impl KeepAwakeAction {
    /// Checks the bounds a change carries by itself. Input: the change. Output: Ok, or
    /// `keep_awake_minutes_invalid` for minutes outside 1 to 1440.
    pub fn validate(&self) -> Result<(), EngineError> {
        let minutes = match self {
            Self::Start { minutes } | Self::Extend { minutes } => *minutes,
            Self::End | Self::Set { .. } => return Ok(()),
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
        _ => None,
    }
}

/// What the owner sees of Keep awake: the settings, the last decision (with the reason the
/// last hold ended until a new one starts), whether the lid is covered, the platform and
/// the running work count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepAwakeView {
    pub settings: KeepAwakeSettings,
    pub state: Decision,
    pub lid_support: LidSupport,
    pub platform: Platform,
    pub work: u32,
}

/// The run state the engine keeps between evaluations, gone at every engine start: the
/// manual deadline, when the running hold began, when work was last seen ending, whether
/// Auto stands down until the running work ends, the last readings, decision and count.
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

/// Applies one owner change to the settings and the run state in memory. Inputs: the
/// change, the settings, the state and the time. Output: none when it applied, or the
/// plain code it is refused with: `keep_awake_off`, `keep_awake_no_timer`,
/// `keep_awake_minutes_invalid` or `keep_awake_not_holding`.
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
    }
    None
}

/// Builds the owner's view from the settings and the run state.
fn view_of(settings: KeepAwakeSettings, stored: &Stored) -> KeepAwakeView {
    KeepAwakeView {
        settings,
        state: stored.decision,
        lid_support: lid_status(&settings, &stored.sensors),
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
    }
    tx.commit().map_err(error)?;
    Ok(view_of(settings, &stored))
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
    if let Some(code) = change(action, &mut settings, &mut stored, now_ms) {
        return Ok(Outcome::Rejected { code: code.into() });
    }
    if matches!(action, KeepAwakeAction::Set { .. }) {
        settings::save(connection, &settings)?;
    }
    let work = work::count(connection)?;
    advance(&settings, &mut stored, work, now_ms);
    save_state(connection, &stored, true)?;
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
/// end reason `HoldLost`, any timer is gone, and Auto stands down until the running work
/// finishes, so a hold that cannot be taken is not tried again every few seconds. Inputs:
/// the engine database outside any transaction. Output: the view after the change.
fn hold_failed(connection: &Connection) -> Result<KeepAwakeView, EngineError> {
    assert!(connection.is_autocommit());
    let tx =
        Transaction::new_unchecked(connection, TransactionBehavior::Immediate).map_err(error)?;
    let settings = settings::load(&tx)?;
    let (mut stored, dirty) = load_state(&tx)?;
    stored.manual_until_ms = None;
    stored.work_last_ms = None;
    stored.hold_since_ms = None;
    stored.stood_down = stored.work > 0;
    stored.decision = Decision {
        ended: Some(EndReason::HoldLost),
        ..Decision::NONE
    };
    save_state(&tx, &stored, dirty)?;
    tx.commit().map_err(error)?;
    assert_eq!(stored.decision.want, Want::Nothing);
    Ok(view_of(settings, &stored))
}

/// Undoes one persistent change an earlier engine recorded. No kind of change exists yet,
/// so every entry is refused and stays recorded. Input: the entry. Output: the refusal.
fn undo(entry: &journal::Entry) -> Result<(), EngineError> {
    assert!(!entry.kind.is_empty());
    Err(error(format!(
        "keep_awake_journal_kind_unknown: {}",
        entry.kind
    )))
}

/// Undoes at engine start every persistent change an earlier engine left behind (DEC-167).
/// Input: the engine root. Output: Ok when the journal is empty afterwards, else the first
/// failure, with the entries that failed still recorded.
pub(crate) fn reconcile(root: &Path) -> Result<(), EngineError> {
    journal::Journal::open(root)?.reconcile(undo)
}

/// The engine's Keep awake pacer: looks once a second for a changed work count, an owner
/// change or a new platform reading and evaluates at once when it finds one, and every
/// `TICK_MS` otherwise. After every evaluation it brings the platform hold, when it has
/// one, in step with the decision. With a reader it asks a sensor thread for a reading
/// every `power::READ_MS`.
#[derive(Debug, Default)]
pub struct Keeper {
    looked_ms: Option<i64>,
    evaluated_ms: Option<i64>,
    work: Option<u32>,
    hold: Option<Box<dyn Hold + Send>>,
    lost: bool,
    sensing: Option<power::Sensing>,
    reading: Option<power::PowerReading>,
    battery_unread: bool,
    battery_unread_twice: bool,
    lid_refused: bool,
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
    pub fn with_reader(mut self, reader: power::Reader) -> Self {
        assert!(self.sensing.is_none());
        match power::Sensing::start(reader) {
            Ok(sensing) => self.sensing = Some(sensing),
            Err(problem) => eprintln!("keep awake: no sensor thread: {problem}"),
        }
        self
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
        let reading = self.sensing.as_mut()?.step(now_ms)?;
        let unread = reading.source == PowerSource::Battery && reading.battery_pct.is_none();
        self.battery_unread_twice = unread && self.battery_unread;
        self.battery_unread = unread;
        self.reading = Some(reading);
        assert!(!self.battery_unread_twice || self.battery_unread);
        self.sensors()
    }

    /// Drops the platform hold, as the engine stops.
    pub fn release(&mut self) {
        if let Some(hold) = self.hold.as_mut() {
            hold.release();
        }
        self.lost = false;
    }

    /// Brings the platform hold in step with the wanted state. A hold found lost is logged
    /// and taken again at once; one lost twice in a row, or one that cannot be taken, is
    /// logged and counts as failed. A hold that came back without the lid it was asked for
    /// marks the lid as refused for the rest of this engine run. Input: the wanted state.
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
                self.lid_refused |= matches!(want, Want::IdleAndLid { .. }) && !held.lid;
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
        let work = work::count(connection)?;
        let sensed = self.sense(now_ms);
        let settled = since(self.evaluated_ms, TICK_MS) && self.work == Some(work);
        if settled && sensed.is_none() && !dirty(connection)? {
            return Ok(None);
        }
        let mut view = evaluate(connection, now_ms, sensed.as_ref())?;
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
            hold_failed(connection)?
        };
        self.evaluated_ms = Some(now_ms);
        self.work = Some(view.work);
        assert!(view.work <= work::WORK_MAX);
        Ok(Some(view))
    }
}
