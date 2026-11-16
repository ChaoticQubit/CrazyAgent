use super::journal::{Entry, Journal};
use super::power::{LidAsk, LidCommand};
use super::state::LidSupport;
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(windows)]
use crazyagent_win_sandbox::lid;

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

const DO_NOTHING: u32 = 0;
const HIBERNATE: u32 = 2;
const NOTHING: Actions = Actions {
    ac: DO_NOTHING,
    dc: DO_NOTHING,
};
const PLAN_TEXT_LEN: usize = 36;
const REASON_CHARS_MAX: usize = 200;

const _: () = assert!(DO_NOTHING < HIBERNATE);
const _: () = assert!(HIBERNATE < ACTION_MAX);

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

/// The restore value of one journal entry: the plan the hold changed and its old lid close
/// action on AC power and on battery.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    scheme: String,
    ac: u32,
    dc: u32,
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

    /// Answers one ask of the keeper. Each ask that does not want the lid puts back what
    /// the journal holds (QUESTION 22:58 Q4); Put the lid setting back now only brings
    /// that ask sooner. Inputs: the lid ask and the owner's command. Output: Held when the
    /// lid is held or can be, `LockedByPolicy`, `SleepNotRestored` while a change is not
    /// put back or the journal cannot be read, or not available with the reason.
    pub(crate) fn step(&mut self, ask: LidAsk, command: Option<LidCommand>) -> LidSupport {
        assert!(ask.rule_on || !ask.wanted);
        assert!(matches!(command, None | Some(LidCommand::PutBack)));
        if self.journal.is_none() {
            self.journal = match Journal::open(&self.root) {
                Ok(journal) => Some(journal),
                Err(problem) => {
                    eprintln!("keep awake lid journal: {}", super::short(&problem.0));
                    None
                }
            };
        }
        let Some(journal) = self.journal.as_mut() else {
            assert!(!self.holding, "a hold always has its journal");
            return LidSupport::SleepNotRestored;
        };
        if ask.wanted {
            return if self.holding {
                LidSupport::Held
            } else {
                self.hold()
            };
        }
        let ended = std::mem::take(&mut self.holding);
        let closed = ended && self.setting.closed() == Some(true);
        let left = settle(&mut self.setting, journal);
        if closed {
            self.sleep_as_set();
        }
        if left {
            return LidSupport::SleepNotRestored;
        }
        match self.setting.writable() {
            Ok(()) => LidSupport::Held,
            Err(refusal) => support_of(refusal),
        }
    }

    /// Holds the lid: records the active plan's old lid close action, then writes Do
    /// nothing; a plan already at Do nothing is held with nothing recorded, and a write
    /// that fails is put back at once. Output: the lid support.
    fn hold(&mut self) -> LidSupport {
        assert!(!self.holding);
        let journal = self.journal.as_mut().expect("the journal opens before a hold");
        if journal.entries().iter().any(|entry| entry.kind == KIND) {
            return LidSupport::SleepNotRestored;
        }
        let (plan, old) = match found(&mut self.setting) {
            Ok(found) => found,
            Err(refusal) => return support_of(refusal),
        };
        if old == NOTHING {
            self.holding = true;
            return LidSupport::Held;
        }
        let saved = Saved {
            scheme: plan.clone(),
            ac: old.ac,
            dc: old.dc,
        };
        let restore = serde_json::to_string(&saved).expect("Saved serializes");
        let entry = Entry {
            kind: KIND.into(),
            restore,
        };
        if let Err(problem) = journal.record(entry) {
            eprintln!("keep awake lid journal: {}", super::short(&problem.0));
            return LidSupport::NotAvailable {
                reason: NOT_RECORDED.into(),
            };
        }
        if let Err(refusal) = self.setting.write(&plan, NOTHING) {
            settle(&mut self.setting, journal);
            return support_of(refusal);
        }
        assert!(journal.entries().iter().any(|entry| entry.kind == KIND));
        self.holding = true;
        LidSupport::Held
    }

    /// Does what the active plan's lid close action says now, for a hold that ended with
    /// the lid closed (QUESTION 22:58 Q2): on battery the battery value, else the AC one;
    /// Do nothing stays awake, Hibernate hibernates, Sleep and Shut down sleep. A failure
    /// is logged.
    fn sleep_as_set(&mut self) {
        let setting = &mut self.setting;
        let actions = match setting.active().and_then(|plan| setting.read(&plan)) {
            Ok(actions) => actions,
            Err(refusal) => {
                eprintln!(
                    "keep awake: the lid action was not read: {}",
                    words(&refusal)
                );
                return;
            }
        };
        let action = if setting.on_ac() == Some(false) {
            actions.dc
        } else {
            actions.ac
        };
        assert!(action <= ACTION_MAX);
        if action == DO_NOTHING {
            return;
        }
        if let Err(refusal) = setting.suspend(action == HIBERNATE) {
            eprintln!(
                "keep awake: the computer did not sleep: {}",
                words(&refusal)
            );
        }
    }
}

/// The active plan and its lid close action, once this account may write it. Input: the
/// lid setting. Output: the plan and its values, or why not.
fn found(setting: &mut impl LidSetting) -> Result<(String, Actions), Refusal> {
    setting.writable()?;
    let plan = setting.active()?;
    let old = setting.read(&plan)?;
    assert!(!plan.is_empty());
    assert!(old.ac <= ACTION_MAX);
    assert!(old.dc <= ACTION_MAX);
    Ok((plan, old))
}

/// Puts back every lid change the journal holds and forgets the ones put back; one that
/// fails is logged and stays for the next ask. Inputs: the lid setting and the journal.
/// Output: whether a lid change is left.
fn settle(setting: &mut impl LidSetting, journal: &mut Journal) -> bool {
    let settled = journal.settle(KIND, |entry| undo_with(setting, entry));
    if let Err(problem) = &settled {
        eprintln!("keep awake lid journal: {}", super::short(&problem.0));
    }
    let left = journal.entries().iter().any(|entry| entry.kind == KIND);
    assert_eq!(settled.is_err(), left);
    left
}

/// The lid support of a refusal, with a reason of at most `REASON_CHARS_MAX` characters.
/// Input: the refusal. Output: the support.
fn support_of(refusal: Refusal) -> LidSupport {
    let reason = match refusal {
        Refusal::Policy => return LidSupport::LockedByPolicy,
        Refusal::Denied => NEEDS_ADMIN.to_owned(),
        Refusal::Gone => PLAN_GONE.to_owned(),
        Refusal::Failed(text) => format!("{NOT_CHANGED}{text}"),
    };
    let reason: String = reason.chars().take(REASON_CHARS_MAX).collect();
    assert!(!reason.is_empty());
    assert!(reason.chars().count() <= REASON_CHARS_MAX);
    LidSupport::NotAvailable { reason }
}

/// A refusal in words, for a log line. Input: the refusal. Output: the words.
fn words(refusal: &Refusal) -> &str {
    match refusal {
        Refusal::Policy => "Group Policy sets the lid action",
        Refusal::Denied => "this account may not change the lid action",
        Refusal::Gone => "the power plan does not exist",
        Refusal::Failed(text) => text,
    }
}

/// Reads one journal entry's restore value: a plan GUID text and two values Windows
/// names, and nothing else. Input: the value. Output: the saved values, or none.
fn parsed(restore: &str) -> Option<Saved> {
    let saved: Saved = serde_json::from_str(restore).ok()?;
    let plan = saved.scheme.as_bytes();
    let shaped = plan.len() == PLAN_TEXT_LEN
        && plan.iter().all(|byte| *byte == b'-' || byte.is_ascii_hexdigit());
    if !shaped || saved.ac > ACTION_MAX || saved.dc > ACTION_MAX {
        return None;
    }
    Some(saved)
}

/// Puts back the lid close action one journal entry recorded (QUESTION 22:58 Q5, Q6):
/// only a value still at Do nothing is written, so a value the owner set during the hold
/// stays, and a plan that no longer exists counts as put back. Inputs: the lid setting and
/// the entry. Output: Ok, or why it is not put back, and the entry then stays.
pub(crate) fn undo_with(setting: &mut impl LidSetting, entry: &Entry) -> Result<(), EngineError> {
    assert_eq!(entry.kind, KIND);
    let Some(saved) = parsed(&entry.restore) else {
        return Err(EngineError("keep_awake_lid_entry_invalid".into()));
    };
    let not_put_back = |refusal: &Refusal| {
        EngineError(format!("keep_awake_lid_not_put_back: {}", words(refusal)))
    };
    let now = match setting.read(&saved.scheme) {
        Ok(now) => now,
        Err(Refusal::Gone) => return Ok(()),
        Err(refusal) => return Err(not_put_back(&refusal)),
    };
    let back = Actions {
        ac: kept(now.ac, saved.ac),
        dc: kept(now.dc, saved.dc),
    };
    assert!(back.ac <= ACTION_MAX);
    assert!(back.dc <= ACTION_MAX);
    if back == now {
        return Ok(());
    }
    setting
        .write(&saved.scheme, back)
        .map_err(|refusal| not_put_back(&refusal))
}

/// The value put back for one power source: the saved one while the value is still Do
/// nothing, else the value the owner set during the hold. Inputs: the value now and the
/// saved one. Output: the value to write.
fn kept(now: u32, saved: u32) -> u32 {
    let back = if now == DO_NOTHING { saved } else { now };
    assert!(back <= ACTION_MAX);
    assert!(now == DO_NOTHING || back == now);
    back
}

/// This computer's own lid close action, through crazyagent-win-sandbox.
#[cfg(windows)]
pub(crate) struct Windows;

#[cfg(windows)]
impl LidSetting for Windows {
    fn active(&mut self) -> Result<String, Refusal> {
        let plan = lid::active().map_err(refused)?;
        Ok(plan.text())
    }

    fn writable(&mut self) -> Result<(), Refusal> {
        lid::writable().map_err(refused)
    }

    fn read(&mut self, plan: &str) -> Result<Actions, Refusal> {
        let found = lid::read(scheme(plan)?).map_err(refused)?;
        Ok(Actions {
            ac: found.ac,
            dc: found.dc,
        })
    }

    fn write(&mut self, plan: &str, actions: Actions) -> Result<(), Refusal> {
        let values = lid::Actions {
            ac: actions.ac,
            dc: actions.dc,
        };
        lid::write(scheme(plan)?, values).map_err(refused)
    }

    fn closed(&mut self) -> Option<bool> {
        lid::closed()
    }

    fn on_ac(&mut self) -> Option<bool> {
        crazyagent_win_sandbox::power::status().ok()?.ac
    }

    fn suspend(&mut self, hibernate: bool) -> Result<(), Refusal> {
        lid::suspend(hibernate).map_err(refused)
    }
}

/// The plan a GUID text names. Input: the text. Output: the plan, or a refusal for text
/// that is not a GUID.
#[cfg(windows)]
fn scheme(plan: &str) -> Result<lid::Scheme, Refusal> {
    lid::Scheme::parse(plan)
        .ok_or_else(|| Refusal::Failed("the power plan is not a GUID".into()))
}

/// The refusal of a Windows lid setting error. Input: the error. Output: the refusal.
#[cfg(windows)]
fn refused(problem: lid::LidError) -> Refusal {
    match problem {
        lid::LidError::Policy => Refusal::Policy,
        lid::LidError::Denied => Refusal::Denied,
        lid::LidError::Gone => Refusal::Gone,
        lid::LidError::Windows { .. } => Refusal::Failed(problem.to_string()),
    }
}

/// Builds the Windows lid line around this computer's lid close action. Input: the engine
/// root. Output: the lid line, which opens its journal at its first ask.
#[cfg(windows)]
pub(crate) fn lid_reader(root: &std::path::Path) -> super::power::LidReader {
    let mut line = WindowsLid::new(Windows, root.to_path_buf());
    Box::new(move |ask, command, _now_ms| line.step(ask, command))
}
