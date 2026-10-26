use super::{ClaudeHostLimits, Fault, Phase, Status, Turn, validate_command};
use crate::adapters::claude::{ClaudeProtocol, State};
use crate::adapters::events::{EventKind, ObservedEvent};
use crate::engine::bootstrap::NativeProfileUse;
use crate::sandbox::InstalledSandbox;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{Pid, Signal};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

const POLL_TIME: Duration = Duration::from_millis(5);
const READ_BATCH: usize = 16;

/// How long cleanup lets the harness exit on its own before it kills the harness group, and
/// how long it still drains the pipes once that group is empty: a pipe still open after it
/// is held by a process that left the group.
const GRACE: Duration = Duration::from_millis(100);

pub(super) struct Channels {
    pub(super) input: Receiver<Turn>,
    pub(super) output: mpsc::Sender<ObservedEvent>,
    pub(super) report: watch::Sender<Status>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) group: Arc<AtomicU32>,
}

struct PendingInput {
    bytes: Vec<u8>,
    written: usize,
    reply: tokio::sync::oneshot::Sender<Result<(), Fault>>,
}

struct Process {
    child: Child,
    group: Pid,
    group_absent: bool,
    profile: Option<NativeProfileUse>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    protocol: ClaudeProtocol,
    limits: ClaudeHostLimits,
    input: Receiver<Turn>,
    output: mpsc::Sender<ObservedEvent>,
    report: watch::Sender<Status>,
    stop: Arc<AtomicBool>,
    pending: Option<PendingInput>,
    status: Option<ExitStatus>,
    fault: Option<Fault>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    events: usize,
    completed_turns: usize,
    frame: Vec<u8>,
    deadline: Instant,
}

pub(super) fn run(
    mut command: Command,
    sandbox: InstalledSandbox,
    profile: NativeProfileUse,
    protocol: ClaudeProtocol,
    limits: ClaudeHostLimits,
    channels: Channels,
) {
    let Channels {
        input,
        output,
        report,
        stop,
        group,
    } = channels;
    if let Err(error) = validate_command(&command, &profile) {
        report.send_replace(Status {
            phase: Phase::Retired,
            fault: Some(error),
        });
        return;
    }
    if stop.load(Ordering::Acquire) {
        report.send_replace(Status {
            phase: Phase::Retired,
            fault: None,
        });
        return;
    }
    let home = match profile.profile_path() {
        Ok(home) => home,
        Err(_) => {
            report.send_replace(Status {
                phase: Phase::Retired,
                fault: Some(Fault::Host("claude_host_profile_changed")),
            });
            return;
        }
    };
    command
        .env_clear()
        .env("HOME", &home)
        .env("CLAUDE_CONFIG_DIR", &home)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", &home)
        .env("CLAUDE_CODE_TMPDIR", sandbox.sandbox().agent_tmp())
        .env("TERM", "dumb")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut command = match sandbox.wrap(&command) {
        Ok(command) => command,
        Err(_) => {
            report.send_replace(Status {
                phase: Phase::Retired,
                fault: Some(Fault::Host("claude_host_sandbox_refused")),
            });
            return;
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert_eq!(command.get_program(), sandbox.launcher().as_os_str());
    let child = match crate::spawn_gate::spawn(&mut command) {
        Ok(child) => child,
        Err(_) => {
            report.send_replace(Status {
                phase: Phase::Retired,
                fault: Some(Fault::Host("claude_host_spawn_failed")),
            });
            return;
        }
    };
    assert!(child.id() > 1);
    group.store(child.id(), Ordering::Release);
    let mut process = Process {
        group: Pid::from_child(&child),
        group_absent: false,
        child,
        profile: Some(profile),
        stdin: None,
        stdout: None,
        stderr: None,
        protocol,
        limits,
        input,
        output,
        report,
        stop,
        pending: None,
        status: None,
        fault: None,
        stdout_bytes: 0,
        stderr_bytes: 0,
        events: 0,
        completed_turns: 0,
        frame: Vec::new(),
        deadline: Instant::now() + limits.run_time,
    };
    process.stdin = process.child.stdin.take();
    process.stdout = process.child.stdout.take();
    process.stderr = process.child.stderr.take();
    if process.prepare().is_ok() {
        process.report.send_replace(Status {
            phase: Phase::Running,
            fault: None,
        });
        process.drive();
    } else {
        process.fail(Fault::Host("claude_host_stdio_unavailable"));
    }
    process.cleanup();
}

impl Process {
    fn prepare(&self) -> io::Result<()> {
        let stdin = self.stdin.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        let stdout = self.stdout.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        let stderr = self.stderr.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        for descriptor in [stdin.as_fd(), stdout.as_fd(), stderr.as_fd()] {
            let flags = fcntl_getfl(descriptor)?;
            fcntl_setfl(descriptor, flags | OFlags::NONBLOCK)?;
        }
        assert!(self.child.id() > 1);
        assert!(self.child.stdin.is_none());
        assert!(self.child.stdout.is_none());
        assert!(self.child.stderr.is_none());
        Ok(())
    }

    fn fail(&mut self, fault: Fault) {
        if self.fault.is_none() {
            self.fault = Some(fault);
        }
    }

    fn drive(&mut self) {
        assert!(self.status.is_none());
        assert!(self.pending.is_none());
        loop {
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            if Instant::now() >= self.deadline {
                self.fail(Fault::Host("claude_host_deadline"));
                return;
            }
            self.read_stdout(true);
            self.read_stderr();
            self.reap();
            if self.status.is_some() {
                if self.fault.is_none() && (self.stdout.is_some() || self.stderr.is_some()) {
                    std::thread::sleep(POLL_TIME);
                    continue;
                }
                if self.fault.is_none()
                    && !(self.status.is_some_and(|status| status.success())
                        && self.completed_turns > 0
                        && self.protocol.state() == State::Idle
                        && self.frame.is_empty())
                {
                    self.fail(Fault::Exit(self.status.and_then(|status| status.code())));
                }
                return;
            }
            if self.fault.is_some() {
                return;
            }
            if self.stdout.is_none() {
                if self.protocol.state() == State::Idle && self.completed_turns > 0 {
                    std::thread::sleep(POLL_TIME);
                    continue;
                }
                self.fail(Fault::Host("claude_host_disconnected"));
                return;
            }
            self.write_input();
            if self.fault.is_some() {
                return;
            }
            std::thread::sleep(POLL_TIME);
        }
    }

    fn write_input(&mut self) {
        if self.pending.is_none() {
            match self.input.try_recv() {
                Ok(turn) => {
                    if self
                        .profile
                        .as_ref()
                        .is_none_or(|profile| profile.revalidate_retained().is_err())
                    {
                        let fault = Fault::Host("claude_host_profile_changed");
                        let _ = turn.reply.send(Err(fault));
                        self.fail(fault);
                        return;
                    }
                    match self.protocol.start_turn(turn.request_id, &turn.text) {
                        Ok(bytes) => {
                            self.pending = Some(PendingInput {
                                bytes,
                                written: 0,
                                reply: turn.reply,
                            });
                        }
                        Err(error) => {
                            let fault = Fault::Protocol(error);
                            let _ = turn.reply.send(Err(fault));
                            self.fail(fault);
                            return;
                        }
                    }
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.stop.store(true, Ordering::Release);
                    return;
                }
            }
        }
        let pending = self.pending.as_mut().expect("retained native input");
        assert!(pending.written < pending.bytes.len());
        assert!(pending.bytes.len() <= crate::protocol::FRAME_BYTES_MAX);
        let Some(stdin) = self.stdin.as_mut() else {
            self.fail(Fault::Host("claude_host_input_closed"));
            return;
        };
        match stdin.write(&pending.bytes[pending.written..]) {
            Ok(0) => self.fail(Fault::Host("claude_host_input_closed")),
            Ok(count) => {
                pending.written += count;
                assert!(pending.written <= pending.bytes.len());
                if pending.written == pending.bytes.len() {
                    let pending = self.pending.take().expect("completed native input");
                    let _ = pending.reply.send(Ok(()));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => self.fail(Fault::Host("claude_host_input_failed")),
        }
    }

    fn read_stdout(&mut self, decode: bool) {
        let mut buffer = [0_u8; 4096];
        for _ in 0..READ_BATCH {
            let Some(stdout) = self.stdout.as_mut() else {
                return;
            };
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    self.stdout = None;
                    if !self.frame.is_empty() && !self.stop.load(Ordering::Acquire) {
                        self.fail(Fault::Host("claude_host_truncated_output"));
                    }
                    return;
                }
                Ok(count) => {
                    self.stdout_bytes = self.stdout_bytes.saturating_add(count);
                    if self.stdout_bytes > self.limits.stdout_bytes {
                        self.fail(Fault::Host("claude_host_stdout_limit"));
                    }
                    if self.stdout_bytes.saturating_add(self.stderr_bytes)
                        > self.limits.stdout_bytes
                    {
                        self.fail(Fault::Host("claude_host_output_limit"));
                    }
                    if decode && self.fault.is_none() {
                        self.decode(&buffer[..count]);
                    }
                    assert!(self.events <= self.limits.events);
                    assert!(count <= buffer.len());
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.fail(Fault::Host("claude_host_stdout_failed"));
                    return;
                }
            }
        }
    }

    fn decode(&mut self, bytes: &[u8]) {
        assert!(bytes.len() <= 4096);
        assert!(self.frame.len() <= crate::protocol::FRAME_BYTES_MAX);
        for byte in bytes {
            if self.frame.len() == crate::protocol::FRAME_BYTES_MAX {
                self.fail(Fault::Host("claude_host_frame_limit"));
                return;
            }
            self.frame.push(*byte);
            if *byte != b'\n' {
                continue;
            }
            let events = match self.protocol.receive(&self.frame) {
                Ok(events) => events,
                Err(error) => {
                    self.fail(Fault::Protocol(error));
                    return;
                }
            };
            self.frame.clear();
            for event in events {
                if self.events == self.limits.events {
                    self.fail(Fault::Host("claude_host_event_limit"));
                    return;
                }
                self.events += 1;
                if matches!(&event.kind, EventKind::TurnFinished { .. }) {
                    self.completed_turns += 1;
                }
                let native_tool = matches!(&event.kind, EventKind::ToolUse { .. });
                if self.output.try_send(event.into()).is_err() {
                    self.fail(Fault::Host("claude_host_event_delivery_failed"));
                    return;
                }
                if native_tool {
                    self.fail(Fault::Host("claude_host_native_tools_unsupported"));
                    return;
                }
            }
        }
        assert!(self.frame.len() <= crate::protocol::FRAME_BYTES_MAX);
        assert!(self.completed_turns <= self.events);
    }

    fn read_stderr(&mut self) {
        let mut buffer = [0_u8; 4096];
        for _ in 0..READ_BATCH {
            let Some(stderr) = self.stderr.as_mut() else {
                return;
            };
            match stderr.read(&mut buffer) {
                Ok(0) => {
                    self.stderr = None;
                    return;
                }
                Ok(count) => {
                    self.stderr_bytes = self.stderr_bytes.saturating_add(count);
                    if self.stderr_bytes > self.limits.stderr_bytes {
                        self.fail(Fault::Host("claude_host_stderr_limit"));
                    }
                    if self.stdout_bytes.saturating_add(self.stderr_bytes)
                        > self.limits.stdout_bytes
                    {
                        self.fail(Fault::Host("claude_host_output_limit"));
                    }
                    assert!(count <= buffer.len());
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.fail(Fault::Host("claude_host_stderr_failed"));
                    return;
                }
            }
        }
    }

    fn reap(&mut self) {
        assert!(self.child.stdout.is_none());
        assert!(self.child.stderr.is_none());
        if self.status.is_none() {
            match self.child.try_wait() {
                Ok(status) => self.status = status,
                Err(_) => self.fail(Fault::Host("claude_host_wait_failed")),
            }
        }
    }

    /// Notes when no process of the harness group is left. Only a reaped harness can leave its
    /// group empty, and a group that refuses the probe still counts as present.
    /// Takes nothing; records a fault when the group cannot be observed.
    fn observe_group(&mut self) {
        if self.status.is_none() || self.group_absent {
            return;
        }
        match rustix::process::test_kill_process_group(self.group) {
            Err(Errno::SRCH) => self.group_absent = true,
            Ok(()) | Err(Errno::PERM) => {}
            Err(_) => self.fail(Fault::Host("claude_host_group_unobservable")),
        }
    }

    /// Kills every process left in the harness group, and the harness itself while it is not
    /// reaped, in case it left its group. A group whose members are all still exiting can
    /// refuse the signal, and an exiting harness is already gone for `kill`; both are retried.
    /// Takes nothing. Returns whether a kill signal reached a process.
    fn kill_group(&mut self) -> bool {
        let mut sent = match rustix::process::kill_process_group(self.group, Signal::KILL) {
            Ok(()) => true,
            Err(Errno::SRCH | Errno::PERM) => false,
            Err(_) => {
                self.fail(Fault::Host("claude_host_kill_failed"));
                false
            }
        };
        if self.status.is_none() {
            match self.child.kill() {
                Ok(()) => sent = true,
                Err(error) if error.raw_os_error() == Some(Errno::SRCH.raw_os_error()) => {}
                Err(_) => {
                    self.reap();
                    if self.status.is_none() {
                        self.fail(Fault::Host("claude_host_kill_failed"));
                    }
                }
            }
        }
        sent
    }

    fn cleanup(&mut self) {
        self.report.send_replace(Status {
            phase: Phase::Closing,
            fault: self.fault,
        });
        self.stdin = None;
        if let Some(pending) = self.pending.take() {
            let _ = pending
                .reply
                .send(Err(Fault::Host("claude_host_input_closed")));
        }
        while let Ok(turn) = self.input.try_recv() {
            let _ = turn
                .reply
                .send(Err(Fault::Host("claude_host_input_closed")));
        }
        assert!(self.stdin.is_none());
        assert!(self.pending.is_none());
        let started = Instant::now();
        let cleanup_deadline = started + self.limits.cleanup_time;
        let kill_at = started + GRACE.min(self.limits.cleanup_time);
        let mut uncertain = false;
        let mut killed = false;
        let mut group_gone_at = None;
        loop {
            self.read_stdout(false);
            self.read_stderr();
            self.reap();
            self.observe_group();
            let now = Instant::now();
            if let Some(status) = self.status
                && self.group_absent
            {
                let gone = *group_gone_at.get_or_insert(now);
                let open = self.stdout.is_some() || self.stderr.is_some();
                if !open || now >= gone + GRACE {
                    if open {
                        self.stdout = None;
                        self.stderr = None;
                        self.fail(Fault::Host("claude_host_cleanup_uncertain"));
                    }
                    if !status.success()
                        && !self.stop.load(Ordering::Acquire)
                        && !(killed && status.signal() == Some(9))
                    {
                        self.fail(Fault::Exit(status.code()));
                    }
                    break;
                }
            } else if now >= kill_at {
                killed |= self.kill_group();
            }
            if !uncertain && now >= cleanup_deadline {
                uncertain = true;
                self.fail(Fault::Host("claude_host_cleanup_uncertain"));
                self.report.send_replace(Status {
                    phase: Phase::Closing,
                    fault: self.fault,
                });
            }
            std::thread::sleep(POLL_TIME);
        }
        assert!(self.status.is_some());
        assert!(self.stdout.is_none());
        assert!(self.stderr.is_none());
        assert!(self.group_absent);
        if self.protocol.state() == State::Active && !self.stop.load(Ordering::Acquire) {
            self.fail(Fault::Host("claude_host_incomplete_turn"));
        }
        self.profile = None;
        self.report.send_replace(Status {
            phase: Phase::Retired,
            fault: self.fault,
        });
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.profile.is_some() {
            self.stop.store(true, Ordering::Release);
            self.fail(Fault::Host("claude_host_custodian_interrupted"));
            self.cleanup();
        }
    }
}
