mod process;
mod process_custody;
mod protocol;

pub use protocol::{CodexHostBatch, CodexHostProtocol};

use crate::adapters::NativeAction;
use crate::adapters::codex::{AuthControl, State};
use crate::adapters::events::ObservedEvent;
use crate::engine::EngineError;
use crate::engine::bootstrap::NativeProfileUse;
use crate::protocol::{CredentialRef, RequestId};
use crate::routing::ResourceBudget;
use crate::sandbox::InstalledSandbox;
use process::{CodexHostOutput, CodexHostProcess};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

const EVENTS_MAX: usize = 128;

pub(in crate::engine) struct CodexHost {
    protocol: CodexHostProtocol,
    process: CodexHostProcess,
    events: VecDeque<ObservedEvent>,
    auth: VecDeque<AuthControl>,
    outbound: VecDeque<Vec<u8>>,
    disconnected: bool,
}

impl CodexHost {
    /// Names the process group the harness was started in, which can outlive this host when
    /// the engine stops. Input: the host. Output: the group.
    pub(in crate::engine) fn process_group(&self) -> u32 {
        self.process.pid()
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::engine) async fn spawn(
        command: Command,
        sandbox: InstalledSandbox,
        profile: NativeProfileUse,
        home: PathBuf,
        action: NativeAction,
        account_ref: CredentialRef,
        budget: ResourceBudget,
        instructions: &str,
    ) -> Result<Self, EngineError> {
        let protocol = CodexHostProtocol::new(action, account_ref)?;
        let (command, cwd, tools) = prepare_command(command, &profile, &home)?;
        let command = sandbox
            .wrap(&command)
            .map_err(|_| EngineError("codex_host_sandbox_refused".into()))?;
        assert_eq!(command.get_program(), sandbox.launcher().as_os_str());
        assert_eq!(command.get_current_dir(), Some(cwd.as_path()));
        let process = CodexHostProcess::spawn_guarded(command, &budget, Some(profile)).await?;
        let mut host = Self {
            protocol,
            process,
            events: VecDeque::new(),
            auth: VecDeque::new(),
            outbound: VecDeque::new(),
            disconnected: false,
        };
        let bytes = host.protocol.initialize()?;
        host.process.send(&bytes).await?;
        host.wait_for(State::Ready).await?;
        let bytes = host
            .protocol
            .start_instructed_thread(&cwd, instructions, tools)?;
        host.process.send(&bytes).await?;
        host.wait_for(State::Idle).await?;
        Ok(host)
    }

    pub(in crate::engine) async fn observe_account(&mut self) -> Result<AuthControl, EngineError> {
        let bytes = self.protocol.read_account()?;
        self.process.send(&bytes).await?;
        loop {
            if let Some(auth) = self.auth.pop_front() {
                return match auth {
                    AuthControl::AccountRead { .. }
                    | AuthControl::RequestFailed {
                        method: "account/read",
                        ..
                    } => Ok(auth),
                    _ => Err(EngineError("codex_host_unexpected_auth_control".into())),
                };
            }
            self.pump().await?;
        }
    }

    pub(in crate::engine) async fn start_turn(
        &mut self,
        request_id: RequestId,
        text: &str,
    ) -> Result<(), EngineError> {
        let bytes = self.protocol.start_turn(request_id, text)?;
        self.process.send(&bytes).await
    }

    pub(in crate::engine) async fn next_event(&mut self) -> Result<ObservedEvent, EngineError> {
        loop {
            self.flush_outbound().await?;
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }
            self.pump().await?;
        }
    }

    pub(in crate::engine) async fn cancel(&mut self) -> Result<(), EngineError> {
        if self.protocol.state() == State::Active
            && let Ok(bytes) = self.protocol.interrupt()
        {
            return self.process.send(&bytes).await;
        }
        self.process.request_stop();
        Ok(())
    }

    pub(in crate::engine) async fn close(&mut self) -> Result<(), EngineError> {
        self.process.close().await?;
        self.process.operation_result()
    }

    async fn wait_for(&mut self, expected: State) -> Result<(), EngineError> {
        while self.protocol.state() != expected {
            if self.protocol.state() == State::Failed {
                return Err(EngineError("codex_host_disconnected".into()));
            }
            if !matches!(
                self.protocol.state(),
                State::Initializing | State::Initialized | State::ThreadStarting
            ) {
                return Err(EngineError("codex_host_bootstrap_failed".into()));
            }
            self.pump().await?;
        }
        Ok(())
    }

    async fn pump(&mut self) -> Result<(), EngineError> {
        self.flush_outbound().await?;
        if self.disconnected {
            return Err(EngineError("codex_host_disconnected".into()));
        }
        match self.process.next_output().await? {
            CodexHostOutput::Stdout(bytes) => {
                let batch = self.protocol.receive(&bytes)?;
                if self.events.len() + batch.events.len() > EVENTS_MAX
                    || self.auth.len() + batch.auth.len() > EVENTS_MAX
                    || self.outbound.len() + batch.outbound.len() > EVENTS_MAX
                {
                    return Err(EngineError("codex_host_event_capacity".into()));
                }
                self.events.extend(batch.events);
                self.auth.extend(batch.auth);
                self.outbound.extend(batch.outbound);
                self.flush_outbound().await?;
                assert!(self.events.len() <= EVENTS_MAX);
                assert!(self.auth.len() <= EVENTS_MAX);
            }
            CodexHostOutput::Exited => {
                self.events.push_back(self.protocol.disconnect());
                self.disconnected = true;
            }
        }
        Ok(())
    }

    async fn flush_outbound(&mut self) -> Result<(), EngineError> {
        while let Some(bytes) = self.outbound.front() {
            self.process.send(bytes).await?;
            self.outbound.pop_front();
        }
        Ok(())
    }
}

impl Drop for CodexHost {
    fn drop(&mut self) {
        self.process.request_stop();
    }
}

/// Takes the master command as the engine built it and keeps only its own environment, with
/// the home it runs in: the account's managed home, or a private turn home outside it that
/// holds a credential copy. The command must name that same home. Inputs: the command, the
/// profile lease and the home. Output: the command, its working directory and whether it is
/// the tools-on master (Codex's own sandbox off), or a plain code.
fn prepare_command(
    command: Command,
    profile: &NativeProfileUse,
    home: &Path,
) -> Result<(Command, PathBuf, bool), EngineError> {
    let cwd = command
        .get_current_dir()
        .filter(|path| path.is_absolute() && path.is_dir())
        .ok_or_else(|| EngineError("codex_host_project_directory".into()))?
        .to_path_buf();
    let program = std::path::Path::new(command.get_program());
    if !program.is_absolute() {
        return Err(EngineError("codex_host_executable_path".into()));
    }
    let args: Vec<_> = command.get_args().collect();
    if !args.windows(2).any(|pair| {
        pair[0] == OsStr::new("-c") && pair[1] == OsStr::new("cli_auth_credentials_store=\"file\"")
    }) || args
        .iter()
        .filter(|arg| {
            arg.to_str()
                .is_some_and(|value| value.contains("cli_auth_credentials_store"))
        })
        .count()
        != 1
    {
        return Err(EngineError("codex_host_file_auth_required".into()));
    }
    let managed = profile.profile_path()?;
    let named = command
        .get_envs()
        .find(|(key, _)| *key == OsStr::new("CODEX_HOME"))
        .and_then(|(_, value)| value);
    if named != Some(home.as_os_str())
        || !home.is_absolute()
        || (home != managed && (home.starts_with(&managed) || managed.starts_with(home)))
    {
        return Err(EngineError("codex_host_home_unsafe".into()));
    }
    let tools = args.windows(2).any(|pair| {
        pair[0] == OsStr::new("-c") && pair[1] == OsStr::new("sandbox_mode=\"danger-full-access\"")
    });
    let mut selected = Command::new(program);
    selected.args(args).current_dir(&cwd).env_clear();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            selected.env(key, value);
        }
    }
    selected.env("HOME", home).env("CODEX_HOME", home);
    Ok((selected, cwd, tools))
}

#[cfg(test)]
#[path = "codex_host/process_tests.rs"]
mod process_tests;
