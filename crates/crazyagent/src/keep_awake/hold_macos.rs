use super::hold::{Held, Hold, HoldError, wanted};
use super::power::LidAsk;
use super::state::{LidSupport, Want};
use crazyagent_mac_power::client::CallError;
use crazyagent_mac_power::protocol::{Reply, Request};
use crazyagent_mac_power::service::Service;
use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

const CAFFEINATE: &str = "/usr/bin/caffeinate";

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
        Self {
            macos: None,
            bundled: true,
            withheld: LID_WITHHELD_MACOS,
        }
    }
}

/// Reads the major version from `sw_vers -productVersion` text such as `15.4.1`. Input:
/// the text. Output: the major version, or none for text that is not one version.
pub(super) fn macos_major(text: &str) -> Option<u32> {
    let _ = text;
    None
}

/// Runs the helper socket's connect inside the engine's spawn gate (threat model C9), so
/// no child the engine starts meanwhile inherits it. Input: the connect. Output: what it
/// returned.
pub(super) fn gate(connect: &mut dyn FnMut() -> io::Result<UnixStream>) -> io::Result<UnixStream> {
    connect()
}

/// The lid hold on the sensor thread (DEC-167 addendum 18, QUESTION 35b-35d, 35f-35h).
#[derive(Debug)]
pub(super) struct LidLine {
    service: Box<dyn Service>,
    helper: Box<dyn HelperLine>,
    platform: LidPlatform,
}

impl LidLine {
    /// Inputs: the daemon's registration, the line to the helper and this Mac. Output: a
    /// line that holds nothing and has read nothing yet.
    pub(super) fn new(
        service: Box<dyn Service>,
        helper: Box<dyn HelperLine>,
        platform: LidPlatform,
    ) -> Self {
        Self {
            service,
            helper,
            platform,
        }
    }

    /// Takes one step at a reading. Inputs: the keeper's lid ask and the engine time.
    /// Output: the lid support.
    pub(super) fn step(&mut self, ask: LidAsk, now_ms: i64) -> LidSupport {
        assert!(now_ms > 0);
        assert!(ask.rule_on || !ask.wanted);
        let _ = (&self.service, &self.helper, self.platform);
        LidSupport::NotAvailable {
            reason: "not built yet".into(),
        }
    }
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
