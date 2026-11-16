use super::hold::{Held, Hold, HoldError, wanted};
use super::power::{LID_REFUSED, LidAsk, LidCommand, LidReader};
pub(super) use super::power::TURN_ON_FAILED;
use super::state::{LidSupport, Want};
use crazyagent_mac_power::client::{self, CallError};
use crazyagent_mac_power::protocol::{FloorView, PinView, Refusal, Reply, Request, View};
use crazyagent_mac_power::service::{Daemon, PLIST_NAME, Service, Status};
use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

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
    if !line.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.') {
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
/// It never connects while the lid rule is Off or the daemon is not enabled.
/// `lease_until_ms` is when the last lease the helper granted runs out at the latest, none
/// with no lease. `trouble` is what the last owner command could not do, with the daemon
/// status after it, and stands as the lid support until that status changes.
#[derive(Debug)]
pub(super) struct LidLine {
    service: Box<dyn Service>,
    helper: Box<dyn HelperLine>,
    platform: LidPlatform,
    lease_until_ms: Option<i64>,
    read_ms: Option<i64>,
    last: Option<LidSupport>,
    trouble: Option<(Option<Status>, LidSupport)>,
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
        assert!(platform.withheld.iter().all(|major| *major >= MACOS_LID_MIN));
        Self {
            service,
            helper,
            platform,
            lease_until_ms: None,
            read_ms: None,
            last: None,
            trouble: None,
        }
    }

    /// Runs one owner command behind the same gate as every step, so a Mac that cannot
    /// hold the lid never reaches SMAppService (QUESTION 35i, 35j, 36a). The next step
    /// reads the helper again, and what the command could not do stands until the daemon
    /// status changes or another command runs. Inputs: the command and the engine time.
    pub(super) fn run(&mut self, command: LidCommand, now_ms: i64) {
        assert!(now_ms > 0);
        if self.platform.refusal().is_some() {
            eprintln!("keep awake: this Mac cannot hold the lid, {command:?} is dropped");
            return;
        }
        let status = self.service.status();
        let trouble = match command {
            LidCommand::TurnOn => self.turn_on(status),
            LidCommand::Remove => self.remove(status, now_ms),
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
            eprintln!("keep awake: Login Items did not open: {}", bounded(&problem));
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

    /// Takes one step at a reading. Inputs: the keeper's lid ask and the engine time.
    /// Output: the lid support, which is never Off or idle only on battery.
    pub(super) fn step(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        assert!(now_ms > 0);
        assert!(ask.rule_on || !ask.wanted);
        if let Some(refused) = self.platform.refusal() {
            assert!(self.lease_until_ms.is_none());
            return refused;
        }
        let status = self.service.status();
        let enabled = status == Some(Status::Enabled);
        if !enabled {
            self.lease_until_ms = None;
            self.read_ms = None;
            self.last = None;
        }
        let support = match status {
            None => not_available(STATUS_UNREAD),
            Some(Status::NotFound) => LidSupport::HelperMissing,
            Some(Status::NotRegistered) => LidSupport::NotTurnedOn,
            Some(Status::RequiresApproval) => LidSupport::NeedsApproval,
            Some(Status::Enabled) => self.enabled(ask, now_ms),
        };
        if self.trouble.as_ref().is_some_and(|(after, _)| *after != status) {
            self.trouble = None;
        }
        let support = self.trouble.as_ref().map_or(support, |(_, trouble)| trouble.clone());
        assert!(enabled || self.lease_until_ms.is_none());
        assert!(!matches!(support, LidSupport::Off | LidSupport::IdleOnlyOnBattery));
        support
    }

    /// The step while the daemon is enabled: forgets a lease that ran out, holds when the
    /// lid is wanted, else lets go of a lease and reads when a read is due and the rule is
    /// on. Inputs: the ask and the time. Output: the lid support.
    fn enabled(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        if self.lease_until_ms.is_some_and(|until| now_ms >= until) {
            self.lease_until_ms = None;
        }
        if ask.wanted {
            return self.hold(now_ms);
        }
        if self.lease_until_ms.is_some() {
            self.release();
        }
        assert!(self.lease_until_ms.is_none());
        if !ask.rule_on {
            return self.last.clone().unwrap_or_else(|| not_available(LID_UNREAD));
        }
        let due = self
            .read_ms
            .is_none_or(|at| !(0..READ_EVERY_MS).contains(&(now_ms - at)));
        match self.last.clone() {
            Some(last) if !due => last,
            _ => self.read(now_ms),
        }
    }

    /// Asks the helper for a lease of `LEASE_S`; a lease it gave counts as a read. While
    /// the last lease still runs, a renewal refused for `pmset` or with no answer keeps
    /// the lid held, and the next reading tries again (threat model sections 9 and 10).
    /// Any other refusal, or one once the lease ran out, reads the helper once and says
    /// why: the view's reason, or the refusal when the view shows none. A first hold with
    /// no answer counts as the last read. Input: the time. Output: the lid support.
    fn hold(&mut self, now_ms: i64) -> LidSupport {
        let running = self.lease_until_ms.is_some_and(|until| now_ms < until);
        let support = match self.helper.call(&Request::Hold { lease_s: LEASE_S }) {
            Ok(Reply::Held { lease_left_s }) if lease_left_s > 0 => {
                let left_ms = i64::from(lease_left_s.min(LEASE_S)) * 1000;
                self.lease_until_ms = Some(now_ms + left_ms);
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
        self.last = Some(support.clone());
        let live = self.lease_until_ms.is_some_and(|until| now_ms < until);
        assert!(support != LidSupport::Held || live);
        support
    }

    /// Reads the helper and keeps what it said as the last lid support. Input: the time.
    /// Output: the support its view gives, or not available when it gave no view.
    fn read(&mut self, now_ms: i64) -> LidSupport {
        self.read_ms = Some(now_ms);
        let support = match self.helper.call(&Request::Read) {
            Ok(Reply::State(view)) => support_of(&view),
            other => {
                eprintln!("keep awake: the lid helper's read answer: {other:?}");
                not_available(HELPER_UNANSWERED)
            }
        };
        self.last = Some(support.clone());
        assert_eq!(self.read_ms, Some(now_ms));
        support
    }

    /// Ends this engine's lease. A release the helper did not accept is logged; the lease
    /// then runs out by itself within `LEASE_S`.
    fn release(&mut self) {
        assert!(self.lease_until_ms.is_some());
        self.lease_until_ms = None;
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
    /// command, then one reading of what stayed. A Mac that cannot hold the lid reaches no
    /// SMAppService. Input: the time. Output: what could not be removed, in words; none
    /// when it was removed, nothing was there, or this Mac cannot hold the lid.
    pub(super) fn uninstall(&mut self, now_ms: i64) -> Option<String> {
        let _ = now_ms;
        Some(String::new())
    }
}

/// Uninstall's lid step on this Mac: a lid line on the system's daemon registration, the
/// installed helper and this Mac, run once. Input: the time. Output: what could not be
/// removed, in words, or none.
pub(super) fn remove_lid_helper(now_ms: i64) -> Option<String> {
    assert!(now_ms > 0);
    let mut line = LidLine::new(Box::new(Daemon), Box::new(Installed), LidPlatform::this_mac());
    line.uninstall(now_ms)
}

/// Builds the engine's lid line: the system's daemon registration, the installed helper,
/// and this Mac, read on the sensor thread at the first reading so the engine start runs
/// no system tool for it. Output: the lid line the sensor thread runs.
pub(super) fn lid_reader() -> LidReader {
    let mut line: Option<LidLine> = None;
    Box::new(move |ask, command, now_ms| {
        let line = line.get_or_insert_with(|| {
            LidLine::new(Box::new(Daemon), Box::new(Installed), LidPlatform::this_mac())
        });
        if let Some(command) = command {
            line.run(command, now_ms);
        }
        line.step(ask, now_ms)
    })
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
