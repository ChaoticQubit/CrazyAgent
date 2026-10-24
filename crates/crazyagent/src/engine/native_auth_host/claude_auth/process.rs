use super::super::RegisteredNativeAuthHost;
use super::super::custody::{CustodySession, cleanup_direct_child, cleanup_owned_group};
use super::transcript::Transcript;
use super::types::{ClaudeAuthObservation, completion_bytes};
use super::{ClaudeAuthCommand, auth_command};
use crate::engine::owner::transport::native_auth_channel::{
    NativeAuthCustodyChannel, NativeAuthCustodyControl,
};
use crate::engine::{EngineError, io_error};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::Pid;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

const OUTPUT_MAX: usize = 65536;
const CLEANUP: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);
const CHECK: Duration = Duration::from_millis(100);

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

struct Pipe {
    fd: OwnedFd,
    bytes: usize,
    eof: bool,
}

impl Pipe {
    fn new(fd: OwnedFd) -> Result<Self, EngineError> {
        nonblocking(&fd)?;
        Ok(Self {
            fd,
            bytes: 0,
            eof: false,
        })
    }

    fn read(&mut self) -> Result<Vec<u8>, EngineError> {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        for _ in 0..16 {
            match rustix::io::read(&self.fd, &mut buffer[..]) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(size) => {
                    if size > OUTPUT_MAX.saturating_sub(self.bytes) {
                        return Err(failure("native_auth_claude_output_bound"));
                    }
                    self.bytes += size;
                    output.extend_from_slice(&buffer[..size]);
                }
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(io_error(error)),
            }
        }
        assert!(self.bytes <= OUTPUT_MAX);
        Ok(output)
    }
}

fn nonblocking(fd: &OwnedFd) -> Result<(), EngineError> {
    let flags = fcntl_getfl(fd).map_err(io_error)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(io_error)?;
    assert!(
        fcntl_getfl(fd)
            .map_err(io_error)?
            .contains(OFlags::NONBLOCK)
    );
    Ok(())
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
            .ok_or_else(|| failure("native_auth_claude_pipe_missing"))?
            .into();
        nonblocking(&input)?;
        let output = Pipe::new(
            child
                .stdout
                .take()
                .ok_or_else(|| failure("native_auth_claude_pipe_missing"))?
                .into(),
        )?;
        let errors = Pipe::new(
            child
                .stderr
                .take()
                .ok_or_else(|| failure("native_auth_claude_pipe_missing"))?
                .into(),
        )?;
        assert!(child.stdin.is_none() && child.stdout.is_none() && child.stderr.is_none());
        Ok(Self {
            input,
            output,
            errors,
        })
    }
}

struct PrivateInput {
    bytes: Vec<u8>,
    sent: usize,
    deadline: Instant,
}

impl Drop for PrivateInput {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

pub(super) struct ChildOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) exit: Option<i32>,
}

async fn drive(
    session: &mut CustodySession,
    channel: &mut NativeAuthCustodyChannel,
    pipes: &mut Pipes,
    action: ClaudeAuthCommand,
    deadline: Instant,
) -> Result<ChildOutput, EngineError> {
    let mut stdout = Vec::new();
    let mut transcript = Transcript::default();
    let mut error_transcript = Transcript::default();
    let mut checked = Instant::now();
    let mut accepts_code = false;
    let mut input: Option<PrivateInput> = None;
    let mut submitted = false;
    loop {
        if Instant::now() >= deadline {
            return Err(failure("native_auth_claude_deadline"));
        }
        if checked.elapsed() >= CHECK {
            if matches!(
                session.check(channel).await?,
                NativeAuthCustodyControl::Cancel
            ) {
                return Err(failure("native_auth_cancelled"));
            }
            if accepts_code
                && !submitted
                && let Some(secret) = session.take_claude_input(channel).await?
            {
                input = Some(PrivateInput {
                    bytes: completion_bytes(secret)?,
                    sent: 0,
                    deadline: Instant::now() + Duration::from_secs(2),
                });
                submitted = true;
            }
            checked = Instant::now();
        }
        let output = pipes.output.read()?;
        let errors = pipes.errors.read()?;
        if action == ClaudeAuthCommand::Login {
            for challenge in [transcript.push(&output)?, error_transcript.push(&errors)?]
                .into_iter()
                .flatten()
            {
                accepts_code |= challenge.accepts_code();
                if matches!(
                    session
                        .observe_claude(channel, ClaudeAuthObservation::Challenge { challenge })
                        .await?,
                    NativeAuthCustodyControl::Cancel
                ) {
                    return Err(failure("native_auth_cancelled"));
                }
            }
        } else if action == ClaudeAuthCommand::Status {
            stdout.extend_from_slice(&output);
        }
        if let Some(pending) = input.as_mut() {
            if Instant::now() >= pending.deadline {
                return Err(failure("native_auth_claude_input_invalid"));
            }
            match rustix::io::write(&pipes.input, &pending.bytes[pending.sent..]) {
                Ok(0) => return Err(failure("native_auth_claude_input_invalid")),
                Ok(size) => {
                    pending.sent += size;
                }
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
                Err(_) => return Err(failure("native_auth_claude_input_invalid")),
            }
            if pending.sent == pending.bytes.len() {
                input.take();
            }
        }
        let exit = session.child_mut()?.try_wait().map_err(io_error)?;
        if let Some(exit) = exit
            && pipes.output.eof
            && pipes.errors.eof
        {
            return Ok(ChildOutput {
                stdout,
                exit: exit.code(),
            });
        }
        tokio::time::sleep(POLL).await;
    }
}

pub(super) async fn run_child(
    session: &mut CustodySession,
    channel: &mut NativeAuthCustodyChannel,
    executable: &RegisteredNativeAuthHost,
    home: &Path,
    action: ClaudeAuthCommand,
    operation_deadline: Instant,
) -> Result<ChildOutput, EngineError> {
    if matches!(
        session.check(channel).await?,
        NativeAuthCustodyControl::Cancel
    ) {
        return Err(failure("native_auth_cancelled"));
    }
    if session.profile_path()? != home {
        return Err(failure("native_auth_claude_profile_mismatch"));
    }
    let command = auth_command(executable.verified_path()?, home, action)?;
    let mut command = command;
    let child = command
        .spawn()
        .map_err(|_| failure("native_auth_claude_launch_failed"))?;
    if let Err((error, mut child)) = session.retain_child(child) {
        let group = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
        let clean = match group {
            Some(group) => cleanup_owned_group(&mut child, group, Instant::now() + CLEANUP),
            None => cleanup_direct_child(&mut child, Instant::now() + CLEANUP)
                .map(|status| status.is_some()),
        };
        if !matches!(clean, Ok(true)) {
            std::mem::forget(child);
            return Err(failure("native_auth_child_cleanup_uncertain"));
        }
        return Err(error);
    }
    let group = i32::try_from(session.child_mut()?.id())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| failure("native_auth_child_cleanup_uncertain"))?;
    let bound = if action == ClaudeAuthCommand::Login {
        Duration::from_secs(575)
    } else {
        Duration::from_secs(2)
    };
    let deadline = operation_deadline.min(Instant::now() + bound);
    let result = match Pipes::take(session.child_mut()?) {
        Ok(mut pipes) => tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            drive(session, channel, &mut pipes, action, deadline),
        )
        .await
        .map_err(|_| failure("native_auth_claude_deadline"))
        .and_then(|result| result),
        Err(error) => Err(error),
    };
    if !cleanup_owned_group(session.child_mut()?, group, Instant::now() + CLEANUP)
        .map_err(|_| failure("native_auth_child_cleanup_uncertain"))?
    {
        return Err(failure("native_auth_child_cleanup_uncertain"));
    }
    result
}
