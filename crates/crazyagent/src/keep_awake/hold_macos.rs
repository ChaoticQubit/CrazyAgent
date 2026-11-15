use super::hold::{Held, Hold, HoldError, wanted};
use super::state::Want;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

const CAFFEINATE: &str = "/usr/bin/caffeinate";

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
