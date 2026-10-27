use super::agent_copy::{Harness, Record, account_record, place};
use super::{HeldLock, NativeProfileUse, failure};
use crate::adapters::claude::{REFRESH_WINDOW, RENEWAL_TURN};
use crate::adapters::codex::CodexProtocol;
use crate::engine::EngineError;
use rustix::fs::{FlockOperation, Mode, OFlags, flock};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RUN_TIME: Duration = Duration::from_secs(30);
const CLAUDE_RUN_TIME: Duration = Duration::from_secs(60);
const LOCK_WAIT: Duration = Duration::from_secs(60);
const RENEW_AHEAD: Duration = Duration::from_secs(4 * 60);
const RETRY: Duration = Duration::from_secs(30);
const OUTPUT_BYTES_MAX: u64 = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(50);
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// Seconds since the epoch as a duration, the clock every token expiry is compared with.
/// Output: now, or the unavailable refusal when the clock is before the epoch.
pub(super) fn since_epoch() -> Result<Duration, EngineError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| failure("native_profile_credential_unavailable"))
}

/// Takes a profile's renewal lock through a fresh open file description of its folder,
/// waiting at most `wait`, so only one renewal or sign-in check runs per account while
/// agents keep their shared leases. Inputs: the profile folder and the wait. Output: the
/// held lock, or `native_profile_renewal_busy` when another renewal or check holds it.
pub(super) fn hold(profile: &File, wait: Duration) -> Result<HeldLock, EngineError> {
    hold_unless(profile, wait, &|| false)
}

/// Takes a profile's renewal lock like [`hold`], giving up as soon as `halted` tells the
/// caller stopped. Inputs: the profile folder, the wait and the stop test. Output: the
/// held lock, `native_profile_renewal_busy`, or `native_profile_renewal_stopped`.
fn hold_unless(
    profile: &File,
    wait: Duration,
    halted: &dyn Fn() -> bool,
) -> Result<HeldLock, EngineError> {
    let folder = File::from(
        rustix::fs::openat(
            profile,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| failure("native_profile_lock_unavailable"))?,
    );
    let deadline = Instant::now() + wait;
    loop {
        match flock(&folder, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(HeldLock(folder)),
            Err(error)
                if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN =>
            {
                if halted() {
                    return Err(failure("native_profile_renewal_stopped"));
                }
                if Instant::now() >= deadline {
                    return Err(failure("native_profile_renewal_busy"));
                }
                std::thread::sleep(POLL);
            }
            Err(_) => return Err(failure("native_profile_lock_unavailable")),
        }
    }
}

/// What one account renewal needs, detached from the lease so the keeper thread of a
/// running turn can renew while the caller still holds that lease.
pub(super) struct Renewal {
    harness: Harness,
    executable: PathBuf,
    managed: PathBuf,
    home: File,
    profile: File,
}

impl NativeProfileUse {
    /// Takes this account's renewal lock. Input: the longest wait. Output: the lock, or
    /// `native_profile_renewal_busy`.
    #[cfg(test)]
    pub(crate) fn hold_renewal(&self, wait: Duration) -> Result<HeldLock, EngineError> {
        hold(&self.profile.file, wait)
    }

    /// Prepares renewals of this account by its own harness binary in the managed home.
    /// Inputs: the harness and its absolute executable. Output: the renewal, or the
    /// unavailable refusal for a relative executable or a lease that no longer verifies.
    pub(super) fn renewal(
        &self,
        harness: Harness,
        executable: &Path,
    ) -> Result<Renewal, EngineError> {
        if !executable.is_absolute() {
            return Err(failure("native_profile_credential_unavailable"));
        }
        let clone = |file: &File| {
            file.try_clone()
                .map_err(|_| failure("native_profile_credential_unavailable"))
        };
        Ok(Renewal {
            harness,
            executable: executable.to_path_buf(),
            managed: self.profile_path()?,
            home: clone(&self.home.file)?,
            profile: clone(&self.profile.file)?,
        })
    }
}

impl Renewal {
    /// Prepares a renewal of the managed profile folder at `profile` outside any lease, for
    /// the live check run against a real account. Inputs: the harness, its absolute
    /// executable and the profile folder that holds `home`. Output: the renewal.
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn of_profile(harness: Harness, executable: &Path, profile: &Path) -> Self {
        let open = |path: &Path| {
            File::from(
                rustix::fs::open(
                    path,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .unwrap(),
            )
        };
        Self {
            harness,
            executable: executable.to_path_buf(),
            managed: profile.join("home"),
            home: open(&profile.join("home")),
            profile: open(profile),
        }
    }

    /// Reads the account's credential record as it is now. Output: the record.
    pub(super) fn record(&self) -> Result<Record, EngineError> {
        account_record(self.harness, &self.home, &self.managed)
    }

    /// Renews the account unless someone already did: under the renewal lock, a record
    /// newer than `stale` is returned as it is, and so is a Claude Code record outside
    /// Claude Code's refresh window, which it would not renew yet; otherwise the harness
    /// runs once in the managed home and the record is read again. A failed run is not an
    /// error here: the record read afterwards decides. Input: the expiry the caller saw.
    /// Output: the record after renewal, or `native_profile_sign_in_revoked`.
    pub(super) fn renew(&self, stale: Duration) -> Result<Record, EngineError> {
        self.renew_unless(stale, &|| false)
    }

    /// Renews like [`Self::renew`], stopping the lock wait and killing the harness run as
    /// soon as `halted` tells the caller stopped. Inputs: the expiry the caller saw and
    /// the stop test. Output: the record after renewal, or the refusal.
    fn renew_unless(
        &self,
        stale: Duration,
        halted: &dyn Fn() -> bool,
    ) -> Result<Record, EngineError> {
        let _lock = hold_unless(&self.profile, LOCK_WAIT, halted)?;
        let current = self.record()?;
        if current.expires > stale {
            return Ok(current);
        }
        let _ = match self.harness {
            Harness::Codex => codex_refresh(&self.executable, &self.managed, halted),
            Harness::Claude if current.expires > since_epoch()? + REFRESH_WINDOW => {
                return Ok(current);
            }
            Harness::Claude => claude_turn(&self.executable, &self.managed, halted),
        };
        self.record()
    }
}

/// Builds a harness command that runs in the managed home with nothing of the engine's
/// own environment, the way the sign-in custodian runs it, in its own process group.
/// Inputs: the executable and the managed home. Output: the command.
fn managed_command(executable: &Path, managed: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("HOME", managed)
        .env("TMPDIR", managed)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(managed)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

/// Waits for a renewal child until the deadline or until `halted` tells the caller
/// stopped, then stops its whole process group. Inputs: the child, the deadline and the
/// stop test. Output: whether it exited successfully in time.
fn settle(child: &mut Child, deadline: Instant, halted: &dyn Fn() -> bool) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline && !halted() => std::thread::sleep(POLL),
            _ => {
                if let Some(group) = i32::try_from(child.id())
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
                {
                    let _ =
                        rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Runs the Claude Code renewal turn in the managed home: Claude Code renews the account's
/// OAuth token in place, and waits for the refresh, before the turn's one Haiku request.
/// Its output is discarded. Inputs: the executable, the managed home and the stop test.
/// Output: whether it succeeded within a minute, which outlasts Claude Code's own refresh
/// request bound.
fn claude_turn(
    executable: &Path,
    managed: &Path,
    halted: &dyn Fn() -> bool,
) -> Result<bool, EngineError> {
    let mut command = managed_command(executable, managed);
    command.args(RENEWAL_TURN).env("CLAUDE_CONFIG_DIR", managed);
    let mut child = crate::spawn_gate::spawn(&mut command)
        .map_err(|_| failure("native_profile_renewal_failed"))?;
    Ok(settle(&mut child, Instant::now() + CLAUDE_RUN_TIME, halted))
}

/// Asks the managed home's Codex app-server to refresh its ChatGPT tokens with
/// `account/read {refreshToken: true}`, then stops it. The reply is matched by id and
/// otherwise ignored. Inputs: the executable, the managed home and the stop test.
/// Output: whether the reply arrived in time.
fn codex_refresh(
    executable: &Path,
    managed: &Path,
    halted: &dyn Fn() -> bool,
) -> Result<bool, EngineError> {
    let refused = |_| failure("native_profile_renewal_failed");
    let mut command = managed_command(executable, managed);
    command
        .args([
            "-c",
            "cli_auth_credentials_store=\"file\"",
            "-c",
            "mcp_servers={}",
            "-c",
            "features.plugins=false",
            "-c",
            "features.apps=false",
            "app-server",
            "--listen",
            "stdio://",
        ])
        .env("CODEX_HOME", managed)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(refused)?;
    let deadline = Instant::now() + RUN_TIME;
    let answered = match (child.stdin.take(), child.stdout.take()) {
        (Some(input), Some(output)) => converse(input, output, deadline, halted),
        _ => false,
    };
    settle(&mut child, Instant::now() + EXIT_GRACE, halted);
    Ok(answered)
}

/// Speaks just enough app-server protocol to request a token refresh: initialize,
/// initialized, then account/read with refreshToken. Inputs: the child's pipes, the
/// deadline and the stop test. Output: whether the account/read reply arrived before the
/// deadline.
fn converse(
    mut input: std::process::ChildStdin,
    output: std::process::ChildStdout,
    deadline: Instant,
    halted: &dyn Fn() -> bool,
) -> bool {
    let lines = read_lines(output);
    let mut protocol = CodexProtocol::new();
    let Ok(request) = protocol.initialize() else {
        return false;
    };
    let Some(reply) = exchange(&mut input, &lines, &request, deadline, halted) else {
        return false;
    };
    if protocol.receive(&reply).is_err() {
        return false;
    }
    let Ok(ready) = protocol.initialized() else {
        return false;
    };
    if send(&mut input, &ready).is_err() {
        return false;
    }
    let Ok(request) = protocol.read_account(true) else {
        return false;
    };
    exchange(&mut input, &lines, &request, deadline, halted).is_some()
}

/// Reads the child's output lines on their own thread, at most 4 MiB in all, so a
/// silent app-server cannot block the deadline. Input: stdout. Output: the line channel.
fn read_lines(output: std::process::ChildStdout) -> Receiver<Vec<u8>> {
    let (sender, lines) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(output.take(OUTPUT_BYTES_MAX));
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    lines
}

/// Writes one request line. Inputs: stdin and the encoded request. Output: done or the
/// write failure.
fn send(input: &mut std::process::ChildStdin, bytes: &[u8]) -> std::io::Result<()> {
    input.write_all(bytes)?;
    if !bytes.ends_with(b"\n") {
        input.write_all(b"\n")?;
    }
    input.flush()
}

/// Sends a request and waits for the reply line with its id. Inputs: stdin, the line
/// channel, the request, the deadline and the stop test. Output: the reply line, or
/// nothing on timeout, a closed output or a stop.
fn exchange(
    input: &mut std::process::ChildStdin,
    lines: &Receiver<Vec<u8>>,
    request: &[u8],
    deadline: Instant,
    halted: &dyn Fn() -> bool,
) -> Option<Vec<u8>> {
    let id = serde_json::from_slice::<Value>(request).ok()?["id"].as_i64()?;
    send(input, request).ok()?;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = match lines.recv_timeout(left.min(POLL)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) if !left.is_zero() && !halted() => continue,
            Err(_) => return None,
        };
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value["id"].as_i64() == Some(id) && value.get("method").is_none() {
            return Some(line);
        }
    }
}

/// Keeps one agent's copy fresh for the rest of its turn: wakes four minutes before the
/// copy lapses (inside Claude Code's refresh window), renews, and replaces the copy when
/// the account's record is newer; retries every 30 seconds. Stopped and joined on drop.
pub(super) struct Keeper {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Keeper {
    /// Starts the keeper. Inputs: the renewal, the agent home, the copy's file name and
    /// its expiry. Output: the running keeper, or unavailable when no thread starts.
    pub(super) fn start(
        renewal: Renewal,
        agent: File,
        name: &'static str,
        expires: Duration,
    ) -> Result<Self, EngineError> {
        let (stop, stopped) = channel();
        let thread = std::thread::Builder::new()
            .name("credential-keeper".into())
            .spawn(move || keep(&renewal, &agent, name, expires, &stopped))
            .map_err(|_| failure("native_profile_credential_unavailable"))?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Keeper {
    /// Stops the keeper and waits for it, so no copy is written after the turn. A stopped
    /// keeper leaves its lock wait and kills its harness run within one poll, and the wait
    /// runs off the async workers. Input: the keeper. Output: none.
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = super::agent_copy::off_worker(|| thread.join());
        }
    }
}

/// Sleeps until `wait` passes, or returns false as soon as the keeper is stopped.
fn idle(stopped: &Receiver<()>, wait: Duration) -> bool {
    matches!(stopped.recv_timeout(wait), Err(RecvTimeoutError::Timeout))
}

/// The keeper's loop. Inputs: the renewal, the agent home, the copy's name, its expiry
/// and the stop channel. Output: none; it returns when stopped.
fn keep(
    renewal: &Renewal,
    agent: &File,
    name: &'static str,
    mut expires: Duration,
    stopped: &Receiver<()>,
) {
    let halted = || matches!(stopped.try_recv(), Err(TryRecvError::Disconnected));
    loop {
        let now = since_epoch().unwrap_or(expires);
        let wake = expires.saturating_sub(RENEW_AHEAD).saturating_sub(now);
        if !idle(stopped, wake) {
            return;
        }
        let renewed = renewal.renew_unless(expires, &halted);
        if halted() {
            return;
        }
        if let Ok(record) = renewed
            && record.expires > expires
            && place(agent, name, &record.copy).is_ok()
        {
            expires = record.expires;
            continue;
        }
        if !idle(stopped, RETRY) {
            return;
        }
    }
}
