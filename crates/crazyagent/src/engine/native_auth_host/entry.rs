use super::custody::CustodySession;
use super::{HandedWord, RegisteredNativeAuthHost};
use crate::engine::bootstrap::runner_binding;
use crate::engine::owner::transport::native_auth_channel::NativeAuthCustodyChannel;
use crate::engine::owner::{HostStamp, NativeAuthFact};
use crate::engine::{EngineError, io_error};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

const TOKEN_BYTES: usize = 32;
const BOOTSTRAP_TIME_MAX: Duration = Duration::from_secs(10);
const CHECK_RENEWAL_WAIT: Duration = Duration::from_secs(90);

pub(super) async fn read_bootstrap(
    input: &OwnedFd,
    deadline: Instant,
) -> Result<[u8; 32], EngineError> {
    let entered = Instant::now();
    if entered >= deadline {
        return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
    }
    let deadline = deadline.min(entered + BOOTSTRAP_TIME_MAX);
    let flags = fcntl_getfl(input).map_err(io_error)?;
    fcntl_setfl(input, flags | OFlags::NONBLOCK).map_err(io_error)?;
    let input = AsyncFd::with_interest(input.as_fd(), Interest::READABLE).map_err(io_error)?;
    let expires = tokio::time::Instant::from_std(deadline);
    let mut buffer = [0; TOKEN_BYTES + 1];
    let mut used = 0;
    loop {
        assert!(used <= TOKEN_BYTES);
        if Instant::now() >= deadline {
            return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
        }
        let mut ready = tokio::time::timeout_at(expires, input.readable())
            .await
            .map_err(|_| EngineError("native_auth_bootstrap_timeout".to_owned()))?
            .map_err(io_error)?;
        if Instant::now() >= deadline {
            return Err(EngineError("native_auth_bootstrap_timeout".to_owned()));
        }
        let read = ready.try_io(|fd| {
            rustix::io::read(fd.get_ref(), &mut buffer[used..]).map_err(std::io::Error::from)
        });
        match read {
            Ok(Ok(0)) if used == TOKEN_BYTES => {
                let mut token = [0; TOKEN_BYTES];
                token.copy_from_slice(&buffer[..TOKEN_BYTES]);
                return Ok(token);
            }
            Ok(Ok(0)) => {
                return Err(EngineError("native_auth_bootstrap_truncated".to_owned()));
            }
            Ok(Ok(size)) => {
                used += size;
                assert!(used <= buffer.len());
                if used > TOKEN_BYTES {
                    return Err(EngineError("native_auth_bootstrap_excess".to_owned()));
                }
            }
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(io_error(error)),
            Err(_would_block) => continue,
        }
    }
}

pub(in crate::engine) async fn run(
    runtime_dir: &Path,
    input: OwnedFd,
) -> Result<NativeAuthFact, EngineError> {
    run_inner(runtime_dir, input, None, None, &[]).await
}

pub(in crate::engine) async fn run_with_codex(
    runtime_dir: &Path,
    input: OwnedFd,
    executable: &Path,
    expected_sha256: &str,
    launch: &[HandedWord<'_>],
) -> Result<NativeAuthFact, EngineError> {
    run_inner(
        runtime_dir,
        input,
        Some((executable, expected_sha256)),
        None,
        launch,
    )
    .await
}

pub(in crate::engine) async fn run_with_claude(
    runtime_dir: &Path,
    input: OwnedFd,
    executable: &Path,
    expected_sha256: &str,
    launch: &[HandedWord<'_>],
) -> Result<NativeAuthFact, EngineError> {
    run_inner(
        runtime_dir,
        input,
        None,
        Some((executable, expected_sha256)),
        launch,
    )
    .await
}

/// The harness the engine handed a custodian, pinned with the launch the engine resolved
/// for it: the harness and each launch file must hash to what the engine sent. Inputs: the
/// harness file, its SHA-256 and the handed words. Output: the harness, or
/// `native_auth_host_executable_changed`.
fn pinned_harness(
    executable: &Path,
    expected_sha256: &str,
    launch: &[HandedWord<'_>],
) -> Result<RegisteredNativeAuthHost, EngineError> {
    let harness = RegisteredNativeAuthHost::capture(executable)?;
    if harness.verified_sha256()? != expected_sha256 {
        return Err(EngineError("native_auth_host_executable_changed".into()));
    }
    Ok(harness.with_launch(super::launch::pinned(launch)?))
}

async fn run_inner(
    runtime_dir: &Path,
    input: OwnedFd,
    codex: Option<(&Path, &str)>,
    claude: Option<(&Path, &str)>,
    launch: &[HandedWord<'_>],
) -> Result<NativeAuthFact, EngineError> {
    assert!(launch.is_empty() || codex.is_some() || claude.is_some());
    let token = read_bootstrap(&input, Instant::now() + BOOTSTRAP_TIME_MAX).await?;
    drop(input);
    let mut channel = NativeAuthCustodyChannel::connect(runtime_dir, token).await?;
    let fact = channel.fact().clone();
    let (_, identity, binding) = runner_binding(runtime_dir)?;
    if identity.database_instance() != fact.binding.database_instance.as_str()
        || identity.engine_epoch() != fact.binding.engine_epoch.as_str()
    {
        return Err(EngineError("native_profile_binding_conflict".into()));
    }
    let host = HostStamp {
        db_instance: identity.database_instance().to_owned(),
        engine_epoch: identity.engine_epoch().to_owned(),
    };
    let purpose = fact.action.profile_purpose();
    let mut custody = binding.capture_native_profile_custody()?;
    let guard = custody.acquire_use_within(
        &host,
        &fact.binding,
        &fact.operation_id,
        fact.generation,
        purpose,
        Some(CHECK_RENEWAL_WAIT),
    )?;
    let session = CustodySession::new(guard);
    session.check(&mut channel).await?;
    let retirement = if let Some((executable, expected_sha256)) = claude {
        if codex.is_some() || fact.binding.connection.harness.as_str() != "claude" {
            return Err(EngineError("native_auth_method_mismatch".into()));
        }
        let executable = pinned_harness(executable, expected_sha256, launch)?;
        super::claude_auth::run(session, &mut channel, &executable).await?
    } else if let Some((executable, expected_sha256)) = codex {
        if fact.binding.connection.harness.as_str() != "codex" {
            return Err(EngineError("native_auth_method_mismatch".into()));
        }
        let executable = pinned_harness(executable, expected_sha256, launch)?;
        super::codex_auth::run(session, &mut channel, &executable).await?
    } else {
        session.seal_without_effect()?
    };
    let retired = if retirement.child_reaped() {
        channel.retire_after_child_reaped(retirement).await?
    } else {
        channel.retire(retirement).await?
    };
    assert_eq!(retired.operation_id, fact.operation_id);
    assert_eq!(retired.generation, fact.generation);
    Ok(retired)
}
