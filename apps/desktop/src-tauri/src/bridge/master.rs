use super::{BridgeError, IntentReceipt, NativeBridge, PAGE_SIZE, bounded, failure};
use crazyagent::engine::owner::{OwnerData, OwnerMutation, OwnerRead};
use crazyagent::master_session::{
    MasterCursor, MasterOwnerAction, MasterSessionView, MasterTurnPage,
};
use crazyagent::protocol::{ProjectId, RequestId, SessionId};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MasterState {
    pub project_id: String,
    pub ticket: String,
    pub revision: String,
    pub config_id: String,
    pub session: Option<MasterSessionView>,
}

#[derive(Debug, Serialize)]
pub struct MasterTurns {
    pub ticket: String,
    pub revision: String,
    pub config_id: String,
    pub page: MasterTurnPage,
}

impl NativeBridge {
    fn master_project(&self, ticket: &str) -> Result<ProjectId, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.project
            .as_ref()
            .filter(|selected| ticket.len() <= 20 && ticket == selected.generation.to_string())
            .map(|selected| selected.id.clone())
            .ok_or_else(|| failure("stale_selection"))
    }

    pub async fn master_state(&mut self, ticket: &str) -> Result<MasterState, BridgeError> {
        let project_id = self.master_project(ticket)?;
        let state = self
            .controller()?
            .read(OwnerRead::ProjectMaster {
                project_id: project_id.clone(),
            })
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        let OwnerData::ProjectMaster { session } = state.data else {
            return Err(failure("unexpected_reply"));
        };
        let session = session.map(|session| *session);
        if session
            .as_ref()
            .is_some_and(|value| value.project_id != project_id)
        {
            return Err(failure("unexpected_reply"));
        }
        bounded(MasterState {
            project_id: project_id.as_str().into(),
            ticket: ticket.into(),
            revision: state.revision.value().to_string(),
            config_id: state.config_id.as_str().into(),
            session,
        })
    }

    pub async fn master_turns(
        &mut self,
        ticket: &str,
        session_id: &str,
        cursor: Option<MasterCursor>,
    ) -> Result<MasterTurns, BridgeError> {
        let project_id = self.master_project(ticket)?;
        let session_id = SessionId::new(session_id).map_err(|_| failure("invalid_session"))?;
        if cursor
            .as_ref()
            .is_some_and(|value| value.project_id != project_id || value.session_id != session_id)
        {
            return Err(failure("stale_master"));
        }
        let current = self.master_state(ticket).await?;
        if !current
            .session
            .as_ref()
            .is_some_and(|value| value.session_id == session_id)
        {
            return Err(failure("stale_master"));
        }
        let state = self
            .controller()?
            .read(OwnerRead::ProjectMasterTurns {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                cursor,
                limit: PAGE_SIZE,
            })
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        let page = match state.data {
            OwnerData::ProjectMasterTurns { page } => page,
            OwnerData::ResyncRequired => return Err(failure("resync_required")),
            _ => return Err(failure("unexpected_reply")),
        };
        if page.project_id != project_id
            || page.session_id != session_id
            || page.turns.len() > usize::from(PAGE_SIZE)
            || page.turns.iter().any(|turn| turn.session_id != session_id)
        {
            return Err(failure("unexpected_reply"));
        }
        bounded(MasterTurns {
            ticket: ticket.into(),
            revision: state.revision.value().to_string(),
            config_id: state.config_id.as_str().into(),
            page,
        })
    }

    pub async fn apply_master(
        &mut self,
        ticket: &str,
        action: MasterOwnerAction,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        let selected = self.master_project(ticket)?;
        let project_id = match &action {
            MasterOwnerAction::Open { project_id, .. }
            | MasterOwnerAction::Submit { project_id, .. }
            | MasterOwnerAction::Cancel { project_id, .. }
            | MasterOwnerAction::Close { project_id, .. } => project_id,
            MasterOwnerAction::RegisterRcliProfile { .. } => {
                return Err(failure("master_bridge_unavailable"));
            }
        };
        if project_id != &selected {
            return Err(failure("stale_selection"));
        }
        self.controller()?
            .read(OwnerRead::ProjectMaster {
                project_id: selected,
            })
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        self.controller()?
            .begin_mutation(OwnerMutation::ProjectMaster { action }, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    /// Opens the selected project's master with the same defaults the TUI uses.
    /// Inputs: the project selection ticket and a fresh draft generation. Output: the
    /// owner intent receipt, or a plain-language reason when no assigned pool can run it.
    pub async fn open_default_master(
        &mut self,
        ticket: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = self.master_project(ticket)?;
        let action = crazyagent::tui::default_master_open(self.controller()?, &project_id)
            .await
            .map_err(|_| failure("master_open_unavailable"))?;
        self.apply_master(ticket, action, generation).await
    }

    /// Sends one owner message to the selected project's open master session.
    /// Inputs: selection ticket, the session id read from master state, message text and
    /// a fresh draft generation. Output: the owner intent receipt for the queued turn.
    pub async fn submit_master(
        &mut self,
        ticket: &str,
        session_id: &str,
        text: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = self.master_project(ticket)?;
        let session_id = SessionId::new(session_id).map_err(|_| failure("invalid_session"))?;
        if text.trim().is_empty() || text.len() > 8192 || text.contains('\0') {
            return Err(failure("conversation_text_bounds"));
        }
        let action = MasterOwnerAction::Submit {
            project_id,
            session_id,
            text: text.into(),
        };
        self.apply_master(ticket, action, generation).await
    }

    /// Stops the master's reply in progress for the selected project, the same engine
    /// cancel the TUI sends on F3. Inputs: selection ticket, the session and turn ids read
    /// from master state, and a fresh draft generation. Output: the owner intent receipt;
    /// a turn that already finished is a plain rejection.
    pub async fn cancel_master_turn(
        &mut self,
        ticket: &str,
        session_id: &str,
        turn_id: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = self.master_project(ticket)?;
        let session_id = SessionId::new(session_id).map_err(|_| failure("invalid_session"))?;
        let turn_id = RequestId::new(turn_id).map_err(|_| failure("invalid_turn"))?;
        let action = MasterOwnerAction::Cancel {
            project_id,
            session_id,
            turn_id,
        };
        self.apply_master(ticket, action, generation).await
    }

    /// Closes the selected project's open master session so a fresh one can be started.
    /// Inputs: selection ticket, the session id read from master state and a fresh draft
    /// generation. Output: the owner intent receipt for the close request.
    pub async fn close_master(
        &mut self,
        ticket: &str,
        session_id: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        let project_id = self.master_project(ticket)?;
        let session_id = SessionId::new(session_id).map_err(|_| failure("invalid_session"))?;
        let action = MasterOwnerAction::Close {
            project_id,
            session_id,
        };
        self.apply_master(ticket, action, generation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::ActionOutcome;
    use crazyagent::client_controller::OwnerController;
    use crazyagent::engine::bootstrap::EngineService;
    use crazyagent::engine::owner::{
        ModelAccessCollection, OwnerClientKind, OwnerMutation, OwnerResponse, OwnerResult,
    };
    use crazyagent::master_session::{
        MasterBackend, MasterReason, MasterSelection, MasterSessionPhase, MasterTurnPhase,
    };
    use crazyagent::protocol::{CredentialRef, ProjectSpec};
    use crazyagent::routing::{ResourceBudget, RouteRequest};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;

    const CATALOG: &str = r#"
version = 1
timeout_ms = 2000
allowed_destinations = ["configured-profile"]
[routing]
approved_accounts = ["configured-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["configured-account"]
[[model_access.connections]]
id = "configured-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "configured-account"
endpoint = { kind="http", url="http://127.0.0.1:11434/v1" }
model = { id="local-model", author="local-author" }
auth = { method="none" }
[[model_access.pools]]
id = "configured-pool"
members = ["configured-connection"]
projects = ["p"]
roles = ["project_leader"]
fallback_allowed = false
"#;

    #[tokio::test]
    async fn real_master_bridge_open_submit_read_and_cancel_preserve_owner_data() {
        let root = tempfile::Builder::new()
            .prefix("ca-desktop-master-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, CATALOG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("p.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let path = root.path().to_path_buf();
        let mut body = tokio::spawn(async move { real_master_flow(&path).await });
        let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
        if result.is_err() {
            body.abort();
            let _ = body.await;
        }
        let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
        assert!(
            matches!(cleanup, Ok(Ok(()))),
            "actual engine cleanup failed"
        );
        result
            .expect("real master bridge case exceeded 20 seconds")
            .expect("real master bridge assertion failed after engine cleanup")
            .expect("real master bridge setup or readback failed");
    }

    async fn real_master_flow(root: &Path) -> Result<(), String> {
        let project_id = ProjectId::new("p").map_err(|error| error.to_string())?;
        let declared = root.join("declared-not-created");
        let mut observer =
            OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .map_err(|error| error.to_string())?;
        observer
            .read(OwnerRead::Capabilities)
            .await
            .map_err(|error| error.to_string())?;
        observer
            .begin_mutation(
                OwnerMutation::CreateProject {
                    project_id: project_id.clone(),
                    spec: ProjectSpec {
                        name: "Real master bridge project".into(),
                        repositories: vec![
                            declared.to_str().ok_or("private path encoding")?.into(),
                        ],
                    },
                },
                1,
            )
            .map_err(|error| error.to_string())?;
        let created = observer
            .send_pending()
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            matches!(created, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
        );
        let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
        bridge.connect().await.map_err(|error| error.code)?;
        let selected = bridge.open_project("p").await.map_err(|error| error.code)?;
        let absent = observer
            .read(OwnerRead::ProjectMaster {
                project_id: project_id.clone(),
            })
            .await
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            absent.data,
            OwnerData::ProjectMaster { session: None }
        ));
        let empty = bridge
            .master_state(&selected.ticket)
            .await
            .map_err(|error| error.code)?;
        assert_eq!(empty.project_id, "p");
        assert_eq!(empty.ticket, selected.ticket);
        assert!(empty.session.is_none());
        assert_eq!(empty.revision, absent.revision.value().to_string());
        assert_eq!(empty.config_id, absent.config_id.as_str());
        let basis = observer
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 16,
            })
            .await
            .map_err(|error| error.to_string())?;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            return Err("actual configured pool source required".into());
        };
        let open = MasterOwnerAction::Open {
            project_id: project_id.clone(),
            expected_config_id: basis.config_id,
            expected_source_hash: source_hash,
            selection: Box::new(MasterSelection {
                backend: MasterBackend::Rcli,
                pool_ref: CredentialRef::new("configured-pool").unwrap(),
                target_profile_ref: CredentialRef::new("configured-profile").unwrap(),
                route: RouteRequest {
                    preferred_target: Some(CredentialRef::new("configured-profile").unwrap()),
                    required_target: Some(CredentialRef::new("configured-profile").unwrap()),
                    required_capabilities: vec![],
                    account_ref: CredentialRef::new("configured-account").unwrap(),
                    budget_ref: CredentialRef::new("conversation-budget").unwrap(),
                    resources: ResourceBudget::unit(),
                },
            }),
        };
        let stale = bridge.apply_master("0", open.clone(), 1).await.unwrap_err();
        assert_eq!(stale.code, "stale_selection");
        let invalid_generation = bridge
            .apply_master(&selected.ticket, open.clone(), 0)
            .await
            .unwrap_err();
        assert_eq!(invalid_generation.code, "draft_generation_limit");
        let mut other_project = open.clone();
        if let MasterOwnerAction::Open { project_id, .. } = &mut other_project {
            *project_id = ProjectId::new("q").unwrap();
        }
        let crossed = bridge
            .apply_master(&selected.ticket, other_project, 1)
            .await
            .unwrap_err();
        assert_eq!(crossed.code, "stale_selection");
        let mut forged = serde_json::to_value(&open).unwrap();
        forged["owner_id"] = serde_json::json!("browser-cannot-supply-authority");
        assert!(serde_json::from_value::<MasterOwnerAction>(forged).is_err());
        assert!(
            bridge
                .pending_intent()
                .map_err(|error| error.code)?
                .is_none()
        );
        let opened = bridge
            .apply_master(&selected.ticket, open, 1)
            .await
            .expect("native master Open must apply through the actual owner controller");
        assert_eq!(opened.outcome, ActionOutcome::Applied);
        assert!(!opened.pending);
        let read = bridge
            .master_state(&selected.ticket)
            .await
            .map_err(|error| error.code)?;
        let actual = observer
            .read(OwnerRead::ProjectMaster {
                project_id: project_id.clone(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let OwnerData::ProjectMaster { session } = actual.data else {
            return Err("actual master session read required".into());
        };
        let session = session.map(|session| *session);
        assert_eq!(
            serde_json::to_value(&read.session).unwrap(),
            serde_json::to_value(&session).unwrap()
        );
        let session = session.ok_or("actual configured master session missing")?;
        assert_eq!(session.phase, MasterSessionPhase::Configured);
        assert_eq!(
            session.owner.operation_id.as_str(),
            opened.intent.operation_id
        );
        assert_eq!(session.project_id, project_id);
        assert!(session.current_turn.is_none());
        let submitted = bridge
            .apply_master(
                &selected.ticket,
                MasterOwnerAction::Submit {
                    project_id: project_id.clone(),
                    session_id: session.session_id.clone(),
                    text: "A genuine owner prompt without launching a model host.".into(),
                },
                2,
            )
            .await
            .map_err(|error| error.code)?;
        assert_eq!(submitted.outcome, ActionOutcome::Applied);
        assert!(!submitted.pending);
        let turns = bridge
            .master_turns(&selected.ticket, session.session_id.as_str(), None)
            .await
            .map_err(|error| error.code)?;
        let actual = observer
            .read(OwnerRead::ProjectMasterTurns {
                project_id: project_id.clone(),
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 16,
            })
            .await
            .map_err(|error| error.to_string())?;
        let OwnerData::ProjectMasterTurns { page } = actual.data else {
            return Err("actual master turns required".into());
        };
        assert_eq!(
            serde_json::to_value(&turns.page).unwrap(),
            serde_json::to_value(&page).unwrap()
        );
        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.turns[0].id.as_str(), submitted.intent.operation_id);
        assert_eq!(page.turns[0].phase, MasterTurnPhase::Queued);
        assert!(page.turns[0].attempt.is_none());
        assert!(page.turns[0].runtime.is_none());
        assert!(page.turns[0].reply.is_none());
        assert_eq!(
            bridge
                .cancel_master_turn(&selected.ticket, session.session_id.as_str(), "bad turn", 3)
                .await
                .unwrap_err()
                .code,
            "invalid_turn"
        );
        let cancelled = bridge
            .cancel_master_turn(
                &selected.ticket,
                session.session_id.as_str(),
                &submitted.intent.operation_id,
                3,
            )
            .await
            .map_err(|error| error.code)?;
        assert_eq!(cancelled.outcome, ActionOutcome::Applied);
        assert_eq!(cancelled.intent.kind, "master");
        assert!(!cancelled.pending);
        let again = bridge
            .cancel_master_turn(
                &selected.ticket,
                session.session_id.as_str(),
                &submitted.intent.operation_id,
                5,
            )
            .await
            .map_err(|error| error.code)?;
        assert_eq!(again.outcome, ActionOutcome::Rejected);
        assert_eq!(again.rejection, Some("master_turn_not_current"));
        let actual = observer
            .read(OwnerRead::ProjectMasterTurns {
                project_id,
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 16,
            })
            .await
            .map_err(|error| error.to_string())?;
        let OwnerData::ProjectMasterTurns { page } = actual.data else {
            return Err("actual cancelled master turn required".into());
        };
        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.turns[0].phase, MasterTurnPhase::Cancelled);
        assert_eq!(page.turns[0].reason, Some(MasterReason::Cancelled));
        assert!(page.turns[0].attempt.is_none());
        assert!(page.turns[0].runtime.is_none());
        assert!(page.turns[0].reply.is_none());
        let cancelled_turns = bridge
            .master_turns(&selected.ticket, session.session_id.as_str(), None)
            .await
            .map_err(|error| error.code)?;
        assert_eq!(
            serde_json::to_value(&cancelled_turns.page).unwrap(),
            serde_json::to_value(&page).unwrap()
        );
        let final_state = bridge
            .master_state(&selected.ticket)
            .await
            .map_err(|error| error.code)?;
        assert!(
            final_state
                .session
                .ok_or("configured session missing after cancel")?
                .current_turn
                .is_none()
        );
        let closed = bridge
            .close_master(&selected.ticket, session.session_id.as_str(), 4)
            .await
            .map_err(|error| error.code)?;
        assert_eq!(closed.outcome, ActionOutcome::Applied);
        let after_close = bridge
            .master_state(&selected.ticket)
            .await
            .map_err(|error| error.code)?;
        assert!(
            after_close
                .session
                .is_none_or(|closed| closed.phase == MasterSessionPhase::Closed)
        );
        assert!(!declared.exists());
        assert!(
            bridge
                .pending_intent()
                .map_err(|error| error.code)?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn real_master_turns_page_past_one_page_and_refuse_a_stale_cursor() {
        let root = tempfile::Builder::new()
            .prefix("ca-desktop-pages-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, CATALOG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("p.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let path = root.path().to_path_buf();
        let mut body = tokio::spawn(async move { real_turn_pages_flow(&path).await });
        let result = tokio::time::timeout(Duration::from_secs(60), &mut body).await;
        if result.is_err() {
            body.abort();
            let _ = body.await;
        }
        let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
        assert!(
            matches!(cleanup, Ok(Ok(()))),
            "actual engine cleanup failed"
        );
        result
            .expect("real master paging case exceeded 60 seconds")
            .expect("real master paging assertion failed after engine cleanup")
            .expect("real master paging setup or readback failed");
    }

    async fn real_turn_pages_flow(root: &Path) -> Result<(), String> {
        let mut observer =
            OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .map_err(|error| error.to_string())?;
        observer
            .read(OwnerRead::Capabilities)
            .await
            .map_err(|error| error.to_string())?;
        observer
            .begin_mutation(
                OwnerMutation::CreateProject {
                    project_id: ProjectId::new("p").unwrap(),
                    spec: ProjectSpec {
                        name: "Paged master".into(),
                        repositories: vec![
                            root.join("p")
                                .to_str()
                                .ok_or("private path encoding")?
                                .into(),
                        ],
                    },
                },
                1,
            )
            .map_err(|error| error.to_string())?;
        let created = observer
            .send_pending()
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            matches!(created, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
        );
        let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
        bridge.connect().await.map_err(|error| error.code)?;
        let ticket = bridge
            .open_project("p")
            .await
            .map_err(|error| error.code)?
            .ticket;
        let opened = bridge
            .open_default_master(&ticket, 1)
            .await
            .map_err(|error| error.code)?;
        assert_eq!(opened.outcome, ActionOutcome::Applied);
        let session = bridge
            .master_state(&ticket)
            .await
            .map_err(|error| error.code)?
            .session
            .ok_or("Start master left no configured session")?;
        let session_id = session.session_id.as_str();
        let count = u64::from(PAGE_SIZE) + 1;
        let mut ids = Vec::new();
        for round in 0..count {
            let sent = bridge
                .submit_master(
                    &ticket,
                    session_id,
                    &format!("Owner message {round}."),
                    2 + 2 * round,
                )
                .await
                .map_err(|error| error.code)?;
            assert_eq!(sent.outcome, ActionOutcome::Applied);
            let stopped = bridge
                .cancel_master_turn(
                    &ticket,
                    session_id,
                    &sent.intent.operation_id,
                    3 + 2 * round,
                )
                .await
                .map_err(|error| error.code)?;
            assert_eq!(stopped.outcome, ActionOutcome::Applied);
            ids.push(sent.intent.operation_id);
        }
        let first = bridge
            .master_turns(&ticket, session_id, None)
            .await
            .map_err(|error| error.code)?;
        assert_eq!(first.page.turns.len(), usize::from(PAGE_SIZE));
        let next = first
            .page
            .next
            .clone()
            .ok_or("a full first page must offer the next page")?;
        let rest = bridge
            .master_turns(&ticket, session_id, Some(next.clone()))
            .await
            .map_err(|error| error.code)?;
        assert!(rest.page.next.is_none());
        let all: Vec<_> = first.page.turns.iter().chain(&rest.page.turns).collect();
        assert_eq!(
            all.iter().map(|turn| turn.ordinal).collect::<Vec<_>>(),
            (1..=count).collect::<Vec<_>>()
        );
        assert_eq!(
            all.iter().map(|turn| turn.id.as_str()).collect::<Vec<_>>(),
            ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(
            all.iter()
                .all(|turn| turn.phase == MasterTurnPhase::Cancelled)
        );
        let later = bridge
            .submit_master(
                &ticket,
                session_id,
                "One more owner message.",
                2 + 2 * count,
            )
            .await
            .map_err(|error| error.code)?;
        assert_eq!(later.outcome, ActionOutcome::Applied);
        let stale = bridge
            .master_turns(&ticket, session_id, Some(next))
            .await
            .unwrap_err();
        assert_eq!(stale.code, "resync_required");
        Ok(())
    }

    #[tokio::test]
    async fn real_start_master_uses_only_the_pool_assigned_to_the_project() {
        let root = tempfile::Builder::new()
            .prefix("ca-desktop-default-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, CATALOG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("p.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let path = root.path().to_path_buf();
        let mut body = tokio::spawn(async move { real_default_open_flow(&path).await });
        let result = tokio::time::timeout(Duration::from_secs(20), &mut body).await;
        if result.is_err() {
            body.abort();
            let _ = body.await;
        }
        let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
        assert!(
            matches!(cleanup, Ok(Ok(()))),
            "actual engine cleanup failed"
        );
        result
            .expect("real default master case exceeded 20 seconds")
            .expect("real default master assertion failed after engine cleanup")
            .expect("real default master setup or readback failed");
    }

    async fn real_default_open_flow(root: &Path) -> Result<(), String> {
        let mut observer =
            OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .map_err(|error| error.to_string())?;
        for (generation, id) in [(1, "p"), (2, "q")] {
            observer
                .read(OwnerRead::Capabilities)
                .await
                .map_err(|error| error.to_string())?;
            observer
                .begin_mutation(
                    OwnerMutation::CreateProject {
                        project_id: ProjectId::new(id).unwrap(),
                        spec: ProjectSpec {
                            name: format!("Default master {id}"),
                            repositories: vec![
                                root.join(id)
                                    .to_str()
                                    .ok_or("private path encoding")?
                                    .into(),
                            ],
                        },
                    },
                    generation,
                )
                .map_err(|error| error.to_string())?;
            let created = observer
                .send_pending()
                .await
                .map_err(|error| error.to_string())?;
            assert!(
                matches!(created, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
            );
        }
        let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
        bridge.connect().await.map_err(|error| error.code)?;
        let unassigned = bridge.open_project("q").await.map_err(|error| error.code)?;
        let refused = bridge
            .open_default_master(&unassigned.ticket, 1)
            .await
            .unwrap_err();
        assert_eq!(refused.code, "master_open_unavailable");
        assert!(
            bridge
                .master_state(&unassigned.ticket)
                .await
                .map_err(|error| error.code)?
                .session
                .is_none()
        );
        assert!(
            bridge
                .pending_intent()
                .map_err(|error| error.code)?
                .is_none()
        );
        let selected = bridge.open_project("p").await.map_err(|error| error.code)?;
        let stale = bridge
            .open_default_master(&unassigned.ticket, 2)
            .await
            .unwrap_err();
        assert_eq!(stale.code, "stale_selection");
        let opened = bridge
            .open_default_master(&selected.ticket, 3)
            .await
            .expect("Start master must open through the actual owner controller");
        assert_eq!(opened.outcome, ActionOutcome::Applied);
        assert!(!opened.pending);
        let state = bridge
            .master_state(&selected.ticket)
            .await
            .map_err(|error| error.code)?;
        let session = state
            .session
            .ok_or("Start master left no configured session")?;
        assert_eq!(session.phase, MasterSessionPhase::Configured);
        assert_eq!(session.selection.backend, MasterBackend::Rcli);
        assert_eq!(session.selection.pool_ref.as_str(), "configured-pool");
        assert_eq!(
            session.selection.target_profile_ref.as_str(),
            "configured-profile"
        );
        assert_eq!(
            session.selection.route.account_ref.as_str(),
            "configured-account"
        );
        let refused_text = bridge
            .submit_master(&selected.ticket, session.session_id.as_str(), "  ", 4)
            .await
            .unwrap_err();
        assert_eq!(refused_text.code, "conversation_text_bounds");
        let sent = bridge
            .submit_master(
                &selected.ticket,
                session.session_id.as_str(),
                "Say hello.",
                5,
            )
            .await
            .map_err(|error| error.code)?;
        assert_eq!(sent.outcome, ActionOutcome::Applied);
        let turns = bridge
            .master_turns(&selected.ticket, session.session_id.as_str(), None)
            .await
            .map_err(|error| error.code)?;
        assert_eq!(turns.page.turns.len(), 1);
        assert_eq!(turns.page.turns[0].id.as_str(), sent.intent.operation_id);
        let other = bridge.open_project("q").await.map_err(|error| error.code)?;
        assert!(
            bridge
                .master_state(&other.ticket)
                .await
                .map_err(|error| error.code)?
                .session
                .is_none()
        );
        Ok(())
    }
}
