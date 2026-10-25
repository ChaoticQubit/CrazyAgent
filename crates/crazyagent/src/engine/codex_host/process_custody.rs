use super::process::{
    CLOSE_TIME, CodexHostOutput, OUTPUT_QUEUE_MAX, POLL_TIME, ProcessShared, WriteRequest,
};
use crate::engine::EngineError;
use crate::engine::bootstrap::NativeProfileUse;
#[cfg(unix)]
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
#[cfg(unix)]
use rustix::io::Errno;
#[cfg(unix)]
use rustix::process::{Pid, Signal};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const READ_BYTES: usize = 4096;
const READS_PER_POLL: usize = 16;

/// How long custody still drains the harness pipes once the harness is reaped and its group
/// is empty; a pipe still open after it is held by a process that left the group.
const DRAIN_TIME: Duration = Duration::from_millis(100);

struct PendingWrite {
    request: WriteRequest,
    written: usize,
}

struct OwnedChild {
    child: Child,
    profile: Option<NativeProfileUse>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    stdout_eof: bool,
    stderr_eof: bool,
    status: Option<ExitStatus>,
    group_absent: bool,
    group_gone_at: Option<Instant>,
    abandoned: bool,
    pending: Option<PendingWrite>,
    kill_sent: bool,
    accounting_failed: bool,
    shared: Arc<ProcessShared>,
}

struct Custody(Option<OwnedChild>);

pub(super) fn run(
    command: Command,
    profile: Option<NativeProfileUse>,
    mut input: mpsc::Receiver<WriteRequest>,
    shared: Arc<ProcessShared>,
    deadline: Instant,
    output_limit: u64,
) {
    let mut custody = match start(command, profile, &shared) {
        Ok(custody) => custody,
        Err(code) => {
            if let Ok(mut state) = shared.lock() {
                state.started = Some(Err(code));
            }
            shared.fail(code);
            return;
        }
    };
    let Some(child) = custody.0.as_mut() else {
        shared.fail("codex_host_custody_missing");
        return;
    };
    loop {
        if Instant::now() >= deadline {
            shared.fail("codex_host_deadline");
        }
        child.input(&mut input);
        child.read_output(output_limit);
        child.observe_exit();
        child.observe_group();
        child.release_held_pipes();
        if child.complete() {
            child.publish_completion();
            return;
        }
        if shared.stop.load(Ordering::Acquire) || input.is_closed() {
            child.stop();
        }
        std::thread::sleep(POLL_TIME);
    }
}

fn start(
    command: Command,
    profile: Option<NativeProfileUse>,
    shared: &Arc<ProcessShared>,
) -> Result<Custody, &'static str> {
    if let Some(profile) = &profile {
        profile
            .revalidate_retained()
            .map_err(|_| "codex_host_profile_invalid")?;
    }
    if shared.stop.load(Ordering::Acquire) {
        return Err("codex_host_start_cancelled");
    }
    let mut command = selected_command(command);
    let child = crate::spawn_gate::spawn(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|_| "codex_host_spawn_failed")?;
    let mut custody = Custody(Some(OwnedChild {
        child,
        profile,
        stdin: None,
        stdout: None,
        stderr: None,
        stdout_eof: false,
        stderr_eof: false,
        status: None,
        group_absent: false,
        group_gone_at: None,
        abandoned: false,
        pending: None,
        kill_sent: false,
        accounting_failed: false,
        shared: Arc::clone(shared),
    }));
    let retained = custody.0.as_mut().ok_or("codex_host_custody_missing")?;
    retained.prepare_pipes()?;
    let mut state = shared.lock().map_err(|_| "codex_host_custody_poisoned")?;
    state.started = Some(Ok(retained.child.id()));
    drop(state);
    Ok(custody)
}

fn selected_command(command: Command) -> Command {
    let mut selected = Command::new(command.get_program());
    selected.args(command.get_args()).env_clear();
    #[cfg(unix)]
    selected.process_group(0);
    if let Some(cwd) = command.get_current_dir() {
        selected.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            selected.env(key, value);
        }
    }
    selected
}

impl OwnedChild {
    fn prepare_pipes(&mut self) -> Result<(), &'static str> {
        self.stdin = self.child.stdin.take();
        self.stdout = self.child.stdout.take();
        self.stderr = self.child.stderr.take();
        nonblocking(self.stdin.as_ref().ok_or("codex_host_stdin_missing")?)?;
        nonblocking(self.stdout.as_ref().ok_or("codex_host_stdout_missing")?)?;
        nonblocking(self.stderr.as_ref().ok_or("codex_host_stderr_missing")?)?;
        Ok(())
    }

    fn input(&mut self, input: &mut mpsc::Receiver<WriteRequest>) {
        if self.shared.stop.load(Ordering::Acquire) {
            self.stdin = None;
            self.pending = None;
            input.close();
            while input.try_recv().is_ok() {}
            return;
        }
        if self.pending.is_none()
            && let Ok(request) = input.try_recv()
        {
            if let Some(profile) = &self.profile
                && let Err(error) = profile.revalidate_retained()
            {
                let _ = request.done.send(Err(error));
                self.shared.fail("codex_host_profile_invalid");
                return;
            }
            self.pending = Some(PendingWrite {
                request,
                written: 0,
            });
        }
        self.write_pending();
        if self.shared.close_stdin.load(Ordering::Acquire)
            && self.pending.is_none()
            && input.is_empty()
        {
            self.stdin = None;
        }
    }

    fn write_pending(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let result = match self.stdin.as_mut() {
            Some(stdin) => stdin.write(&pending.request.bytes[pending.written..]),
            None => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        };
        match result {
            Ok(0) => self.write_failed(),
            Ok(count) => {
                pending.written += count;
                assert!(pending.written <= pending.request.bytes.len());
                if pending.written == pending.request.bytes.len()
                    && let Some(done) = self.pending.take()
                {
                    let _ = done.request.done.send(Ok(()));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => self.write_failed(),
        }
    }

    fn write_failed(&mut self) {
        self.shared.fail("codex_host_input_failed");
        if let Some(pending) = self.pending.take() {
            let _ = pending
                .request
                .done
                .send(Err(EngineError("codex_host_input_failed".into())));
        }
    }

    fn read_output(&mut self, limit: u64) {
        let stdout = read_pipe(
            &mut self.stdout,
            &mut self.stdout_eof,
            false,
            &self.shared,
            limit,
        );
        let stderr = read_pipe(
            &mut self.stderr,
            &mut self.stderr_eof,
            true,
            &self.shared,
            limit,
        );
        if stdout.is_err() || stderr.is_err() {
            self.accounting_failed = true;
            self.shared.accounting_failed();
        }
    }

    fn observe_exit(&mut self) {
        if self.status.is_some() {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => self.status = Some(status),
            Ok(None) => {}
            Err(_) => {
                self.shared.fail("codex_host_wait_failed");
                if let Ok(mut state) = self.shared.lock() {
                    state.cleanup_error = Some("codex_host_wait_uncertain");
                }
            }
        }
    }

    fn stop(&mut self) {
        self.stdin = None;
        self.pending = None;
        self.kill_group();
        if self.status.is_some() || self.kill_sent {
            return;
        }
        self.kill_sent = true;
        if self.child.kill().is_err() {
            self.observe_exit();
            if self.status.is_none() {
                self.shared.fail("codex_host_kill_uncertain");
                if let Ok(mut state) = self.shared.lock() {
                    state.cleanup_error = Some("codex_host_kill_uncertain");
                }
            }
        }
    }

    /// Kills every process left in the harness group; the harness runs as its group leader.
    /// A group whose members are all still exiting can refuse the signal; `stop` retries it.
    /// Takes nothing; records an uncertain cleanup for any other refusal.
    #[cfg(unix)]
    fn kill_group(&mut self) {
        if self.group_absent {
            return;
        }
        match rustix::process::kill_process_group(Pid::from_child(&self.child), Signal::KILL) {
            Ok(()) | Err(Errno::SRCH | Errno::PERM) => {}
            Err(_) => self.cleanup_uncertain("codex_host_kill_uncertain"),
        }
    }

    /// Stands in for the group kill where processes have no groups: `stop` kills the child.
    #[cfg(not(unix))]
    fn kill_group(&mut self) {}

    /// Notes when no process of the harness group is left. Only a reaped harness can leave
    /// its group empty, and a group that refuses the probe still counts as present.
    /// Takes nothing; records an uncertain cleanup when the group cannot be observed.
    fn observe_group(&mut self) {
        if self.status.is_none() || self.group_absent {
            return;
        }
        #[cfg(unix)]
        match rustix::process::test_kill_process_group(Pid::from_child(&self.child)) {
            Err(Errno::SRCH) => {}
            Ok(()) | Err(Errno::PERM) => return,
            Err(_) => {
                self.cleanup_uncertain("codex_host_group_uncertain");
                return;
            }
        }
        self.group_absent = true;
    }

    /// Gives up the pipes once the harness is reaped, its group has been empty for
    /// `DRAIN_TIME` and a pipe is still open, which only a process outside the group can hold.
    /// Takes nothing; the completion then reports an uncertain cleanup.
    fn release_held_pipes(&mut self) {
        if self.status.is_none() || !self.group_absent || (self.stdout_eof && self.stderr_eof) {
            return;
        }
        let gone = *self.group_gone_at.get_or_insert_with(Instant::now);
        if gone.elapsed() < DRAIN_TIME {
            return;
        }
        self.stdout = None;
        self.stderr = None;
        self.stdout_eof = true;
        self.stderr_eof = true;
        self.abandoned = true;
    }

    fn cleanup_uncertain(&self, code: &'static str) {
        self.shared.fail(code);
        if let Ok(mut state) = self.shared.lock() {
            state.cleanup_error = Some(code);
        }
    }

    fn complete(&self) -> bool {
        self.status.is_some()
            && self.group_absent
            && self.stdout_eof
            && self.stderr_eof
            && !self.accounting_failed
    }

    /// Settles custody that ended before completion: kills the harness group until the
    /// harness is reaped and the group is empty, for at most `CLOSE_TIME`, so dropping it
    /// releases the profile lease only once its processes are gone or were sent a kill.
    fn settle(&mut self) {
        let deadline = Instant::now() + CLOSE_TIME;
        while !(self.status.is_some() && self.group_absent) {
            if Instant::now() >= deadline {
                self.cleanup_uncertain("codex_host_cleanup_uncertain");
                return;
            }
            self.stop();
            self.observe_exit();
            self.observe_group();
            std::thread::sleep(POLL_TIME);
        }
    }

    fn publish_completion(&mut self) {
        assert!(self.complete());
        assert!(self.status.is_some());
        if let Some(profile) = &self.profile
            && profile.revalidate_retained().is_err()
        {
            self.shared.fail("codex_host_profile_invalid");
        }
        self.profile = None;
        if let Ok(mut state) = self.shared.lock() {
            state.status = self.status;
            state.complete = true;
            state.cleanup_error = self.abandoned.then_some("codex_host_cleanup_uncertain");
        }
    }
}

impl Drop for Custody {
    fn drop(&mut self) {
        if let Some(mut retained) = self.0.take()
            && !retained.complete()
        {
            retained.shared.accounting_failed();
            retained.settle();
        }
    }
}

/// Refuses the host's child pipes where std offers no non-blocking anonymous pipe.
/// Takes the pipe, unused. Returns `codex_host_unavailable`.
#[cfg(not(unix))]
fn nonblocking<T>(_pipe: &T) -> Result<(), &'static str> {
    Err("codex_host_unavailable")
}

#[cfg(unix)]
fn nonblocking(pipe: &impl AsFd) -> Result<(), &'static str> {
    let flags = fcntl_getfl(pipe).map_err(|_| "codex_host_pipe_flags")?;
    fcntl_setfl(pipe, flags | OFlags::NONBLOCK).map_err(|_| "codex_host_pipe_flags")
}

fn read_pipe<R: Read>(
    pipe: &mut Option<R>,
    eof: &mut bool,
    stderr: bool,
    shared: &ProcessShared,
    limit: u64,
) -> Result<(), EngineError> {
    if *eof {
        return Ok(());
    }
    let reader = pipe
        .as_mut()
        .ok_or_else(|| EngineError("codex_host_pipe_missing".into()))?;
    let mut bytes = [0; READ_BYTES];
    for _ in 0..READS_PER_POLL {
        match reader.read(&mut bytes) {
            Ok(0) => {
                *eof = true;
                *pipe = None;
                return Ok(());
            }
            Ok(count) => record_output(shared, &bytes[..count], stderr, limit)?,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(EngineError("codex_host_pipe_accounting".into())),
        }
    }
    Ok(())
}

fn record_output(
    shared: &ProcessShared,
    bytes: &[u8],
    stderr: bool,
    limit: u64,
) -> Result<(), EngineError> {
    let mut state = shared.lock()?;
    let count = if stderr {
        &mut state.observed.1
    } else {
        &mut state.observed.0
    };
    *count = count
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| EngineError("codex_host_output_accounting".into()))?;
    let total = state.observed.0.checked_add(state.observed.1);
    if total.is_none_or(|total| total > limit) {
        state.error.get_or_insert("codex_host_output_limit");
        shared.stop.store(true, Ordering::Release);
        return Ok(());
    }
    if shared.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    if state.output.len() == OUTPUT_QUEUE_MAX {
        state.error.get_or_insert("codex_host_output_backpressure");
        shared.stop.store(true, Ordering::Release);
        return Ok(());
    }
    if stderr {
        return Ok(());
    }
    state
        .output
        .push_back(CodexHostOutput::Stdout(bytes.to_vec()));
    assert!(state.output.len() <= OUTPUT_QUEUE_MAX);
    Ok(())
}
