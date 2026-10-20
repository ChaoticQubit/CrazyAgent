use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal};
use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIME_MAX: Duration = Duration::from_secs(600);
const CLEANUP_TIME_MAX: Duration = Duration::from_secs(5);
const STREAM_BYTES_MAX: usize = 65_536;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const TERM_GRACE: Duration = Duration::from_millis(100);
const DROP_TIME: Duration = Duration::from_secs(2);
const READ_BYTES: usize = 4096;
const READ_BATCH: usize = 16;

pub(super) struct ProcessBounds {
    pub(super) run_time: Duration,
    pub(super) cleanup_time: Duration,
    pub(super) stream_bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DirectIdentity {
    pub(super) pid: u32,
    pub(super) process_group: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ProcessStop {
    Exited,
    Deadline,
    Cancelled,
    StdoutLimit,
    StderrLimit,
}

#[derive(Default)]
pub(super) struct StreamObservation {
    pub(super) bytes: Vec<u8>,
    pub(super) observed_bytes: u64,
    pub(super) eof: bool,
    pub(super) truncated: bool,
}

pub(super) struct GroupCleanup {
    pub(super) direct_reaped: bool,
    pub(super) group_absent: bool,
    pub(super) uncertain: bool,
}

pub(super) struct ProcessObservation {
    pub(super) identity: DirectIdentity,
    pub(super) stop: ProcessStop,
    pub(super) status: Option<ExitStatus>,
    pub(super) stdout: StreamObservation,
    pub(super) stderr: StreamObservation,
    pub(super) cleanup: GroupCleanup,
}

struct Stream {
    descriptor: Option<OwnedFd>,
    observation: StreamObservation,
}

pub(super) struct OwnedProcess {
    child: Child,
    identity: DirectIdentity,
    bounds: ProcessBounds,
    run_deadline: Instant,
    stdout: Stream,
    stderr: Stream,
    status: Option<ExitStatus>,
    group_absent: bool,
    faulted: bool,
    finished: bool,
}

impl ProcessBounds {
    fn validate(&self, started: Instant) -> io::Result<Instant> {
        if self.run_time.is_zero()
            || self.run_time > RUN_TIME_MAX
            || self.cleanup_time.is_zero()
            || self.cleanup_time > CLEANUP_TIME_MAX
            || self.stream_bytes == 0
            || self.stream_bytes > STREAM_BYTES_MAX
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process time or stream bounds are outside the supported range",
            ));
        }
        let deadline = started.checked_add(self.run_time).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "process deadline overflow")
        })?;
        assert!(deadline > started);
        assert!(deadline.duration_since(started) <= RUN_TIME_MAX);
        Ok(deadline)
    }
}

impl Stream {
    fn new(descriptor: OwnedFd, retention: usize) -> Self {
        let stream = Self {
            descriptor: Some(descriptor),
            observation: StreamObservation {
                bytes: Vec::with_capacity(retention),
                ..StreamObservation::default()
            },
        };
        assert!(stream.observation.bytes.is_empty());
        assert!(!stream.observation.eof);
        stream
    }

    fn nonblocking(&self) -> io::Result<()> {
        let descriptor = self.descriptor.as_ref().expect("retained output pipe");
        let flags = fcntl_getfl(descriptor)?;
        fcntl_setfl(descriptor, flags | OFlags::NONBLOCK)?;
        if !fcntl_getfl(descriptor)?.contains(OFlags::NONBLOCK) {
            return Err(io::Error::other("output pipe did not become nonblocking"));
        }
        assert!(self.descriptor.is_some());
        assert!(!self.observation.eof);
        Ok(())
    }

    fn poll(&mut self, retention: usize) -> io::Result<()> {
        let mut buffer = [0_u8; READ_BYTES];
        for _ in 0..READ_BATCH {
            let Some(descriptor) = self.descriptor.as_ref() else {
                return Ok(());
            };
            match rustix::io::read(descriptor, &mut buffer[..]) {
                Ok(0) => {
                    self.observation.eof = true;
                    self.descriptor = None;
                    assert!(self.descriptor.is_none());
                    assert!(self.observation.eof);
                    return Ok(());
                }
                Ok(count) => self.record(&buffer[..count], retention)?,
                Err(rustix::io::Errno::AGAIN) => return Ok(()),
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn record(&mut self, bytes: &[u8], retention: usize) -> io::Result<()> {
        assert!(self.observation.bytes.len() <= retention);
        assert!(self.observation.observed_bytes >= self.observation.bytes.len() as u64);
        let observed = self
            .observation
            .observed_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("observed output byte count overflow"))?;
        let remaining = retention - self.observation.bytes.len();
        self.observation
            .bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        self.observation.observed_bytes = observed;
        self.observation.truncated = observed > self.observation.bytes.len() as u64;
        assert!(self.observation.bytes.len() <= retention);
        assert!(observed >= self.observation.bytes.len() as u64);
        assert_eq!(self.observation.truncated, observed > retention as u64);
        Ok(())
    }
}

impl OwnedProcess {
    pub(super) fn spawn(
        mut command: Command,
        cwd: &Path,
        environment: &[(OsString, OsString)],
        bounds: ProcessBounds,
    ) -> io::Result<Self> {
        let run_deadline = bounds.validate(Instant::now())?;
        let mut child = command
            .env_clear()
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let identity = DirectIdentity {
            pid: child.id(),
            process_group: child.id(),
        };
        let stdout = child.stdout.take().expect("configured stdout pipe");
        let stderr = child.stderr.take().expect("configured stderr pipe");
        let retention = bounds.stream_bytes + 1;
        let process = Self {
            child,
            identity,
            bounds,
            run_deadline,
            stdout: Stream::new(stdout.into(), retention),
            stderr: Stream::new(stderr.into(), retention),
            status: None,
            group_absent: false,
            faulted: false,
            finished: false,
        };
        assert!(process.identity.pid > 1);
        assert_eq!(process.identity.process_group, process.child.id());
        assert!(process.child.stdin.is_none());
        assert!(process.child.stdout.is_none());
        assert!(process.child.stderr.is_none());
        process.stdout.nonblocking()?;
        process.stderr.nonblocking()?;
        Ok(process)
    }

    pub(super) fn identity(&self) -> DirectIdentity {
        DirectIdentity {
            pid: self.identity.pid,
            process_group: self.identity.process_group,
        }
    }

    fn poll(&mut self) -> io::Result<()> {
        assert_eq!(self.child.id(), self.identity.pid);
        assert_eq!(self.identity.pid, self.identity.process_group);
        let retention = self.bounds.stream_bytes + 1;
        self.stdout.poll(retention)?;
        self.stderr.poll(retention)?;
        self.reap()?;
        assert!(
            self.stdout.observation.observed_bytes >= self.stdout.observation.bytes.len() as u64
        );
        assert!(
            self.stderr.observation.observed_bytes >= self.stderr.observation.bytes.len() as u64
        );
        Ok(())
    }

    fn reap(&mut self) -> io::Result<()> {
        assert_eq!(self.child.id(), self.identity.pid);
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        assert_eq!(self.child.id(), self.identity.pid);
        Ok(())
    }

    fn output_limit(&self) -> Option<ProcessStop> {
        let retention = self.bounds.stream_bytes + 1;
        assert!(self.stdout.observation.bytes.len() <= retention);
        assert!(self.stderr.observation.bytes.len() <= retention);
        assert!(
            self.stdout.observation.observed_bytes >= self.stdout.observation.bytes.len() as u64
        );
        assert!(
            self.stderr.observation.observed_bytes >= self.stderr.observation.bytes.len() as u64
        );
        if self.stdout.observation.observed_bytes > self.bounds.stream_bytes as u64 {
            Some(ProcessStop::StdoutLimit)
        } else if self.stderr.observation.observed_bytes > self.bounds.stream_bytes as u64 {
            Some(ProcessStop::StderrLimit)
        } else {
            None
        }
    }

    fn wait_for_stop(&mut self, cancelled: &AtomicBool) -> io::Result<ProcessStop> {
        loop {
            self.poll()?;
            if let Some(stop) = self.output_limit() {
                return Ok(stop);
            }
            if cancelled.load(Ordering::Acquire) {
                return Ok(ProcessStop::Cancelled);
            }
            if self.status.is_some() {
                return Ok(ProcessStop::Exited);
            }
            if Instant::now() >= self.run_deadline {
                return Ok(ProcessStop::Deadline);
            }
            sleep_until(self.run_deadline);
        }
    }

    fn group_pid(&self) -> Pid {
        assert!(self.identity.pid > 1);
        assert_eq!(self.identity.process_group, self.identity.pid);
        Pid::from_raw(i32::try_from(self.identity.process_group).expect("OS child PID range"))
            .expect("positive child group")
    }

    fn observe_group(&mut self) -> io::Result<()> {
        if !self.group_absent {
            match rustix::process::test_kill_process_group(self.group_pid()) {
                Ok(()) => {}
                Err(rustix::io::Errno::SRCH) => self.group_absent = true,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn signal_group(&mut self, signal: Signal) -> io::Result<()> {
        if !self.group_absent {
            match rustix::process::kill_process_group(self.group_pid(), signal) {
                Ok(()) => {}
                Err(rustix::io::Errno::SRCH) => self.group_absent = true,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn kill_direct(&mut self) -> io::Result<()> {
        if self.status.is_none() {
            match rustix::process::kill_process(self.group_pid(), Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<GroupCleanup> {
        let started = Instant::now();
        let deadline = started + self.bounds.cleanup_time;
        let kill_at = started + TERM_GRACE.min(self.bounds.cleanup_time);
        assert!(deadline > started);
        assert!(kill_at <= deadline);
        let mut term_sent = false;
        let mut kill_sent = false;
        while Instant::now() < deadline {
            self.poll()?;
            self.observe_group()?;
            if self.cleanup_complete() {
                return Ok(self.cleanup_observation(Instant::now() >= deadline));
            }
            if !term_sent {
                self.signal_group(Signal::TERM)?;
                term_sent = true;
            }
            if !kill_sent && Instant::now() >= kill_at {
                self.signal_group(Signal::KILL)?;
                self.kill_direct()?;
                kill_sent = true;
            }
            sleep_until(deadline);
        }
        Ok(self.cleanup_observation(true))
    }

    fn cleanup_complete(&self) -> bool {
        self.status.is_some()
            && self.group_absent
            && self.stdout.observation.eof
            && self.stderr.observation.eof
    }

    fn cleanup_observation(&self, expired: bool) -> GroupCleanup {
        let cleanup = GroupCleanup {
            direct_reaped: self.status.is_some(),
            group_absent: self.group_absent,
            uncertain: expired || self.faulted || !self.cleanup_complete(),
        };
        assert_eq!(cleanup.direct_reaped, self.status.is_some());
        assert_eq!(cleanup.group_absent, self.group_absent);
        assert!(cleanup.uncertain || self.cleanup_complete());
        cleanup
    }

    pub(super) fn finish(&mut self, cancelled: &AtomicBool) -> io::Result<ProcessObservation> {
        if self.finished {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process observation already consumed",
            ));
        }
        let stopped = self.wait_for_stop(cancelled);
        self.faulted |= stopped.is_err();
        let cleaned = self.cleanup();
        self.faulted |= cleaned.is_err();
        let (mut stop, cleanup) = match (stopped, cleaned) {
            (Ok(stop), Ok(cleanup)) => (stop, cleanup),
            (Err(run), Err(cleanup)) => {
                return Err(io::Error::other(format!(
                    "process observation failed ({run}); cleanup failed ({cleanup})"
                )));
            }
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => return Err(error),
        };
        if matches!(stop, ProcessStop::Exited) {
            stop = self.output_limit().unwrap_or(stop);
        }
        self.finished = true;
        assert_eq!(cleanup.direct_reaped, self.status.is_some());
        assert!(cleanup.uncertain || self.cleanup_complete());
        Ok(ProcessObservation {
            identity: self.identity(),
            stop,
            status: self.status,
            stdout: std::mem::take(&mut self.stdout.observation),
            stderr: std::mem::take(&mut self.stderr.observation),
            cleanup,
        })
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.status.is_some() && self.group_absent {
            return;
        }
        let deadline = Instant::now() + DROP_TIME;
        if self.signal_group(Signal::KILL).is_err() {
            self.faulted = true;
        }
        if self.kill_direct().is_err() {
            self.faulted = true;
        }
        while Instant::now() < deadline {
            if self.reap().is_err() {
                self.faulted = true;
            }
            if self.observe_group().is_err() {
                self.faulted = true;
            }
            if self.status.is_some() && self.group_absent {
                return;
            }
            sleep_until(deadline);
        }
        self.faulted = true;
    }
}

fn sleep_until(deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    thread::sleep(POLL_INTERVAL.min(remaining));
}

pub(super) fn retain_direct_child(
    retained: &mut Option<Child>,
    incoming: Child,
) -> Result<(), Child> {
    if retained.is_some() {
        return Err(incoming);
    }
    let direct = incoming.id();
    *retained = Some(incoming);
    assert!(retained.is_some());
    assert_eq!(retained.as_ref().map(Child::id), Some(direct));
    Ok(())
}

pub(super) fn cleanup_direct_child(
    child: &mut Child,
    deadline: Instant,
) -> io::Result<Option<ExitStatus>> {
    if let Some(status) = child.try_wait()? {
        return Ok(Some(status));
    }
    let started = Instant::now();
    if started >= deadline {
        return Ok(None);
    }
    let deadline = deadline.min(started + DROP_TIME);
    assert!(deadline > started);
    assert!(deadline.duration_since(started) <= DROP_TIME);
    if let Err(error) = child.kill() {
        return match child.try_wait()? {
            Some(status) => Ok(Some(status)),
            None => Err(error),
        };
    }
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep_until(deadline);
    }
}

use crate::engine::EngineError;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::RequestId;

pub(in crate::engine) struct CustodySession {
    child: Option<Child>,
    child_attempted: bool,
    guard: Option<NativeProfileUse>,
}

pub(in crate::engine) struct Retirement {
    guard: NativeProfileUse,
}

impl CustodySession {
    pub(in crate::engine) fn new(guard: NativeProfileUse) -> Self {
        let session = Self {
            child: None,
            child_attempted: false,
            guard: Some(guard),
        };
        assert!(session.child.is_none());
        assert!(!session.child_attempted);
        session
    }

    pub(in crate::engine) fn retain_child(
        &mut self,
        child: Child,
    ) -> Result<(), (EngineError, Child)> {
        let previously_attempted = self.child_attempted;
        self.child_attempted = true;
        if previously_attempted {
            return Err((
                EngineError("native_auth_child_already_attempted".into()),
                child,
            ));
        }
        match retain_direct_child(&mut self.child, child) {
            Ok(()) => {
                assert!(self.child_attempted);
                assert!(self.child.is_some());
                Ok(())
            }
            Err(child) => Err((
                EngineError("native_auth_child_already_retained".into()),
                child,
            )),
        }
    }

    pub(in crate::engine) fn seal_without_effect(mut self) -> Result<Retirement, EngineError> {
        if self.child_attempted || self.child.is_some() {
            return Err(EngineError(
                "native_auth_child_effect_requires_retirement".into(),
            ));
        }
        let guard = self
            .guard
            .as_ref()
            .expect("session retains its physical guard");
        guard.revalidate_retained()?;
        let guard = self
            .guard
            .take()
            .expect("validated physical guard remains retained");
        assert!(self.child.is_none());
        assert!(!self.child_attempted);
        Ok(Retirement { guard })
    }
}

impl Drop for CustodySession {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            match cleanup_direct_child(child, Instant::now() + DROP_TIME) {
                Ok(Some(_)) => {}
                Ok(None) => eprintln!("native_auth_direct_child_cleanup_deadline_uncertain"),
                Err(_) => eprintln!("native_auth_direct_child_cleanup_error_uncertain"),
            }
        }
    }
}

impl Retirement {
    pub(in crate::engine) fn revalidate(
        &self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        operation_id: &RequestId,
        generation: u64,
        purpose: NativeProfileUsePurpose,
    ) -> Result<(), EngineError> {
        self.guard
            .revalidate(host, binding, operation_id, generation, purpose)
    }
}
