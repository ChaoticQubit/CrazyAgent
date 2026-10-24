use crazyagent::client_controller::OwnerController;
use crazyagent::conversation::{
    ConversationContent, ConversationCursor, ConversationEntry, ConversationMutation,
    TEXT_BYTES_MAX,
};
use crazyagent::engine::Client;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerPageCursor, OwnerRead,
    OwnerResponse, OwnerResult, OwnerSummary, OwnerView,
};
use crazyagent::protocol::{BaselineId, ProjectId, ProjectSpec};
use crazyagent::verification::BaselineDraft;
use serde::Serialize;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

mod account_profile;
mod account_setup;
mod accounts;
mod agents;
mod harnesses;
pub mod intake;
pub mod master;
pub mod memory;
mod model_access;
mod native_auth;
pub mod open;
mod owner_setup;
pub mod picker;
mod plugins;
pub mod project_setup;
mod team;
pub mod terminal;
pub use account_profile::{AccountProfileView, ConfiguredAccountPlan};
pub use account_setup::ManagedAccountPlan;
pub use model_access::{ModelAccessDetail, ModelAccessPage};
pub use native_auth::NativeAuthView;

const PAGE_SIZE: u8 = 16;
const PROJECTION_BYTES_MAX: usize = 65_536;

#[derive(Debug, Serialize)]
pub struct BridgeError {
    pub code: &'static str,
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}

impl std::error::Error for BridgeError {}

fn failure(code: &'static str) -> BridgeError {
    BridgeError { code }
}

#[derive(Debug, Serialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub pending: bool,
}

#[derive(Debug, Serialize)]
pub struct BaselineRow {
    pub id: String,
    pub project_id: String,
    pub approved: bool,
    pub revoked: bool,
    pub current: bool,
}

#[derive(Debug, Serialize)]
pub struct BaselinePage {
    pub items: Vec<BaselineRow>,
    pub has_more: bool,
    pub fresh: bool,
}

#[derive(Debug, Serialize)]
pub struct BaselineCheck {
    pub id: String,
    pub kind: String,
    pub minimum_risk: String,
    pub content: String,
    pub encoding: &'static str,
}

#[derive(Debug, Serialize)]
pub struct BaselineRule {
    pub path_prefix: String,
    pub minimum_risk: String,
}

#[derive(Debug, Serialize)]
pub struct BaselineDetail {
    pub ticket: String,
    pub id: String,
    pub project_id: String,
    pub origin: String,
    pub requirements: String,
    pub checks: Vec<BaselineCheck>,
    pub rules: Vec<BaselineRule>,
    pub retry_limit: u8,
    pub approved: bool,
    pub revoked: bool,
    pub current: bool,
    pub fresh: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    Applied,
    Rejected,
    Unavailable,
    Unknown,
    NotRecordedAtBarrier,
    Uncertain,
}

#[derive(Debug, Serialize)]
pub struct ActionReceipt {
    pub baseline_id: String,
    pub outcome: ActionOutcome,
    pub pending: bool,
}

struct BaselineSelection {
    id: BaselineId,
    generation: u64,
    approved: bool,
    revoked: bool,
}

#[derive(Debug, Serialize)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub repository_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ProjectPage {
    pub items: Vec<ProjectRow>,
    pub has_more: bool,
    pub fresh: bool,
}

#[derive(Debug, Serialize)]
pub struct ConversationRow {
    pub kind: &'static str,
    pub version: Option<String>,
    pub text: String,
    pub event: String,
    pub revision: String,
    pub origin: String,
}

#[derive(Debug, Serialize)]
pub struct ProjectDetail {
    pub project_id: String,
    pub ticket: String,
    pub brief: Option<ConversationRow>,
    pub fresh: bool,
    pub revision: String,
}

#[derive(Debug, Serialize)]
pub struct HistoryPage {
    pub project_id: String,
    pub entries: Vec<ConversationRow>,
    pub has_more: bool,
    pub fresh: bool,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingIntent {
    pub kind: &'static str,
    pub project_id: Option<String>,
    pub baseline_id: Option<String>,
    pub generation: String,
    pub model_access: Option<crazyagent::config::ModelAccessEdit>,
    pub project_spec: Option<ProjectSpec>,
    pub operation_id: String,
}

#[derive(Debug, Serialize)]
pub struct IntentReceipt {
    #[serde(flatten)]
    pub intent: PendingIntent,
    pub outcome: ActionOutcome,
    pub pending: bool,
    pub rejection: Option<&'static str>,
}

struct ProjectSelection {
    id: ProjectId,
    generation: u64,
    expected_version: u64,
    next: Option<ConversationCursor>,
}

pub struct NativeBridge {
    socket: Option<PathBuf>,
    ordinary: Option<Client>,
    controller: Option<OwnerController>,
    next: Option<OwnerPageCursor>,
    generation: u64,
    selected: Option<BaselineSelection>,
    projects_next: Option<OwnerPageCursor>,
    project: Option<ProjectSelection>,
    model_access: Option<model_access::Selection>,
    model_access_next: Option<crazyagent::engine::owner::ModelAccessCursor>,
    native_auth: Option<native_auth::Operation>,
    account_profile: Option<account_profile::Operation>,
}

impl NativeBridge {
    pub fn new(socket: Option<PathBuf>) -> Self {
        Self {
            socket,
            ordinary: None,
            controller: None,
            next: None,
            generation: 0,
            selected: None,
            projects_next: None,
            project: None,
            model_access: None,
            model_access_next: None,
            native_auth: None,
            account_profile: None,
        }
    }

    pub async fn connect(&mut self) -> Result<ConnectionStatus, BridgeError> {
        self.model_access = None;
        self.model_access_next = None;
        self.next = None;
        self.selected = None;
        self.project = None;
        self.projects_next = None;
        self.ordinary = None;
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| failure("not_configured"))?;
        let client = Client::new(socket.clone(), Duration::from_secs(5))
            .map_err(|_| failure("owner_unavailable"))?;
        let ordinary = if let Some(controller) = self.controller.as_mut() {
            controller
                .reattach_configured(client)
                .await
                .map_err(|_| failure("owner_reconnect_failed"))?
        } else {
            let (ordinary, controller) =
                OwnerController::attach_configured(client, OwnerClientKind::Desktop)
                    .await
                    .map_err(|_| failure("owner_unavailable"))?;
            self.controller = Some(controller);
            ordinary
        };
        self.ordinary = Some(ordinary);
        assert!(self.controller.is_some());
        assert!(self.ordinary.is_some());
        Ok(self.status())
    }

    pub fn status(&self) -> ConnectionStatus {
        ConnectionStatus {
            connected: self.ordinary.is_some(),
            pending: self.profile_pending()
                || self
                    .controller
                    .as_ref()
                    .is_some_and(|c| c.pending().is_some()),
        }
    }

    fn controller(&mut self) -> Result<&mut OwnerController, BridgeError> {
        self.controller
            .as_mut()
            .ok_or_else(|| failure("not_connected"))
    }

    pub async fn baselines(&mut self, next: bool) -> Result<BaselinePage, BridgeError> {
        self.selected = None;
        let cursor = if next {
            Some(self.next.take().ok_or_else(|| failure("no_next_page"))?)
        } else {
            self.next = None;
            None
        };
        let state = self
            .controller()?
            .read_page(OwnerView::Baselines, cursor, Some(PAGE_SIZE))
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        let OwnerData::Page { items, next } = state.data else {
            return Err(failure("resync_required"));
        };
        if items.len() > usize::from(PAGE_SIZE) {
            return Err(failure("projection_limit"));
        }
        let rows = items
            .into_iter()
            .map(baseline_row)
            .collect::<Result<Vec<_>, _>>()?;
        let result = bounded(BaselinePage {
            items: rows,
            has_more: next.is_some(),
            fresh: true,
        })?;
        self.next = next;
        assert!(result.items.len() <= usize::from(PAGE_SIZE));
        assert!(result.fresh);
        Ok(result)
    }

    pub async fn inspect(&mut self, id: &str) -> Result<BaselineDetail, BridgeError> {
        self.selected = None;
        let baseline_id = BaselineId::new(id).map_err(|_| failure("invalid_baseline"))?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| failure("selection_limit"))?;
        let state = self
            .controller()?
            .read(OwnerRead::Baseline {
                baseline_id: baseline_id.clone(),
            })
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        let OwnerData::Baseline {
            draft,
            approved,
            revoked,
            current,
        } = state.data
        else {
            return Err(failure("unexpected_reply"));
        };
        let detail = project_detail(id, self.generation, draft, approved, revoked, current)?;
        self.selected = Some(BaselineSelection {
            id: baseline_id,
            generation: self.generation,
            approved,
            revoked,
        });
        assert_eq!(detail.id, id);
        assert!(detail.fresh);
        Ok(detail)
    }

    pub async fn approve(&mut self, ticket: &str) -> Result<ActionReceipt, BridgeError> {
        self.prepare(ticket, false)?;
        self.deliver(false).await
    }

    pub async fn revoke(&mut self, ticket: &str) -> Result<ActionReceipt, BridgeError> {
        self.prepare(ticket, true)?;
        self.deliver(false).await
    }

    fn prepare(&mut self, ticket: &str, revoke: bool) -> Result<(), BridgeError> {
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        let selected = self
            .selected
            .as_ref()
            .filter(|s| ticket.len() <= 20 && ticket == s.generation.to_string())
            .ok_or_else(|| failure("stale_selection"))?;
        if selected.revoked || (!revoke && selected.approved) {
            return Err(failure("decision_not_applicable"));
        }
        let mutation = if revoke {
            OwnerMutation::RevokeBaseline {
                baseline_id: selected.id.clone(),
            }
        } else {
            OwnerMutation::ApproveBaseline {
                baseline_id: selected.id.clone(),
            }
        };
        let generation = selected.generation;
        self.controller()?
            .begin_mutation(mutation, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        self.selected = None;
        self.next = None;
        assert!(self.status().pending);
        Ok(())
    }

    pub async fn reconcile(&mut self) -> Result<ActionReceipt, BridgeError> {
        self.deliver(true).await
    }

    pub async fn retry(&mut self) -> Result<ActionReceipt, BridgeError> {
        self.deliver(false).await
    }

    async fn deliver(&mut self, reconcile: bool) -> Result<ActionReceipt, BridgeError> {
        let target = self
            .pending_intent()?
            .ok_or_else(|| failure("no_pending_intent"))?;
        let baseline_id = target
            .baseline_id
            .ok_or_else(|| failure("unexpected_pending_intent"))?;
        let result = self.deliver_intent(reconcile).await?;
        bounded(ActionReceipt {
            baseline_id,
            outcome: result.outcome,
            pending: result.pending,
        })
    }

    pub fn pending_intent(&self) -> Result<Option<PendingIntent>, BridgeError> {
        let Some(pending) = self.controller.as_ref().and_then(|c| c.pending()) else {
            return Ok(None);
        };
        let OwnerOperation::Apply { mutation } = &pending.request().operation else {
            return Err(failure("unexpected_pending_intent"));
        };
        let (kind, project_id, baseline_id) = match mutation {
            OwnerMutation::ApproveBaseline { baseline_id } => (
                "approve_baseline",
                None,
                Some(baseline_id.as_str().to_owned()),
            ),
            OwnerMutation::RevokeBaseline { baseline_id } => (
                "revoke_baseline",
                None,
                Some(baseline_id.as_str().to_owned()),
            ),
            OwnerMutation::Conversation {
                mutation: ConversationMutation::AppendMessage { project_id, .. },
            } => ("message", Some(project_id.as_str().to_owned()), None),
            OwnerMutation::Conversation {
                mutation: ConversationMutation::ReplaceBrief { project_id, .. },
            } => ("brief", Some(project_id.as_str().to_owned()), None),
            OwnerMutation::ModelAccess { .. } => ("model_access", None, None),
            OwnerMutation::SaveOwnerProfile { .. } => ("owner_profile", None, None),
            OwnerMutation::CreateProject { project_id, .. } => {
                ("create_project", Some(project_id.as_str().to_owned()), None)
            }
            OwnerMutation::SetupProject { project_id, .. } => {
                ("setup_project", Some(project_id.as_str().to_owned()), None)
            }
            OwnerMutation::SaveResearchToMemory { project_id, .. } => (
                "research_memory",
                Some(project_id.as_str().to_owned()),
                None,
            ),
            OwnerMutation::AddProjectDocuments { project_id, .. } => (
                "project_documents",
                Some(project_id.as_str().to_owned()),
                None,
            ),
            OwnerMutation::DeleteProjectContext { project_id, .. } => (
                "project_context_delete",
                Some(project_id.as_str().to_owned()),
                None,
            ),
            OwnerMutation::Team { action } => {
                ("team", Some(action.project().as_str().to_owned()), None)
            }
            OwnerMutation::UpdateProjectSetup { project_id, .. } => (
                "project_setup_update",
                Some(project_id.as_str().to_owned()),
                None,
            ),
            OwnerMutation::RemoveProject { project_id } => {
                ("project_remove", Some(project_id.as_str().to_owned()), None)
            }
            OwnerMutation::SetProjectsHome { .. } => ("projects_home", None, None),
            OwnerMutation::MoveProjectsHome { .. } => ("move_projects_home", None, None),
            OwnerMutation::AddExistingFolder { .. } => ("add_existing_folder", None, None),
            OwnerMutation::StartOnboarding { .. } => ("start_onboarding", None, None),
            OwnerMutation::CancelResearch { project_id, .. } => (
                "research_cancel",
                Some(project_id.as_str().to_owned()),
                None,
            ),
            OwnerMutation::RetryResearch { project_id, .. } => {
                ("research_retry", Some(project_id.as_str().to_owned()), None)
            }
            OwnerMutation::Knowledge {
                review: crazyagent::memory::HumanReview::Review { .. },
            } => ("memory_review", None, None),
            OwnerMutation::Memory { action } => match action {
                crazyagent::memory::library::LibraryAction::Add { scope, .. } => (
                    "memory_add",
                    match scope {
                        crazyagent::memory::library::MemoryScope::Global => None,
                        crazyagent::memory::library::MemoryScope::Project { project_id } => {
                            Some(project_id.as_str().to_owned())
                        }
                    },
                    None,
                ),
                crazyagent::memory::library::LibraryAction::Promote { project_id, .. } => {
                    ("memory_promote", Some(project_id.as_str().to_owned()), None)
                }
                crazyagent::memory::library::LibraryAction::Delete { .. } => {
                    ("memory_delete", None, None)
                }
                crazyagent::memory::library::LibraryAction::Forget { project_id, .. } => {
                    ("memory_forget", Some(project_id.as_str().to_owned()), None)
                }
                crazyagent::memory::library::LibraryAction::Remember { project_id, .. } => (
                    "memory_remember",
                    Some(project_id.as_str().to_owned()),
                    None,
                ),
                crazyagent::memory::library::LibraryAction::SaveSelection {
                    project_id, ..
                } => (
                    "memory_selection",
                    Some(project_id.as_str().to_owned()),
                    None,
                ),
            },
            OwnerMutation::ProjectMaster { action } => match action {
                crazyagent::master_session::MasterOwnerAction::Open { project_id, .. }
                | crazyagent::master_session::MasterOwnerAction::Submit { project_id, .. }
                | crazyagent::master_session::MasterOwnerAction::Cancel { project_id, .. }
                | crazyagent::master_session::MasterOwnerAction::Close { project_id, .. } => {
                    ("master", Some(project_id.as_str().to_owned()), None)
                }
                crazyagent::master_session::MasterOwnerAction::RegisterRcliProfile { .. } => {
                    return Err(failure("unexpected_pending_intent"));
                }
            },
            _ => return Err(failure("unexpected_pending_intent")),
        };
        bounded(Some(PendingIntent {
            kind,
            project_id,
            baseline_id,
            generation: pending.generation().to_string(),
            model_access: match mutation {
                OwnerMutation::ModelAccess { edit, .. } => Some(edit.clone()),
                _ => None,
            },
            project_spec: match mutation {
                OwnerMutation::CreateProject { spec, .. }
                | OwnerMutation::SetupProject { spec, .. } => Some(spec.clone()),
                _ => None,
            },
            operation_id: pending.request().request_id.as_str().to_owned(),
        }))
    }

    pub async fn reconcile_intent(&mut self) -> Result<IntentReceipt, BridgeError> {
        self.deliver_intent(true).await
    }

    pub async fn retry_intent(&mut self) -> Result<IntentReceipt, BridgeError> {
        self.deliver_intent(false).await
    }

    async fn deliver_intent(&mut self, reconcile: bool) -> Result<IntentReceipt, BridgeError> {
        let intent = self
            .pending_intent()?
            .ok_or_else(|| failure("no_pending_intent"))?;
        self.selected = None;
        self.next = None;
        self.model_access = None;
        self.model_access_next = None;
        self.projects_next = None;
        let response = if reconcile {
            self.controller()?.reconcile_pending().await
        } else {
            self.controller()?.retry_pending().await
        };
        let mut rejection = None;
        let outcome = match response {
            Ok(OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }) => {
                match operation.result {
                    OwnerResult::Applied => ActionOutcome::Applied,
                    OwnerResult::DispatchPrepared { .. }
                    | OwnerResult::RcliProfileRegistered { .. } => {
                        return Err(failure("unexpected_reply"));
                    }
                    OwnerResult::Rejected { code } => {
                        rejection = Some(rejection_code(&code));
                        ActionOutcome::Rejected
                    }
                    OwnerResult::VerificationUnavailable { .. } => ActionOutcome::Unavailable,
                }
            }
            Ok(OwnerResponse::Rejected { code }) => {
                rejection = Some(rejection_code(&code));
                ActionOutcome::Rejected
            }
            Ok(OwnerResponse::OutcomeUnknown { .. }) => ActionOutcome::Unknown,
            Ok(OwnerResponse::NotRecordedAtBarrier { .. }) => ActionOutcome::NotRecordedAtBarrier,
            Ok(OwnerResponse::Read { .. }) => return Err(failure("unexpected_reply")),
            Err(_) => {
                self.ordinary = None;
                self.controller()?.detach();
                ActionOutcome::Uncertain
            }
        };
        bounded(IntentReceipt {
            intent,
            outcome,
            pending: self.status().pending,
            rejection,
        })
    }

    pub async fn projects(&mut self, next: bool) -> Result<ProjectPage, BridgeError> {
        self.selected = None;
        let cursor = if next {
            Some(
                self.projects_next
                    .take()
                    .ok_or_else(|| failure("no_next_page"))?,
            )
        } else {
            self.projects_next = None;
            None
        };
        let state = self
            .controller()?
            .read_page(OwnerView::Projects, cursor, Some(PAGE_SIZE))
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        let OwnerData::Page { items, next } = state.data else {
            return Err(failure("resync_required"));
        };
        if items.len() > usize::from(PAGE_SIZE) {
            return Err(failure("projection_limit"));
        }
        let rows = items
            .into_iter()
            .map(|item| match item {
                OwnerSummary::Project {
                    project_id,
                    name,
                    repository_count,
                } => Ok(ProjectRow {
                    id: project_id.as_str().into(),
                    name,
                    repository_count,
                }),
                _ => Err(failure("unexpected_reply")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let page = bounded(ProjectPage {
            items: rows,
            has_more: next.is_some(),
            fresh: true,
        })?;
        self.projects_next = next;
        Ok(page)
    }

    pub async fn create_project(
        &mut self,
        spec: ProjectSpec,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        spec.validate()
            .map_err(|_| failure("project_spec_bounds"))?;
        if spec.repositories.is_empty()
            || spec
                .repositories
                .iter()
                .any(|path| !std::path::Path::new(path).is_absolute())
        {
            return Err(failure("project_spec_bounds"));
        }
        let project_id = native_project_id()?;
        let preview = PendingIntent {
            kind: "create_project",
            project_id: Some(project_id.as_str().to_owned()),
            baseline_id: None,
            generation: generation.to_string(),
            model_access: None,
            project_spec: Some(spec.clone()),
            operation_id: "0".repeat(64),
        };
        let length = serde_json::to_vec(&preview)
            .map_err(|_| failure("project_spec_bounds"))?
            .len();
        if length > PROJECTION_BYTES_MAX - 128 {
            return Err(failure("project_spec_bounds"));
        }
        self.controller()?
            .begin_mutation(
                OwnerMutation::CreateProject { project_id, spec },
                generation,
            )
            .map_err(|_| failure("owner_not_ready"))?;
        assert!(self.status().pending);
        self.deliver_intent(false).await
    }

    pub async fn open_project(&mut self, id: &str) -> Result<ProjectDetail, BridgeError> {
        self.project = None;
        self.selected = None;
        let project_id = ProjectId::new(id).map_err(|_| failure("invalid_project"))?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| failure("selection_limit"))?;
        let state = self
            .controller()?
            .read(OwnerRead::CurrentBrief {
                project_id: project_id.clone(),
            })
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        let OwnerData::CurrentBrief {
            project_id: actual,
            brief,
        } = state.data
        else {
            return Err(failure("unexpected_reply"));
        };
        if actual != project_id {
            return Err(failure("unexpected_reply"));
        }
        let expected_version = match brief.as_ref().map(|entry| &entry.content) {
            None => 0,
            Some(ConversationContent::Brief { version }) => *version,
            _ => return Err(failure("unexpected_reply")),
        };
        let result = bounded(ProjectDetail {
            project_id: id.into(),
            ticket: self.generation.to_string(),
            brief: brief.map(conversation_row),
            fresh: true,
            revision: state.revision.value().to_string(),
        })?;
        self.project = Some(ProjectSelection {
            id: project_id,
            generation: self.generation,
            expected_version,
            next: None,
        });
        Ok(result)
    }

    pub async fn history(&mut self, next: bool) -> Result<HistoryPage, BridgeError> {
        self.selected = None;
        let selected = self
            .project
            .as_mut()
            .ok_or_else(|| failure("stale_selection"))?;
        let id = selected.id.clone();
        let cursor = if next {
            Some(
                selected
                    .next
                    .take()
                    .ok_or_else(|| failure("no_next_page"))?,
            )
        } else {
            selected.next = None;
            None
        };
        let state = self
            .controller()?
            .read(OwnerRead::ConversationHistory {
                project_id: id.clone(),
                cursor,
                limit: 1,
            })
            .await
            .map_err(|_| failure("owner_read_failed"))?;
        let OwnerData::ConversationHistory { page } = state.data else {
            return Err(failure("resync_required"));
        };
        if page.project_id != id || page.entries.len() > 1 {
            return Err(failure("unexpected_reply"));
        }
        let result = bounded(HistoryPage {
            project_id: id.as_str().into(),
            entries: page.entries.into_iter().map(conversation_row).collect(),
            has_more: page.next.is_some(),
            fresh: true,
            revision: state.revision.value().to_string(),
        })?;
        self.project
            .as_mut()
            .ok_or_else(|| failure("stale_selection"))?
            .next = page.next;
        Ok(result)
    }

    pub async fn send_message(
        &mut self,
        ticket: &str,
        text: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.prepare_conversation(ticket, text, generation, false)?;
        self.deliver_intent(false).await
    }

    pub async fn replace_brief(
        &mut self,
        ticket: &str,
        text: &str,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.prepare_conversation(ticket, text, generation, true)?;
        self.deliver_intent(false).await
    }

    fn prepare_conversation(
        &mut self,
        ticket: &str,
        text: &str,
        generation: u64,
        brief: bool,
    ) -> Result<(), BridgeError> {
        self.allow_new_auth()?;
        if self.status().pending {
            return Err(failure("pending_intent"));
        }
        if generation == 0 || generation > 9_007_199_254_740_991 {
            return Err(failure("draft_generation_limit"));
        }
        if text.trim().is_empty() || text.len() > TEXT_BYTES_MAX || text.contains('\0') {
            return Err(failure("conversation_text_bounds"));
        }
        let selected = self
            .project
            .as_ref()
            .filter(|s| ticket.len() <= 20 && ticket == s.generation.to_string())
            .ok_or_else(|| failure("stale_selection"))?;
        let mutation = if brief {
            ConversationMutation::ReplaceBrief {
                project_id: selected.id.clone(),
                expected_version: selected.expected_version,
                text: text.into(),
            }
        } else {
            ConversationMutation::AppendMessage {
                project_id: selected.id.clone(),
                text: text.into(),
            }
        };
        self.controller()?
            .begin_mutation(OwnerMutation::Conversation { mutation }, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        self.selected = None;
        assert!(self.status().pending);
        Ok(())
    }
}

fn native_project_id() -> Result<ProjectId, BridgeError> {
    ProjectId::new(format!("project-{}", native_random_id()?))
        .map_err(|_| failure("project_identity_unavailable"))
}

fn native_random_id() -> Result<String, BridgeError> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| failure("project_identity_unavailable"))?;
    let mut id = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut id, "{byte:02x}").map_err(|_| failure("project_identity_unavailable"))?;
    }
    assert_eq!(id.len(), 64);
    Ok(id)
}

fn conversation_row(entry: ConversationEntry) -> ConversationRow {
    let (kind, version) = match entry.content {
        ConversationContent::Message => ("message", None),
        ConversationContent::Brief { version } => ("brief", Some(version.to_string())),
    };
    ConversationRow {
        kind,
        version,
        text: entry.text,
        event: entry.event_cursor.value().to_string(),
        revision: entry.provenance.revision_after.value().to_string(),
        origin: entry.provenance.origin,
    }
}

fn rejection_code(code: &str) -> &'static str {
    match code {
        "settings_source_conflict" => "settings_source_conflict",
        "settings_source_replaced" => "settings_source_replaced",
        "settings_publication_pending" => "settings_publication_pending",
        "account_exists" => "account_exists",
        "connection_exists" => "connection_exists",
        "pool_exists" => "pool_exists",
        "account_not_found" => "account_not_found",
        "connection_not_found" => "connection_not_found",
        "pool_not_found" => "pool_not_found",
        "settings_read_only" => "settings_read_only",
        "settings_source_changed" => "settings_source_changed",
        "settings_project_changed" => "settings_project_changed",
        "settings_pending" => "settings_pending",
        "stale_config" => "stale_config",
        "invalid_model_access" => "invalid_model_access",
        "model_access_duplicate" => "model_access_duplicate",
        "model_access_missing" => "model_access_missing",
        "model_access_referenced" => "model_access_referenced",
        "model_access_edit_bounds" => "model_access_edit_bounds",
        "pool_member_limit" => "pool_member_limit",
        "pool_in_use" => "pool_in_use",
        "pool_name_bounds" => "pool_name_bounds",
        "owner_profile_name" => "owner_profile_name",
        "owner_profile_role" => "owner_profile_role",
        "owner_profile_bio" => "owner_profile_bio",
        "owner_profile_stack" => "owner_profile_stack",
        "owner_profile_bounds" => "owner_profile_bounds",
        "stale_brief_version" => "stale_brief_version",
        "conversation_capacity" => "conversation_capacity",
        "project_missing" => "project_missing",
        "stale_revision" => "stale_revision",
        "owner_revoked" => "owner_revoked",
        "project_exists" => "project_exists",
        "project_folder_exists" => "project_folder_exists",
        "project_folder_unwritable" => "project_folder_unwritable",
        "pool_missing" => "pool_missing",
        "document_not_found" => "document_not_found",
        "document_not_a_regular_file" => "document_not_a_regular_file",
        "document_type_not_allowed" => "document_type_not_allowed",
        "document_too_large" => "document_too_large",
        "document_name_duplicate" => "document_name_duplicate",
        "document_name_invalid" => "document_name_invalid",
        "document_unreadable" => "document_unreadable",
        "document_changed_while_copying" => "document_changed_while_copying",
        "documents_too_many" => "documents_too_many",
        "project_documents_too_large" => "project_documents_too_large",
        "project_setup_missing" => "project_setup_missing",
        "research_report_missing" => "research_report_missing",
        "research_section_missing" => "research_section_missing",
        "memory_text_too_long" => "memory_text_too_long",
        "memory_entry_exists" => "memory_entry_exists",
        "project_folder_missing" => "project_folder_missing",
        "context_item_protected" => "context_item_protected",
        "context_item_missing" => "context_item_missing",
        "context_path_invalid" => "context_path_invalid",
        "context_item_unreadable" => "context_item_unreadable",
        "master_turn_not_current" => "master_turn_not_current",
        "knowledge_entry_missing" => "knowledge_entry_missing",
        "knowledge_version_conflict" => "knowledge_version_conflict",
        "invalid_project" => "invalid_project",
        "project_master_open" => "project_master_open",
        "research_job_missing" => "research_job_missing",
        "research_job_finished" => "research_job_finished",
        "research_job_not_retryable" => "research_job_not_retryable",
        "projects_home_unset" => "projects_home_unset",
        "projects_home_path_invalid" => "projects_home_path_invalid",
        "projects_home_unwritable" => "projects_home_unwritable",
        "home_source_invalid" => "home_source_invalid",
        "folder_missing" => "folder_missing",
        "folder_not_a_directory" => "folder_not_a_directory",
        "folder_inside_home" => "folder_inside_home",
        "folder_contains_home" => "folder_contains_home",
        "folder_name_taken" => "folder_name_taken",
        "folder_move_failed" => "folder_move_failed",
        "folder_copy_failed" => "folder_copy_failed",
        "onboarding_path_invalid" => "onboarding_path_invalid",
        "onboarding_running" => "onboarding_running",
        "onboarding_missing" => "onboarding_missing",
        _ => crazyagent::memory::library::known_code(code).unwrap_or("owner_rejected"),
    }
}

fn baseline_row(value: OwnerSummary) -> Result<BaselineRow, BridgeError> {
    let OwnerSummary::Baseline {
        baseline_id,
        project_id,
        approved,
        revoked,
        current,
    } = value
    else {
        return Err(failure("unexpected_reply"));
    };
    Ok(BaselineRow {
        id: baseline_id.as_str().into(),
        project_id: project_id.as_str().into(),
        approved,
        revoked,
        current,
    })
}

fn project_detail(
    id: &str,
    generation: u64,
    draft: BaselineDraft,
    approved: bool,
    revoked: bool,
    current: bool,
) -> Result<BaselineDetail, BridgeError> {
    draft
        .validate()
        .map_err(|_| failure("invalid_baseline_data"))?;
    let checks = draft
        .checks
        .into_iter()
        .map(|check| {
            let (content, encoding) = display_content(check.content);
            BaselineCheck {
                id: check.id.as_str().into(),
                kind: format!("{:?}", check.kind),
                minimum_risk: format!("{:?}", check.minimum_risk),
                content,
                encoding,
            }
        })
        .collect();
    let rules = draft
        .rules
        .into_iter()
        .map(|rule| BaselineRule {
            path_prefix: rule.path_prefix,
            minimum_risk: format!("{:?}", rule.minimum),
        })
        .collect();
    bounded(BaselineDetail {
        ticket: generation.to_string(),
        id: id.into(),
        project_id: draft.project_id.as_str().into(),
        origin: format!("{:?}", draft.origin),
        requirements: draft.requirements,
        checks,
        rules,
        retry_limit: draft.retry_limit,
        approved,
        revoked,
        current,
        fresh: true,
    })
}

fn display_content(bytes: Vec<u8>) -> (String, &'static str) {
    match String::from_utf8(bytes) {
        Ok(text) => (text, "utf8"),
        Err(error) => {
            let bytes = error.into_bytes();
            let encoded: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(encoded.len(), bytes.len() * 2);
            assert!(encoded.is_ascii());
            (encoded, "hex")
        }
    }
}

fn bounded<T: Serialize>(value: T) -> Result<T, BridgeError> {
    let bytes = serde_json::to_vec(&value).map_err(|_| failure("projection_failed"))?;
    if bytes.len() > PROJECTION_BYTES_MAX {
        return Err(failure("projection_limit"));
    }
    assert!(!bytes.is_empty());
    assert!(bytes.len() <= PROJECTION_BYTES_MAX);
    Ok(value)
}
