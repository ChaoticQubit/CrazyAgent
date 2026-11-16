use super::hold::{Held, Hold, HoldError, wanted};
use super::power::{LID_REFUSED, LidAsk, LidReader};
use super::state::{LidSupport, Want};
use crazyagent_mac_power::client::{self, CallError};
use crazyagent_mac_power::protocol::{FloorView, PinView, Reply, Request, View};
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
/// The lid words while the helper cannot read its journal (threat model section 8).
pub(super) const SLEEP_NOT_RESTORED: &str =
    "Sleep not restored: the lid helper cannot read its journal";
/// The lid words while the helper's last-resort floor holds (threat model section 10).
pub(super) const ENDED_TO_PROTECT: &str = "Ended to protect this Mac";
/// The Remove lid helper reason when another account's uid is pinned.
pub(super) const PIN_OTHER: &str = "The lid helper belongs to another account on this Mac";
/// The Remove lid helper reason when the helper pinned no account.
pub(super) const PIN_NONE: &str = "The lid helper was approved with no account at the screen";
/// The lid words while the rule is Off and the helper was never read.
pub(super) const LID_UNREAD: &str = "The lid helper is read once the lid rule is on";

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
    /// app bundle puts them next to the running engine. Output: what it found.
    pub(super) fn this_mac() -> Self {
        let macos = super::power_macos::run(SW_VERS, &["-productVersion"])
            .and_then(|text| macos_major(&text));
        let folder = std::env::current_exe()
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
/// the helper at most every `READ_EVERY_MS`, and at once after an approval. It never
/// connects while the lid rule is Off or the daemon is not enabled.
#[derive(Debug)]
pub(super) struct LidLine {
    service: Box<dyn Service>,
    helper: Box<dyn HelperLine>,
    platform: LidPlatform,
    holding: bool,
    read_ms: Option<i64>,
    last: Option<LidSupport>,
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
            holding: false,
            read_ms: None,
            last: None,
        }
    }

    /// Takes one step at a reading. Inputs: the keeper's lid ask and the engine time.
    /// Output: the lid support, which is never Off or idle only on battery.
    pub(super) fn step(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        assert!(now_ms > 0);
        assert!(ask.rule_on || !ask.wanted);
        if let Some(refused) = self.platform.refusal() {
            assert!(!self.holding);
            return refused;
        }
        let status = self.service.status();
        let enabled = status == Some(Status::Enabled);
        if !enabled {
            self.holding = false;
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
        assert!(enabled || !self.holding);
        assert!(!matches!(support, LidSupport::Off | LidSupport::IdleOnlyOnBattery));
        support
    }

    /// The step while the daemon is enabled: holds when the lid is wanted, else lets go
    /// of a lease and reads when a read is due and the rule is on. Inputs: the ask and the
    /// time. Output: the lid support.
    fn enabled(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        if ask.wanted {
            return self.hold(now_ms);
        }
        if self.holding {
            self.release();
        }
        assert!(!self.holding);
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

    /// Asks the helper for a lease of `LEASE_S`; a lease it gave counts as a read. A
    /// refused hold reads the helper once and says why: the view's reason, or the refusal
    /// when the view shows none. A lease that may still run stays marked for release.
    /// Input: the time. Output: the lid support.
    fn hold(&mut self, now_ms: i64) -> LidSupport {
        let support = match self.helper.call(&Request::Hold { lease_s: LEASE_S }) {
            Ok(Reply::Held { .. }) => {
                self.holding = true;
                self.read_ms = Some(now_ms);
                LidSupport::Held
            }
            Ok(Reply::Refused { reason }) => {
                eprintln!("keep awake: the lid helper refused the hold: {reason:?}");
                match self.read(now_ms) {
                    LidSupport::Held => not_available(LID_REFUSED),
                    other => other,
                }
            }
            other => {
                eprintln!("keep awake: the lid helper's hold answer: {other:?}");
                not_available(HELPER_UNANSWERED)
            }
        };
        self.last = Some(support.clone());
        assert!(support != LidSupport::Held || self.holding);
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
        assert!(self.holding);
        self.holding = false;
        match self.helper.call(&Request::Release) {
            Ok(Reply::Released) => {}
            other => eprintln!("keep awake: the lid helper's release answer: {other:?}"),
        }
    }
}

impl Drop for LidLine {
    fn drop(&mut self) {
        if self.holding {
            self.release();
        }
    }
}

/// Picks the lid support a helper view gives (QUESTION 35f, 35g): a journal it cannot
/// read first, then its floor, then the pin. Input: the view. Output: the lid support.
fn support_of(view: &View) -> LidSupport {
    if !view.journal_ok {
        return not_available(SLEEP_NOT_RESTORED);
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

fn not_available(reason: &str) -> LidSupport {
    LidSupport::NotAvailable {
        reason: reason.into(),
    }
}

/// Builds the engine's lid line: the system's daemon registration, the installed helper,
/// and this Mac, read on the sensor thread at the first reading so the engine start runs
/// no system tool for it. Output: the lid line the sensor thread runs.
pub(super) fn lid_reader() -> LidReader {
    let mut line: Option<LidLine> = None;
    Box::new(move |ask, now_ms| {
        line.get_or_insert_with(|| {
            LidLine::new(Box::new(Daemon), Box::new(Installed), LidPlatform::this_mac())
        })
        .step(ask, now_ms)
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
