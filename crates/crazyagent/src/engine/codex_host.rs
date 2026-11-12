mod process;
mod process_custody;
mod protocol;

pub use protocol::{CodexHostBatch, CodexHostProtocol};

use crate::adapters::NativeAction;
use crate::adapters::codex::{AuthControl, State};
use crate::adapters::events::{EventKind, ObservedEvent, TurnOutcome};
use crate::engine::EngineError;
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::claude_host::diagnostic::StreamDiagnostic;
use crate::master_frame_limit::FrameSize;
use crate::master_step_limit::StepLimit;
use crate::master_time_limit::START_TIME_MAX;
use crate::protocol::{CredentialRef, RequestId};
use crate::routing::ResourceBudget;
use crate::sandbox::InstalledSandbox;
use process::{CodexHostOutput, CodexHostProcess};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub(in crate::engine) struct CodexHost {
    protocol: CodexHostProtocol,
    process: CodexHostProcess,
    events: VecDeque<ObservedEvent>,
    auth: VecDeque<AuthControl>,
    outbound: VecDeque<Vec<u8>>,
    disconnected: bool,
    diagnostic: StreamDiagnostic,
    steps: StepStop,
}

/// The owner's master step limit on one Codex host (PA-9) and whether the host already
/// interrupted the turn for it.
#[derive(Clone, Copy, Debug, Default)]
struct StepStop {
    limit: StepLimit,
    stopped: bool,
}

impl CodexHost {
    /// Names the process group the harness was started in, which can outlive this host when
    /// the engine stops. Input: the host. Output: the group.
    pub(in crate::engine) fn process_group(&self) -> u32 {
        self.process.pid()
    }

    /// Starts the Codex harness under custody and brings its thread to idle. `run_time` is
    /// the wall clock of the whole run; with none the run has no clock and only its start
    /// is bounded, by `START_TIME_MAX`. Inputs: the launch, its sandbox, lease, home, action,
    /// account, clock, budget, instructions and the owner's master frame size limit, which
    /// bounds every frame the host writes. Output: the idle host or a plain code.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::engine) async fn spawn(
        command: Command,
        sandbox: InstalledSandbox,
        profile: NativeProfileUse,
        home: PathBuf,
        action: NativeAction,
        account_ref: CredentialRef,
        run_time: Option<Duration>,
        budget: ResourceBudget,
        instructions: &str,
        frames: FrameSize,
    ) -> Result<Self, EngineError> {
        let mut protocol = CodexHostProtocol::new(action, account_ref)?;
        protocol.limit_frames(frames.bytes());
        let (command, cwd, tools) = prepare_command(command, &profile, &home)?;
        let command = sandbox
            .wrap(&command)
            .map_err(|_| EngineError("codex_host_sandbox_refused".into()))?;
        assert_eq!(command.get_program(), sandbox.launcher().as_os_str());
        assert_eq!(command.get_current_dir(), Some(cwd.as_path()));
        let mut process = CodexHostProcess::spawn_guarded(
            command,
            &budget,
            run_time,
            Some(profile),
            sandbox.is_unconfined(),
        )
        .await?;
        process.limit_input(frames.bytes());
        let mut host = Self {
            protocol,
            process,
            events: VecDeque::new(),
            auth: VecDeque::new(),
            outbound: VecDeque::new(),
            disconnected: false,
            diagnostic: StreamDiagnostic::default(),
            steps: StepStop::default(),
        };
        let started = host.start(&cwd, instructions, tools);
        match run_time {
            Some(_) => started.await?,
            None => tokio::time::timeout(START_TIME_MAX, started)
                .await
                .map_err(|_| EngineError("codex_host_startup_timeout".into()))??,
        }
        assert!(host.protocol.state() == State::Idle);
        Ok(host)
    }

    /// Initializes the harness and starts its instructed thread. Inputs: the host, the
    /// thread's folder, the instructions and whether tools are on. Output: Ok once the
    /// thread is idle, or a plain code.
    async fn start(
        &mut self,
        cwd: &Path,
        instructions: &str,
        tools: bool,
    ) -> Result<(), EngineError> {
        let bytes = self.protocol.initialize()?;
        self.process.send(&bytes).await?;
        self.wait_for(State::Ready).await?;
        let bytes = self
            .protocol
            .start_instructed_thread(cwd, instructions, tools)?;
        self.process.send(&bytes).await?;
        self.wait_for(State::Idle).await
    }

    /// What this host kept about its app-server stream for the engine log: the protocol
    /// fault that ended it, the frame that fault was found on and the last raw bytes.
    /// Input: the host. Output: a copy of the diagnostic.
    pub(in crate::engine) fn diagnostic(&self) -> StreamDiagnostic {
        self.diagnostic.clone()
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

    /// Gives a tools-on master the owner's master step limit, as Claude Code's --max-turns
    /// does (PA-9): once the turn has taken that many steps, the host interrupts it before
    /// its next model round, and the interrupted turn ends with the limit's cause,
    /// `master_step_limit_<steps>`. No limit leaves the turn bounded by its run time only.
    /// Input: the limit. Output: none.
    pub(in crate::engine) fn limit_steps(&mut self, limit: StepLimit) {
        assert!(!self.steps.stopped);
        self.steps.limit = limit;
    }

    pub(in crate::engine) async fn next_event(&mut self) -> Result<ObservedEvent, EngineError> {
        loop {
            self.flush_outbound().await?;
            if let Some(event) = self.events.pop_front() {
                return self.step_end(event);
            }
            self.pump().await?;
            self.stop_at_step_limit().await?;
        }
    }

    /// Interrupts the running turn once it has taken the steps its limit allows. Input: the
    /// host. Output: Ok, or the send's plain code.
    async fn stop_at_step_limit(&mut self) -> Result<(), EngineError> {
        let reached = self
            .steps
            .limit
            .steps()
            .is_some_and(|steps| self.protocol.steps() >= steps);
        if !reached || self.steps.stopped || self.protocol.state() != State::Active {
            return Ok(());
        }
        let bytes = self.protocol.interrupt()?;
        self.steps.stopped = true;
        self.process.send(&bytes).await
    }

    /// Names the step limit in the end of a turn this host interrupted for it; a turn that
    /// still completed on its own keeps its end. Input: the event. Output: the event, or
    /// the limit's cause for the interrupted turn's end.
    fn step_end(&self, event: ObservedEvent) -> Result<ObservedEvent, EngineError> {
        let interrupted = matches!(
            event.kind,
            EventKind::TurnFinished {
                outcome: TurnOutcome::Incomplete,
                ..
            }
        );
        if !(self.steps.stopped && interrupted) {
            return Ok(event);
        }
        let cause = self.steps.limit.cause();
        assert!(cause.is_some());
        Err(EngineError(cause.unwrap_or_default()))
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

    /// Takes one chunk of the harness's output, or its exit, into the host's queues. No count
    /// of events ends a run (OB-5e): one chunk holds at most as many frames as bytes, a turn
    /// takes the next chunk only once it has read these events, and the process holds the
    /// rest back. Input: the host. Output: Ok, or a plain code.
    async fn pump(&mut self) -> Result<(), EngineError> {
        self.flush_outbound().await?;
        if self.disconnected {
            return Err(EngineError("codex_host_disconnected".into()));
        }
        match self.process.next_output().await? {
            CodexHostOutput::Stdout(bytes) => {
                self.diagnostic.keep(&bytes);
                let batch = self.protocol.receive(&bytes).inspect_err(|error| {
                    self.diagnostic.fault = Some(error.0.clone());
                    self.diagnostic.message = self.protocol.refused().map(str::to_owned);
                })?;
                assert!(batch.events.len() + batch.auth.len() <= bytes.len());
                assert!(batch.outbound.len() <= batch.events.len());
                self.events.extend(batch.events);
                self.auth.extend(batch.auth);
                self.outbound.extend(batch.outbound);
                self.flush_outbound().await?;
                assert!(self.outbound.is_empty());
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
