use super::master_launch::error;
use crate::harnesses::leftovers::{
    self, Follower, HoldBy, LINEAGE_VARIABLE, Leftovers, Lineage, LineageToken, Unstopped,
};
use crate::protocol::CredentialRef;
use crate::protocol::{FRAME_BYTES_MAX, ProtocolError};
use crate::sandbox::LineageCheck;
use crate::sandbox::warden::{self, Warden};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

/// The bytes of frames one rcli master host may write in its run, the same total a Claude
/// Code master has for its output (OB-5 final). Past it the run ends with the output limit
/// code. It bounds the bytes read, not the frames: no count of frames ends a run.
const STDOUT_BYTES_MAX: usize = 64 * 1024 * 1024;
const WRITE_TIME: Duration = Duration::from_secs(1);
const CLEANUP_TIME: Duration = Duration::from_secs(6);
const INTERVAL: Duration = Duration::from_millis(5);

pub(super) struct HostProcess {
    child: Child,
    warden: Option<Warden>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    pending: Vec<u8>,
    stdout_bytes: usize,
    status: Option<ExitStatus>,
    group_absent: bool,
    clean: bool,
    follower: Option<Follower>,
    left: Option<Leftovers>,
    by: HoldBy,
}

impl HostProcess {
    /// Starts the rcli master host with its pipes open and not blocking. Inputs: the wrapped
    /// command, what the stop checks of the processes it leaves outside its group and who to
    /// hold those under. Output: the process, or a plain refusal.
    pub(super) fn spawn(
        mut command: Command,
        check: LineageCheck,
        by: HoldBy,
    ) -> Result<Self, ProtocolError> {
        let token = match check {
            LineageCheck::Unneeded => None,
            LineageCheck::FullAccess | LineageCheck::Sandboxed => {
                Some(LineageToken::new().ok_or_else(|| error("rcli_master_spawn_failed"))?)
            }
        };
        if let Some(token) = &token {
            command.env(LINEAGE_VARIABLE, token.as_str());
        }
        let (mut child, warden) =
            warden::spawn(&mut command).map_err(|_| error("rcli_master_spawn_failed"))?;
        assert!(child.id() > 1);
        let follower = Follower::start(child.id(), token, check);
        assert_eq!(follower.is_some(), check != LineageCheck::Unneeded);
        let stdin = child.stdin.take().expect("configured host input pipe");
        let stdout = child.stdout.take().expect("configured host output pipe");
        let stderr = child.stderr.take().expect("configured host error pipe");
        let process = Self {
            child,
            warden,
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr: Some(stderr),
            pending: Vec::new(),
            stdout_bytes: 0,
            status: None,
            group_absent: false,
            clean: false,
            follower,
            left: None,
            by,
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
                    return Err(if self.pending.is_empty() || self.held() {
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

    /// Drains what the host wrote to its error pipe so the host never waits on it. The
    /// bytes are read and dropped, at most 16 KiB a call: nothing of them is kept, so no
    /// total of them ends a turn (OB-5 r2). Input: the process. Output: Ok, or a plain code
    /// for a pipe that failed.
    fn read_stderr(&mut self) -> Result<(), ProtocolError> {
        let mut scratch = [0; 1024];
        let mut drained = 0;
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
                    assert!(count <= scratch.len());
                    drained += count;
                }
                Err(failure) if failure.kind() == io::ErrorKind::Interrupted => {}
                Err(failure) if failure.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(_) => return Err(error("rcli_master_stderr_failed")),
            }
        }
        assert!(drained <= 16 * scratch.len());
        assert!(self.child.id() > 1);
        Ok(())
    }

    /// Tells whether the host was stopped by the Linux call supervisor because it could not
    /// take back an entry read as a held name ([`crate::sandbox::held_stop`]). Input: the
    /// process. Output: true for that exit, which outranks what the host left half written.
    fn held(&self) -> bool {
        let code = self.status.as_ref().and_then(ExitStatus::code);
        code.and_then(|code| crate::sandbox::held_stop(std::env::consts::OS, code))
            .is_some()
    }

    fn exit_error(&self) -> ProtocolError {
        if self.held() {
            return error(crate::sandbox::HELD_STOP_CAUSE);
        }
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

    /// Names the process group the rcli master was started in. Input: the process.
    /// Output: the group, which is the host's own process id.
    pub(super) fn process_group(&self) -> u32 {
        assert!(self.child.id() > 1);
        self.child.id()
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
    /// member has exited, or is still exiting, but some are not reaped yet. Input: the
    /// retained child. Output: Ok, marking the group absent only after ESRCH; a leader that
    /// cannot be reaped yet, or members still exiting, leave the group present so the
    /// cleanup deadline decides.
    fn exited_group(&mut self) -> Result<(), ProtocolError> {
        self.reap()?;
        if self.status.is_none() {
            return Ok(());
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

    /// Ends the host and everything it left in its group. At Full access, and under the plain
    /// Linux Landlock helper, no warden reaches what left the group (LINUX-KILL-GROUP r1 F1),
    /// so the lineage followed from the start is taken before the kill and settled after it.
    /// What still runs once the group ended is kept for [`Self::hold_left`], so the custody
    /// holds it with the profile; past a group that did not end it is held at once with none.
    /// Input: the process. Output: Ok, `rcli_master_cleanup_incomplete`, or the leftover code.
    pub(super) fn cleanup(&mut self) -> Result<(), ProtocolError> {
        if self.clean {
            return Ok(());
        }
        let lineage = self.follower.take().map(Follower::stop);
        let ended = self.end_group();
        let Some(Err(unstopped)) = lineage.map(|lineage| lineage.and_then(Lineage::settle)) else {
            return ended;
        };
        let code = unstopped.code();
        match unstopped {
            Unstopped::Left(left) if ended.is_ok() => self.left = Some(left),
            Unstopped::Left(left) => {
                let mode = leftovers::mode_of(self.by.project.as_deref());
                left.hold(None, (), self.by.clone(), mode);
            }
            Unstopped::Uncertain => {}
        }
        assert!(self.left.is_none() || ended.is_ok());
        ended.and(Err(error(code)))
    }

    /// Names what the host's stop left running once its group ended, if anything. Input: the
    /// process. Output: the leftover code, or none.
    pub(super) fn left_code(&self) -> Option<&'static str> {
        self.left.as_ref().map(Leftovers::code)
    }

    /// Holds what the host's stop left running with `kept` listed under `account`, by the
    /// project's leftover setting, as a Claude host turn's hold keeps its lease until none
    /// run; with nothing left, `kept` is dropped. Inputs: the process, the account and what
    /// the hold keeps. Output: none.
    pub(super) fn hold_left<T: Send + 'static>(&mut self, account: Option<CredentialRef>, kept: T) {
        if let Some(left) = self.left.take() {
            let mode = leftovers::mode_of(self.by.project.as_deref());
            left.hold(account, kept, self.by.clone(), mode);
        }
        assert!(self.left.is_none());
    }

    fn end_group(&mut self) -> Result<(), ProtocolError> {
        assert!(!self.clean);
        assert!(self.follower.is_none());
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
                uncertain |= self
                    .warden
                    .take()
                    .is_some_and(|warden| warden.fire(warden::FIRE_TIME).is_err());
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
        self.hold_left(None, ());
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

    /// Who a row's host leaves a hold under. Output: no project and a plain label.
    fn test_by() -> HoldBy {
        HoldBy {
            project: None,
            who: "an rcli master test host".to_owned(),
        }
    }

    /// LINUX-KILL-GROUP r1 F1: an rcli master host whose boundary leaves a process in a
    /// session of its own running, as the plain Linux Landlock helper's does, has its lineage
    /// followed from its start, so the process it left outside its group is named as a
    /// sandboxed run's leftover once the group is gone, and the stop ends with that code. Its
    /// leader leaves the group it was started in and leads a session of its own, as the Linux
    /// helper's `own_session` makes a wrapped harness, so the lineage admits its children.
    #[test]
    fn a_followed_rcli_master_names_what_it_left_outside_its_group() {
        let folder = tempfile::tempdir().unwrap();
        let mut command = Command::new("/usr/bin/perl");
        command
            .args(["-mPOSIX", "-e", "POSIX::setpgid(0, getpgrp(getppid())) or exit 1; POSIX::setsid() > 0 or exit 1; exec '/bin/sh', '-c', $ARGV[0] or exit 1"])
            .arg("/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, \">\", \"child.pid\") or exit 1; print $file \"$$\\n\"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 & exec /bin/sleep 30")
            .current_dir(folder.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command, LineageCheck::Sandboxed, test_by()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let child = loop {
            if let Some(child) = std::fs::read_to_string(folder.path().join("child.pid"))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                break child;
            }
            assert!(Instant::now() < deadline, "the stand-in never started");
            thread::sleep(Duration::from_millis(10));
        };

        let cleaned = process.cleanup();
        if let Some(pid) = Pid::from_raw(child) {
            let _ = rustix::process::kill_process(pid, Signal::KILL);
        }

        assert_eq!(
            cleaned.map_err(|failure| failure.0),
            Err(crate::harnesses::leftovers::SANDBOX_LEFT.to_owned())
        );
        assert!(process.clean);
        assert!(process.group_absent);
        assert_eq!(
            process.left_code(),
            Some(crate::harnesses::leftovers::SANDBOX_LEFT)
        );
        process.hold_left(None, ());
        assert_eq!(process.left_code(), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cleanup_stops_what_a_sandboxed_rcli_master_detached() {
        use crate::sandbox::warden::fixture::{DetachingRun, survivors};

        let run = DetachingRun::new();
        let mut command = run.command(&run.profile);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
        assert!(process.warden.is_some());
        let processes = run.processes();
        process.cleanup().unwrap();
        assert!(process.warden.is_none());
        assert_eq!(survivors(processes, Duration::from_secs(1)), []);
    }

    /// macOS only: it waits for the EPERM that macOS alone answers for a group whose members
    /// all exited and are not reaped. Linux answers Ok until the reap; its twin row covers that.
    #[cfg(target_os = "macos")]
    #[test]
    fn exited_unreaped_host_group_is_not_cleanup_uncertainty() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
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

    /// The Linux twin of `exited_unreaped_host_group_is_not_cleanup_uncertainty`. A group
    /// whose only member exited and is not reaped still takes signal 0 and TERM there, so
    /// the cleanup reaps the host first and ends clean once the group answers ESRCH.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_exited_unreaped_host_group_answers_until_the_reap_and_is_then_absent() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
        let stat = format!("/proc/{}/stat", process.child.id());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(&stat).is_ok_and(|stat| {
            stat.rsplit(')')
                .next()
                .is_some_and(|rest| rest.starts_with(" Z"))
        }) {
            assert!(
                Instant::now() < deadline,
                "host did not become an exited unreaped group"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let group = process.group();
        assert_eq!(rustix::process::test_kill_process_group(group), Ok(()));
        assert!(process.status.is_none());
        process.observe_group().unwrap();
        process.signal(Signal::TERM).unwrap();
        assert!(!process.group_absent);
        process.cleanup().unwrap();
        assert!(process.status.is_some());
        assert!(process.group_absent);
        assert_eq!(
            rustix::process::test_kill_process_group(group),
            Err(rustix::io::Errno::SRCH)
        );
    }

    #[test]
    fn a_refused_group_signal_before_the_leader_can_be_reaped_leaves_the_deadline_to_decide() {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("5")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
        process.exited_group().unwrap();
        assert!(process.status.is_none());
        assert!(!process.group_absent);
        process.cleanup().unwrap();
        assert!(process.status.is_some());
        assert!(process.group_absent);
    }

    /// macOS only: it waits for the EPERM that macOS alone answers for a group whose members
    /// all exited and are not reaped. Linux answers Ok until the reap; its twin row covers that.
    #[cfg(target_os = "macos")]
    #[test]
    fn exited_group_member_reaped_by_its_own_parent_is_not_cleanup_uncertainty() {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("0.3")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
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

    /// A stand-in host that writes 1,100 frames of the largest size, 65,536 bytes after the
    /// four byte length, and then waits on its input.
    const FLOODING_HOST: &str = r#"i=0
while [ "$i" -lt 1100 ]; do
  printf '\000\001\000\000%065536d' 0
  i=$((i + 1))
done
read -r _
"#;

    /// The frames of one run are bounded by 64 MiB of bytes, as a Claude Code master's
    /// output is, and not by the 16 MiB an rcli master had (OB-5 final). Every whole frame
    /// under the bound is handed over: 1,023 of the largest size, where the old bound gave
    /// 255. The frame that crosses it ends the run with the output limit code.
    #[test]
    fn frames_up_to_sixty_four_mebibytes_are_read_and_the_next_ends_with_the_output_code() {
        const FRAME: usize = FRAME_BYTES_MAX + 4;
        assert_eq!(STDOUT_BYTES_MAX, 64 * 1024 * 1024);
        assert_eq!(FRAME, 65_540);
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", FLOODING_HOST])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut process = HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut frames = 0;
        let failure = loop {
            assert!(Instant::now() < deadline, "{frames} frames read in time");
            match process.frame() {
                Ok(Some(frame)) => {
                    assert_eq!(frame.len(), FRAME);
                    assert_eq!(frame[..4], [0, 1, 0, 0]);
                    frames += 1;
                }
                Ok(None) => thread::sleep(Duration::from_millis(1)),
                Err(failure) => break failure,
            }
        };
        assert_eq!(failure.0, "rcli_master_output_limit");
        assert_eq!(frames, STDOUT_BYTES_MAX / FRAME);
        assert_eq!(frames, 1023);
        assert!(frames > 16_777_216 / FRAME);
        assert!(process.stdout_bytes > STDOUT_BYTES_MAX);
        assert!(process.stdout_bytes <= STDOUT_BYTES_MAX + 4096);
        process.cleanup().unwrap();
    }

    /// DEC-185 S2a (rv-sec3 F1): an rcli master host that exits with the Linux call
    /// supervisor's held-stop code ends with the sandbox cause, whether it left half a frame
    /// or nothing, never the truncated event nor the plain exit code.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_rcli_master_the_sandbox_stopped_ends_with_the_sandbox_cause() {
        for host in [r"printf '\000\000\000\020ab'; exit 122", "exit 122"] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", host])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0);
            let mut process =
                HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let failure = loop {
                assert!(Instant::now() < deadline, "{host}");
                match process.frame() {
                    Ok(Some(_)) => panic!("{host} wrote no whole frame"),
                    Ok(None) => thread::sleep(Duration::from_millis(1)),
                    Err(failure) => break failure,
                }
            };
            assert_eq!(failure.0, crate::sandbox::HELD_STOP_CAUSE, "{host}");
            process.cleanup().unwrap();
        }
    }

    /// V243 P6 (rv-code #501 F1): the engine's runtime has two workers, and a task that hands
    /// its worker's core off for blocking work, as `off_worker` does, leaves that thread to
    /// exit once idle. A host started on it whose first process is bubblewrap with
    /// `--die-with-parent`, as the closed backend's is, dies with the thread that forked it,
    /// so the custody forks it and the host outlives the worker.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_host_outlives_a_runtime_worker_that_handed_its_core_off() {
        let bwrap = crate::sandbox::linux::BWRAP_PATHS
            .iter()
            .map(std::path::Path::new)
            .find(|path| path.is_file())
            .expect("Linux test machines need bubblewrap installed");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_keep_alive(Duration::from_millis(100))
            .build()
            .unwrap();
        let mut command = Command::new(bwrap);
        command
            .args(["--die-with-parent", "--unshare-pid", "--dev-bind", "/", "/"])
            .args(["--", "/bin/sh", "-c", "read -r line"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let task = runtime.spawn(async move {
            let process = HostProcess::spawn(command, LineageCheck::Unneeded, test_by()).unwrap();
            let worker = std::fs::read_link("/proc/thread-self").unwrap();
            tokio::task::block_in_place(|| thread::sleep(Duration::from_millis(100)));
            (process, worker)
        });
        let (mut process, worker) = runtime.block_on(task).unwrap();
        let worker = std::path::Path::new("/proc").join(worker);
        let deadline = Instant::now() + Duration::from_secs(10);
        while worker.exists() {
            assert!(
                Instant::now() < deadline,
                "the worker that started the host never exited"
            );
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(500));
        process.reap().unwrap();
        assert_eq!(
            process.status, None,
            "the host ended with the runtime worker that started it"
        );
        process.cleanup().unwrap();
        drop(runtime);
    }
}
