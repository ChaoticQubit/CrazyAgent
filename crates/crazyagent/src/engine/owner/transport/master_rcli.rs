#[cfg(test)]
#[path = "master_rcli_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "master_rcli_capture_tests.rs"]
mod capture_tests;
use super::master_runtime::Capture;
use super::{EngineError, OwnerRuntimeHandle, failure};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::engine::bootstrap::NativeProfileUsePurpose;
use crate::master_session::{
    MasterBackend, MasterReason, MasterReply, MasterReplyProvenance, MasterRuntimeView,
    MasterTurnPhase, MasterTurnView, fitted_reply,
};
use crate::protocol::RequestId;
use crate::rcli_host::RcliMasterStatus;
use crate::storage::{MasterRuntimeObservation, PreparedMasterTurn};
use std::time::Duration;
use tokio::time::Instant;

#[path = "master_rcli_custody.rs"]
mod custody;

use custody::RcliCustody;

const INSPECT_INTERVAL: Duration = Duration::from_millis(100);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

struct ActiveRcliMaster {
    custody: RcliCustody,
    capture: Capture,
    runtime: MasterRuntimeView,
    capture_ref: RequestId,
    generation: u64,
    deadline: Instant,
}

fn require_rcli_connection(connection: &ModelConnection) -> Result<(), EngineError> {
    if connection.harness.as_str() != "rcli"
        || !matches!(
            connection.protocol.as_str(),
            "openai-chat" | "openai-responses" | "anthropic-messages"
        )
        || !matches!(connection.endpoint, ModelEndpoint::Http { .. })
        || !matches!(
            connection.auth,
            ModelAuthentication::ApiKey { .. } | ModelAuthentication::None {}
        )
    {
        return Err(failure("master_runtime_backend_unsupported"));
    }
    assert_eq!(connection.harness.as_str(), "rcli");
    Ok(())
}

fn cleanup_uncertain(error: &EngineError) -> bool {
    error.0.starts_with("master_runtime_cleanup_uncertain:")
        || error.0.split(';').any(|part| {
            matches!(
                part,
                "rcli_master_cleanup_incomplete" | "rcli_master_wait_failed"
            )
        })
}

fn rcli_failure_observation(error: &EngineError) -> MasterRuntimeObservation {
    if cleanup_uncertain(error) {
        return MasterRuntimeObservation::Disconnected;
    }
    let reason = match error.0.as_str() {
        "rcli_master_output_limit" | "rcli_master_stderr_limit" | "codex_host_output_limit" => {
            MasterReason::OutputLimit
        }
        "rcli_master_capture_incomplete"
        | "rcli_master_empty_reply"
        | "rcli_master_message_order"
        | "rcli_master_turn_scope"
        | "rcli_master_truncated_event"
        | "master_capture_incomplete"
        | "master_capture_identity"
        | "master_capture_unavailable"
        | "master_capture_encoding" => MasterReason::CaptureIncomplete,
        "rcli_master_input_required" => MasterReason::ApprovalRequired,
        "rcli_master_turn_deadline" | "master_runtime_budget_exhausted" => {
            MasterReason::BudgetExhausted
        }
        "rcli_master_profile_changed"
        | "rcli_master_profile_mismatch"
        | "native_profile_binding_conflict"
        | "native_profile_identity_changed"
        | "gateway_connection_changed_or_denied" => MasterReason::ConnectionChanged,
        "owner_runtime_stale" | "master_runtime_scope_revoked" => MasterReason::ScopeRevoked,
        "master_runtime_backend_unsupported" => MasterReason::UnsupportedCapability,
        "native_profile_in_use" => MasterReason::ProfileBusy,
        "gateway_invalid_authentication" => MasterReason::InvalidAuthentication,
        "gateway_health_unknown" => MasterReason::ReadinessUnknown,
        "gateway_quota_unknown" => MasterReason::QuotaUnknown,
        "gateway_quota_exhausted" => MasterReason::QuotaExhausted,
        "gateway_rate_limited" => MasterReason::RateLimited,
        "gateway_cooldown" => MasterReason::CooldownActive,
        "rcli_master_startup_deadline"
        | "rcli_master_spawn_failed"
        | "rcli_master_artifact_changed"
        | "rcli_master_process_exited"
        | "rcli_master_control_rejected"
        | "rcli_master_execution_failed"
        | "rcli_master_closed"
        | "native_profile_lock_unavailable"
        | "master_runtime_host_unavailable" => MasterReason::RuntimeUnavailable,
        _ => MasterReason::Unknown,
    };
    MasterRuntimeObservation::Failed {
        reason,
        cause: error.0.clone(),
    }
}

impl OwnerRuntimeHandle {
    pub(super) async fn launch_rcli_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        match self.run_rcli_master_turn(prepared, turn_id).await {
            Ok(observation) => {
                self.observe_master_turn(prepared, turn_id, observation)
                    .await
            }
            Err(error) if error.0 == "master_runtime_profile_not_acquired" => {
                self.inspect_master_turn(
                    &prepared.session.project_id,
                    &prepared.session.session_id,
                    turn_id,
                )
                .await
            }
            Err(error) => {
                self.observe_master_turn(prepared, turn_id, rcli_failure_observation(&error))
                    .await?;
                Err(error)
            }
        }
    }

    async fn run_rcli_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
    ) -> Result<MasterRuntimeObservation, EngineError> {
        require_rcli_connection(&prepared.attempt.connection)?;
        if prepared.session.selection.backend != MasterBackend::Rcli
            || prepared.session.current_turn.as_ref() != Some(turn_id)
            || prepared.attempt.request.project_id != prepared.session.project_id
            || !prepared.attempt_on_current_turn()
            || prepared.attempt.connection != prepared.profile.binding().connection
        {
            return Err(failure("master_runtime_binding"));
        }
        let resources = &prepared.attempt.request.route.resources;
        resources
            .validate()
            .map_err(|_| failure("master_runtime_budget_exhausted"))?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(resources.wall_time_ms))
            .ok_or_else(|| failure("master_runtime_budget_exhausted"))?;
        let (profile, _hold) = self.acquire_master_profile(prepared, turn_id).await?;
        let current = self
            .inspect_master_turn(
                &prepared.session.project_id,
                &prepared.session.session_id,
                turn_id,
            )
            .await?;
        validate_turn(prepared, turn_id, &current)?;
        if current.phase == MasterTurnPhase::Cancelled {
            return Ok(MasterRuntimeObservation::Cancelled);
        }
        profile.revalidate(
            &self.identity(),
            prepared.profile.binding(),
            turn_id,
            current.ordinal,
            NativeProfileUsePurpose::Execution,
        )?;
        let launch = self.prepare_rcli_master_host(prepared, &profile)?;
        if launch.binding.project_id != prepared.session.project_id
            || launch.binding.session_id != prepared.session.session_id
            || launch.binding.connection != prepared.attempt.connection
            || launch.binding.target_profile_ref != prepared.session.selection.target_profile_ref
            || launch.home_dir != profile.profile_path()?
            || launch.cwd != prepared.cwd
            || !crate::engine::owner::is_hex_id(&launch.artifact_sha256)
        {
            return Err(failure("master_runtime_binding"));
        }
        launch.command().map_err(|error| EngineError(error.0))?;
        let (runtime_ref, capture_ref, file) = self.allocate_master_capture()?;
        let _folder =
            super::master_host_launch::MasterRuntimeFolder::new(&self.authority.root, &runtime_ref);
        let sandbox = super::master_host_launch::master_sandbox(
            &self.authority.root,
            &runtime_ref,
            &prepared.repository,
            &prepared.cwd,
            &launch.home_dir,
            &launch.executable,
            &prepared.attempt.connection.endpoint,
        )?;
        let capture = Capture::new(file, resources.output_bytes)?;
        let launching = self
            .inspect_master_turn(
                &prepared.session.project_id,
                &prepared.session.session_id,
                turn_id,
            )
            .await?;
        validate_turn(prepared, turn_id, &launching)?;
        if launching.phase == MasterTurnPhase::Cancelled {
            return Ok(MasterRuntimeObservation::Cancelled);
        }
        if launching.ordinal != current.ordinal {
            return Err(failure("master_runtime_scope_revoked"));
        }
        profile.revalidate(
            &self.identity(),
            prepared.profile.binding(),
            turn_id,
            current.ordinal,
            NativeProfileUsePurpose::Execution,
        )?;
        if Instant::now() >= deadline {
            return Err(failure("master_runtime_budget_exhausted"));
        }
        let runtime = MasterRuntimeView {
            runtime_ref,
            artifact_sha256: launch.artifact_sha256.clone(),
            native_session_id: None,
        };
        let custody = RcliCustody::launch(launch, sandbox, profile)?;
        let mut active = ActiveRcliMaster {
            custody,
            capture,
            runtime,
            capture_ref,
            generation: current.ordinal,
            deadline,
        };
        let observed = self
            .collect_rcli_master_turn(prepared, turn_id, &mut active)
            .await;
        let cancel = !matches!(&observed, Ok(MasterRuntimeObservation::Response { .. }));
        let retirement = active.custody.retire(cancel.then_some(turn_id));
        let capture = if active.capture.sequence() > 0 {
            active.capture.finish()
        } else {
            Ok(())
        };
        if let Err(retirement) = retirement {
            return Err(match observed {
                Err(error) => EngineError(format!("{};{}", retirement.0, error.0)),
                Ok(_) => retirement,
            });
        }
        capture?;
        if matches!(&observed, Ok(MasterRuntimeObservation::Response { .. }))
            && Instant::now() >= active.deadline
        {
            return Err(failure("master_runtime_budget_exhausted"));
        }
        observed
    }

    async fn collect_rcli_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        active: &mut ActiveRcliMaster,
    ) -> Result<MasterRuntimeObservation, EngineError> {
        self.record_master_child(prepared, turn_id, active.custody.process_group())
            .await?;
        let mut prompted = false;
        let mut inspect_at = Instant::now();
        loop {
            if Instant::now() >= active.deadline {
                return Err(failure("master_runtime_budget_exhausted"));
            }
            if Instant::now() >= inspect_at {
                let current = self.inspect_rcli_scope(prepared, turn_id, active).await?;
                if current.phase == MasterTurnPhase::Cancelled {
                    return Ok(MasterRuntimeObservation::Cancelled);
                }
                inspect_at = Instant::now() + INSPECT_INTERVAL;
            }
            let Some(observed) = active.custody.poll()? else {
                tokio::time::sleep_until((Instant::now() + POLL_INTERVAL).min(active.deadline))
                    .await;
                continue;
            };
            active.capture.record(&observed.event)?;
            if observed.sequence != active.capture.sequence() {
                return Err(failure("master_capture_identity"));
            }
            if observed.is_ready() {
                if prompted {
                    return Err(failure("rcli_master_message_order"));
                }
                let current = self.inspect_rcli_scope(prepared, turn_id, active).await?;
                if current.phase == MasterTurnPhase::Cancelled {
                    return Ok(MasterRuntimeObservation::Cancelled);
                }
                let running = self
                    .observe_master_turn(
                        prepared,
                        turn_id,
                        MasterRuntimeObservation::Running {
                            runtime: active.runtime.clone(),
                        },
                    )
                    .await?;
                if running.phase == MasterTurnPhase::Cancelled {
                    return Ok(MasterRuntimeObservation::Cancelled);
                }
                let current = self.inspect_rcli_scope(prepared, turn_id, active).await?;
                if current.phase == MasterTurnPhase::Cancelled {
                    return Ok(MasterRuntimeObservation::Cancelled);
                }
                active.custody.prompt(turn_id.clone(), &prepared.text)?;
                prompted = true;
            }
            let Some(completion) = observed.completion else {
                continue;
            };
            if !prompted || completion.turn_id != *turn_id {
                return Err(failure("rcli_master_turn_scope"));
            }
            let current = self.inspect_rcli_scope(prepared, turn_id, active).await?;
            if current.phase == MasterTurnPhase::Cancelled {
                return Ok(MasterRuntimeObservation::Cancelled);
            }
            match completion.status {
                RcliMasterStatus::Succeeded => {
                    let text = completion
                        .reply
                        .ok_or_else(|| failure("rcli_master_empty_reply"))?;
                    if text.trim().is_empty() || text.contains('\0') {
                        return Err(failure("rcli_master_capture_incomplete"));
                    }
                    assert!(active.capture.sequence() > 0);
                    return Ok(MasterRuntimeObservation::Response {
                        reply: MasterReply {
                            text: fitted_reply(&text, 0),
                            provenance: MasterReplyProvenance {
                                attempt_id: prepared.attempt.request.id.clone(),
                                runtime_ref: active.runtime.runtime_ref.clone(),
                                capture_ref: active.capture_ref.clone(),
                                capture_sequence: active.capture.sequence(),
                            },
                        },
                        delegations: Vec::new(),
                    });
                }
                RcliMasterStatus::Cancelled => return Ok(MasterRuntimeObservation::Cancelled),
                RcliMasterStatus::Failed => return Err(failure("rcli_master_execution_failed")),
            }
        }
    }

    async fn inspect_rcli_scope(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        active: &ActiveRcliMaster,
    ) -> Result<MasterTurnView, EngineError> {
        let current = self
            .inspect_master_turn(
                &prepared.session.project_id,
                &prepared.session.session_id,
                turn_id,
            )
            .await?;
        validate_turn(prepared, turn_id, &current)?;
        if current.ordinal != active.generation {
            return Err(failure("master_runtime_scope_revoked"));
        }
        if current.phase != MasterTurnPhase::Cancelled {
            active.custody.profile().revalidate(
                &self.identity(),
                prepared.profile.binding(),
                turn_id,
                active.generation,
                NativeProfileUsePurpose::Execution,
            )?;
            if Instant::now() >= active.deadline {
                return Err(failure("master_runtime_budget_exhausted"));
            }
        }
        Ok(current)
    }
}

fn validate_turn(
    prepared: &PreparedMasterTurn,
    turn_id: &RequestId,
    current: &MasterTurnView,
) -> Result<(), EngineError> {
    if current.id != *turn_id || current.session_id != prepared.session.session_id {
        return Err(failure("master_runtime_binding"));
    }
    if current.phase == MasterTurnPhase::Cancelled {
        return Ok(());
    }
    if !matches!(
        current.phase,
        MasterTurnPhase::Prepared | MasterTurnPhase::Delivering | MasterTurnPhase::Running
    ) || current.attempt.as_ref().is_none_or(|attempt| {
        attempt.request.id != prepared.attempt.request.id
            || attempt.connection != prepared.attempt.connection
    }) {
        return Err(failure("master_runtime_scope_revoked"));
    }
    assert!(current.attempt.is_some());
    Ok(())
}
