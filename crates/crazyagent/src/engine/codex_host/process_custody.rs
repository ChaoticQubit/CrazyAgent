use super::process::{
    CodexHostOutput, OUTPUT_QUEUE_MAX, POLL_TIME, ProcessShared, WriteRequest,
};
use crate::engine::EngineError;
use crate::engine::bootstrap::NativeProfileUse;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::mpsc;

const READ_BYTES: usize = 4096;
const READS_PER_POLL: usize = 16;

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
        profile.revalidate_retained().map_err(|_| "codex_host_profile_invalid")?;
    }
    if shared.stop.load(Ordering::Acquire) {
        return Err("codex_host_start_cancelled");
    }
    let mut command = selected_command(command);
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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
        if self.pending.is_none() {
            if let Ok(request) = input.try_recv() {
                if let Some(profile) = &self.profile {
                    if let Err(error) = profile.revalidate_retained() {
                        let _ = request.done.send(Err(error));
                        self.shared.fail("codex_host_profile_invalid");
                        return;
                    }
                }
                self.pending = Some(PendingWrite { request, written: 0 });
            }
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
                if pending.written == pending.request.bytes.len() {
                    if let Some(done) = self.pending.take() {
                        let _ = done.request.done.send(Ok(()));
                    }
                }
            }
            Err(error) if matches!(
                error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) => {}
            Err(_) => self.write_failed(),
        }
    }

    fn write_failed(&mut self) {
        self.shared.fail("codex_host_input_failed");
        if let Some(pending) = self.pending.take() {
            let _ = pending.request.done.send(Err(EngineError("codex_host_input_failed".into())));
        }
    }

    fn read_output(&mut self, limit: u64) {
        let stdout = read_pipe(
            &mut self.stdout, &mut self.stdout_eof, false, &self.shared, limit,
        );
        let stderr = read_pipe(
            &mut self.stderr, &mut self.stderr_eof, true, &self.shared, limit,
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

    fn complete(&self) -> bool {
        self.status.is_some() && self.stdout_eof && self.stderr_eof && !self.accounting_failed
    }

    fn publish_completion(&self) {
        assert!(self.complete());
        assert!(self.status.is_some());
        if let Some(profile) = &self.profile {
            if profile.revalidate_retained().is_err() {
                self.shared.fail("codex_host_profile_invalid");
            }
        }
        if let Ok(mut state) = self.shared.lock() {
            state.status = self.status;
            state.complete = true;
            state.cleanup_error = None;
        }
    }
}

impl Drop for Custody {
    fn drop(&mut self) {
        if let Some(mut retained) = self.0.take() {
            if !retained.complete() {
                retained.stop();
                retained.observe_exit();
                retained.shared.accounting_failed();
                std::mem::forget(retained);
            }
        }
    }
}

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
    let reader = pipe.as_mut().ok_or_else(|| EngineError("codex_host_pipe_missing".into()))?;
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
    let count = if stderr { &mut state.observed.1 } else { &mut state.observed.0 };
    *count = count.checked_add(bytes.len() as u64)
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
    let output = if stderr {
        CodexHostOutput::Stderr(bytes.to_vec())
    } else {
        CodexHostOutput::Stdout(bytes.to_vec())
    };
    state.output.push_back(output);
    assert!(state.output.len() <= OUTPUT_QUEUE_MAX);
    Ok(())
}
