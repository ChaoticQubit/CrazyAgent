use crate::engine::{EngineError, io_error};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;

/// The PATH every sign-in or token renewal child runs with, no folder added: a script
/// harness starts through its resolved interpreter instead ([`launch::handed_command`],
/// [`sign_in_command`]).
pub(crate) const SIGN_IN_PATH: &str = "/usr/bin:/bin";

#[cfg(test)]
use custodian::CAUSE_BYTES_MAX;
use custodian::CustodianCause;
#[cfg(test)]
use handed::handed_words;
pub use handed::{
    CUSTODIAN_ARGUMENTS_MAX, CustodianArguments, HandedHarness, HandedWord, custodian_arguments,
};
pub(crate) use launch::{LaunchWord, handed_command};
#[cfg(test)]
use pin::EXECUTABLE_BYTES_MAX;
#[cfg(test)]
use pin::SETTLE_AFTER;
#[cfg(test)]
use pin::settles;
pub(crate) use pin::{RegisteredNativeAuthHost, pinnable};

/// Starts a token renewal child in the engine with no search path: the harness file itself,
/// or for a script that asks `env` for its interpreter, such as an npm-built Codex asking
/// for `node`, that interpreter's absolute file and then the script
/// ([`crate::harnesses::interactive::launch_words`]). A sign-in custodian never resolves:
/// it runs the words the engine handed it ([`launch`]). Input: the harness's real file.
/// Output: the command with those words, or the tool resolver's owner line.
pub(crate) fn sign_in_command(executable: &Path) -> Result<std::process::Command, EngineError> {
    let words = crate::harnesses::interactive::launch_words(executable)
        .map_err(|problem| EngineError(problem.0))?;
    let (program, script) = words
        .split_first()
        .ok_or_else(|| EngineError("native_auth_launch_invalid".into()))?;
    assert!(Path::new(program).is_absolute());
    let mut command = std::process::Command::new(program);
    command.args(script);
    Ok(command)
}

pub async fn run_native_auth_custodian(
    runtime_dir: &Path,
    input: OwnedFd,
) -> Result<(), EngineError> {
    entry::run(runtime_dir, input).await.map(|_| ())
}

pub async fn run_native_auth_custodian_with_codex(
    runtime_dir: &Path,
    input: OwnedFd,
    executable: &Path,
    expected_sha256: &str,
    launch: &[HandedWord<'_>],
) -> Result<(), EngineError> {
    entry::run_with_codex(runtime_dir, input, executable, expected_sha256, launch)
        .await
        .map(|_| ())
}

pub async fn run_native_auth_custodian_with_claude(
    runtime_dir: &Path,
    input: OwnedFd,
    executable: &Path,
    expected_sha256: &str,
    launch: &[HandedWord<'_>],
) -> Result<(), EngineError> {
    entry::run_with_claude(runtime_dir, input, executable, expected_sha256, launch)
        .await
        .map(|_| ())
}

/// Builds the sign-in custodian's command with a cleared environment: this engine's own
/// file, the runtime folder and, for a Codex or Claude Code sign-in, the pinned harness and
/// the launch the engine resolved for it ([`launch::hand_over`]). Inputs: the engine file,
/// the runtime folder and the harness kind, file and words. Output: the command, or the pin
/// error of a file that changed.
fn custodian_command(
    executable: &Path,
    runtime_dir: &Path,
    harness: Option<(&str, &RegisteredNativeAuthHost, &[LaunchWord])>,
) -> Result<std::process::Command, EngineError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(executable);
    command.arg("native-auth-custodian").arg(runtime_dir);
    if let Some((kind, harness, launch)) = harness {
        launch::hand_over(&mut command, kind, harness, launch)?;
    }
    command
        .env_clear()
        .current_dir(runtime_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(
            if std::env::var_os("CRAZYAGENT_DEBUG_CUSTODIAN").is_some() {
                Stdio::inherit()
            } else {
                Stdio::piped()
            },
        )
        .process_group(0);
    Ok(command)
}

/// The code of a sign-in the engine refuses before any custodian runs, because the harness
/// it would run is older than that harness's sign-in minimum (DEC-166).
const TOO_OLD: &str = "native_auth_harness_too_old";

/// The harness a custodian gets with the launch the engine resolved for it, a sign-in
/// waiting for the first probe run when that is still going ([`launch::resolved_after_probe`]),
/// or none when it cannot start, so the custodian ends the action without effect and it
/// settles unavailable. Inputs: the pinned harness, its kind and the action. Output: both,
/// or none.
async fn handed(
    harness: Arc<RegisteredNativeAuthHost>,
    kind: &str,
    action: &crate::engine::owner::NativeAuthAction,
) -> Option<(Arc<RegisteredNativeAuthHost>, Vec<LaunchWord>)> {
    let may_wait = matches!(
        action,
        crate::engine::owner::NativeAuthAction::SignIn { .. }
    );
    match launch::resolved_after_probe(&harness, kind, may_wait).await {
        Some(words) => Some((harness, words)),
        None => {
            eprintln!("native_auth_harness_unresolved harness={kind}");
            None
        }
    }
}

pub(in crate::engine) async fn dispatch(
    work: crate::storage::NativeAuthWork,
    host: crate::engine::owner::transport::NativeAuthDispatchHost,
) -> Result<(), EngineError> {
    let executable = host.registered_executable()?;
    let kind = work.fact().binding.connection.harness.as_str().to_owned();
    let registered = match kind.as_str() {
        "codex" => host.registered_codex_executable()?,
        "claude" => host.registered_claude_executable()?,
        _ => None,
    };
    let harness = match registered {
        Some(harness) => handed(harness, &kind, &work.fact().action).await,
        None => None,
    };
    let too_old = match harness.as_ref().map(|(harness, _)| harness.verified_path()) {
        Some(Ok(runs)) => host.harness_too_old(&kind, runs).await,
        _ => false,
    };
    let runtime_dir = host.owner_runtime_dir()?;
    let launch = host.prepare_native_auth_custody(work).await?;
    if too_old {
        host.record_custodian_failure(TOO_OLD)?;
        drop(launch);
        return Err(EngineError(TOO_OLD.into()));
    }
    let mut command = custodian_command(
        executable.verified_path()?,
        &runtime_dir,
        harness
            .as_ref()
            .map(|(harness, words)| (kind.as_str(), harness.as_ref(), words.as_slice())),
    )?;
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(io_error)?;
    let cause = child.stderr.take().map(CustodianCause::new).transpose();
    let streams = match (child.stdin.take(), cause) {
        (Some(input), Ok(cause)) => Ok((input, cause)),
        (None, _) => Err(EngineError("native_auth_host_input_unavailable".into())),
        (_, Err(error)) => Err(error),
    };
    let (input, mut cause) = match streams {
        Ok(streams) => streams,
        Err(error) => {
            custody::cleanup_direct_child(
                &mut child,
                std::time::Instant::now() + std::time::Duration::from_secs(2),
            )
            .map_err(io_error)?;
            return Err(error);
        }
    };
    if let Err((error, mut child)) = host.retain_child(child) {
        custody::cleanup_direct_child(
            &mut child,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        )
        .map_err(io_error)?;
        return Err(error);
    }
    launch.write_bootstrap(input)?;
    loop {
        if let Some(cause) = cause.as_mut() {
            cause.read()?;
        }
        if let Some(status) = host.try_wait_child()? {
            if status.success() {
                return Ok(());
            }
            let code = match cause.as_mut() {
                Some(cause) => {
                    cause.read()?;
                    cause.code()
                }
                None => None,
            };
            eprintln!(
                "native_auth_custodian_failed cause={}",
                code.unwrap_or("native_auth_custodian_cause_unnamed")
            );
            if let Some(code) = code {
                host.record_custodian_failure(code)?;
            }
            return Err(EngineError("native_auth_host_child_failed".into()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

pub(in crate::engine) mod claude_auth;

#[cfg(test)]
mod bootstrap_pipe_tests;
#[cfg(test)]
mod cause_tests;
pub(in crate::engine) mod codex_auth;
mod custodian;
pub(super) mod custody;
#[cfg(test)]
mod custody_session_tests;
#[cfg(test)]
mod custody_tests;
pub(in crate::engine) mod entry;
mod entry_bootstrap;
#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod executable_tests;
mod handed;
#[cfg(test)]
mod hash_backend_tests;
mod launch;
mod pin;
