use super::custody::{CustodySession, Retirement, cleanup_owned_group};
use crate::adapters::codex::{AuthControl, CodexProtocol, Event, FRAME_BYTES_MAX, LoginChallenge};
use crate::engine::owner::native_auth::{
    CODEX_DEVICE_CODE_LIFE, CODEX_LOG_DROPPED_MAX, CODEX_LOG_NAME_BYTES_MAX,
    CODEX_LOG_STDERR_BYTES_MAX, CodexAuthObservation, CodexHostFailure, CodexLog,
};
use crate::engine::owner::transport::native_auth_channel::{
    NativeAuthCustodyChannel, NativeAuthCustodyControl,
};
use crate::engine::owner::{NativeAuthAction, NativeAuthMethod};
use crate::engine::{EngineError, io_error};
use crate::storage::CUSTODIAN_CODES;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::Pid;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const STREAM_BYTES_MAX: usize = 65_536;
const READ_BYTES: usize = 4096;
const READ_BATCH: usize = 16;
const POLL_TIME: Duration = Duration::from_millis(20);
const CHECK_TIME: Duration = Duration::from_millis(100);
const WINDOW_HEADROOM: Duration = Duration::from_secs(10);
const EXPIRY_MARGIN: Duration = Duration::from_secs(40);
const STDERR_TAIL_BYTES: usize = 4096;
const WORD_BYTES_MAX: usize = 32;
const CHECK_TIME_MAX: Duration = Duration::from_secs(60);
const LOGOUT_TIME: Duration = Duration::from_secs(5);
const CLEANUP_TIME: Duration = Duration::from_secs(2);

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn protocol_error(_: crate::adapters::codex::ProtocolError) -> EngineError {
    failure("native_auth_codex_protocol_invalid")
}

fn command(executable: &Path, home: &Path) -> Result<Command, EngineError> {
    if !executable.is_absolute() || !home.is_absolute() {
        return Err(failure("native_auth_codex_launch_invalid"));
    }
    let mut command = Command::new(executable);
    command
        .args([
            "-c",
            "cli_auth_credentials_store=\"file\"",
            "app-server",
            "--listen",
            "stdio://",
        ])
        .env_clear()
        .env("HOME", home)
        .env("CODEX_HOME", home)
        .env("TMPDIR", home)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    assert_eq!(command.get_current_dir(), Some(home));
    assert_eq!(command.get_program(), executable.as_os_str());
    Ok(command)
}

struct Pipe {
    descriptor: OwnedFd,
    bytes: usize,
    eof: bool,
}

impl Pipe {
    fn new(descriptor: OwnedFd) -> Result<Self, EngineError> {
        let flags = fcntl_getfl(&descriptor).map_err(io_error)?;
        fcntl_setfl(&descriptor, flags | OFlags::NONBLOCK).map_err(io_error)?;
        assert!(
            fcntl_getfl(&descriptor)
                .map_err(io_error)?
                .contains(OFlags::NONBLOCK)
        );
        Ok(Self {
            descriptor,
            bytes: 0,
            eof: false,
        })
    }

    fn read(&mut self) -> Result<Vec<u8>, EngineError> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; READ_BYTES];
        for _ in 0..READ_BATCH {
            match rustix::io::read(&self.descriptor, &mut buffer[..]) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(size) => {
                    if size > STREAM_BYTES_MAX.saturating_sub(self.bytes) {
                        return Err(failure("native_auth_codex_output_bound"));
                    }
                    self.bytes += size;
                    bytes.extend_from_slice(&buffer[..size]);
                }
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(io_error(error)),
            }
        }
        assert!(self.bytes <= STREAM_BYTES_MAX);
        assert!(bytes.len() <= self.bytes);
        Ok(bytes)
    }
}

struct Pipes {
    input: OwnedFd,
    output: Pipe,
    errors: Pipe,
}

impl Pipes {
    fn take(child: &mut Child) -> Result<Self, EngineError> {
        let input: OwnedFd = child
            .stdin
            .take()
            .ok_or_else(|| failure("native_auth_codex_pipe_missing"))?
            .into();
        let output = child
            .stdout
            .take()
            .ok_or_else(|| failure("native_auth_codex_pipe_missing"))?;
        let errors = child
            .stderr
            .take()
            .ok_or_else(|| failure("native_auth_codex_pipe_missing"))?;
        let flags = fcntl_getfl(&input).map_err(io_error)?;
        fcntl_setfl(&input, flags | OFlags::NONBLOCK).map_err(io_error)?;
        assert!(child.stdin.is_none());
        assert!(child.stdout.is_none());
        assert!(child.stderr.is_none());
        Ok(Self {
            input,
            output: Pipe::new(output.into())?,
            errors: Pipe::new(errors.into())?,
        })
    }
}

struct Driver<'a> {
    session: &'a mut CustodySession,
    channel: &'a mut NativeAuthCustodyChannel,
    pipes: Pipes,
    protocol: CodexProtocol,
    action: NativeAuthAction,
    login_id: Option<String>,
    user_code: Option<String>,
    stderr: Vec<u8>,
    dropped: Vec<String>,
    logged: bool,
    completion: Option<AuthControl>,
    account_requested: bool,
    cancel_requested: bool,
    cancel_sent: bool,
    done: bool,
    deadline: Instant,
}

impl Driver<'_> {
    async fn write(&mut self, bytes: Vec<u8>, allow_cancel: bool) -> Result<(), EngineError> {
        if bytes.is_empty() || bytes.len() > FRAME_BYTES_MAX {
            return Err(failure("native_auth_codex_input_bound"));
        }
        self.check().await?;
        if self.cancel_requested && !allow_cancel {
            return Err(failure("native_auth_cancelled"));
        }
        let mut sent = 0;
        let mut checked = Instant::now();
        while sent < bytes.len() {
            if Instant::now() >= self.deadline {
                return Err(failure("native_auth_codex_deadline"));
            }
            if checked.elapsed() >= CHECK_TIME {
                self.check().await?;
                checked = Instant::now();
                if self.cancel_requested && !allow_cancel {
                    return Err(failure("native_auth_cancelled"));
                }
            }
            match rustix::io::write(&self.pipes.input, &bytes[sent..]) {
                Ok(0) => return Err(failure("native_auth_codex_input_closed")),
                Ok(size) => sent += size,
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::AGAIN) => tokio::time::sleep(POLL_TIME).await,
                Err(error) => return Err(io_error(error)),
            }
            assert!(sent <= bytes.len());
        }
        assert_eq!(sent, bytes.len());
        Ok(())
    }

    async fn check(&mut self) -> Result<(), EngineError> {
        if matches!(
            self.session.check(self.channel).await?,
            NativeAuthCustodyControl::Cancel
        ) {
            self.cancel_requested = true;
        }
        Ok(())
    }

    async fn observe(&mut self, event: AuthControl) -> Result<(), EngineError> {
        if matches!(
            self.session.observe_codex(self.channel, event).await?,
            NativeAuthCustodyControl::Cancel
        ) {
            self.cancel_requested = true;
        }
        Ok(())
    }

    /// Keeps the last bytes Codex wrote to stderr, at most `STDERR_TAIL_BYTES`. Input: the
    /// bytes just read. Output: none.
    fn keep_stderr(&mut self, bytes: &[u8]) {
        self.stderr.extend_from_slice(bytes);
        let over = self.stderr.len().saturating_sub(STDERR_TAIL_BYTES);
        self.stderr.drain(..over);
        assert!(self.stderr.len() <= STDERR_TAIL_BYTES);
    }

    /// Takes what the driver saw of Codex for the engine log, once per run. Input: the driver.
    /// Output: the log, or none once it was taken.
    fn take_log(&mut self) -> Option<CodexLog> {
        if self.logged {
            return None;
        }
        self.logged = true;
        let code = self.user_code.as_deref();
        let log = CodexLog {
            dropped: self
                .dropped
                .iter()
                .map(|name| dropped_name(name, code))
                .collect(),
            stderr: masked(&self.stderr, code),
        };
        assert!(log.validate().is_ok());
        Some(log)
    }

    /// Sends what the driver saw of Codex to the engine log when a sign-in fails or runs out
    /// of time. The log is best effort, except that a failed send naming a custodian code,
    /// such as a profile guard the send refused, is the driver's error, so the custodian
    /// reports the code that sealed its channel. Input: the driver. Output: done, or that
    /// error.
    async fn log(&mut self) -> Result<(), EngineError> {
        let Some(log) = self.take_log() else {
            return Ok(());
        };
        match self.session.log_codex(self.channel, log).await {
            Ok(NativeAuthCustodyControl::Cancel) => self.cancel_requested = true,
            Ok(NativeAuthCustodyControl::Continue) => {}
            Err(error) => custodian_cause(error)?,
        }
        Ok(())
    }

    async fn read_account(&mut self) -> Result<(), EngineError> {
        if self.account_requested {
            return Ok(());
        }
        let frame = self.protocol.read_account(false).map_err(protocol_error)?;
        self.write(frame, true).await?;
        self.account_requested = true;
        assert!(self.account_requested);
        assert!(!self.done);
        Ok(())
    }

    async fn initialized(&mut self) -> Result<(), EngineError> {
        let frame = self.protocol.initialized().map_err(protocol_error)?;
        self.write(frame, false).await?;
        let frame = match self.action {
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexBrowser,
            } => self.protocol.start_chatgpt_login(),
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexDeviceCode,
            } => self.protocol.start_device_code_login(),
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexExisting,
            } => return self.read_account().await,
            NativeAuthAction::Logout => self.protocol.logout_account(),
            _ => return Err(failure("native_auth_codex_method_unsupported")),
        }
        .map_err(protocol_error)?;
        self.write(frame, false).await
    }

    async fn auth(&mut self, event: AuthControl) -> Result<(), EngineError> {
        match event {
            AuthControl::LoginStarted { ref challenge, .. } => {
                self.login_id = Some(match challenge {
                    LoginChallenge::Browser { login_id, .. }
                    | LoginChallenge::DeviceCode { login_id, .. } => login_id.clone(),
                });
                if let LoginChallenge::DeviceCode { user_code, .. } = challenge {
                    self.user_code = Some(user_code.expose().to_owned());
                }
                self.observe(event).await?;
                if let Some(completed) = self.completion.take() {
                    self.observe(completed.clone()).await?;
                    self.completion = Some(completed);
                    self.read_account().await?;
                }
            }
            AuthControl::LoginCompleted { success, .. } => {
                if !success {
                    self.log().await?;
                }
                self.completion = Some(event.clone());
                if self.login_id.is_some() {
                    self.observe(event).await?;
                    self.read_account().await?;
                }
            }
            AuthControl::AccountRead { .. } => {
                self.observe(event).await?;
                self.done = true;
                assert!(self.account_requested);
                assert!(self.done);
            }
            AuthControl::LoggedOut { .. } | AuthControl::LoginCanceled { .. } => {
                self.observe(event).await?;
                self.read_account().await?;
            }
            AuthControl::RequestFailed { .. } => {
                self.observe(event).await?;
                return Err(failure("native_auth_codex_request_failed"));
            }
        }
        Ok(())
    }

    async fn event(&mut self, event: Event) -> Result<(), EngineError> {
        match event {
            Event::Initialized => self.initialized().await,
            Event::AuthControl(event) => self.auth(event).await,
            Event::UnsupportedNotification { method } => {
                let name = dropped_name(&method, None);
                if self.dropped.len() < CODEX_LOG_DROPPED_MAX && !self.dropped.contains(&name) {
                    self.dropped.push(name);
                }
                Ok(())
            }
            Event::ApprovalRequired { id, .. } | Event::UnsupportedRequest { id, .. } => {
                let frame = self.protocol.deny_request(&id).map_err(protocol_error)?;
                self.write(frame, true).await
            }
            _ => Err(failure("native_auth_codex_event_unexpected")),
        }
    }

    async fn cancel(&mut self) -> Result<(), EngineError> {
        if !self.cancel_requested || self.cancel_sent || self.completion.is_some() {
            return Ok(());
        }
        let Some(login_id) = self.login_id.as_ref() else {
            return Ok(());
        };
        let frame = self
            .protocol
            .cancel_login(login_id)
            .map_err(protocol_error)?;
        self.write(frame, true).await?;
        self.cancel_sent = true;
        assert!(self.cancel_sent);
        assert!(self.login_id.is_some());
        Ok(())
    }

    async fn run(&mut self) -> Result<(), EngineError> {
        let frame = self.protocol.initialize().map_err(protocol_error)?;
        self.write(frame, false).await?;
        let mut checked = Instant::now();
        while Instant::now() < self.deadline {
            if checked.elapsed() >= CHECK_TIME {
                self.check().await?;
                checked = Instant::now();
            }
            let bytes = self.pipes.output.read()?;
            let errors = self.pipes.errors.read()?;
            self.keep_stderr(&errors);
            let events = self.protocol.receive(&bytes).map_err(protocol_error)?;
            for event in events {
                self.event(event).await?;
            }
            if self.done {
                return Ok(());
            }
            if self.login_id.is_some()
                && self.completion.is_none()
                && !self.cancel_requested
                && Instant::now() + EXPIRY_MARGIN >= self.deadline
            {
                self.log().await?;
                self.cancel_requested = true;
            }
            self.cancel().await?;
            if self.pipes.output.eof
                || self
                    .session
                    .child_mut()?
                    .try_wait()
                    .map_err(io_error)?
                    .is_some()
            {
                let _ = self.protocol.finish();
                return Err(failure("native_auth_codex_exited_before_observation"));
            }
            tokio::time::sleep(POLL_TIME).await;
        }
        Err(failure("native_auth_codex_deadline"))
    }
}

/// Keeps a failed engine log write as the custodian's cause when it names one of
/// `CUSTODIAN_CODES`, such as a profile guard the write refused, which sealed the channel.
/// Any other failure is dropped, since the log is best effort. Input: the write's error.
/// Output: done, or that error.
fn custodian_cause(error: EngineError) -> Result<(), EngineError> {
    if CUSTODIAN_CODES.contains(&error.0.as_str()) {
        return Err(error);
    }
    Ok(())
}

/// Settles a sign-in whose started Codex child ended without an app-server outcome, after
/// the child is reaped: Cancelled when the owner cancelled, else Failed with the Codex
/// failure code, so the custodian retires instead of leaving custody uncertain. An error
/// from the engine or the channel is returned as it is, since custody is then in doubt, and a
/// channel the driver's error already closed, such as a refused profile guard, takes no
/// report, so that error is returned as the custodian's cause. The engine log goes first and
/// is best effort: when it fails, the driver's error is returned as the cause, unless the
/// failure names a custodian code, such as a profile guard the log write refused, which is
/// then the cause.
/// Inputs: the session, the channel, the driver's error and the log. Output: done, or the
/// error.
pub(in crate::engine) async fn observe_failure(
    session: &CustodySession,
    channel: &mut NativeAuthCustodyChannel,
    error: EngineError,
    log: Option<CodexLog>,
) -> Result<(), EngineError> {
    if channel.sealed() {
        return Err(error);
    }
    let reason = match error.0.as_str() {
        "native_auth_cancelled" => None,
        "native_auth_codex_exited_before_observation" | "native_auth_codex_input_closed" => {
            Some(CodexHostFailure::ExitedBeforeObservation)
        }
        "native_auth_codex_deadline" => Some(CodexHostFailure::Deadline),
        "native_auth_codex_protocol_invalid"
        | "native_auth_codex_event_unexpected"
        | "native_auth_codex_output_bound"
        | "native_auth_codex_input_bound"
        | "native_auth_codex_request_failed" => Some(CodexHostFailure::ProtocolInvalid),
        code if !code.starts_with("native_auth_") => Some(CodexHostFailure::IoFailed),
        _ => return Err(error),
    };
    if let (Some(_), Some(log)) = (reason, log)
        && let Err(refused) = session.log_codex(channel, log).await
    {
        custodian_cause(refused)?;
        return Err(error);
    }
    let observation = reason.map_or(CodexAuthObservation::HostCancelled, |reason| {
        CodexAuthObservation::HostFailed { reason }
    });
    let cancelled = matches!(observation, CodexAuthObservation::HostCancelled);
    if matches!(
        session.observe_codex_host(channel, observation).await?,
        NativeAuthCustodyControl::Cancel
    ) && !cancelled
    {
        session
            .observe_codex_host(channel, CodexAuthObservation::HostCancelled)
            .await?;
    }
    Ok(())
}

/// How long the custodian drives one Codex action before it gives up. Input: the action.
/// Output: the run time, or an error for a method Codex does not run.
fn run_time(action: &NativeAuthAction) -> Result<Duration, EngineError> {
    match action {
        NativeAuthAction::SignIn {
            method: method @ (NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode),
        } => {
            let time = method.sign_in_window() - WINDOW_HEADROOM;
            assert!(time > EXPIRY_MARGIN);
            assert!(
                *method != NativeAuthMethod::CodexDeviceCode
                    || time - EXPIRY_MARGIN > CODEX_DEVICE_CODE_LIFE
            );
            Ok(time)
        }
        NativeAuthAction::SignIn {
            method: NativeAuthMethod::CodexExisting,
        } => Ok(CHECK_TIME_MAX),
        NativeAuthAction::Logout => Ok(LOGOUT_TIME),
        _ => Err(failure("native_auth_codex_method_unsupported")),
    }
}

/// Squashes text to its letters and digits in upper case, so a code matches however Codex
/// spaced or cased it. Input: the text. Output: the squashed text.
fn squashed(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Tells whether text holds any part of the device code three or more characters long, in any
/// case and however Codex spaced or glued it, or the whole of a shorter code. Inputs: the text
/// and the squashed device code. Output: true when it does.
fn holds_code(text: &str, code: Option<&str>) -> bool {
    let Some(code) = code.filter(|code| !code.is_empty()) else {
        return false;
    };
    let part = squashed(text);
    if code.len() < 3 {
        return part.contains(code);
    }
    code.as_bytes()
        .windows(3)
        .any(|window| part.as_bytes().windows(3).any(|seen| seen == window))
}

/// Tells whether text names a code, as a `code`, `user_code` or `userCode` key does. Input:
/// the text. Output: true when it does.
fn names_code(text: &str) -> bool {
    text.to_ascii_lowercase().contains("code")
}

/// Tells whether text holds something shaped like a device code: two runs of three or more
/// letters or digits joined by a dash, as in `ABCD-EFGH`. Input: the text. Output: true when
/// it does.
fn code_shaped(text: &str) -> bool {
    let bytes = text.as_bytes();
    let run = |side: &mut dyn Iterator<Item = &u8>| {
        side.take_while(|byte| byte.is_ascii_alphanumeric()).count()
    };
    (0..bytes.len()).any(|at| {
        bytes[at] == b'-'
            && run(&mut bytes[..at].iter().rev()) >= 3
            && run(&mut bytes[at + 1..].iter()) >= 3
    })
}

/// Tells whether one word of Codex stderr may reach the engine log as it is: a short word of
/// letters and plain punctuation that holds no address, no part of the device code and no
/// code-shaped value at or after a code key, or a number of at most four digits. Inputs: the
/// word, the squashed device code and whether the word before it names a code. Output: true
/// to keep it.
fn plain(word: &str, code: Option<&str>, after_key: bool) -> bool {
    let number = word.len() <= 4 && word.bytes().all(|byte| byte.is_ascii_digit());
    let text = word.len() <= WORD_BYTES_MAX
        && !word.contains("//")
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || b".,:;()[]'\"_-/".contains(&byte));
    let keyed = code_shaped(word) && (after_key || names_code(word));
    (number || text) && !holds_code(word, code) && !keyed
}

/// Masks a tail of Codex stderr for the engine log: every word that is not plain becomes `*`
/// and only the last words that fit in `CODEX_LOG_STDERR_BYTES_MAX` stay. Inputs: the raw
/// tail and the device code on screen, if any. Output: the masked text.
fn masked(bytes: &[u8], code: Option<&str>) -> String {
    let code = code.map(squashed).filter(|code| !code.is_empty());
    let text = String::from_utf8_lossy(bytes);
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let mut kept = Vec::new();
    let mut size = 0;
    for (at, word) in words.iter().enumerate().rev() {
        let after_key = at > 0 && names_code(words[at - 1]);
        let word = if plain(word, code.as_deref(), after_key) {
            word
        } else {
            "*"
        };
        let grown = size + word.len() + usize::from(!kept.is_empty());
        if grown > CODEX_LOG_STDERR_BYTES_MAX {
            break;
        }
        size = grown;
        kept.push(word);
    }
    kept.reverse();
    let shown = kept.join(" ");
    assert_eq!(shown.len(), size);
    assert!(shown.len() <= CODEX_LOG_STDERR_BYTES_MAX);
    shown
}

/// The name a dropped Codex notification goes into the engine log as: its method when that
/// is a short plain name that holds no part of the device code and no code-shaped value under
/// a code key, else `*`. Inputs: the method and the device code on screen, if any. Output:
/// the name.
fn dropped_name(method: &str, code: Option<&str>) -> String {
    let code = code.map(squashed);
    let plain = !method.is_empty()
        && method.len() <= CODEX_LOG_NAME_BYTES_MAX
        && method
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_.-".contains(&byte))
        && !holds_code(method, code.as_deref())
        && !(code_shaped(method) && names_code(method));
    if plain {
        method.to_owned()
    } else {
        "*".to_owned()
    }
}

pub(in crate::engine) async fn run(
    mut session: CustodySession,
    channel: &mut NativeAuthCustodyChannel,
    executable: &Path,
) -> Result<Retirement, EngineError> {
    let action = channel.fact().action.clone();
    let run_time = run_time(&action)?;
    if matches!(
        session.check(channel).await?,
        NativeAuthCustodyControl::Cancel
    ) {
        return session.seal_without_effect();
    }
    let home = session.profile_path()?;
    let Ok(child) = crate::spawn_gate::spawn(&mut command(executable, &home)?) else {
        return session.seal_without_effect();
    };
    let group = Pid::from_raw(i32::try_from(child.id()).map_err(io_error)?)
        .ok_or_else(|| failure("native_auth_child_cleanup_uncertain"))?;
    if let Err((error, mut child)) = session.retain_child(child) {
        let _ = cleanup_owned_group(&mut child, group, Instant::now() + CLEANUP_TIME);
        return Err(error);
    }
    let pipes = Pipes::take(session.child_mut()?)?;
    let mut driver = Driver {
        session: &mut session,
        channel,
        pipes,
        protocol: CodexProtocol::new(),
        action,
        login_id: None,
        user_code: None,
        stderr: Vec::new(),
        dropped: Vec::new(),
        logged: false,
        completion: None,
        account_requested: false,
        cancel_requested: false,
        cancel_sent: false,
        done: false,
        deadline: Instant::now() + run_time,
    };
    let result = driver.run().await;
    let log = result.as_ref().err().and_then(|_| driver.take_log());
    drop(driver);
    let cleaned = cleanup_owned_group(session.child_mut()?, group, Instant::now() + CLEANUP_TIME);
    if !cleaned.map_err(io_error)? {
        return Err(result
            .err()
            .unwrap_or_else(|| failure("native_auth_child_cleanup_uncertain")));
    }
    if let Err(error) = result {
        observe_failure(&session, channel, error, log).await?;
    }
    session.seal_after_child_reaped()
}

#[cfg(test)]
mod tests;
