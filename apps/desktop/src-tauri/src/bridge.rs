use crazyagent::client_controller::OwnerController;
use crazyagent::engine::Client;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerPageCursor, OwnerRead,
    OwnerResponse, OwnerResult, OwnerSummary, OwnerView,
};
use crazyagent::protocol::BaselineId;
use crazyagent::verification::BaselineDraft;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

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

pub struct NativeBridge {
    socket: Option<PathBuf>,
    ordinary: Option<Client>,
    controller: Option<OwnerController>,
    next: Option<OwnerPageCursor>,
    generation: u64,
    selected: Option<BaselineSelection>,
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
        }
    }

    pub async fn connect(&mut self) -> Result<ConnectionStatus, BridgeError> {
        self.next = None;
        self.selected = None;
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
            pending: self
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
        let controller = self.controller()?;
        let pending = controller
            .pending()
            .ok_or_else(|| failure("no_pending_intent"))?;
        let OwnerOperation::Apply { mutation } = &pending.request().operation else {
            return Err(failure("unexpected_pending_intent"));
        };
        let baseline_id = match mutation {
            OwnerMutation::ApproveBaseline { baseline_id }
            | OwnerMutation::RevokeBaseline { baseline_id } => baseline_id.as_str().to_owned(),
            _ => return Err(failure("unexpected_pending_intent")),
        };
        self.selected = None;
        self.next = None;
        let response = if reconcile {
            self.controller()?.reconcile_pending().await
        } else {
            self.controller()?.retry_pending().await
        };
        let outcome = match response {
            Ok(OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }) => {
                match operation.result {
                    OwnerResult::Applied => ActionOutcome::Applied,
                    OwnerResult::Rejected { .. } => ActionOutcome::Rejected,
                    OwnerResult::VerificationUnavailable { .. } => ActionOutcome::Unavailable,
                }
            }
            Ok(OwnerResponse::Rejected { .. }) => ActionOutcome::Rejected,
            Ok(OwnerResponse::OutcomeUnknown { .. }) => ActionOutcome::Unknown,
            Ok(OwnerResponse::NotRecordedAtBarrier { .. }) => ActionOutcome::NotRecordedAtBarrier,
            Ok(OwnerResponse::Read { .. }) => return Err(failure("unexpected_reply")),
            Err(_) => {
                self.ordinary = None;
                self.controller()?.detach();
                ActionOutcome::Uncertain
            }
        };
        bounded(ActionReceipt {
            baseline_id,
            outcome,
            pending: self.status().pending,
        })
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
