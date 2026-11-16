use super::journal::{Entry, Journal};
use super::power::{LidAsk, LidCommand};
use super::state::LidSupport;
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The journal kind of a lid close action Keep awake changed on Windows (DEC-167
/// addendum 26).
pub(crate) const KIND: &str = "windows_lid_action";

/// The last lid close action value Windows names: 0 Do nothing, 1 Sleep, 2 Hibernate,
/// 3 Shut down.
pub(crate) const ACTION_MAX: u32 = 3;

/// The lid reason of an account that may not change the lid close action.
pub(crate) const NEEDS_ADMIN: &str = "Lid-closed hold needs an administrator on this computer";
/// The lid reason when the old lid close action cannot be recorded first.
pub(crate) const NOT_RECORDED: &str =
    "Keep awake could not record the lid setting, so it leaves it as it is";
/// The start of the lid reason when Windows refused a change.
pub(crate) const NOT_CHANGED: &str = "The lid setting could not be changed: ";
/// The lid reason when the power plan cannot be found.
pub(crate) const PLAN_GONE: &str = "The power plan could not be read";

/// The lid close action of one power plan on AC power and on battery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Actions {
    pub ac: u32,
    pub dc: u32,
}

/// Why a lid setting call did not work: Group Policy sets it, this account may not
/// change it, the power plan does not exist, or another failure in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Policy,
    Denied,
    Gone,
    Failed(String),
}

/// The power settings the Windows lid line reads and changes: Windows itself in the
/// engine and a stand-in in rows, so no row changes this computer's lid close action.
/// Plans are named by their GUID text.
pub(crate) trait LidSetting: Send {
    /// The active power plan.
    fn active(&mut self) -> Result<String, Refusal>;
    /// Whether this account may write the lid close action now.
    fn writable(&mut self) -> Result<(), Refusal>;
    /// One plan's lid close action on AC power and on battery.
    fn read(&mut self, plan: &str) -> Result<Actions, Refusal>;
    /// Writes one plan's lid close action, applied when the plan is the active one.
    fn write(&mut self, plan: &str, actions: Actions) -> Result<(), Refusal>;
    /// Whether the lid is closed, none when unknown.
    fn closed(&mut self) -> Option<bool>;
    /// Whether the computer runs on AC power, none when unknown.
    fn on_ac(&mut self) -> Option<bool>;
    /// Sleeps or hibernates the computer.
    fn suspend(&mut self, hibernate: bool) -> Result<(), Refusal>;
}

/// The Windows lid line (DEC-167 addendum 26) on the sensor thread: with the closed lid
/// wanted it records the old lid close action of the active plan in the engine journal,
/// then sets Do nothing; when the lid is no longer wanted it puts the old value back and,
/// with the lid closed, does what the lid action says. It is the journal's one writer
/// while the engine runs.
pub(crate) struct WindowsLid<S> {
    setting: S,
    root: PathBuf,
    journal: Option<Journal>,
    holding: bool,
}

impl<S: LidSetting> WindowsLid<S> {
    /// A lid line that holds nothing yet. Inputs: the lid setting and the engine root
    /// the journal lives under. Output: the line; the journal opens at the first ask.
    pub(crate) fn new(setting: S, root: PathBuf) -> Self {
        assert!(!root.as_os_str().is_empty());
        Self {
            setting,
            root,
            journal: None,
            holding: false,
        }
    }

    /// Answers one ask of the keeper. Inputs: the lid ask and the owner's command, which
    /// is only ever Put the lid setting back now. Output: the lid support: Held when the
    /// lid is held or can be, `LockedByPolicy`, `SleepNotRestored` while a change is not
    /// put back or the journal cannot be read, or not available with the reason.
    pub(crate) fn step(&mut self, _ask: LidAsk, _command: Option<LidCommand>) -> LidSupport {
        LidSupport::NotAvailable {
            reason: "not built yet".into(),
        }
    }
}

/// Puts back the lid close action one journal entry recorded (QUESTION 22:58 Q5, Q6):
/// only a value still at Do nothing is written, so a value the owner set during the hold
/// stays, and a plan that no longer exists counts as put back. Inputs: the lid setting and
/// the entry. Output: Ok, or why it is not put back, and the entry then stays.
pub(crate) fn undo_with(_setting: &mut impl LidSetting, entry: &Entry) -> Result<(), EngineError> {
    assert_eq!(entry.kind, KIND);
    Err(EngineError("keep_awake_lid_not_put_back: not built yet".into()))
}

/// This computer's own lid close action, through crazyagent-win-sandbox.
#[cfg(windows)]
pub(crate) struct Windows;

#[cfg(windows)]
impl LidSetting for Windows {
    fn active(&mut self) -> Result<String, Refusal> {
        Err(Refusal::Failed("not built yet".into()))
    }

    fn writable(&mut self) -> Result<(), Refusal> {
        Err(Refusal::Failed("not built yet".into()))
    }

    fn read(&mut self, _plan: &str) -> Result<Actions, Refusal> {
        Err(Refusal::Failed("not built yet".into()))
    }

    fn write(&mut self, _plan: &str, _actions: Actions) -> Result<(), Refusal> {
        Err(Refusal::Failed("not built yet".into()))
    }

    fn closed(&mut self) -> Option<bool> {
        None
    }

    fn on_ac(&mut self) -> Option<bool> {
        None
    }

    fn suspend(&mut self, _hibernate: bool) -> Result<(), Refusal> {
        Err(Refusal::Failed("not built yet".into()))
    }
}
