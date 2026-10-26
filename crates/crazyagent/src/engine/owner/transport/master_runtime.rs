use super::{EngineError, OwnerRuntimeHandle, failure};
use crate::adapters::codex::{AuthAccount, AuthControl};
use crate::adapters::events::{EventKind, ObservedEvent, Provenance, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::agents::{Hold, Holder};
use crate::config::ModelAuthentication;
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::claude_host::ClaudeHost;
use crate::engine::claude_host::diagnostic::StreamDiagnostic;
use crate::engine::codex_host::CodexHost;
use crate::master_session::{
    MasterBackend, MasterReason, MasterReply, MasterReplyProvenance, MasterRuntimeView,
    MasterTurnPhase, MasterTurnView,
};
use crate::protocol::{ProjectId, RequestId, SessionId};
use crate::sandbox::InstalledSandbox;
use crate::storage::{
    Job, MasterRuntimeCommand, MasterRuntimeObservation, MasterRuntimeResult, PreparedMasterTurn,
};
use std::fs::File;
#[cfg(unix)]
use std::io::Seek;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::time::Duration;

const REPLY_BYTES_MAX: usize = 8192;
#[cfg(unix)]
const CAPTURE_BYTES_MAX: u64 = 8 * 1024 * 1024;
const INSPECT_INTERVAL: Duration = Duration::from_millis(100);
/// SQLite's refusal when another engine connection held the database write lock past the
/// storage wait; the write was rolled back, so the turn is driven again (LB-28).
const STORAGE_BUSY: &str = "database is locked";
const BUSY_ATTEMPTS: u32 = 5;
const BUSY_PAUSE: Duration = Duration::from_millis(250);

pub(super) struct MasterHostLaunch {
    pub(super) command: Command,
    pub(super) sandbox: InstalledSandbox,
    pub(super) home: std::path::PathBuf,
    pub(super) credential: crate::engine::bootstrap::AgentCredential,
    pub(super) artifact_sha256: String,
    pub(super) runtime_ref: RequestId,
    pub(super) capture_ref: RequestId,
    pub(super) capture: File,
    pub(super) folder: super::master_host_launch::MasterRuntimeFolder,
    pub(super) tool_record: Option<std::path::PathBuf>,
}

pub(super) struct Capture {
    file: File,
    bytes: u64,
    sequence: u64,
    limit: u64,
}

enum MasterHost {
    Codex(Box<CodexHost>),
    Claude(Box<ClaudeHost>),
}

struct ActiveMaster {
    host: MasterHost,
    capture: Capture,
    runtime: MasterRuntimeView,
    capture_ref: RequestId,
    provenance: Provenance,
}

impl MasterHost {
    async fn start_turn(&mut self, turn_id: RequestId, text: &str) -> Result<(), EngineError> {
        match self {
            Self::Codex(host) => host.start_turn(turn_id, text).await,
            Self::Claude(host) => host.start_turn(turn_id, text).await,
        }
    }

    async fn next_event(&mut self) -> Result<ObservedEvent, EngineError> {
        match self {
            Self::Codex(host) => host.next_event().await,
            Self::Claude(host) => host.next_event().await,
        }
    }

    async fn cancel(&mut self) -> Result<(), EngineError> {
        match self {
            Self::Codex(host) => host.cancel().await,
            Self::Claude(host) => host.cancel().await,
        }
    }

    async fn close(&mut self) -> Result<(), EngineError> {
        match self {
            Self::Codex(host) => host.close().await,
            Self::Claude(host) => host.close().await,
        }
    }

    fn process_group(&self) -> Option<u32> {
        match self {
            Self::Codex(host) => Some(host.process_group()),
            Self::Claude(host) => host.process_group(),
        }
    }

    /// What the host kept about its harness stream for the engine log. Input: the host.
    /// Output: the diagnostic, or none before a Claude Code host retired its stream.
    fn diagnostic(&self) -> Option<StreamDiagnostic> {
        match self {
            Self::Codex(host) => Some(host.diagnostic()),
            Self::Claude(host) => host.diagnostic(),
        }
    }
}

impl Capture {
    /// Refuses a capture file where its Unix owner and 0600 mode cannot be checked.
    /// Takes the file, dropped, and the limit, unused. Returns `master_capture_unavailable`.
    #[cfg(not(unix))]
    pub(super) fn new(_file: File, _limit: u64) -> Result<Self, EngineError> {
        Err(failure("master_capture_unavailable"))
    }

    #[cfg(unix)]
    pub(super) fn new(mut file: File, limit: u64) -> Result<Self, EngineError> {
        assert!(limit > 0);
        let metadata = file
            .metadata()
            .map_err(|_| failure("master_capture_unavailable"))?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() != 0
            || file
                .stream_position()
                .map_err(|_| failure("master_capture_unavailable"))?
                != 0
        {
            return Err(failure("master_capture_identity"));
        }
        let capture = Self {
            file,
            bytes: 0,
            sequence: 0,
            limit: limit.min(CAPTURE_BYTES_MAX),
        };
        assert_eq!(capture.bytes, 0);
        assert_eq!(capture.sequence, 0);
        Ok(capture)
    }

    pub(super) fn record<T: serde::Serialize>(&mut self, event: &T) -> Result<(), EngineError> {
        assert!(self.bytes <= self.limit);
        assert_eq!(self.sequence == 0, self.bytes == 0);
        let bytes = serde_json::to_vec(event).map_err(|_| failure("master_capture_encoding"))?;
        let length = u64::try_from(bytes.len()).map_err(|_| failure("codex_host_output_limit"))?;
        let next = self
            .bytes
            .checked_add(length)
            .and_then(|length| length.checked_add(1))
            .filter(|length| *length <= self.limit)
            .ok_or_else(|| failure("codex_host_output_limit"))?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| failure("codex_host_output_limit"))?;
        if self
            .file
            .metadata()
            .map_err(|_| failure("master_capture_unavailable"))?
            .len()
            != self.bytes
        {
            return Err(failure("master_capture_identity"));
        }
        self.file
            .write_all(&bytes)
            .and_then(|()| self.file.write_all(b"\n"))
            .map_err(|_| failure("master_capture_incomplete"))?;
        self.bytes = next;
        self.sequence = sequence;
        assert!(self.sequence > 0);
        assert!(self.bytes <= self.limit);
        Ok(())
    }

    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) fn finish(&self) -> Result<(), EngineError> {
        assert!(self.sequence > 0);
        assert!(self.bytes > 0);
        self.file
            .sync_all()
            .map_err(|_| failure("master_capture_incomplete"))
    }
}

#[cfg(all(test, unix))]
mod routing_tests;

/// Runs one master turn step again while another engine connection holds the database
/// write lock past the storage wait, which rolls the step's write back (LB-28). Input: the
/// step. Output: its result, or `storage_busy` when the lock never freed.
async fn when_storage_frees<T, F, Fut>(mut step: F) -> Result<T, EngineError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, EngineError>>,
{
    let mut attempt = 1;
    loop {
        assert!(attempt <= BUSY_ATTEMPTS);
        match step().await {
            Err(error) if error.0 == STORAGE_BUSY && attempt < BUSY_ATTEMPTS => {
                tokio::time::sleep(BUSY_PAUSE * attempt).await;
                attempt += 1;
            }
            Err(error) if error.0 == STORAGE_BUSY => return Err(failure("storage_busy")),
            result => return result,
        }
    }
}

impl OwnerRuntimeHandle {
    /// Runs one master runtime command in storage. A first preparation that reserves the
    /// conversation worktree checks it out on a blocking thread without the owner barrier,
    /// so owner reads and the database keep serving, then stores it and prepares again
    /// (LB-28). Input: the command. Output: the storage result, never a checkout.
    async fn master_runtime_command(
        &self,
        command: MasterRuntimeCommand,
    ) -> Result<MasterRuntimeResult, EngineError> {
        let checkout = match self.master_runtime_job(command).await? {
            MasterRuntimeResult::Checkout(checkout) => checkout,
            result => return Ok(result),
        };
        let checked = tokio::task::spawn_blocking(move || checkout.materialize())
            .await
            .map_err(|_| failure("master_worktree_checkout_interrupted"))?;
        match self.master_runtime_job(checked).await? {
            MasterRuntimeResult::Checkout(_) => Err(failure("master_worktree_checkout_repeated")),
            result => Ok(result),
        }
    }

    /// Sends one master runtime command to storage under the owner barrier. Input: the
    /// command. Output: the storage result.
    async fn master_runtime_job(
        &self,
        command: MasterRuntimeCommand,
    ) -> Result<MasterRuntimeResult, EngineError> {
        let _barrier = tokio::time::timeout(self.engine.timeout, self.engine.owner_egress.write())
            .await
            .map_err(|_| failure("master_runtime_not_submitted"))?;
        self.engine
            .owner_job(|reply| Job::MasterRuntime(Box::new(command), reply))
            .await
    }

    async fn route_master_turn(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        match self
            .master_runtime_command(MasterRuntimeCommand::Route {
                host: self.identity(),
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            })
            .await?
        {
            MasterRuntimeResult::Turn(turn) => Ok(*turn),
            _ => Err(failure("master_runtime_result")),
        }
    }

    pub(in crate::engine) async fn prepare_master_turn(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<MasterRuntimeResult, EngineError> {
        self.master_runtime_command(MasterRuntimeCommand::Prepare {
            host: self.identity(),
            project_id: project_id.clone(),
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
        })
        .await
    }

    pub(super) async fn inspect_master_turn(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        match self
            .master_runtime_command(MasterRuntimeCommand::Inspect {
                host: self.identity(),
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            })
            .await?
        {
            MasterRuntimeResult::Turn(turn) => Ok(*turn),
            _ => Err(failure("master_runtime_result")),
        }
    }

    /// Takes the profile of a prepared master turn and names its conversation in the agent
    /// register as a holder of the turn's account until the returned hold is dropped, which
    /// the caller keeps until the turn's host has closed. Takes the prepared turn and its
    /// id. Returns the profile and its hold, none without a register, or the refusal.
    pub(super) async fn acquire_master_profile(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
    ) -> Result<(NativeProfileUse, Option<Hold>), EngineError> {
        let profile = match self
            .master_runtime_command(MasterRuntimeCommand::AcquireProfile {
                host: self.identity(),
                project_id: prepared.session.project_id.clone(),
                session_id: prepared.session.session_id.clone(),
                turn_id: turn_id.clone(),
                attempt_id: prepared.attempt.request.id.clone(),
            })
            .await?
        {
            MasterRuntimeResult::Profile(profile) => *profile,
            MasterRuntimeResult::Turn(_) => {
                return Err(failure("master_runtime_profile_not_acquired"));
            }
            _ => return Err(failure("master_runtime_result")),
        };
        let hold = self.agents().ok().map(|agents| {
            agents.hold(
                prepared.profile.binding().connection.id.clone(),
                Holder::Master {
                    project: prepared.session.project_id.clone(),
                    session: prepared.session.session_id.clone(),
                },
            )
        });
        Ok((profile, hold))
    }

    /// Records the process group a master turn's harness started in, so a later engine run
    /// can tell whether it may still write in the conversation worktree (LB-23). Takes the
    /// prepared turn, its id and the group. Returns success, or the refusal ending the turn.
    pub(super) async fn record_master_child(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        group: Option<u32>,
    ) -> Result<(), EngineError> {
        let group = group
            .and_then(|group| i32::try_from(group).ok())
            .filter(|group| *group > 1)
            .ok_or_else(|| failure("master_runtime_group_unknown"))?;
        match self
            .master_runtime_command(MasterRuntimeCommand::RecordChild {
                host: self.identity(),
                project_id: prepared.session.project_id.clone(),
                session_id: prepared.session.session_id.clone(),
                turn_id: turn_id.clone(),
                group,
            })
            .await?
        {
            MasterRuntimeResult::Turn(_) => Ok(()),
            _ => Err(failure("master_runtime_result")),
        }
    }

    /// Stores what a running master turn did, writing the same observation again while
    /// another engine connection holds the database, so a reply already in hand is never
    /// lost to a busy write (CR2-22). Inputs: the prepared turn, its id and the observation.
    /// Output: the stored turn, or `storage_busy` when the lock never freed.
    pub(super) async fn observe_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        observation: MasterRuntimeObservation,
    ) -> Result<MasterTurnView, EngineError> {
        let observed = when_storage_frees(|| {
            self.master_runtime_command(MasterRuntimeCommand::Observe {
                host: self.identity(),
                project_id: prepared.session.project_id.clone(),
                session_id: prepared.session.session_id.clone(),
                turn_id: turn_id.clone(),
                attempt_id: prepared.attempt.request.id.clone(),
                observation: observation.clone(),
            })
        })
        .await?;
        match observed {
            MasterRuntimeResult::Turn(turn) => Ok(*turn),
            _ => Err(failure("master_runtime_result")),
        }
    }

    /// Settles a master turn whose dispatch failed before it ran, so it never stays queued.
    /// Inputs: the turn's project, session and id, and the dispatch error, which names the
    /// reason the owner sees. Output: the turn as stored: blocked (never admitted) or failed
    /// (admitted) with the reason when it was the session's queued or prepared current
    /// turn, else unchanged.
    async fn fail_master_turn(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
        cause: &EngineError,
    ) -> Result<MasterTurnView, EngineError> {
        match self
            .master_runtime_command(MasterRuntimeCommand::Fail {
                host: self.identity(),
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                reason: reason_for_error(cause),
                cause: cause.0.clone(),
            })
            .await?
        {
            MasterRuntimeResult::Turn(turn) => Ok(*turn),
            _ => Err(failure("master_runtime_result")),
        }
    }

    /// Drives one committed master turn, again after a write another engine connection kept
    /// locked, and when the dispatch fails with a known outcome settles the turn failed with
    /// the reason so the owner sees it and can retry. Inputs: the turn's project, session and
    /// id. Output: the turn view, or the dispatch error after the turn was settled; an
    /// unknown outcome is returned for the caller to drive the same turn again.
    pub async fn drive_master_turn(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        let result = when_storage_frees(|| self.drive_turn(project_id, session_id, turn_id)).await;
        if let Err(cause) = &result
            && cause.0 != super::master_dispatch::UNKNOWN_OUTCOME
        {
            let settled = when_storage_frees(|| {
                self.fail_master_turn(project_id, session_id, turn_id, cause)
            })
            .await?;
            assert_eq!(&settled.id, turn_id);
            assert_eq!(&settled.session_id, session_id);
        }
        result
    }

    async fn drive_turn(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        let routed = self
            .route_master_turn(project_id, session_id, turn_id)
            .await?;
        if !matches!(
            routed.phase,
            MasterTurnPhase::Queued | MasterTurnPhase::Prepared
        ) {
            return Ok(routed);
        }
        if routed.phase == MasterTurnPhase::Queued {
            self.reprove_sign_ins(Some(session_id.clone())).await;
        }
        let prepared = match self
            .prepare_master_turn(project_id, session_id, turn_id)
            .await?
        {
            MasterRuntimeResult::Prepared(prepared) => prepared,
            MasterRuntimeResult::Turn(turn) => return Ok(*turn),
            MasterRuntimeResult::Profile(_) | MasterRuntimeResult::Checkout(_) => {
                return Err(failure("master_runtime_result"));
            }
            #[cfg(all(test, unix))]
            MasterRuntimeResult::ConfiguredProfile(_) => {
                return Err(failure("master_runtime_result"));
            }
        };
        if prepared.session.project_id != *project_id
            || prepared.session.session_id != *session_id
            || prepared.session.current_turn.as_ref() != Some(turn_id)
            || prepared.attempt.request.project_id != *project_id
            || !prepared.attempt_on_current_turn()
            || prepared.attempt.connection != prepared.profile.binding().connection
        {
            return Err(failure("master_runtime_binding"));
        }
        if prepared.session.selection.backend == MasterBackend::Rcli {
            return self.launch_rcli_master_turn(&prepared, turn_id).await;
        }
        let result = self.launch_master_turn(&prepared, turn_id).await;
        match result {
            Ok(turn) => Ok(turn),
            Err(error) if error.0 == "master_runtime_profile_not_acquired" => {
                self.inspect_master_turn(project_id, session_id, turn_id)
                    .await
            }
            Err(error) => {
                let observation = if cleanup_uncertain(&error) {
                    MasterRuntimeObservation::Disconnected
                } else {
                    MasterRuntimeObservation::Failed {
                        reason: reason_for_error(&error),
                        cause: error.0.clone(),
                    }
                };
                self.observe_master_turn(&prepared, turn_id, observation)
                    .await?;
                Err(error)
            }
        }
    }

    async fn launch_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        let harness = match prepared.session.selection.backend {
            MasterBackend::Codex => NativeHarness::Codex,
            MasterBackend::Claude => NativeHarness::Claude,
            MasterBackend::Rcli => return Err(failure("master_runtime_backend_unsupported")),
        };
        let profile_ref = match &prepared.profile.binding().connection.auth {
            ModelAuthentication::NativeManaged { profile_ref } => profile_ref.clone(),
            _ => return Err(failure("gateway_invalid_authentication")),
        };
        let (profile, _hold) = self.acquire_master_profile(prepared, turn_id).await?;
        let launch = self.prepare_master_host(prepared, &profile)?;
        let _folder = launch.folder;
        let _credential = launch.credential;
        let tool_record = launch.tool_record;
        if !crate::engine::owner::is_hex_id(&launch.artifact_sha256)
            || launch.command.get_current_dir() != Some(prepared.cwd.as_path())
        {
            return Err(failure("master_runtime_binding"));
        }
        let capture = Capture::new(
            launch.capture,
            prepared.attempt.request.route.resources.output_bytes,
        )?;
        let runtime = MasterRuntimeView {
            runtime_ref: launch.runtime_ref,
            artifact_sha256: launch.artifact_sha256,
            native_session_id: None,
        };
        let action = NativeAction::Launch {
            session_id: prepared.session.session_id.clone(),
            harness: harness.clone(),
            profile_ref: profile_ref.clone(),
            worktree_lease: prepared.worktree_lease.clone(),
        };
        let account_ref = prepared.profile.binding().connection.account_ref.clone();
        let resources = prepared.attempt.request.route.resources.clone();
        let host = if harness == NativeHarness::Claude {
            MasterHost::Claude(Box::new(
                ClaudeHost::spawn(
                    launch.command,
                    launch.sandbox,
                    profile,
                    launch.home,
                    action,
                    account_ref,
                    resources,
                )
                .await?,
            ))
        } else {
            MasterHost::Codex(Box::new(
                CodexHost::spawn(
                    launch.command,
                    launch.sandbox,
                    profile,
                    launch.home,
                    action,
                    account_ref,
                    resources,
                    &prepared.instructions,
                )
                .await?,
            ))
        };
        let mut active = ActiveMaster {
            host,
            capture,
            runtime,
            capture_ref: launch.capture_ref,
            provenance: Provenance {
                session_id: prepared.session.session_id.clone(),
                harness,
                account_ref: prepared.profile.binding().connection.account_ref.clone(),
                profile_ref,
                worktree_lease: prepared.worktree_lease.clone(),
            },
        };
        let observed = self
            .collect_master_turn(prepared, turn_id, &mut active)
            .await;
        if !matches!(&observed, Ok(MasterRuntimeObservation::Response { .. })) {
            let _ = active.host.cancel().await;
        }
        let closed = active.host.close().await;
        if let Some(diagnostic) = active.host.diagnostic()
            && (closed.is_err() || observed.is_err() || diagnostic.late_lines > 0)
            && self.authority.root.verify().is_ok()
            && let Some(root) = self.authority.root.path.parent()
        {
            let error = closed.as_ref().err().or(observed.as_ref().err());
            let _ = keep_stream_diagnostic(
                root,
                &prepared.session.session_id,
                turn_id,
                &active.runtime,
                &diagnostic,
                error.map(|error| error.0.as_str()),
            );
        }
        closed.map_err(|error| {
            EngineError(format!("master_runtime_cleanup_uncertain:{}", error.0))
        })?;
        let observed = match (observed, tool_record) {
            (Ok(MasterRuntimeObservation::Response { reply, .. }), Some(record)) => {
                Ok(MasterRuntimeObservation::Response {
                    reply,
                    delegations: crate::team::tool::take(&record),
                })
            }
            (observed, _) => observed,
        };
        match observed {
            Ok(observation) => {
                self.observe_master_turn(prepared, turn_id, observation)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn collect_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        active: &mut ActiveMaster,
    ) -> Result<MasterRuntimeObservation, EngineError> {
        self.record_master_child(prepared, turn_id, active.host.process_group())
            .await?;
        self.observe_master_turn(
            prepared,
            turn_id,
            MasterRuntimeObservation::Running {
                runtime: active.runtime.clone(),
            },
        )
        .await?;
        if self
            .inspect_master_turn(
                &prepared.session.project_id,
                &prepared.session.session_id,
                turn_id,
            )
            .await?
            .phase
            == MasterTurnPhase::Cancelled
        {
            return Ok(MasterRuntimeObservation::Cancelled);
        }
        if let MasterHost::Codex(host) = &mut active.host {
            account_failure(&host.observe_account().await?)?;
        }
        if self
            .inspect_master_turn(
                &prepared.session.project_id,
                &prepared.session.session_id,
                turn_id,
            )
            .await?
            .phase
            == MasterTurnPhase::Cancelled
        {
            return Ok(MasterRuntimeObservation::Cancelled);
        }
        active
            .host
            .start_turn(turn_id.clone(), &prepared.text)
            .await?;
        let mut reply = ReplyText::default();
        let mut inspect = tokio::time::interval(INSPECT_INTERVAL);
        inspect.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let event = tokio::select! {
                biased;
                _ = inspect.tick() => {
                    let turn = self.inspect_master_turn(
                        &prepared.session.project_id,
                        &prepared.session.session_id,
                        turn_id,
                    ).await?;
                    if turn.phase == MasterTurnPhase::Cancelled {
                        return Ok(MasterRuntimeObservation::Cancelled);
                    }
                    if !matches!(turn.phase, MasterTurnPhase::Prepared | MasterTurnPhase::Running) {
                        return Err(failure("master_runtime_state_changed"));
                    }
                    continue;
                }
                event = active.host.next_event() => event?,
            };
            validate_event(&active.provenance, turn_id, &event)?;
            active.capture.record(&event)?;
            if let Some(native) = event
                .native_session_id
                .as_ref()
                .or(event.native_thread_id.as_ref())
            {
                if active
                    .runtime
                    .native_session_id
                    .as_ref()
                    .is_some_and(|previous| previous != native)
                {
                    return Err(failure("master_capture_identity"));
                }
                if active.runtime.native_session_id.is_none() {
                    active.runtime.native_session_id = Some(native.clone());
                    self.observe_master_turn(
                        prepared,
                        turn_id,
                        MasterRuntimeObservation::Running {
                            runtime: active.runtime.clone(),
                        },
                    )
                    .await?;
                }
            }
            match event.kind {
                EventKind::AssistantText {
                    parent_tool_use_id: Some(_),
                    ..
                } if prepared.tools => {}
                EventKind::ToolUse {
                    parent_tool_use_id, ..
                } if prepared.tools => {
                    if parent_tool_use_id.is_none() {
                        reply.tool_step();
                    }
                }
                EventKind::AssistantDelta {
                    item_id,
                    text: delta,
                } => {
                    reply.push(Some(&item_id), &delta, prepared.tools)?;
                    assert!(active.capture.sequence > 0);
                }
                EventKind::AssistantText { text: delta, .. } => {
                    reply.push(None, &delta, prepared.tools)?;
                    assert!(active.capture.sequence > 0);
                }
                EventKind::TurnFinished {
                    outcome: TurnOutcome::Success,
                    ..
                } => {
                    if reply.text.trim().is_empty() || active.runtime.native_session_id.is_none() {
                        return Err(failure("master_capture_incomplete"));
                    }
                    active.capture.finish()?;
                    return Ok(MasterRuntimeObservation::Response {
                        reply: MasterReply {
                            text: reply.text,
                            provenance: MasterReplyProvenance {
                                attempt_id: prepared.attempt.request.id.clone(),
                                runtime_ref: active.runtime.runtime_ref.clone(),
                                capture_ref: active.capture_ref.clone(),
                                capture_sequence: active.capture.sequence,
                            },
                        },
                        delegations: Vec::new(),
                    });
                }
                EventKind::TurnFinished { .. }
                | EventKind::Disconnected
                | EventKind::ProtocolFailure { .. } => {
                    return Err(failure("master_capture_incomplete"));
                }
                EventKind::ApprovalRequired { .. }
                | EventKind::UnsupportedRequest { .. }
                | EventKind::Unsupported { .. } => {}
                EventKind::AssistantError { code } => {
                    return Err(failure(assistant_failure(&code)));
                }
                EventKind::RequestFailed { .. }
                | EventKind::TurnError {
                    will_retry: false, ..
                } => return Err(failure("codex_host_turn_failed")),
                EventKind::ToolUse { .. } => return Err(failure("codex_host_approval_required")),
                _ => {}
            }
        }
    }
}

/// Appends a master turn's stream diagnostic to the engine log
/// `logs/master-stream.log` (0600, rotated; frame kinds and sizes, never stream content) so a
/// failed turn or one whose harness kept talking after its result can be explained later
/// without copying tool output or credentials into the log (CR2-24). Input: the engine
/// root, the conversation, turn, runtime, diagnostic and the turn's error code when it
/// failed. Output: an error code when the log could not be written safely.
fn keep_stream_diagnostic(
    root: &std::path::Path,
    session_id: &SessionId,
    turn_id: &RequestId,
    runtime: &MasterRuntimeView,
    diagnostic: &StreamDiagnostic,
    error: Option<&str>,
) -> Result<(), EngineError> {
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let entry = serde_json::json!({
        "at_ms": at_ms,
        "session_id": session_id.as_str(),
        "turn_id": turn_id.as_str(),
        "native_session_id": runtime.native_session_id,
        "artifact_sha256": runtime.artifact_sha256,
        "error": error,
        "fault": diagnostic.fault,
        "message": diagnostic.message,
        "late_lines": diagnostic.late_lines,
        "late": diagnostic.late,
        "frames": diagnostic.frames(),
    });
    crate::engine::claude_host::diagnostic::append(
        &crate::paths::Layout::new(root.to_path_buf())
            .map_err(|_| failure("master_stream_log_unavailable"))?
            .logs(),
        &entry,
    )
}

/// The reply a master turn is building: its text so far and, for a tools-on master, the
/// agent message that text belongs to. A tools-on master's reply is what it says after its
/// last tool step, so a new message or a top-level tool step starts the text over.
#[derive(Default)]
pub(super) struct ReplyText {
    pub(super) text: String,
    message: Option<String>,
}

impl ReplyText {
    /// Starts the reply over after a top-level tool step of a tools-on master.
    pub(super) fn tool_step(&mut self) {
        self.text.clear();
        self.message = None;
    }

    /// Adds streamed reply text. With tools on, text of a new agent message replaces what an
    /// earlier message said; with tools off every message adds to the reply, as before.
    /// Inputs: the agent message the text belongs to when the harness names one, the text and
    /// the tools switch. Output: Ok, or `codex_host_output_limit` past `REPLY_BYTES_MAX`.
    pub(super) fn push(
        &mut self,
        message: Option<&str>,
        part: &str,
        tools: bool,
    ) -> Result<(), EngineError> {
        if tools && message.is_some() && message != self.message.as_deref() {
            self.text.clear();
            self.message = message.map(str::to_owned);
        }
        if self
            .text
            .len()
            .checked_add(part.len())
            .is_none_or(|length| length > REPLY_BYTES_MAX)
        {
            return Err(failure("codex_host_output_limit"));
        }
        self.text.push_str(part);
        assert!(self.text.len() <= REPLY_BYTES_MAX);
        Ok(())
    }
}

fn validate_event(
    provenance: &Provenance,
    turn_id: &RequestId,
    event: &ObservedEvent,
) -> Result<(), EngineError> {
    if event.provenance != *provenance
        || event
            .request_id
            .as_ref()
            .is_some_and(|request| request != turn_id)
        || (matches!(
            &event.kind,
            EventKind::AssistantDelta { .. }
                | EventKind::AssistantText { .. }
                | EventKind::TurnFinished { .. }
        ) && event.request_id.as_ref() != Some(turn_id))
    {
        return Err(failure("master_capture_identity"));
    }
    Ok(())
}

/// Names the failure a harness error ends a master turn with: a provider's rejected sign-in,
/// or a sign-in the harness found missing itself, which is no word from the provider.
/// Input: the error code the harness reported. Output: the code the turn's reason reads.
fn assistant_failure(code: &str) -> &'static str {
    match code {
        "authentication_failed" => "gateway_invalid_authentication",
        "not_signed_in" => "master_sign_in_missing",
        "rate_limit" => "gateway_rate_limited",
        "billing_error" => "gateway_quota_exhausted",
        _ => "codex_host_turn_failed",
    }
}

/// Checks the account Codex reads locally before a master turn is a ChatGPT sign-in; no
/// account at all is a missing sign-in, not a provider rejection.
/// Input: Codex's account read. Output: success, or the failure the turn ends with.
pub(super) fn account_failure(read: &AuthControl) -> Result<(), EngineError> {
    match read {
        AuthControl::AccountRead {
            account: Some(AuthAccount::Chatgpt { .. }),
            ..
        } => Ok(()),
        AuthControl::AccountRead { account: None, .. } => Err(failure("master_sign_in_missing")),
        _ => Err(failure("gateway_invalid_authentication")),
    }
}

fn cleanup_uncertain(error: &EngineError) -> bool {
    error.0.starts_with("master_runtime_cleanup_uncertain:")
}

fn reason_for_error(error: &EngineError) -> MasterReason {
    match error.0.as_str() {
        "native_profile_in_use" => MasterReason::ProfileBusy,
        "gateway_health_unknown"
        | "master_sign_in_missing"
        | "native_profile_credential_unavailable" => MasterReason::ReadinessUnknown,
        "gateway_invalid_authentication"
        | "native_profile_credential_expiring"
        | "native_profile_sign_in_revoked" => MasterReason::InvalidAuthentication,
        "gateway_connection_changed_or_denied" => MasterReason::ConnectionChanged,
        "gateway_quota_unknown" => MasterReason::QuotaUnknown,
        "gateway_quota_exhausted" => MasterReason::QuotaExhausted,
        "gateway_rate_limited" => MasterReason::RateLimited,
        "gateway_cooldown" => MasterReason::CooldownActive,
        "native_profile_renewal_failed" => MasterReason::TransientFailure,
        "master_runtime_scope_revoked" | "owner_runtime_stale" => MasterReason::ScopeRevoked,
        "codex_host_output_limit" => MasterReason::OutputLimit,
        "codex_host_capture_incomplete"
        | "master_capture_incomplete"
        | "master_capture_identity"
        | "master_capture_unavailable"
        | "master_capture_encoding" => MasterReason::CaptureIncomplete,
        "codex_host_approval_required" => MasterReason::ApprovalRequired,
        "codex_host_cancelled" => MasterReason::Cancelled,
        "master_runtime_backend_unsupported" => MasterReason::UnsupportedCapability,
        "master_repository_missing" => MasterReason::RepositoryMissing,
        code if code.starts_with("repository_config_refused:") => {
            MasterReason::RepositoryConfigRefused
        }
        "native_profile_lock_unavailable"
        | "master_runtime_host_unavailable"
        | "rcli_profile_configuration_required" => MasterReason::RuntimeUnavailable,
        _ => MasterReason::Unknown,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::resolve_config;
    use crate::engine::EngineError;
    use crate::engine::bootstrap::EngineService;
    use crate::engine::owner::transport::OwnerRuntimeHandle;
    use crate::master_session::MasterReason;
    use crate::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_tools_on_master_reply_is_its_last_message_and_a_tools_off_reply_is_unchanged() {
        let mut reply = ReplyText::default();
        reply
            .push(Some("message-1"), "I will write ", true)
            .unwrap();
        reply.push(Some("message-1"), "the note.", true).unwrap();
        reply.push(Some("message-2"), "DO", true).unwrap();
        reply.push(Some("message-2"), "NE", true).unwrap();
        assert_eq!(reply.text, "DONE");
        reply.push(None, " more", true).unwrap();
        assert_eq!(reply.text, "DONE more");
        reply.tool_step();
        assert_eq!(reply.text, "");
        reply.push(None, "After the step.", true).unwrap();
        assert_eq!(reply.text, "After the step.");

        let mut reply = ReplyText::default();
        reply.push(Some("message-1"), "First. ", false).unwrap();
        reply.push(Some("message-2"), "Second.", false).unwrap();
        assert_eq!(reply.text, "First. Second.");

        let mut reply = ReplyText::default();
        let long = "x".repeat(REPLY_BYTES_MAX);
        reply.push(Some("message-1"), &long, true).unwrap();
        assert_eq!(
            reply.push(Some("message-1"), "y", true),
            Err(failure("codex_host_output_limit"))
        );
        reply.push(Some("message-2"), "z", true).unwrap();
        assert_eq!(reply.text, "z");
    }

    fn protocol_input() -> ObservedEvent {
        ObservedEvent {
            provenance: Provenance {
                session_id: SessionId::new("unit-session").unwrap(),
                harness: NativeHarness::Codex,
                account_ref: CredentialRef::new("unit-account").unwrap(),
                profile_ref: CredentialRef::new("unit-profile").unwrap(),
                worktree_lease: RequestId::new("unit-lease").unwrap(),
            },
            request_id: None,
            native_session_id: None,
            native_thread_id: None,
            native_turn_id: None,
            native_event_id: None,
            native_user_message_uuid: None,
            kind: EventKind::ProtocolInitialized,
        }
    }

    #[test]
    fn capture_records_actual_file_bytes_and_syncs_before_reply_provenance() {
        let file = tempfile::tempfile().unwrap();
        let readback = file.try_clone().unwrap();
        let mut capture = Capture::new(file, 8192).unwrap();
        let event = protocol_input();
        capture.record(&event).unwrap();
        capture.finish().unwrap();
        let mut expected = serde_json::to_vec(&event).unwrap();
        expected.push(b'\n');
        let mut bytes = vec![0; expected.len()];
        std::os::unix::fs::FileExt::read_exact_at(&readback, &mut bytes, 0).unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(capture.sequence, 1);
        assert_eq!(capture.bytes, expected.len() as u64);
    }

    #[test]
    fn capture_rejects_preexisting_data_and_public_permissions() {
        let mut existing = tempfile::tempfile().unwrap();
        existing.write_all(b"existing").unwrap();
        assert!(
            matches!(Capture::new(existing, 8192), Err(EngineError(code)) if code == "master_capture_identity")
        );
        let public = tempfile::tempfile().unwrap();
        public
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(
            matches!(Capture::new(public, 8192), Err(EngineError(code)) if code == "master_capture_identity")
        );
    }

    #[test]
    fn capture_rejects_over_limit_input_without_partial_file_or_sequence() {
        let file = tempfile::tempfile().unwrap();
        let readback = file.try_clone().unwrap();
        let mut capture = Capture::new(file, 1).unwrap();
        let error = capture.record(&protocol_input()).unwrap_err();
        assert_eq!(error.0, "codex_host_output_limit");
        assert_eq!(readback.metadata().unwrap().len(), 0);
        assert_eq!(capture.sequence, 0);
        assert_eq!(capture.bytes, 0);
    }

    #[test]
    #[should_panic(expected = "limit > 0")]
    fn capture_zero_budget_trips_internal_invariant() {
        let _ = Capture::new(tempfile::tempfile().unwrap(), 0);
    }

    #[test]
    #[should_panic(expected = "self.sequence > 0")]
    fn capture_without_an_observed_event_cannot_finish() {
        let capture = Capture::new(tempfile::tempfile().unwrap(), 8192).unwrap();
        let _ = capture.finish();
    }

    #[test]
    fn event_provenance_cannot_cross_a_session_account_or_local_turn() {
        let mut event = protocol_input();
        let expected = event.provenance.clone();
        let turn = RequestId::new("unit-turn").unwrap();
        validate_event(&expected, &turn, &event).unwrap();
        event.provenance.account_ref = CredentialRef::new("other-account").unwrap();
        assert_eq!(
            validate_event(&expected, &turn, &event).unwrap_err().0,
            "master_capture_identity"
        );
        event.provenance = expected.clone();
        event.request_id = Some(RequestId::new("other-turn").unwrap());
        assert_eq!(
            validate_event(&expected, &turn, &event).unwrap_err().0,
            "master_capture_identity"
        );
        event.request_id = None;
        event.provenance.session_id = SessionId::new("other-session").unwrap();
        assert_eq!(
            validate_event(&expected, &turn, &event).unwrap_err().0,
            "master_capture_identity"
        );
    }

    #[test]
    fn a_master_stream_diagnostic_names_the_turn_and_its_late_lines_in_the_engine_log() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::Builder::new()
            .prefix("crazyagent-stream-log")
            .tempdir()
            .unwrap();
        let mut diagnostic = StreamDiagnostic::default();
        diagnostic.keep(b"{\"type\":\"result\",\"subtype\":\"success\"}\n");
        diagnostic.late(b"{\"type\":\"system\",\"subtype\":\"task_notification\"}\n");
        diagnostic.fault = Some("claude_host_protocol_invalid_state".to_owned());
        let runtime = MasterRuntimeView {
            runtime_ref: RequestId::new("runtime-1").unwrap(),
            artifact_sha256: "a".repeat(64),
            native_session_id: Some("native-1".to_owned()),
        };
        keep_stream_diagnostic(
            root.path(),
            &SessionId::new("session-1").unwrap(),
            &RequestId::new("turn-1").unwrap(),
            &runtime,
            &diagnostic,
            Some("claude_host_protocol_invalid_state"),
        )
        .unwrap();
        let log = root.path().join("logs").join("master-stream.log");
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        let entry: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&log).unwrap().trim_end()).unwrap();
        assert_eq!(entry["session_id"], "session-1");
        assert_eq!(entry["turn_id"], "turn-1");
        assert_eq!(entry["native_session_id"], "native-1");
        assert_eq!(entry["artifact_sha256"], "a".repeat(64));
        assert_eq!(entry["error"], "claude_host_protocol_invalid_state");
        assert_eq!(entry["fault"], "claude_host_protocol_invalid_state");
        assert_eq!(entry["late_lines"], 1);
        assert_eq!(
            entry["late"],
            serde_json::json!(["system/task_notification"])
        );
        assert_eq!(entry["frames"], serde_json::json!(["result/success 37"]));
        assert!(entry["at_ms"].as_u64().is_some_and(|at| at > 0));
    }

    #[test]
    fn a_master_stream_diagnostic_logs_frame_kinds_and_sizes_and_never_stream_content() {
        let root = tempfile::Builder::new()
            .prefix("crazyagent-stream-log")
            .tempdir()
            .unwrap();
        let secrets = [
            "ghp_0123456789abcdefABCDEF0123456789abcd",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
            "sk-proj-AbC_12-x",
        ];
        let claude = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\
\"content\":\"GITHUB_TOKEN=x\\n{}\\t{}\\n{}\\n{}\"}}]}}}}\n",
            secrets[0], secrets[1], secrets[2], secrets[3]
        );
        let codex = format!(
            "{{\"method\":\"item/completed\",\"params\":{{\"item\":{{\
\"aggregatedOutput\":\"\\n{}\"}}}}}}\n",
            secrets[0]
        );
        let mut diagnostic = StreamDiagnostic::default();
        diagnostic.keep(claude.as_bytes());
        diagnostic.keep(codex.as_bytes());
        diagnostic.keep(b"{\"id\":7,\"result\":{}}\n{\"type\":\"assistant\"");
        let runtime = MasterRuntimeView {
            runtime_ref: RequestId::new("runtime-1").unwrap(),
            artifact_sha256: "a".repeat(64),
            native_session_id: None,
        };
        keep_stream_diagnostic(
            root.path(),
            &SessionId::new("session-1").unwrap(),
            &RequestId::new("turn-1").unwrap(),
            &runtime,
            &diagnostic,
            Some("claude_host_protocol_invalid_state"),
        )
        .unwrap();
        let text =
            std::fs::read_to_string(root.path().join("logs").join("master-stream.log")).unwrap();
        for secret in secrets {
            assert!(!text.contains(secret), "{text}");
        }
        assert!(!text.contains("GITHUB_TOKEN"), "{text}");
        let entry: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(
            entry["frames"],
            serde_json::json!([
                format!("user {}", claude.len() - 1),
                format!("item/completed {}", codex.len() - 1),
                "response 20",
                "unfinished 19"
            ])
        );
    }

    struct RuntimeFixture {
        root: tempfile::TempDir,
        service: EngineService,
        _shutdown: tokio::sync::watch::Sender<bool>,
    }

    impl RuntimeFixture {
        async fn start() -> Self {
            let root = tempfile::Builder::new()
                .prefix("ca-md-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let config = resolve_config(
                &root.path().join("global.toml"),
                &root.path().join("project.toml"),
            )
            .unwrap();
            let (shutdown, stopped) = tokio::sync::watch::channel(false);
            let service = EngineService::start(root.path(), "e.sock", &config, &stopped)
                .await
                .unwrap();
            Self {
                root,
                service,
                _shutdown: shutdown,
            }
        }

        fn counts(&self) -> (i64, i64) {
            let database = rusqlite::Connection::open_with_flags(
                self.root.path().join("engine.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let count = |table: &str| -> i64 {
                database
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap()
            };
            (count("master_turns"), count("model_gateway_attempts"))
        }
    }

    #[tokio::test]
    async fn master_prepare_missing_session_never_creates_an_attempt_or_turn() {
        let mut fixture = RuntimeFixture::start().await;
        let before = fixture.counts();
        let result = fixture
            .service
            .owner_runtime()
            .unwrap()
            .prepare_master_turn(
                &ProjectId::new("missing-project").unwrap(),
                &SessionId::new("missing-session").unwrap(),
                &RequestId::new("missing-turn").unwrap(),
            )
            .await;
        let Err(error) = result else {
            panic!("missing master session was admitted");
        };
        assert_eq!(error.0, "master_session_missing");
        assert_eq!(fixture.counts(), before);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn master_prepare_rejects_a_foreign_host_before_storage_writes() {
        let mut first = RuntimeFixture::start().await;
        let mut second = RuntimeFixture::start().await;
        let foreign = OwnerRuntimeHandle {
            engine: second.service.owner_runtime().unwrap().engine.clone(),
            authority: first.service.owner_runtime().unwrap().authority.clone(),
        };
        let before = second.counts();
        let result = foreign
            .prepare_master_turn(
                &ProjectId::new("missing-project").unwrap(),
                &SessionId::new("missing-session").unwrap(),
                &RequestId::new("missing-turn").unwrap(),
            )
            .await;
        let Err(error) = result else {
            panic!("foreign owner host was admitted");
        };
        assert_eq!(error.0, "owner_runtime_stale");
        assert_eq!(second.counts(), before);
        first.service.shutdown().await.unwrap();
        second.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn master_driver_rejects_unadmitted_input_before_native_host_launch() {
        let mut fixture = RuntimeFixture::start().await;
        let before = fixture.counts();
        let result = fixture
            .service
            .owner_runtime()
            .unwrap()
            .drive_master_turn(
                &ProjectId::new("missing-project").unwrap(),
                &SessionId::new("missing-session").unwrap(),
                &RequestId::new("missing-turn").unwrap(),
            )
            .await;
        let Err(error) = result else {
            panic!("driver launched an unadmitted master turn");
        };
        assert_eq!(error.0, "master_session_missing");
        assert_eq!(fixture.counts(), before);
        fixture.service.shutdown().await.unwrap();
    }

    #[test]
    fn profile_custody_conflicts_are_reported_as_profile_busy() {
        assert_eq!(
            reason_for_error(&EngineError("native_profile_in_use".into())),
            MasterReason::ProfileBusy
        );
        assert_eq!(
            reason_for_error(&EngineError("native_profile_lock_unavailable".into())),
            MasterReason::RuntimeUnavailable
        );
    }

    #[test]
    fn native_failure_reasons_preserve_authority_and_capture_failures() {
        for (code, expected) in [
            (
                "gateway_invalid_authentication",
                MasterReason::InvalidAuthentication,
            ),
            (
                "gateway_connection_changed_or_denied",
                MasterReason::ConnectionChanged,
            ),
            ("gateway_health_unknown", MasterReason::ReadinessUnknown),
            (
                "repository_config_refused:core_hookspath",
                MasterReason::RepositoryConfigRefused,
            ),
            ("gateway_quota_unknown", MasterReason::QuotaUnknown),
            ("gateway_quota_exhausted", MasterReason::QuotaExhausted),
            ("gateway_rate_limited", MasterReason::RateLimited),
            ("gateway_cooldown", MasterReason::CooldownActive),
            ("master_runtime_scope_revoked", MasterReason::ScopeRevoked),
            ("codex_host_output_limit", MasterReason::OutputLimit),
            (
                "codex_host_capture_incomplete",
                MasterReason::CaptureIncomplete,
            ),
            (
                "codex_host_approval_required",
                MasterReason::ApprovalRequired,
            ),
            ("codex_host_cancelled", MasterReason::Cancelled),
        ] {
            assert_eq!(
                reason_for_error(&EngineError(code.into())),
                expected,
                "{code}"
            );
        }
    }

    #[test]
    fn a_harness_with_no_sign_in_fails_the_turn_without_a_provider_rejection() {
        let read = |account| AuthControl::AccountRead {
            id: 1,
            account,
            requires_openai_auth: true,
        };
        let chatgpt = AuthAccount::Chatgpt {
            email: None,
            plan_type: "plus".into(),
        };
        assert!(account_failure(&read(Some(chatgpt))).is_ok());
        assert_eq!(assistant_failure("not_signed_in"), "master_sign_in_missing");
        for (gap, reason) in [
            (
                assistant_failure("not_signed_in").to_owned(),
                MasterReason::ReadinessUnknown,
            ),
            (
                account_failure(&read(None)).unwrap_err().0,
                MasterReason::ReadinessUnknown,
            ),
            (
                assistant_failure("authentication_failed").to_owned(),
                MasterReason::InvalidAuthentication,
            ),
            (
                account_failure(&read(Some(AuthAccount::ApiKey)))
                    .unwrap_err()
                    .0,
                MasterReason::InvalidAuthentication,
            ),
        ] {
            assert_eq!(reason_for_error(&EngineError(gap.clone())), reason, "{gap}");
        }
    }

    #[test]
    fn a_revoked_or_expiring_profile_sign_in_asks_the_owner_to_sign_in_again() {
        for code in [
            "native_profile_sign_in_revoked",
            "native_profile_credential_expiring",
        ] {
            assert_eq!(
                reason_for_error(&EngineError(code.into())),
                MasterReason::InvalidAuthentication,
                "{code}"
            );
        }
    }

    #[test]
    fn unknown_native_error_stays_unknown() {
        assert_eq!(
            reason_for_error(&EngineError("unclassified-native-host-error".into())),
            MasterReason::Unknown
        );
        assert_ne!(
            reason_for_error(&EngineError("unclassified-native-host-error".into())),
            MasterReason::Cancelled
        );
    }
}
