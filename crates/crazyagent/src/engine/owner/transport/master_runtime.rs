use super::{EngineError, OwnerRuntimeHandle, failure};
use crate::adapters::codex::{AuthAccount, AuthControl};
use crate::adapters::events::{EventKind, ObservedEvent, Provenance, TurnOutcome};
use crate::adapters::{NativeAction, NativeHarness};
use crate::config::ModelAuthentication;
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::claude_host::ClaudeHost;
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

pub(super) struct MasterHostLaunch {
    pub(super) command: Command,
    pub(super) sandbox: InstalledSandbox,
    pub(super) artifact_sha256: String,
    pub(super) runtime_ref: RequestId,
    pub(super) capture_ref: RequestId,
    pub(super) capture: File,
    pub(super) folder: super::master_host_launch::MasterRuntimeFolder,
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

impl OwnerRuntimeHandle {
    async fn master_runtime_command(
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

    pub(super) async fn acquire_master_profile(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
    ) -> Result<NativeProfileUse, EngineError> {
        match self
            .master_runtime_command(MasterRuntimeCommand::AcquireProfile {
                host: self.identity(),
                project_id: prepared.session.project_id.clone(),
                session_id: prepared.session.session_id.clone(),
                turn_id: turn_id.clone(),
                attempt_id: prepared.attempt.request.id.clone(),
            })
            .await?
        {
            MasterRuntimeResult::Profile(profile) => Ok(*profile),
            MasterRuntimeResult::Turn(_) => Err(failure("master_runtime_profile_not_acquired")),
            _ => Err(failure("master_runtime_result")),
        }
    }

    pub(super) async fn observe_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        observation: MasterRuntimeObservation,
    ) -> Result<MasterTurnView, EngineError> {
        match self
            .master_runtime_command(MasterRuntimeCommand::Observe {
                host: self.identity(),
                project_id: prepared.session.project_id.clone(),
                session_id: prepared.session.session_id.clone(),
                turn_id: turn_id.clone(),
                attempt_id: prepared.attempt.request.id.clone(),
                observation,
            })
            .await?
        {
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

    /// Drives one committed master turn and, when the dispatch fails with a known outcome,
    /// settles the turn failed with the reason so the owner sees it and can retry.
    /// Inputs: the turn's project, session and id. Output: the turn view, or the dispatch
    /// error after the turn was settled; an unknown outcome is returned for the caller to
    /// drive the same turn again.
    pub async fn drive_master_turn(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        let result = self.drive_turn(project_id, session_id, turn_id).await;
        if let Err(cause) = &result
            && cause.0 != super::master_dispatch::UNKNOWN_OUTCOME
        {
            let settled = self
                .fail_master_turn(project_id, session_id, turn_id, cause)
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
        let prepared = match self
            .prepare_master_turn(project_id, session_id, turn_id)
            .await?
        {
            MasterRuntimeResult::Prepared(prepared) => prepared,
            MasterRuntimeResult::Turn(turn) => return Ok(*turn),
            MasterRuntimeResult::Profile(_) => return Err(failure("master_runtime_result")),
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
        let profile = self.acquire_master_profile(prepared, turn_id).await?;
        let launch = self.prepare_master_host(prepared, &profile)?;
        let _folder = launch.folder;
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
        active.host.close().await.map_err(|error| {
            EngineError(format!("master_runtime_cleanup_uncertain:{}", error.0))
        })?;
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
            match host.observe_account().await? {
                AuthControl::AccountRead {
                    account: Some(AuthAccount::Chatgpt { .. }),
                    ..
                } => {}
                _ => return Err(failure("gateway_invalid_authentication")),
            }
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
        let mut text = String::new();
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
                EventKind::AssistantDelta { text: delta, .. }
                | EventKind::AssistantText { text: delta, .. } => {
                    if text
                        .len()
                        .checked_add(delta.len())
                        .is_none_or(|length| length > REPLY_BYTES_MAX)
                    {
                        return Err(failure("codex_host_output_limit"));
                    }
                    text.push_str(&delta);
                    assert!(text.len() <= REPLY_BYTES_MAX);
                    assert!(active.capture.sequence > 0);
                }
                EventKind::TurnFinished {
                    outcome: TurnOutcome::Success,
                    ..
                } => {
                    if text.trim().is_empty() || active.runtime.native_session_id.is_none() {
                        return Err(failure("master_capture_incomplete"));
                    }
                    active.capture.finish()?;
                    return Ok(MasterRuntimeObservation::Response {
                        reply: MasterReply {
                            text,
                            provenance: MasterReplyProvenance {
                                attempt_id: prepared.attempt.request.id.clone(),
                                runtime_ref: active.runtime.runtime_ref.clone(),
                                capture_ref: active.capture_ref.clone(),
                                capture_sequence: active.capture.sequence,
                            },
                        },
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
                    return Err(failure(match code.as_str() {
                        "authentication_failed" => "gateway_invalid_authentication",
                        "rate_limit" => "gateway_rate_limited",
                        "billing_error" => "gateway_quota_exhausted",
                        _ => "codex_host_turn_failed",
                    }));
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

fn cleanup_uncertain(error: &EngineError) -> bool {
    error.0.starts_with("master_runtime_cleanup_uncertain:")
}

fn reason_for_error(error: &EngineError) -> MasterReason {
    match error.0.as_str() {
        "native_profile_in_use" => MasterReason::ProfileBusy,
        "gateway_health_unknown" => MasterReason::ReadinessUnknown,
        "gateway_invalid_authentication" => MasterReason::InvalidAuthentication,
        "gateway_connection_changed_or_denied" => MasterReason::ConnectionChanged,
        "gateway_quota_unknown" => MasterReason::QuotaUnknown,
        "gateway_quota_exhausted" => MasterReason::QuotaExhausted,
        "gateway_rate_limited" => MasterReason::RateLimited,
        "gateway_cooldown" => MasterReason::CooldownActive,
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
