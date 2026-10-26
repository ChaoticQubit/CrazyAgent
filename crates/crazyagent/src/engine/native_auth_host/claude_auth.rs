use crate::adapters::claude::{AuthMethod, AuthStatus, decode_auth_status};
use crate::engine::EngineError;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(target_os = "macos")]
mod keychain;
mod process;
mod transcript;
pub(crate) mod types;

use super::RegisteredNativeAuthHost;
use super::custody::{CustodySession, Retirement};
use crate::engine::owner::transport::native_auth_channel::{
    NativeAuthCustodyChannel, NativeAuthCustodyControl,
};
use crate::engine::owner::{NativeAuthAction, NativeAuthMethod};
use process::{ChildOutput, run_child};
use std::time::{Duration, Instant};
use types::{ClaudeAuthFailure, ClaudeAuthObservation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaudeAuthCommand {
    Login,
    Status,
    Logout,
}

fn auth_command(
    executable: &Path,
    home: &Path,
    action: ClaudeAuthCommand,
) -> Result<Command, EngineError> {
    if !executable.is_absolute() || !home.is_absolute() {
        return Err(EngineError("native_auth_claude_launch_invalid".into()));
    }
    let operation = match action {
        ClaudeAuthCommand::Login => "login",
        ClaudeAuthCommand::Status => "status",
        ClaudeAuthCommand::Logout => "logout",
    };
    let mut command = Command::new(executable);
    command
        .args(["auth", operation])
        .env_clear()
        .env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", home)
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

fn verified_claude_account(stdout: &[u8], home: &Path) -> Result<Option<AuthStatus>, EngineError> {
    let expected = home
        .to_str()
        .filter(|_| home.is_absolute())
        .ok_or_else(|| EngineError("native_auth_claude_profile_invalid".into()))?;
    let account = decode_auth_status(stdout)
        .map_err(|_| EngineError("native_auth_claude_status_invalid".into()))?;
    if account.config_directory.as_str() != expected {
        return Err(EngineError("native_auth_claude_profile_mismatch".into()));
    }
    if !account.logged_in && matches!(account.auth_method, AuthMethod::None) {
        return Ok(None);
    }
    if !account.logged_in
        || !matches!(account.auth_method, AuthMethod::ClaudeAi)
        || account.api_provider.as_str() != "firstParty"
    {
        return Err(EngineError(
            "native_auth_claude_subscription_unsupported".into(),
        ));
    }
    assert!(account.logged_in);
    assert_eq!(account.config_directory.as_str(), expected);
    Ok(Some(account))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod executable_fixture_tests;

#[cfg(test)]
mod protocol_tests;

fn verified_status(
    stdout: &[u8],
    home: &Path,
    exit: Option<i32>,
) -> Result<Option<AuthStatus>, EngineError> {
    let account = verified_claude_account(stdout, home)?;
    if (account.is_some() && exit != Some(0)) || (account.is_none() && exit != Some(1)) {
        return Err(EngineError(
            "native_auth_claude_status_exit_mismatch".into(),
        ));
    }
    Ok(account)
}

async fn finish_failure(
    session: CustodySession,
    channel: &mut NativeAuthCustodyChannel,
    error: EngineError,
) -> Result<Retirement, EngineError> {
    if error.0 == "native_auth_child_cleanup_uncertain" {
        std::mem::forget(session);
        return Err(error);
    }
    let observation = if error.0 == "native_auth_cancelled" {
        ClaudeAuthObservation::Cancelled
    } else {
        let reason = match error.0.as_str() {
            "native_auth_claude_deadline" => ClaudeAuthFailure::Deadline,
            "native_auth_claude_native_exit" => ClaudeAuthFailure::NativeExit,
            "native_auth_claude_subscription_unsupported" => {
                ClaudeAuthFailure::SubscriptionUnsupported
            }
            "native_auth_claude_profile_mismatch" => ClaudeAuthFailure::ProfileMismatch,
            "native_auth_claude_launch_failed"
            | "native_auth_claude_keychain_unavailable"
            | "native_auth_host_executable_changed"
            | "native_auth_host_executable_unsafe" => ClaudeAuthFailure::LaunchFailed,
            "native_auth_claude_input_invalid" => ClaudeAuthFailure::InputInvalid,
            "native_auth_claude_destination_unsupported" => {
                ClaudeAuthFailure::DestinationUnsupported
            }
            "native_auth_claude_status_invalid"
            | "native_auth_claude_status_exit_mismatch"
            | "native_auth_claude_logout_not_observed" => ClaudeAuthFailure::StatusInvalid,
            _ => ClaudeAuthFailure::OutputInvalid,
        };
        ClaudeAuthObservation::Failed { reason }
    };
    let cancelled = matches!(observation, ClaudeAuthObservation::Cancelled);
    if matches!(
        session.observe_claude(channel, observation).await?,
        NativeAuthCustodyControl::Cancel
    ) && !cancelled
    {
        session
            .observe_claude(channel, ClaudeAuthObservation::Cancelled)
            .await?;
    }
    session.seal_after_child_reaped()
}

pub(in crate::engine) async fn run(
    mut session: CustodySession,
    channel: &mut NativeAuthCustodyChannel,
    executable: &RegisteredNativeAuthHost,
) -> Result<Retirement, EngineError> {
    let first = match channel.fact().action {
        NativeAuthAction::SignIn {
            method: NativeAuthMethod::ClaudeOfficial,
        } => Some(ClaudeAuthCommand::Login),
        NativeAuthAction::SignIn {
            method: NativeAuthMethod::ClaudeExisting,
        } => None,
        NativeAuthAction::Logout => Some(ClaudeAuthCommand::Logout),
        _ => return Err(EngineError("native_auth_claude_method_unsupported".into())),
    };
    if channel.fact().binding.connection.harness.as_str() != "claude" {
        return Err(EngineError("native_auth_method_mismatch".into()));
    }
    let deadline = Instant::now()
        + if first == Some(ClaudeAuthCommand::Login) {
            Duration::from_secs(590)
        } else {
            Duration::from_secs(9)
        };
    if matches!(
        session.check(channel).await?,
        NativeAuthCustodyControl::Cancel
    ) {
        return Err(EngineError("native_auth_cancelled".into()));
    }
    let home = session.profile_path()?;
    #[cfg(target_os = "macos")]
    if let Err(error) = keychain::prepare_profile_keychain(&home) {
        return finish_failure(session, channel, error).await;
    }
    if let Some(first) = first {
        let first_result =
            run_child(&mut session, channel, executable, &home, first, deadline).await;
        match first_result {
            Ok(ChildOutput { exit: Some(0), .. }) => {}
            Ok(_) => {
                return finish_failure(
                    session,
                    channel,
                    EngineError("native_auth_claude_native_exit".into()),
                )
                .await;
            }
            Err(error) => return finish_failure(session, channel, error).await,
        }
        if let Err(error) = session.prepare_status_child() {
            return finish_failure(session, channel, error).await;
        }
    }
    if matches!(
        session.check(channel).await?,
        NativeAuthCustodyControl::Cancel
    ) {
        return finish_failure(
            session,
            channel,
            EngineError("native_auth_cancelled".into()),
        )
        .await;
    }
    if matches!(
        session
            .observe_claude(channel, ClaudeAuthObservation::CheckingAccount)
            .await?,
        NativeAuthCustodyControl::Cancel
    ) {
        return finish_failure(
            session,
            channel,
            EngineError("native_auth_cancelled".into()),
        )
        .await;
    }
    let status = match run_child(
        &mut session,
        channel,
        executable,
        &home,
        ClaudeAuthCommand::Status,
        deadline,
    )
    .await
    {
        Ok(status) => status,
        Err(error) => return finish_failure(session, channel, error).await,
    };
    let account = match verified_status(&status.stdout, &home, status.exit) {
        Ok(account) => account,
        Err(error) => return finish_failure(session, channel, error).await,
    };
    if first == Some(ClaudeAuthCommand::Logout) && account.is_some() {
        return finish_failure(
            session,
            channel,
            EngineError("native_auth_claude_logout_not_observed".into()),
        )
        .await;
    }
    let observed = decode_auth_status(&status.stdout)
        .map_err(|_| EngineError("native_auth_claude_status_invalid".into()))?;
    let observation = ClaudeAuthObservation::status(
        &observed,
        status.exit.expect("verified status has a normal exit"),
    );
    if matches!(
        session.observe_claude(channel, observation).await?,
        NativeAuthCustodyControl::Cancel
    ) {
        return finish_failure(
            session,
            channel,
            EngineError("native_auth_cancelled".into()),
        )
        .await;
    }
    session.seal_after_child_reaped()
}
