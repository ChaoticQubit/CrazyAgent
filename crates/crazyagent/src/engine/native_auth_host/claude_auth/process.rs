use super::super::RegisteredNativeAuthHost;
use super::super::custody::CustodySession;
use super::super::login_pipe::{LoginPipe, nonblocking, unix_pipe};
use super::transcript::Transcript;
use super::types::{ClaudeAuthObservation, completion_bytes};
use super::{CHECK_TIME, ClaudeAuthCommand, RENEWAL_TIME, auth_command};
use crate::engine::owner::transport::native_auth_channel::{
    NativeAuthCustodyChannel, NativeAuthCustodyControl,
};
use crate::engine::{EngineError, io_error};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

const OUTPUT_BOUND: &str = "native_auth_claude_output_bound";
const CLEANUP: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);
const CHECK: Duration = Duration::from_millis(100);

/// The longest one official `claude` command may run, inside its operation's own deadline. A
/// status may use the whole account check bound: under build load a cold start passed 2 s and
/// cut checks of signed-in accounts (v2.4.0 owner update). A logout starts the same cold
/// `claude`, so it may too, and is never cut partway through removing a sign-in
/// (RV-DS241-55). Input: the command. Output: its bound.
pub(super) fn command_bound(action: ClaudeAuthCommand) -> Duration {
    match action {
        ClaudeAuthCommand::Login => Duration::from_secs(575),
        ClaudeAuthCommand::Renew => RENEWAL_TIME,
        ClaudeAuthCommand::Status => CHECK_TIME,
        ClaudeAuthCommand::Logout => CHECK_TIME,
    }
}

fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

struct Pipes {
    input: Option<OwnedFd>,
    output: LoginPipe<std::fs::File>,
    errors: LoginPipe<std::fs::File>,
}

impl Pipes {
    fn take(child: &mut Child, action: ClaudeAuthCommand) -> Result<Self, EngineError> {
        let input: Option<OwnedFd> = child.stdin.take().map(Into::into);
        if input.is_none() != (action == ClaudeAuthCommand::Renew) {
            return Err(failure("native_auth_claude_pipe_missing"));
        }
        if let Some(input) = &input {
            nonblocking(input)?;
        }
        let output = unix_pipe(
            child
                .stdout
                .take()
                .ok_or_else(|| failure("native_auth_claude_pipe_missing"))?
                .into(),
            OUTPUT_BOUND,
        )?;
        let errors = unix_pipe(
            child
                .stderr
                .take()
                .ok_or_else(|| failure("native_auth_claude_pipe_missing"))?
                .into(),
            OUTPUT_BOUND,
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
        crate::credentials::wipe(&mut self.bytes);
    }
}

pub(in crate::engine) struct ChildOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) exit: Option<i32>,
}

/// Runs one Claude Code auth child to its end: reads its output, answers the custody checks
/// and hands it the owner's code. The deadline is checked between steps and bounds only the
/// poll wait, never a custody exchange, so an exchange in flight finishes and the channel can
/// still report the failure (RV-DS241-53). Inputs: the session, channel, pipes, action and
/// deadline. Output: the child's output, or the failure.
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
            let Some(target) = pipes.input.as_ref() else {
                return Err(failure("native_auth_claude_input_invalid"));
            };
            match rustix::io::write(target, &pending.bytes[pending.sent..]) {
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
            && pipes.output.ended()
            && pipes.errors.ended()
        {
            return Ok(ChildOutput {
                stdout,
                exit: exit.code(),
            });
        }
        tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

pub(in crate::engine) async fn run_child(
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
    let command = auth_command(&executable.launch_words()?, home, action)?;
    let mut command = command;
    let child = crate::spawn_gate::spawn(&mut command)
        .map_err(|_| failure("native_auth_claude_launch_failed"))?;
    if let Err((error, mut child)) = session.retain_child(child) {
        let clean = CustodySession::cleanup_returned(&mut child, Instant::now() + CLEANUP);
        if !matches!(clean, Ok(true)) {
            std::mem::forget(child);
            return Err(failure("native_auth_child_cleanup_uncertain"));
        }
        return Err(error);
    }
    let deadline = operation_deadline.min(Instant::now() + command_bound(action));
    let result = match Pipes::take(session.child_mut()?, action) {
        Ok(mut pipes) => drive(session, channel, &mut pipes, action, deadline).await,
        Err(error) => Err(error),
    };
    if !session
        .cleanup_child(Instant::now() + CLEANUP)?
        .map_err(|_| failure("native_auth_child_cleanup_uncertain"))?
    {
        return Err(failure("native_auth_child_cleanup_uncertain"));
    }
    result
}
