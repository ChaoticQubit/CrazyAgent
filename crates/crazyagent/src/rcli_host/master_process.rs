use super::master_launch::error;
use crate::protocol::{FRAME_BYTES_MAX, ProtocolError};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

const STDOUT_BYTES_MAX: usize = 16_777_216;
const STDERR_BYTES_MAX: usize = 16_384;
const WRITE_TIME: Duration = Duration::from_secs(1);
const CLEANUP_TIME: Duration = Duration::from_secs(6);
const INTERVAL: Duration = Duration::from_millis(5);

pub(super) struct HostProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    pending: Vec<u8>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    status: Option<ExitStatus>,
    group_absent: bool,
    clean: bool,
}

impl HostProcess {
    pub(super) fn spawn(mut command: Command) -> Result<Self, ProtocolError> {
        let mut child = crate::spawn_gate::spawn(&mut command)
            .map_err(|_| error("rcli_master_spawn_failed"))?;
        let stdin = child.stdin.take().expect("configured host input pipe");
        let stdout = child.stdout.take().expect("configured host output pipe");
        let stderr = child.stderr.take().expect("configured host error pipe");
        let process = Self {
            child,
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr: Some(stderr),
            pending: Vec::new(),
            stdout_bytes: 0,
            stderr_bytes: 0,
            status: None,
            group_absent: false,
            clean: false,
        };
        assert!(process.child.id() > 1);
        assert!(process.pending.is_empty());
        nonblocking(process.stdin.as_ref().expect("retained host input"))?;
        nonblocking(process.stdout.as_ref().expect("retained host output"))?;
        nonblocking(process.stderr.as_ref().expect("retained host error"))?;
        Ok(process)
    }

    pub(super) fn write(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        assert!(!bytes.is_empty());
        assert!(bytes.len() <= FRAME_BYTES_MAX + 4);
        let deadline = Instant::now() + WRITE_TIME;
        let mut written = 0;
        while written < bytes.len() && Instant::now() < deadline {
            let input = self
                .stdin
                .as_mut()
                .ok_or_else(|| error("rcli_master_input_closed"))?;
            match input.write(&bytes[written..]) {
                Ok(0) => return Err(error("rcli_master_input_closed")),
                Ok(count) => written += count,
                Err(failure) if failure.kind() == io::ErrorKind::Interrupted => {}
                Err(failure) if failure.kind() == io::ErrorKind::WouldBlock => {
                    self.read_stderr()?;
                    thread::sleep(INTERVAL);
                }
                Err(_) => return Err(error("rcli_master_input_failed")),
            }
        }
        if written != bytes.len() {
            return Err(error("rcli_master_input_deadline"));
        }
        assert_eq!(written, bytes.len());
        Ok(())
    }

    pub(super) fn frame(&mut self) -> Result<Option<Vec<u8>>, ProtocolError> {
        self.read_stderr()?;
        self.reap()?;
        let mut scratch = [0; 4096];
        for _ in 0..20 {
            let target = self.frame_size()?;
            if self.pending.len() == target && target > 4 {
                assert!(target <= FRAME_BYTES_MAX + 4);
                return Ok(Some(std::mem::take(&mut self.pending)));
            }
            let remaining = (target - self.pending.len()).min(scratch.len());
            let Some(output) = self.stdout.as_mut() else {
                return Err(self.exit_error());
            };
            match output.read(&mut scratch[..remaining]) {
                Ok(0) => {
                    self.stdout = None;
                    self.reap()?;
                    return Err(if self.pending.is_empty() {
                        self.exit_error()
                    } else {
                        error("rcli_master_truncated_event")
                    });
                }
                Ok(count) => {
                    self.stdout_bytes += count;
                    if self.stdout_bytes > STDOUT_BYTES_MAX {
                        return Err(error("rcli_master_output_limit"));
                    }
                    self.pending.extend_from_slice(&scratch[..count]);
                }
                Err(failure) if failure.kind() == io::ErrorKind::Interrupted => {}
                Err(failure) if failure.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => return Err(error("rcli_master_output_failed")),
            }
        }
        assert!(self.pending.len() <= FRAME_BYTES_MAX + 4);
        let target = self.frame_size()?;
        if self.pending.len() == target && target > 4 {
            return Ok(Some(std::mem::take(&mut self.pending)));
        }
        if self.status.is_some() {
            return Err(self.exit_error());
        }
        Ok(None)
    }

    fn frame_size(&self) -> Result<usize, ProtocolError> {
        if self.pending.len() < 4 {
            return Ok(4);
        }
        let header: [u8; 4] = self.pending[..4].try_into().expect("complete host header");
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > FRAME_BYTES_MAX {
            return Err(error("rcli_master_event_length"));
        }
        assert!(length <= FRAME_BYTES_MAX);
        assert!(self.pending.len() <= length + 4);
        Ok(length + 4)
    }

    fn read_stderr(&mut self) -> Result<(), ProtocolError> {
        let mut scratch = [0; 1024];
        for _ in 0..16 {
            let Some(output) = self.stderr.as_mut() else {
                return Ok(());
            };
            match output.read(&mut scratch) {
                Ok(0) => {
                    self.stderr = None;
                    return Ok(());
                }
                Ok(count) => {
                    self.stderr_bytes += count;
                    if self.stderr_bytes > STDERR_BYTES_MAX {
                        return Err(error("rcli_master_stderr_limit"));
                    }
                }
                Err(failure) if failure.kind() == io::ErrorKind::Interrupted => {}
                Err(failure) if failure.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(_) => return Err(error("rcli_master_stderr_failed")),
            }
        }
        assert!(self.stderr_bytes <= STDERR_BYTES_MAX);
        assert!(self.child.id() > 1);
        Ok(())
    }

    fn exit_error(&self) -> ProtocolError {
        error(match self.status.as_ref().and_then(ExitStatus::code) {
            Some(64) => "rcli_master_control_rejected",
            Some(70) => "rcli_master_execution_failed",
            _ => "rcli_master_process_exited",
        })
    }

    fn reap(&mut self) -> Result<(), ProtocolError> {
        if self.status.is_none() {
            self.status = self
                .child
                .try_wait()
                .map_err(|_| error("rcli_master_wait_failed"))?;
        }
        Ok(())
    }

    fn group(&self) -> Pid {
        assert!(self.child.id() > 1);
        Pid::from_raw(i32::try_from(self.child.id()).expect("OS child PID range"))
            .expect("positive host group")
    }

    fn observe_group(&mut self) -> Result<(), ProtocolError> {
        if !self.group_absent {
            match rustix::process::test_kill_process_group(self.group()) {
                Ok(()) => {}
                Err(rustix::io::Errno::SRCH) => self.group_absent = true,
                Err(rustix::io::Errno::PERM) => self.exited_group()?,
                Err(_) => return Err(error("rcli_master_cleanup_incomplete")),
            }
        }
        Ok(())
    }

    fn signal(&mut self, signal: Signal) -> Result<(), ProtocolError> {
        if !self.group_absent {
            match rustix::process::kill_process_group(self.group(), signal) {
                Ok(()) => {}
                Err(rustix::io::Errno::SRCH) => self.group_absent = true,
                Err(rustix::io::Errno::PERM) => self.exited_group()?,
                Err(_) => return Err(error("rcli_master_cleanup_incomplete")),
            }
        }
        Ok(())
    }

    /// Settles a group signal refused with EPERM, which macOS returns while every group
    /// member has exited but some are not reaped yet. Input: the retained child. Output: Ok
    /// once the leader is reaped, marking the group absent only after ESRCH; members that
    /// are still exiting leave the group present so the cleanup deadline decides.
    fn exited_group(&mut self) -> Result<(), ProtocolError> {
        self.reap()?;
        if self.status.is_none() {
            return Err(error("rcli_master_cleanup_incomplete"));
        }
        match rustix::process::test_kill_process_group(self.group()) {
            Ok(()) | Err(rustix::io::Errno::PERM) => Ok(()),
            Err(rustix::io::Errno::SRCH) => {
                self.group_absent = true;
                Ok(())
            }
            Err(_) => Err(error("rcli_master_cleanup_incomplete")),
        }
    }

    pub(super) fn cleanup(&mut self) -> Result<(), ProtocolError> {
        if self.clean {
            return Ok(());
        }
        self.stdin = None;
        self.stdout = None;
        self.stderr = None;
        self.pending.clear();
        let started = Instant::now();
        let deadline = started + CLEANUP_TIME;
        let mut terminated = false;
        let mut killed = false;
        let mut uncertain = false;
        while Instant::now() < deadline {
            uncertain |= self.reap().is_err();
            uncertain |= self.observe_group().is_err();
            if self.status.is_some() && self.group_absent {
                self.clean = true;
                assert!(self.stdin.is_none());
                assert!(self.status.is_some());
                return if uncertain {
                    Err(error("rcli_master_cleanup_incomplete"))
                } else {
                    Ok(())
                };
            }
            if !terminated && started.elapsed() >= Duration::from_millis(250) {
                uncertain |= self.signal(Signal::TERM).is_err();
                terminated = true;
            }
            if !killed && started.elapsed() >= Duration::from_millis(500) {
                uncertain |= self.signal(Signal::KILL).is_err();
                if self.status.is_none() && self.child.kill().is_err() {
                    uncertain |= self.reap().is_err() || self.status.is_none();
                }
                killed = true;
            }
            thread::sleep(INTERVAL);
        }
        Err(error("rcli_master_cleanup_incomplete"))
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if !self.clean {
            let _cleanup = self.cleanup();
        }
    }
}

fn nonblocking(descriptor: &impl AsFd) -> Result<(), ProtocolError> {
    let flags = fcntl_getfl(descriptor).map_err(|_| error("rcli_master_pipe_failed"))?;
    fcntl_setfl(descriptor, flags | OFlags::NONBLOCK)
        .map_err(|_| error("rcli_master_pipe_failed"))?;
    let actual = fcntl_getfl(descriptor).map_err(|_| error("rcli_master_pipe_failed"))?;
    if !actual.contains(OFlags::NONBLOCK) {
        return Err(error("rcli_master_pipe_failed"));
    }
    assert!(actual.contains(OFlags::NONBLOCK));
    assert!(actual.contains(flags));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    #[test]
    fn exited_unreaped_host_group_is_not_cleanup_uncertainty() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while rustix::process::test_kill_process_group(process.group())
            != Err(rustix::io::Errno::PERM)
        {
            assert!(
                Instant::now() < deadline,
                "host did not become an exited unreaped group"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(process.status.is_none());
        process.observe_group().unwrap();
        process.signal(Signal::TERM).unwrap();
        process.cleanup().unwrap();
        assert!(process.status.is_some());
        assert!(process.group_absent);
    }

    #[test]
    fn exited_group_member_reaped_by_its_own_parent_is_not_cleanup_uncertainty() {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("0.3")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command).unwrap();
        let group = i32::try_from(process.child.id()).unwrap();
        let mut member = crate::spawn_gate::spawn(
            Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .process_group(group),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while rustix::process::test_kill_process_group(process.group())
            != Err(rustix::io::Errno::PERM)
        {
            assert!(
                Instant::now() < deadline,
                "group did not hold only exited members"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let reaper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            member.wait().unwrap()
        });
        process.cleanup().unwrap();
        assert!(reaper.join().unwrap().success());
        assert!(process.status.is_some());
        assert!(process.group_absent);
    }
}
