use super::hold::{Held, Hold, HoldError, wanted};
pub(super) use super::power::TURN_ON_FAILED;
use super::power::{LID_REFUSED, LidAsk, LidCommand, LidReader, PowerLine};
use super::state::{LidSupport, LowPowerSupport, Want};
use crazyagent_mac_power::client::{self, CallError};
use crazyagent_mac_power::protocol::{
    FloorView, LidDisplay, PinView, Refusal, Reply, Request, View,
};
use crazyagent_mac_power::service::{Daemon, PLIST_NAME, Service, Status};
use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

const CAFFEINATE: &str = "/usr/bin/caffeinate";
const SW_VERS: &str = "/usr/bin/sw_vers";
const MACOS_LID_MIN: u32 = 13;
const MACOS_MAJOR_MAX: u32 = 99;
const VERSION_BYTES_MAX: usize = 32;
const HELPER_PROGRAM: &str = "crazyagent-power-helper";
const DAEMONS_FOLDER: &str = "../Library/LaunchDaemons";
const REASON_CHARS_MAX: usize = 200;

/// The lease each `Hold` asks the lid helper for: four readings, so a renewal or two that
/// fail still hold, and an engine that stops lets the lid go within two minutes.
pub(super) const LEASE_S: u16 = 120;

/// With no hold, the lid helper is read at most this often, so it can exit idle between.
pub(super) const READ_EVERY_MS: i64 = 300_000;

/// macOS major versions where the lid-closed hold is withheld (QUESTION 31c A: a W1 or W3
/// writer can swap the helper there). Empty until live row L16 names one.
pub(super) const LID_WITHHELD_MACOS: &[u32] = &[];

/// The lid words when the daemon's status could not be read.
pub(super) const STATUS_UNREAD: &str = "The lid helper's state could not be read";
/// The lid words when the macOS version could not be read.
pub(super) const VERSION_UNREAD: &str = "The macOS version could not be read";
/// The lid words when the helper gave no reply, or one that does not fit the call.
pub(super) const HELPER_UNANSWERED: &str = "The lid helper did not answer";
/// The lid words while the helper's last-resort floor holds (threat model section 10).
pub(super) const ENDED_TO_PROTECT: &str = "Ended to protect this Mac";
/// The Remove lid helper reason when another account's uid is pinned.
pub(super) const PIN_OTHER: &str = "The lid helper belongs to another account on this Mac";
/// The Remove lid helper reason when the helper pinned no account.
pub(super) const PIN_NONE: &str = "The lid helper was approved with no account at the screen";
/// The lid words while the rule is Off and the helper was never read.
pub(super) const LID_UNREAD: &str = "The lid helper is read once the lid rule is on";
/// The start of the Remove lid helper reason for an unregister that failed.
pub(super) const REMOVE_UNFINISHED: &str = "Remove lid helper did not finish: ";
/// The Remove lid helper reason for a Forget refused while the journal reads fine: the
/// pin file is still there (rv-sec2 r2 F1).
pub(super) const PIN_FILE_KEPT: &str = "The lid helper could not remove its pin file";
/// The Remove lid helper reason for a Forget refused while a sleep value is not put back.
pub(super) const REMOVE_NOT_PUT_BACK: &str = "The lid helper could not put sleep back yet";
/// The Remove lid helper reason for a Forget from an account the helper is not pinned to.
pub(super) const REMOVE_NOT_YOURS: &str =
    "The lid helper belongs to another account; remove it in System Settings > Login Items";
/// The Remove lid helper reason for any other refused Forget.
pub(super) const REMOVE_REFUSED: &str = "The lid helper refused Remove lid helper";
/// The Remove lid helper reason for a Forget with no answer.
pub(super) const REMOVE_UNANSWERED: &str = "The lid helper did not answer Remove lid helper";
/// What uninstall says when the helper stays to put sleep back.
pub(super) const UNINSTALL_SLEEP_KEPT: &str = "The lid helper stays until it can put sleep back";
/// What uninstall says when the daemon is still registered for another reason.
pub(super) const UNINSTALL_KEPT: &str = "The lid helper is still turned on";
/// The low power words while the lid helper does not run for this engine (QUESTION 39d A,
/// 40a): the row offers the same Turn on lid-closed hold.
pub(super) const LOW_POWER_NEEDS_HELPER: &str = "Needs the lid helper";
/// The low power words for a Mac whose battery settings have no Low Power Mode.
pub(super) const LOW_POWER_NOT_ON_THIS_MAC: &str = "Not available on this Mac";
/// The low power words when the helper's view shows no lease asking for it after this
/// engine asked.
pub(super) const LOW_POWER_NOT_TAKEN: &str = "The lid helper did not take Low Power Mode";

/// The engine's line to the lid helper: one request, one reply.
pub(super) trait HelperLine: Send + std::fmt::Debug {
    fn call(&mut self, request: &Request) -> Result<Reply, CallError>;
}

/// The installed helper, reached only at `client::PRODUCTION` (QUESTION 35e): no env, argv
/// or setting names another socket, and the socket is made inside the spawn gate.
#[derive(Debug)]
struct Installed;

impl HelperLine for Installed {
    fn call(&mut self, request: &Request) -> Result<Reply, CallError> {
        client::call(&client::PRODUCTION, request, &gate)
    }
}

/// What this Mac offers the lid line before any call: its macOS major version, none when
/// it could not be read; whether the helper and its plist are in the app bundle; and the
/// versions where the lid hold is withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LidPlatform {
    pub(super) macos: Option<u32>,
    pub(super) bundled: bool,
    pub(super) withheld: &'static [u32],
}

impl LidPlatform {
    /// Reads this Mac: `sw_vers -productVersion`, and the helper and its plist where the
    /// app bundle puts them next to the running engine, its path resolved through any
    /// link. Output: what it found.
    pub(super) fn this_mac() -> Self {
        let macos = super::power_macos::run(SW_VERS, &["-productVersion"])
            .and_then(|text| macos_major(&text));
        let folder = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok()
            .and_then(|engine| engine.parent().map(Path::to_path_buf));
        let bundled = folder.is_some_and(|folder| {
            folder.join(HELPER_PROGRAM).is_file()
                && folder.join(DAEMONS_FOLDER).join(PLIST_NAME).is_file()
        });
        let mac = Self {
            macos,
            bundled,
            withheld: LID_WITHHELD_MACOS,
        };
        assert!(mac.macos.is_none_or(|major| major <= MACOS_MAJOR_MAX));
        mac
    }

    /// The lid support this Mac gives before the daemon or the helper is asked: a version
    /// that could not be read, one below 13 or withheld, or no helper in the app bundle.
    /// Output: that support, or none when the daemon may be asked.
    fn refusal(&self) -> Option<LidSupport> {
        let Some(major) = self.macos else {
            return Some(not_available(VERSION_UNREAD));
        };
        if major < MACOS_LID_MIN {
            return Some(LidSupport::NeedsMacos13);
        }
        if self.withheld.contains(&major) {
            let reason = format!("Lid-closed hold is not available on macOS {major}");
            return Some(LidSupport::NotAvailable { reason });
        }
        (!self.bundled).then_some(LidSupport::HelperMissing)
    }
}

/// Reads the major version from `sw_vers -productVersion` text such as `15.4.1`: one
/// short line of digits and dots. Input: the text. Output: the major version, or none for
/// text that is not one version.
pub(super) fn macos_major(text: &str) -> Option<u32> {
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.is_empty() || line.len() > VERSION_BYTES_MAX {
        return None;
    }
    if !line
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let digits = line.split('.').next()?;
    if digits.is_empty() {
        return None;
    }
    let major: u32 = digits.parse().ok()?;
    (1..=MACOS_MAJOR_MAX).contains(&major).then_some(major)
}

/// Runs the helper socket's connect inside the engine's spawn gate (threat model C9), so
/// no child the engine starts meanwhile inherits it. Input: the connect. Output: what it
/// returned.
pub(super) fn gate(connect: &mut dyn FnMut() -> io::Result<UnixStream>) -> io::Result<UnixStream> {
    crate::spawn_gate::guarded(connect)
}

/// The lid hold on the sensor thread (DEC-167 addendum 18, QUESTION 35b-35d, 35f-35h). At
/// each reading it reads the daemon's status. While the daemon is enabled it renews the
/// helper's lease when the lid is wanted, releases it once it is not, and otherwise reads
/// the helper at most every `READ_EVERY_MS`, and at once after an approval or a command.
/// It never connects while the daemon is not enabled, and with the lid rule Off only for a
/// low power ask (QUESTION 40c).
/// `lease_until_ms` is when the last lease the helper granted runs out at the latest, none
/// with no lease, and `lease_sleep_off` says that lease is a lid hold, not power only.
/// `trouble` is what the last owner command could not do, with the daemon
/// status after it, and stands as the lid support until that status changes. `low_power`
/// is the low power support the last step left (QUESTION 40a), `low_power_on_seen` says the
/// running low power ask read it on, and `viewed` is the helper's last view with its time.
#[derive(Debug)]
pub(super) struct LidLine {
    service: Box<dyn Service>,
    helper: Box<dyn HelperLine>,
    platform: LidPlatform,
    lease_until_ms: Option<i64>,
    lease_sleep_off: bool,
    read_ms: Option<i64>,
    last: Option<LidSupport>,
    trouble: Option<(Option<Status>, LidSupport)>,
    low_power: LowPowerSupport,
    low_power_on_seen: bool,
    viewed: Option<(i64, View)>,
}

impl LidLine {
    /// Inputs: the daemon's registration, the line to the helper and this Mac. Output: a
    /// line that holds nothing and has read nothing yet.
    pub(super) fn new(
        service: Box<dyn Service>,
        helper: Box<dyn HelperLine>,
        platform: LidPlatform,
    ) -> Self {
        assert!(platform.macos.is_none_or(|major| major <= MACOS_MAJOR_MAX));
        assert!(
            platform
                .withheld
                .iter()
                .all(|major| *major >= MACOS_LID_MIN)
        );
        Self {
            service,
            helper,
            platform,
            lease_until_ms: None,
            lease_sleep_off: false,
            read_ms: None,
            last: None,
            trouble: None,
            low_power: LowPowerSupport::Unread,
            low_power_on_seen: false,
            viewed: None,
        }
    }

    /// The low power support the last step left. Output: the support.
    pub(super) fn low_power(&self) -> LowPowerSupport {
        self.low_power.clone()
    }

    /// Runs one owner command. Turn on runs behind the same gate as every step, so a Mac
    /// that cannot hold the lid never registers the helper (QUESTION 35i, 35j, 36a). Remove
    /// needs only the helper in the bundle: a helper registered before its macOS version
    /// was withheld, or while the version cannot be read, still starts as root, so Remove
    /// always works there (rv-sec2 22:38 F1). The next step reads the helper again, and
    /// what the command could not do stands until the daemon status changes or another
    /// command runs. Inputs: the command and the engine time.
    pub(super) fn run(&mut self, command: LidCommand, now_ms: i64) {
        assert!(now_ms > 0);
        let gated = match command {
            LidCommand::TurnOn => self.platform.refusal().is_some(),
            LidCommand::Remove => !self.platform.bundled,
            LidCommand::PutBack => true,
        };
        if gated {
            eprintln!("keep awake: this Mac cannot hold the lid, {command:?} is dropped");
            return;
        }
        let status = self.service.status();
        let trouble = match command {
            LidCommand::TurnOn => self.turn_on(status),
            LidCommand::Remove => self.remove(status, now_ms),
            LidCommand::PutBack => unreachable!("the gate drops Put back on macOS"),
        };
        self.read_ms = None;
        self.last = None;
        self.trouble = trouble.map(|support| (self.service.status(), support));
        assert!(self.read_ms.is_none());
        assert!(self.last.is_none());
    }

    /// Turn on: registers the daemon when it is not registered, then opens Login Items for
    /// the approval, and opens them again while the approval waits. An enabled or missing
    /// daemon is left alone. Input: the daemon status. Output: why it could not turn on.
    fn turn_on(&self, status: Option<Status>) -> Option<LidSupport> {
        match status {
            None | Some(Status::NotRegistered) => {
                if let Err(problem) = self.service.register() {
                    let reason = format!("{TURN_ON_FAILED}{}", bounded(&problem));
                    return Some(LidSupport::NotAvailable { reason });
                }
            }
            Some(Status::RequiresApproval) => {}
            Some(Status::Enabled | Status::NotFound) => return None,
        }
        if let Err(problem) = self.service.open_login_items() {
            eprintln!(
                "keep awake: Login Items did not open: {}",
                bounded(&problem)
            );
        }
        None
    }

    /// Remove lid helper: an enabled daemon's helper forgets the pin and puts sleep back
    /// first, and is unregistered only once it did; a daemon waiting for approval runs no
    /// helper, so it holds no lease, and is unregistered at once. Inputs: the daemon status
    /// and the time. Output: why it could not remove the helper.
    fn remove(&mut self, status: Option<Status>, now_ms: i64) -> Option<LidSupport> {
        match status {
            Some(Status::Enabled) => self.forget_then_unregister(now_ms),
            Some(Status::RequiresApproval) => {
                self.lease_until_ms = None;
                self.unregister()
            }
            None | Some(Status::NotRegistered | Status::NotFound) => None,
        }
    }

    /// Sends Forget and unregisters on Forgotten. A Forget refused for the journal reads
    /// the helper once: a journal it cannot read leaves the read to say so, else the pin
    /// file stays. Input: the time. Output: why the helper stays, or none.
    fn forget_then_unregister(&mut self, now_ms: i64) -> Option<LidSupport> {
        let reason = match self.helper.call(&Request::Forget) {
            Ok(Reply::Forgotten) => {
                self.lease_until_ms = None;
                return self.unregister();
            }
            Ok(Reply::Refused {
                reason: Refusal::JournalUnreadable,
            }) => match self.read(now_ms) {
                LidSupport::SleepNotRestored => return None,
                LidSupport::NotAvailable { reason } if reason == HELPER_UNANSWERED => {
                    REMOVE_UNANSWERED
                }
                _ => PIN_FILE_KEPT,
            },
            Ok(Reply::Refused {
                reason: Refusal::Pmset,
            }) => REMOVE_NOT_PUT_BACK,
            Ok(Reply::Refused {
                reason: Refusal::Unpinned,
            }) => REMOVE_NOT_YOURS,
            Ok(Reply::Refused { reason }) => {
                eprintln!("keep awake: the lid helper refused Forget: {reason:?}");
                REMOVE_REFUSED
            }
            other => {
                eprintln!("keep awake: the lid helper's Forget answer: {other:?}");
                REMOVE_UNANSWERED
            }
        };
        Some(LidSupport::RemoveHelper {
            reason: reason.into(),
        })
    }

    /// Unregisters the daemon; launchd then stops the helper. Output: why it could not.
    fn unregister(&self) -> Option<LidSupport> {
        assert!(self.lease_until_ms.is_none());
        let problem = self.service.unregister().err()?;
        let reason = format!("{REMOVE_UNFINISHED}{}", bounded(&problem));
        Some(LidSupport::RemoveHelper { reason })
    }

    /// Takes one step at a reading. On a withheld or unread macOS version with the helper
    /// in the bundle it reads only the daemon status, never the helper: a daemon that is
    /// enabled or waits for approval was registered before, so the support offers Remove
    /// lid helper with the reason the lid is not held (rv-sec2 22:38 F1). Inputs: the
    /// keeper's lid ask and the engine time. Output: the lid support, which is never Off
    /// or idle only on battery.
    pub(super) fn step(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        assert!(now_ms > 0);
        assert!(ask.rule_on || !ask.wanted);
        assert!(ask.wanted || !ask.sleep_display);
        let held_back = match self.platform.refusal() {
            Some(LidSupport::NotAvailable { reason }) if self.platform.bundled => Some(reason),
            Some(refused) => {
                assert!(self.lease_until_ms.is_none());
                self.needs_helper();
                return refused;
            }
            None => None,
        };
        let status = self.service.status();
        let enabled = held_back.is_none() && status == Some(Status::Enabled);
        if !enabled {
            self.lease_until_ms = None;
            self.read_ms = None;
            self.last = None;
        }
        let support = match (held_back, status) {
            (Some(reason), Some(Status::Enabled | Status::RequiresApproval)) => {
                LidSupport::RemoveHelper { reason }
            }
            (Some(reason), _) => LidSupport::NotAvailable { reason },
            (None, None) => not_available(STATUS_UNREAD),
            (None, Some(Status::NotFound)) => LidSupport::HelperMissing,
            (None, Some(Status::NotRegistered)) => LidSupport::NotTurnedOn,
            (None, Some(Status::RequiresApproval)) => LidSupport::NeedsApproval,
            (None, Some(Status::Enabled)) => self.enabled(ask, now_ms),
        };
        if enabled {
            self.settle_low_power(ask.low_power, now_ms);
        } else {
            self.needs_helper();
        }
        if self
            .trouble
            .as_ref()
            .is_some_and(|(after, _)| *after != status)
        {
            self.trouble = None;
        }
        let support = self
            .trouble
            .as_ref()
            .map_or(support, |(_, trouble)| trouble.clone());
        self.lease_sleep_off &= self.lease_until_ms.is_some();
        assert!(enabled || self.lease_until_ms.is_none());
        assert!(self.lease_until_ms.is_some() || !self.lease_sleep_off);
        assert!(!matches!(
            support,
            LidSupport::Off | LidSupport::IdleOnlyOnBattery
        ));
        support
    }

    /// The step while the daemon is enabled: forgets a lease that ran out, holds when the
    /// lid is wanted, else keeps a power-only lease while low power is asked or lets go of
    /// a lease, and reads when a read is due and the rule is on. A step that asks low power
    /// reads the helper's view once if nothing else did (QUESTION 40c, 40d). Inputs: the ask
    /// and the time. Output: the lid support.
    fn enabled(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        if self.lease_until_ms.is_some_and(|until| now_ms >= until) {
            self.lease_until_ms = None;
        }
        let support = if ask.wanted {
            self.hold(ask, now_ms)
        } else {
            if ask.low_power {
                self.power_only(now_ms);
            } else if self.lease_until_ms.is_some() {
                self.release();
            }
            assert!(ask.low_power || self.lease_until_ms.is_none());
            self.resting(ask.rule_on, now_ms)
        };
        let viewed = self.viewed.is_some_and(|(at_ms, _)| at_ms == now_ms);
        if ask.low_power && !viewed {
            self.view(now_ms);
        }
        support
    }

    /// The lid support while the lid is not wanted: the last one while the rule is Off, else
    /// a read when one is due. Inputs: whether the rule is on, and the time. Output: the lid
    /// support.
    fn resting(&mut self, rule_on: bool, now_ms: i64) -> LidSupport {
        if !rule_on {
            return self
                .last
                .clone()
                .unwrap_or_else(|| not_available(LID_UNREAD));
        }
        let due = self
            .read_ms
            .is_none_or(|at| !(0..READ_EVERY_MS).contains(&(now_ms - at)));
        match self.last.clone() {
            Some(last) if !due => last,
            _ => self.read(now_ms),
        }
    }

    /// Asks the helper for this engine's power-only lease of `LEASE_S` (QUESTION 40c): sent
    /// while low power is asked and the lid is not wanted, with the lid rule on or Off. A
    /// lease it gave keeps `lease_until_ms`, so a step that asks neither releases it. A
    /// refusal or no answer is logged; a lid hold's lease it did not replace is released,
    /// since the lid is no longer wanted (advisor 02:04). The view this step reads says
    /// whether a lease asks. Input: the time.
    fn power_only(&mut self, now_ms: i64) {
        let request = Request::LowPower { lease_s: LEASE_S };
        match self.helper.call(&request) {
            Ok(Reply::Held { lease_left_s }) if lease_left_s > 0 => {
                let left_ms = i64::from(lease_left_s.min(LEASE_S)) * 1000;
                self.lease_until_ms = Some(now_ms + left_ms);
                self.lease_sleep_off = false;
            }
            other => {
                eprintln!("keep awake: the lid helper's low power answer: {other:?}");
                if self.lease_until_ms.is_some() && self.lease_sleep_off {
                    self.release();
                }
            }
        }
        assert!(self.lease_until_ms.is_none() || !self.lease_sleep_off);
        let longest_ms = now_ms + i64::from(LEASE_S) * 1000;
        assert!(self.lease_until_ms.is_none_or(|until| until <= longest_ms));
    }

    /// Says low power needs the lid helper, on every step that does not ask the helper
    /// (QUESTION 39d A, 40a).
    fn needs_helper(&mut self) {
        self.low_power = LowPowerSupport::NotAvailable {
            reason: LOW_POWER_NEEDS_HELPER.into(),
        };
        self.low_power_on_seen = false;
        assert_ne!(self.low_power, LowPowerSupport::Held);
    }

    /// Brings the low power support up to date at the end of a step with the daemon enabled
    /// (QUESTION 40a, 40d, 40e): from the view this step read, else what it was while low
    /// power is asked, else what is left once no ask runs (`unasked`). Inputs: whether low
    /// power is asked, and the time.
    fn settle_low_power(&mut self, asked: bool, now_ms: i64) {
        if !asked {
            self.low_power_on_seen = false;
        }
        let seen = self.viewed.filter(|(at_ms, _)| *at_ms == now_ms);
        self.low_power = match seen {
            Some((_, view)) => low_power_of(&view, asked, self.low_power_on_seen, &self.low_power),
            None if asked => self.low_power.clone(),
            None => unasked(&self.low_power),
        };
        self.low_power_on_seen |= asked && self.low_power == LowPowerSupport::Held;
        let ours = matches!(
            self.low_power,
            LowPowerSupport::Held | LowPowerSupport::OwnerChanged
        );
        assert!(asked || !ours);
        assert!(asked || !self.low_power_on_seen);
    }

    /// Asks the helper for a lease of `LEASE_S`, naming the owner's display pick (Task 12);
    /// a lease it gave counts as a read. While the last lease still runs, a renewal refused
    /// for `pmset` or with no answer keeps the lid held, and the next reading tries again
    /// (threat model sections 9 and 10). Any other refusal, or one once the lease ran out,
    /// reads the helper once and says why: the view's reason, or the refusal when the view
    /// shows none. A first hold with no answer counts as the last read. A failed hold that
    /// asks no low power releases a power-only lease still running from the step before
    /// (rv-ds241 N2). Inputs: whether the display sleeps once per close and whether low power
    /// is asked (Task 16), and the time. Output: the lid support.
    fn hold(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        assert!(ask.wanted);
        let lease_live = self.lease_until_ms.is_some_and(|until| now_ms < until);
        let running = lease_live && self.lease_sleep_off;
        let lid_display = if ask.sleep_display {
            LidDisplay::Sleep
        } else {
            LidDisplay::LeaveOn
        };
        let request = Request::Hold {
            lease_s: LEASE_S,
            lid_display,
            low_power: ask.low_power,
        };
        let support = match self.helper.call(&request) {
            Ok(Reply::Held { lease_left_s }) if lease_left_s > 0 => {
                let left_ms = i64::from(lease_left_s.min(LEASE_S)) * 1000;
                self.lease_until_ms = Some(now_ms + left_ms);
                self.lease_sleep_off = true;
                self.read_ms = Some(now_ms);
                LidSupport::Held
            }
            Ok(Reply::Refused { reason }) if reason != Refusal::Pmset || !running => {
                eprintln!("keep awake: the lid helper refused the hold: {reason:?}");
                match self.read(now_ms) {
                    LidSupport::Held => not_available(LID_REFUSED),
                    other => other,
                }
            }
            other if running => {
                eprintln!("keep awake: a lid lease renewal failed, the lease runs: {other:?}");
                LidSupport::Held
            }
            other => {
                eprintln!("keep awake: the lid helper's hold answer: {other:?}");
                self.read_ms = Some(now_ms);
                not_available(HELPER_UNANSWERED)
            }
        };
        let power_only_left = self.lease_until_ms.is_some() && !self.lease_sleep_off;
        if support != LidSupport::Held && power_only_left && !ask.low_power {
            self.release();
        }
        self.last = Some(support.clone());
        let live = self.lease_until_ms.is_some_and(|until| now_ms < until);
        assert!(support != LidSupport::Held || live);
        assert!(support != LidSupport::Held || self.lease_sleep_off);
        assert!(ask.low_power || self.lease_until_ms.is_none() || self.lease_sleep_off);
        support
    }

    /// Reads the helper and keeps what it said as the last lid support. Input: the time.
    /// Output: the support its view gives, or not available when it gave no view.
    fn read(&mut self, now_ms: i64) -> LidSupport {
        self.read_ms = Some(now_ms);
        let support = match self.view(now_ms) {
            Some(view) => support_of(&view),
            None => not_available(HELPER_UNANSWERED),
        };
        self.last = Some(support.clone());
        assert_eq!(self.read_ms, Some(now_ms));
        support
    }

    /// Asks the helper for its view and keeps it with its time for the low power support.
    /// Input: the time. Output: the view, or none when the helper gave none.
    fn view(&mut self, now_ms: i64) -> Option<View> {
        let view = match self.helper.call(&Request::Read) {
            Ok(Reply::State(view)) => view,
            other => {
                eprintln!("keep awake: the lid helper's read answer: {other:?}");
                return None;
            }
        };
        self.viewed = Some((now_ms, view));
        assert!(self.viewed.is_some_and(|(at_ms, _)| at_ms == now_ms));
        Some(view)
    }

    /// Ends this engine's lease. A release the helper did not accept is logged; the lease
    /// then runs out by itself within `LEASE_S`.
    fn release(&mut self) {
        assert!(self.lease_until_ms.is_some());
        self.lease_until_ms = None;
        self.lease_sleep_off = false;
        match self.helper.call(&Request::Release) {
            Ok(Reply::Released) => {}
            other => eprintln!("keep awake: the lid helper's release answer: {other:?}"),
        }
    }
}

impl Drop for LidLine {
    fn drop(&mut self) {
        if self.lease_until_ms.is_some() {
            self.release();
        }
    }
}

/// Picks the lid support a helper view gives (QUESTION 35f, 35g): a journal it cannot
/// read first, then its floor, then the pin. Input: the view. Output: the lid support.
fn support_of(view: &View) -> LidSupport {
    if !view.journal_ok {
        return LidSupport::SleepNotRestored;
    }
    if view.floor != FloorView::None {
        return not_available(ENDED_TO_PROTECT);
    }
    match view.pin {
        PinView::You => LidSupport::Held,
        PinView::Other => LidSupport::RemoveHelper {
            reason: PIN_OTHER.into(),
        },
        PinView::None => LidSupport::RemoveHelper {
            reason: PIN_NONE.into(),
        },
    }
}

/// The low power support a helper view gives (QUESTION 40a, 40d, 40e). Not asked: ready.
/// Asked: not taken when no lease asks for it, held while it reads on, the owner's once it
/// reads off after this ask read it on, else ready, since the helper sets it after its
/// reply. A view with no value is a Mac without the key, unless the support before came
/// from a value: one failed pmset run then changes nothing. Inputs: the view, whether low
/// power is asked, whether this ask read it on, and the support before. Output: the support.
fn low_power_of(
    view: &View,
    asked: bool,
    on_seen: bool,
    before: &LowPowerSupport,
) -> LowPowerSupport {
    let from_value = matches!(
        before,
        LowPowerSupport::Ready | LowPowerSupport::Held | LowPowerSupport::OwnerChanged
    );
    let unavailable = |reason: &str| LowPowerSupport::NotAvailable {
        reason: reason.into(),
    };
    match view.low_power {
        None if from_value && asked => before.clone(),
        None if from_value => LowPowerSupport::Ready,
        None => unavailable(LOW_POWER_NOT_ON_THIS_MAC),
        Some(_) if !asked => LowPowerSupport::Ready,
        Some(_) if !view.low_power_held => unavailable(LOW_POWER_NOT_TAKEN),
        Some(true) => LowPowerSupport::Held,
        Some(false) if on_seen => LowPowerSupport::OwnerChanged,
        Some(false) => LowPowerSupport::Ready,
    }
}

/// The low power support a step with the daemon enabled leaves when no ask runs and it read
/// no view (advisor 01:52): words about an ask that ended become ready, "Needs the lid
/// helper" becomes unread since the helper now runs, and a Mac without the key stays so.
/// Input: the support before. Output: the support.
fn unasked(before: &LowPowerSupport) -> LowPowerSupport {
    let after = match before {
        LowPowerSupport::NotAvailable { reason } if reason == LOW_POWER_NEEDS_HELPER => {
            LowPowerSupport::Unread
        }
        LowPowerSupport::NotAvailable { reason } if reason == LOW_POWER_NOT_ON_THIS_MAC => {
            before.clone()
        }
        LowPowerSupport::Unread => LowPowerSupport::Unread,
        LowPowerSupport::Ready
        | LowPowerSupport::Held
        | LowPowerSupport::OwnerChanged
        | LowPowerSupport::NotAvailable { .. } => LowPowerSupport::Ready,
    };
    assert!(!matches!(
        after,
        LowPowerSupport::Held | LowPowerSupport::OwnerChanged
    ));
    assert!(*before != LowPowerSupport::Unread || after == LowPowerSupport::Unread);
    after
}

/// A system error's words for the lid support: no control characters, at most
/// `REASON_CHARS_MAX` characters. Input: the words. Output: the bounded words.
fn bounded(text: &str) -> String {
    let words: String = text
        .chars()
        .filter(|letter| !letter.is_control())
        .take(REASON_CHARS_MAX)
        .collect();
    assert!(words.chars().count() <= REASON_CHARS_MAX);
    words
}

fn not_available(reason: &str) -> LidSupport {
    LidSupport::NotAvailable {
        reason: reason.into(),
    }
}

impl LidLine {
    /// Uninstall's lid step (QUESTION 37b): Remove lid helper behind the same gate as every
    /// command, then one reading of what stayed. A bundle with no helper reaches no
    /// SMAppService; a withheld or unread macOS version still removes a helper registered
    /// before (rv-sec2 22:38 F1). Input: the time. Output: what could not be removed, in
    /// words; none when it was removed, nothing was there, or the bundle has no helper.
    pub(super) fn uninstall(&mut self, now_ms: i64) -> Option<String> {
        assert!(now_ms > 0);
        if !self.platform.bundled {
            return None;
        }
        self.run(LidCommand::Remove, now_ms);
        let ask = LidAsk {
            rule_on: true,
            wanted: false,
            sleep_display: false,
            low_power: false,
        };
        let refused = self.platform.refusal();
        let support = self.step(ask, now_ms);
        if refused.as_ref() == Some(&support) {
            return None;
        }
        let kept = match support {
            LidSupport::NotTurnedOn | LidSupport::HelperMissing => return None,
            LidSupport::RemoveHelper { reason } | LidSupport::NotAvailable { reason } => reason,
            LidSupport::SleepNotRestored => UNINSTALL_SLEEP_KEPT.into(),
            _ => UNINSTALL_KEPT.into(),
        };
        assert!(!kept.is_empty());
        assert!(self.lease_until_ms.is_none());
        Some(kept)
    }
}

/// Uninstall's lid step on this Mac: a lid line on the system's daemon registration, the
/// installed helper and this Mac, run once. Input: the time. Output: what could not be
/// removed, in words, or none.
pub(super) fn remove_lid_helper(now_ms: i64) -> Option<String> {
    assert!(now_ms > 0);
    let mut line = LidLine::new(
        Box::new(Daemon),
        Box::new(Installed),
        LidPlatform::this_mac(),
    );
    line.uninstall(now_ms)
}

/// Builds the engine's lid line and power line on the system's daemon registration, the
/// installed helper and this Mac, read on the sensor thread at the first reading so the
/// engine start runs no system tool for it. Output: both lines.
pub(super) fn lines() -> (LidReader, PowerLine) {
    lines_around(|| {
        LidLine::new(
            Box::new(Daemon),
            Box::new(Installed),
            LidPlatform::this_mac(),
        )
    })
}

/// Builds the lid line and the power line over one lid line (QUESTION 40b). The lid closure
/// makes the line with `make` at its first call, runs a command when one came, steps, and
/// leaves the step's low power support in a cell the power closure only returns, so the
/// power line never calls the helper. The sensor thread runs the lid line first in every
/// reading. Input: what makes the line. Output: both closures.
pub(super) fn lines_around(
    make: impl FnOnce() -> LidLine + Send + 'static,
) -> (LidReader, PowerLine) {
    let left = Arc::new(Mutex::new(LowPowerSupport::Unread));
    let shared = left.clone();
    let mut make = Some(make);
    let mut line: Option<LidLine> = None;
    let lid: LidReader = Box::new(move |ask, command, now_ms| {
        let line = line.get_or_insert_with(|| make.take().expect("the line is made once")());
        if let Some(command) = command {
            line.run(command, now_ms);
        }
        let support = line.step(ask, now_ms);
        *shared.lock().unwrap_or_else(PoisonError::into_inner) = line.low_power();
        support
    });
    let power: PowerLine = Box::new(move |_asked, now_ms| {
        assert!(now_ms > 0);
        left.lock().unwrap_or_else(PoisonError::into_inner).clone()
    });
    (lid, power)
}

/// The macOS idle hold: one `caffeinate -i -w <engine pid>` child (with `-d` while the
/// display stays on) in its own process group. The system drops the assertion when the
/// child ends, and the child ends by itself when the engine does.
#[derive(Debug)]
pub(super) struct MacHold {
    engine_pid: u32,
    child: Option<Child>,
    display: bool,
}

impl MacHold {
    /// Input: the pid of the engine the hold is for. Output: a hold with no child yet.
    pub(super) fn new(engine_pid: u32) -> Self {
        assert!(engine_pid > 1);
        Self {
            engine_pid,
            child: None,
            display: false,
        }
    }

    /// Starts one caffeinate child through the spawn gate, with an empty environment and no
    /// open pipes. Input: whether the display stays on. Output: the child, or `Unavailable`.
    fn start(&self, display: bool) -> Result<Child, HoldError> {
        assert!(self.engine_pid > 1);
        let mut command = Command::new(CAFFEINATE);
        if display {
            command.arg("-d");
        }
        command
            .args(["-i", "-w"])
            .arg(self.engine_pid.to_string())
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = crate::spawn_gate::spawn(&mut command)
            .map_err(|problem| HoldError::unavailable("caffeinate", problem))?;
        assert!(child.id() > 1);
        Ok(child)
    }

    /// Tells whether a child this hold had ended by itself, and forgets it then. Output:
    /// true when a hold that was in place is gone.
    fn lost(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        let gone = !matches!(child.try_wait(), Ok(None));
        if gone {
            self.child = None;
        }
        assert_eq!(gone, self.child.is_none());
        gone
    }
}

/// Stops one caffeinate child and reaps it: kills its process group, then the child
/// itself in case the group signal did not reach it, then waits. Input: the child.
fn stop(mut child: Child) {
    let group = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    if let Some(group) = group {
        assert!(group.as_raw_nonzero().get() > 1);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl Hold for MacHold {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        let lost = self.lost();
        let Some(display) = wanted(want) else {
            self.release();
            return Ok(Held::NONE);
        };
        if lost {
            return Err(HoldError::Lost);
        }
        if self.child.is_none() || self.display != display {
            let fresh = self.start(display)?;
            if let Some(old) = self.child.replace(fresh) {
                stop(old);
            }
            self.display = display;
        }
        assert!(self.child.is_some());
        assert_eq!(self.display, display);
        Ok(Held {
            idle: true,
            lid: false,
            display,
        })
    }

    fn release(&mut self) {
        if let Some(child) = self.child.take() {
            stop(child);
        }
        self.display = false;
        assert!(self.child.is_none());
    }
}

impl Drop for MacHold {
    fn drop(&mut self) {
        self.release();
    }
}
