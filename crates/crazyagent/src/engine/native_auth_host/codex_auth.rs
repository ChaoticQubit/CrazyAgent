use super::custody::{CustodySession, Retirement, cleanup_owned_group};
use crate::adapters::codex::{AuthControl, CodexProtocol, Event, LoginChallenge, FRAME_BYTES_MAX};
use crate::engine::owner::transport::native_auth_channel::{
    NativeAuthCustodyChannel, NativeAuthCustodyControl,
};
use crate::engine::owner::{NativeAuthAction, NativeAuthMethod};
use crate::engine::{EngineError, io_error};
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
const SIGN_IN_TIME: Duration = Duration::from_secs(590);
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
        assert!(fcntl_getfl(&descriptor).map_err(io_error)?.contains(OFlags::NONBLOCK));
        Ok(Self { descriptor, bytes: 0, eof: false })
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
        let input: OwnedFd = child.stdin.take().ok_or_else(|| failure("native_auth_codex_pipe_missing"))?.into();
        let output = child.stdout.take().ok_or_else(|| failure("native_auth_codex_pipe_missing"))?;
        let errors = child.stderr.take().ok_or_else(|| failure("native_auth_codex_pipe_missing"))?;
        let flags = fcntl_getfl(&input).map_err(io_error)?;
        fcntl_setfl(&input, flags | OFlags::NONBLOCK).map_err(io_error)?;
        assert!(child.stdin.is_none());
        assert!(child.stdout.is_none());
        assert!(child.stderr.is_none());
        Ok(Self { input, output: Pipe::new(output.into())?, errors: Pipe::new(errors.into())? })
    }
}

struct Driver<'a> {
    session: &'a mut CustodySession,
    channel: &'a mut NativeAuthCustodyChannel,
    pipes: Pipes,
    protocol: CodexProtocol,
    action: NativeAuthAction,
    login_id: Option<String>,
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
        if matches!(self.session.check(self.channel).await?, NativeAuthCustodyControl::Cancel) {
            self.cancel_requested = true;
        }
        Ok(())
    }

    async fn observe(&mut self, event: AuthControl) -> Result<(), EngineError> {
        if matches!(self.session.observe_codex(self.channel, event).await?, NativeAuthCustodyControl::Cancel) {
            self.cancel_requested = true;
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
            NativeAuthAction::SignIn { method: NativeAuthMethod::CodexBrowser } => self.protocol.start_chatgpt_login(),
            NativeAuthAction::SignIn { method: NativeAuthMethod::CodexDeviceCode } => self.protocol.start_device_code_login(),
            NativeAuthAction::Logout => self.protocol.logout_account(),
            _ => return Err(failure("native_auth_codex_method_unsupported")),
        }.map_err(protocol_error)?;
        self.write(frame, false).await
    }

    async fn auth(&mut self, event: AuthControl) -> Result<(), EngineError> {
        match event {
            AuthControl::LoginStarted { ref challenge, .. } => {
                self.login_id = Some(match challenge {
                    LoginChallenge::Browser { login_id, .. } | LoginChallenge::DeviceCode { login_id, .. } => login_id.clone(),
                });
                self.observe(event).await?;
                if let Some(completed) = self.completion.take() {
                    self.observe(completed.clone()).await?;
                    self.completion = Some(completed);
                    self.read_account().await?;
                }
            }
            AuthControl::LoginCompleted { .. } => {
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
            Event::UnsupportedNotification { .. } => Ok(()),
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
        let frame = self.protocol.cancel_login(login_id).map_err(protocol_error)?;
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
            self.pipes.errors.read()?;
            let events = self.protocol.receive(&bytes).map_err(protocol_error)?;
            for event in events {
                self.event(event).await?;
            }
            if self.done {
                return Ok(());
            }
            self.cancel().await?;
            if self.pipes.output.eof || self.session.child_mut()?.try_wait().map_err(io_error)?.is_some() {
                self.protocol.finish().map_err(protocol_error)?;
                return Err(failure("native_auth_codex_exited_before_observation"));
            }
            tokio::time::sleep(POLL_TIME).await;
        }
        Err(failure("native_auth_codex_deadline"))
    }
}

pub(in crate::engine) async fn run(
    mut session: CustodySession,
    channel: &mut NativeAuthCustodyChannel,
    executable: &Path,
) -> Result<Retirement, EngineError> {
    let action = channel.fact().action.clone();
    let run_time = match action {
        NativeAuthAction::SignIn { method: NativeAuthMethod::CodexBrowser | NativeAuthMethod::CodexDeviceCode } => SIGN_IN_TIME,
        NativeAuthAction::Logout => LOGOUT_TIME,
        _ => return Err(failure("native_auth_codex_method_unsupported")),
    };
    if matches!(session.check(channel).await?, NativeAuthCustodyControl::Cancel) {
        return Err(failure("native_auth_cancelled"));
    }
    let home = session.profile_path()?;
    let child = command(executable, &home)?.spawn().map_err(io_error)?;
    let group = Pid::from_raw(i32::try_from(child.id()).map_err(io_error)?)
        .ok_or_else(|| failure("native_auth_child_cleanup_uncertain"))?;
    if let Err((error, mut child)) = session.retain_child(child) {
        let _ = cleanup_owned_group(&mut child, group, Instant::now() + CLEANUP_TIME);
        return Err(error);
    }
    let pipes = Pipes::take(session.child_mut()?)?;
    let mut driver = Driver {
        session: &mut session, channel, pipes, protocol: CodexProtocol::new(), action,
        login_id: None, completion: None, account_requested: false, cancel_requested: false,
        cancel_sent: false, done: false, deadline: Instant::now() + run_time,
    };
    let result = driver.run().await;
    drop(driver);
    let cleaned = cleanup_owned_group(session.child_mut()?, group, Instant::now() + CLEANUP_TIME);
    result?;
    if !cleaned.map_err(io_error)? {
        return Err(failure("native_auth_child_cleanup_uncertain"));
    }
    session.seal_after_child_reaped()
}

#[cfg(test)]
mod tests;
