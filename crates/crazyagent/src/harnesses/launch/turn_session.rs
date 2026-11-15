use super::super::drivers::jsonl_turn::{Config, JsonlTurn};
use super::super::drivers::text_turn;
use super::super::drivers::{self, Piped};
use super::super::registry::registry;
use super::{
    LINE_BYTES_MAX, LINEAGE_VARIABLE, LINES_AHEAD_MAX, LaunchError, LaunchEvent, LaunchRequest,
    Line, LineageToken, Unstopped, failure, forward, sandboxed, validate,
};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Child, Command};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

/// A piped harness that runs one process per turn, on the `jsonl-turn` driver: each prompt
/// starts a sandboxed process with that turn's arguments, its output lines become launch
/// events by the manifest's rules, and its exit ends the turn. The harness session id a turn
/// reports is passed to the next turn. Between turns no process runs, so the process group
/// a caller can stop is the running turn's, if any. With tools off, a turn whose harness
/// reports a tool is stopped at once. A run at Full access, an account with a key and a
/// project context are refused until the tasks that hand them over land.
pub struct TurnSession {
    request: LaunchRequest,
    turns: JsonlTurn,
    running: Option<Running>,
    queue: VecDeque<LaunchEvent>,
    deadline: Instant,
    uncertain: bool,
}

/// The process of the running turn, its sandbox warden, its output lines and whether its
/// process group was already asked to stop.
struct Running {
    child: Child,
    warden: Option<crate::sandbox::warden::Warden>,
    lines: Receiver<Line>,
    stopping: bool,
}

impl TurnSession {
    /// Starts a conversation with a catalog harness on the `jsonl-turn` driver; no process
    /// runs until the first prompt. Input: the launch request. Output: the session, whose
    /// first event is `Ready`, or `launch_harness_unsupported` for a harness on another
    /// driver or whose settings its manifest does not give, and the refusals of
    /// [`TurnSession::start_with`].
    pub fn start(request: LaunchRequest) -> Result<Self, LaunchError> {
        if drivers::piped(&request.harness)? != Piped::JsonlTurn {
            return Err(failure("launch_harness_unsupported"));
        }
        let harness = registry()
            .get(&request.harness)
            .ok_or_else(|| failure("launch_harness_unsupported"))?;
        let config =
            Config::read(harness.manifest()).map_err(|_| failure("launch_harness_unsupported"))?;
        Self::start_with(request, config)
    }

    /// Starts a conversation with the given jsonl-turn settings. Inputs: the launch request
    /// and the settings. Output: the session, or the launch's own refusals,
    /// `launch_key_relay_unsupported` for an account that carries a key and
    /// `launch_harness_unsupported` for a run at Full access.
    pub fn start_with(request: LaunchRequest, config: Config) -> Result<Self, LaunchError> {
        validate(&request)?;
        if request.account.key().is_some() {
            return Err(failure("launch_key_relay_unsupported"));
        }
        if request.unconfined {
            return Err(failure("launch_harness_unsupported"));
        }
        let (turns, ready) = JsonlTurn::new(
            config,
            request.account.home(),
            &request.working_dir,
            request.model.as_deref(),
            request.tools,
            None,
        )?;
        let deadline = Instant::now() + request.limits.run_time;
        Ok(Self {
            request,
            turns,
            running: None,
            queue: ready.into(),
            deadline,
            uncertain: false,
        })
    }

    /// Starts a conversation with the given text-turn settings; no process runs until the
    /// first prompt. Inputs: the launch request and the settings. Output: the session, or the
    /// refusals of [`TurnSession::start_with`].
    pub fn start_text(
        request: LaunchRequest,
        config: text_turn::Config,
    ) -> Result<Self, LaunchError> {
        let _ = (request, config);
        Err(failure("launch_harness_unsupported"))
    }

    /// The harness session id the next turn continues. Output: the id, or none before a
    /// turn reported one.
    pub fn native_session(&self) -> Option<&str> {
        self.turns.session()
    }

    /// The process group of the running turn, which stopping the session ends. Output: the
    /// group id, or none between turns.
    pub fn turn_group(&self) -> Option<u32> {
        self.running.as_ref().map(|running| running.child.id())
    }

    /// Starts a turn: the launch is checked again, as its home, profile or folders may have
    /// changed since the last turn, then one sandboxed process starts with the turn's
    /// arguments and no input. Input: the prompt. Output: Ok once the process runs, the
    /// launch's own refusals, which start nothing, the driver's refusals such as
    /// `launch_busy`, or `launch_spawn_failed` and the sandbox's refusals, after which the
    /// turn counts as ended and the next prompt may start one.
    pub fn send(&mut self, prompt: &str) -> Result<(), LaunchError> {
        validate(&self.request)?;
        let argv = self.turns.start(prompt)?;
        assert!(self.running.is_none());
        let arguments: Vec<OsString> = argv.into_iter().map(OsString::from).collect();
        let started = sandboxed(&self.request, None, &arguments, &[])
            .and_then(|mut command| self.spawn_turn(&mut command));
        if let Err(error) = started {
            assert!(self.running.is_none());
            drop(self.turns.exited(None)?);
            return Err(error);
        }
        assert!(self.running.is_some());
        Ok(())
    }

    /// Stops the running turn: the driver marks it interrupted and its process group gets
    /// TERM; the turn ends as interrupted when its output closes. Output: Ok, or
    /// `launch_not_running` with no turn or an interrupt already asked.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        self.turns.interrupt()?;
        self.stop_turn();
        Ok(())
    }

    /// Waits up to `wait` for the next event. A line too long to hold is passed over with one
    /// note in the engine log, and a breach of tools off stops the turn's process group.
    /// Output: Some(event), None on timeout or between turns, or an
    /// error once the session deadline passes or the driver refuses a line, which also stops
    /// the running turn.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<LaunchEvent>, LaunchError> {
        let until = Instant::now() + wait;
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(Some(event));
            }
            let now = Instant::now();
            if now >= self.deadline {
                self.end_turn(true);
                return Err(failure("launch_deadline"));
            }
            if now >= until {
                return Ok(None);
            }
            let Some(running) = &self.running else {
                std::thread::sleep(until.min(self.deadline) - now);
                continue;
            };
            match running.lines.recv_timeout(until.min(self.deadline) - now) {
                Ok(Line::Bytes(line)) => match self.turns.receive(&line) {
                    Ok(events) => {
                        self.queue.extend(events);
                        if self.turns.breached() {
                            self.stop_turn();
                        }
                    }
                    Err(error) => {
                        self.end_turn(true);
                        return Err(error);
                    }
                },
                Ok(Line::Skipped) => {
                    eprintln!("launch_output_line_skipped over={LINE_BYTES_MAX}");
                }
                Ok(Line::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    let code = self.end_turn(false);
                    let events = self.turns.exited(code)?;
                    self.queue.extend(events);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Stops the running turn's process group, if any, and its warden. Output: Ok, or
    /// [`Unstopped::Uncertain`] when a warden of any turn did not confirm its sandbox empty.
    pub fn stop(mut self) -> Result<(), Unstopped> {
        self.end_turn(true);
        assert!(self.running.is_none());
        if self.uncertain {
            return Err(Unstopped::Uncertain);
        }
        Ok(())
    }

    fn spawn_turn(&mut self, command: &mut Command) -> Result<(), LaunchError> {
        let token = LineageToken::new().ok_or_else(|| failure("launch_spawn_failed"))?;
        command.env(LINEAGE_VARIABLE, token.as_str());
        let (sender, lines) = sync_channel(LINES_AHEAD_MAX);
        let (mut child, warden) =
            crate::sandbox::warden::spawn(command).map_err(|_| failure("launch_spawn_failed"))?;
        drop(child.stdin.take());
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        self.running = Some(Running {
            child,
            warden,
            lines,
            stopping: false,
        });
        let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
            self.end_turn(true);
            return Err(failure("launch_stdio_unavailable"));
        };
        let readers = std::thread::Builder::new()
            .name("harness-stdout".into())
            .spawn(move || forward(stdout, sender))
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("harness-stderr".into())
                    .spawn(move || {
                        let mut stderr = stderr;
                        let mut buffer = [0_u8; 4096];
                        while matches!(stderr.read(&mut buffer), Ok(count) if count > 0) {}
                    })
            });
        if readers.is_err() {
            self.end_turn(true);
            return Err(failure("launch_reader_unavailable"));
        }
        Ok(())
    }

    /// Asks the running turn's process group to stop, once; its turn ends when its output
    /// closes.
    fn stop_turn(&mut self) {
        if let Some(running) = self.running.as_mut()
            && !running.stopping
        {
            terminate(&mut running.child);
            running.stopping = true;
        }
    }

    /// Ends the running turn's process group: TERM first when it is stopped `at_once`, a
    /// wait of at most the stop grace for its leader to exit, KILL to what is left of the
    /// group, then the warden. A turn whose output closed is not stopped at once, so its
    /// leader exits by itself with its own code. Input: whether to stop at once. Output: the
    /// leader's exit code, none for a signal, a leader that did not exit, or no turn.
    #[cfg(unix)]
    fn end_turn(&mut self, at_once: bool) -> Option<i32> {
        let mut running = self.running.take()?;
        let group = rustix::process::Pid::from_child(&running.child);
        if at_once {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
        }
        let code = reap(&mut running.child, self.request.limits.stop_grace);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        let _ = running.child.kill();
        let status = running.child.wait().ok();
        self.uncertain |=
            crate::sandbox::warden::fire_reported(running.warden.take(), "harness").is_err();
        assert!(running.warden.is_none());
        code.or_else(|| status.and_then(|status| status.code()))
    }

    #[cfg(not(unix))]
    fn end_turn(&mut self, at_once: bool) -> Option<i32> {
        let mut running = self.running.take()?;
        if let Some(warden) = running.warden.take() {
            match warden {}
        }
        let wait = if at_once {
            Duration::ZERO
        } else {
            self.request.limits.stop_grace
        };
        let code = reap(&mut running.child, wait);
        let _ = running.child.kill();
        let status = running.child.wait().ok();
        code.or_else(|| status.and_then(|status| status.code()))
    }
}

impl Drop for TurnSession {
    fn drop(&mut self) {
        self.end_turn(true);
    }
}

/// Asks a turn's process group to stop. Input: the turn's leader.
#[cfg(unix)]
fn terminate(child: &mut Child) {
    let group = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
}

#[cfg(not(unix))]
fn terminate(child: &mut Child) {
    let _ = child.kill();
}

/// Waits for a turn's leader to exit. Inputs: the leader and the longest wait. Output: its
/// exit code, none for a signal or a leader still running.
fn reap(child: &mut Child, wait: Duration) -> Option<i32> {
    let until = Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => return None,
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "turn_session_tests.rs"]
mod tests;
