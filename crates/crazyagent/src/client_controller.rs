use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::engine::owner::{
    NativeAuthCancel, NativeAuthLogout, NativeAuthQuery, NativeAuthReply, NativeAuthStart,
    OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerPageCursor,
    OwnerRead, OwnerReadReply, OwnerRequest, OwnerResponse, OwnerView,
};
use crate::engine::{Client, EngineError};
use crate::protocol::{Cursor, PROTOCOL_VERSION, RequestId};
use std::path::Path;
use std::time::Duration;

mod account_profile;
mod project_setup;
mod owner_setup;
mod accounts;
mod plugins;
mod memory;
pub use account_profile::PendingAccountProfile;

impl OwnerController {
    /// Lists the engine's harness catalog for the master, sub-agent and research pickers.
    /// Input: none; uses the attached owner connection. Output: entries in owner
    /// priority order, or `controller_disconnected` without a connection.
    pub async fn harness_catalog(&self) -> Result<Vec<crate::harnesses::HarnessEntry>, EngineError> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?;
        client.harness_catalog().await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerView {
    pub state: OwnerReadReply,
    pub fresh: bool,
}

pub struct OwnerController {
    client: Option<OwnerClient>,
    identity: OwnerConnectionIdentity,
    view: Option<ControllerView>,
    pending: Option<PendingMutation>,
    pending_account_profile: Option<PendingAccountProfile>,
    kind: OwnerClientKind,
    timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMutation {
    request: OwnerRequest,
    hash: String,
    identity: OwnerConnectionIdentity,
    generation: u64,
    uncertain: bool,
}

impl PendingMutation {
    /// Borrows the exact submitted request including original revision.
    /// Takes a pending intent without changing its identity.
    /// Returns immutable native content bounded by owner validation.
    pub fn request(&self) -> &OwnerRequest {
        &self.request
    }

    /// Borrows the public owner mutation hash of the original request.
    /// Takes the pending intent without recomputing or changing it.
    /// Returns its exact content and request identity binding.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Returns the actual submitted native draft generation.
    /// Takes the pending intent independently of current editor state.
    /// Identical replacement drafts do not replace this generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Reports whether delivery may have reached the owner.
    /// Takes the pending intent without asserting nonapplication.
    /// Returns true after dispatch begins or its outcome is uncertain.
    pub fn uncertain(&self) -> bool {
        self.uncertain
    }
}

impl OwnerController {
    pub async fn start_native_auth(
        &mut self,
        request: NativeAuthStart,
    ) -> Result<NativeAuthReply, EngineError> {
        if self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .start_native_auth(request)
            .await
    }

    pub async fn read_native_auth(
        &mut self,
        request: NativeAuthQuery,
    ) -> Result<NativeAuthReply, EngineError> {
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .read_native_auth(request)
            .await
    }

    pub async fn cancel_native_auth(
        &mut self,
        request: NativeAuthCancel,
    ) -> Result<NativeAuthReply, EngineError> {
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .cancel_native_auth(request)
            .await
    }

    pub async fn submit_claude_auth_code(
        &mut self,
        request: crate::engine::owner::ClaudeAuthInput,
        secret: crate::engine::owner::AccountSecret,
    ) -> Result<(), EngineError> {
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        if request.query.scope.expected_database_instance != self.identity.database_instance() {
            return Err(EngineError("native_auth_database_mismatch".into()));
        }
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .submit_claude_auth_code(request, secret)
            .await
    }

    pub async fn logout_native_auth(
        &mut self,
        request: NativeAuthLogout,
    ) -> Result<NativeAuthReply, EngineError> {
        if self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .logout_native_auth(request)
            .await
    }

    /// Admits the configured ordinary endpoint with its actual owner connection.
    /// Consumes a native client and fixes its kind and configured timeout.
    /// Returns the bound ordinary client and controller without rediscovery.
    pub async fn attach_configured(
        ordinary: Client,
        kind: OwnerClientKind,
    ) -> Result<(Client, Self), EngineError> {
        let timeout = ordinary.timeout();
        let (ordinary, client, identity) = ordinary.with_owner(kind).await?;
        Ok((
            ordinary,
            Self {
                client: Some(client),
                identity,
                view: None,
                pending: None,
                pending_account_profile: None,
                kind,
                timeout,
            },
        ))
    }

    /// Discovers one authenticated native owner and its paired identity.
    /// Takes a trusted anchor, fixed client kind and bounded timeout.
    /// Returns connected state without host control or credentials.
    pub async fn attach(
        anchor: &Path,
        kind: OwnerClientKind,
        timeout: Duration,
    ) -> Result<Self, EngineError> {
        let (client, identity) = OwnerClient::discover(anchor, kind, timeout).await?;
        Ok(Self {
            client: Some(client),
            identity,
            view: None,
            pending: None,
            pending_account_profile: None,
            kind,
            timeout,
        })
    }

    /// Borrows the identity returned by actual authenticated discovery.
    /// Takes the controller and exposes no authority or capability.
    /// Returns the current database and engine identity.
    pub fn identity(&self) -> &OwnerConnectionIdentity {
        &self.identity
    }

    /// Borrows the last bounded native read and its freshness.
    /// Takes the controller without issuing any I/O.
    /// Returns absence before the first successful read.
    pub fn view(&self) -> Option<&ControllerView> {
        self.view.as_ref()
    }

    /// Borrows the one unresolved exact native mutation.
    /// Takes the controller without sending or editing intent.
    /// Returns absence only when no mutation is retained.
    pub fn pending(&self) -> Option<&PendingMutation> {
        self.pending.as_ref()
    }

    /// Disconnects this client without stopping the shared engine.
    /// Takes exclusive state and preserves any exact pending request.
    /// Marks cached reads stale until an authoritative refresh.
    pub fn detach(&mut self) {
        self.client = None;
        if let Some(view) = &mut self.view {
            view.fresh = false;
        }
    }

    /// Discovers a current native connection for this fixed client kind.
    /// Takes a trusted anchor and preserves pending native intent.
    /// Returns actual discovery failure without repairing protected state.
    pub async fn reattach(&mut self, anchor: &Path) -> Result<(), EngineError> {
        self.detach();
        let (client, identity) = OwnerClient::discover(anchor, self.kind, self.timeout).await?;
        self.install_connection(client, identity)
    }

    /// Reconnects this controller through configured native endpoint admission.
    /// Consumes a client while retaining pending intent and the fixed owner kind.
    /// Returns its bound ordinary peer only after database fencing succeeds.
    pub async fn reattach_configured(&mut self, ordinary: Client) -> Result<Client, EngineError> {
        self.detach();
        let timeout = ordinary.timeout();
        let (ordinary, client, identity) = ordinary.with_owner(self.kind).await?;
        self.install_connection(client, identity)?;
        self.timeout = timeout;
        Ok(ordinary)
    }

    fn install_connection(
        &mut self,
        client: OwnerClient,
        identity: OwnerConnectionIdentity,
    ) -> Result<(), EngineError> {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.identity.database_instance() != identity.database_instance())
        {
            return Err(EngineError("controller_pending_database_mismatch".into()));
        }
        if self.pending_account_profile.as_ref().is_some_and(|pending|
            pending.identity.database_instance() != identity.database_instance()) {
            return Err(EngineError("controller_pending_database_mismatch".into()));
        }
        if self.identity.database_instance() != identity.database_instance() {
            self.view = None;
        }
        self.client = Some(client);
        self.identity = identity;
        Ok(())
    }

    /// Prepares one exact mutation from a fresh authoritative read.
    /// Takes validated typed content and its submitted native generation.
    /// Returns immutable retained intent before any mutation I/O.
    pub fn begin_mutation(
        &mut self,
        mutation: OwnerMutation,
        generation: u64,
    ) -> Result<&PendingMutation, EngineError> {
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        if self.client.is_none() || generation == 0 {
            return Err(EngineError("controller_not_ready".into()));
        }
        let view = self
            .view
            .as_ref()
            .filter(|v| v.fresh)
            .ok_or_else(|| EngineError("controller_stale_view".into()))?;
        let request = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(crate::engine::owner::random_id()?)
                .map_err(|e| EngineError(e.to_string()))?,
            expected_revision: view.state.revision.clone(),
            operation: OwnerOperation::Apply { mutation },
        };
        let hash = request.mutation_hash()?;
        self.pending = Some(PendingMutation {
            request,
            hash,
            identity: self.identity.clone(),
            generation,
            uncertain: false,
        });
        Ok(self.pending.as_ref().unwrap())
    }

    /// Sends the prepared mutation through the actual owner client.
    /// Takes exclusive access and uses the stored exact request.
    /// Returns the actual typed owner response or transport error.
    pub async fn send_pending(&mut self) -> Result<OwnerResponse, EngineError> {
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| EngineError("controller_no_pending_intent".into()))?;
        if pending.identity.database_instance() != self.identity.database_instance() {
            return Err(EngineError("controller_pending_database_mismatch".into()));
        }
        pending.uncertain = true;
        let request = pending.request.clone();
        if let Some(view) = &mut self.view {
            view.fresh = false;
        }
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?;
        let response = client.request(request).await?.response;
        self.settle_response(response)
    }

    /// Reconciles the original mutation through the actual owner actor.
    /// Takes exclusive state and sends a separate control request identity.
    /// Returns the real receipt or barrier observation without replaying.
    pub async fn reconcile_pending(&mut self) -> Result<OwnerResponse, EngineError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or_else(|| EngineError("controller_no_pending_intent".into()))?;
        let request = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(crate::engine::owner::random_id()?)
                .map_err(|e| EngineError(e.to_string()))?,
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Reconcile {
                operation_id: pending.request.request_id.clone(),
                expected_hash: Some(pending.hash.clone()),
            },
        };
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?;
        if let Some(view) = &mut self.view {
            view.fresh = false;
        }
        let response = client.request(request).await?.response;
        self.settle_response(response)
    }

    fn settle_response(&mut self, response: OwnerResponse) -> Result<OwnerResponse, EngineError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or_else(|| EngineError("controller_no_pending_intent".into()))?;
        match &response {
            OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
                if operation.provenance.operation_id != pending.request.request_id
                    || operation.provenance.request_hash != pending.hash
                {
                    return Err(EngineError("controller_receipt_identity_mismatch".into()));
                }
                self.pending = None;
            }
            OwnerResponse::OutcomeUnknown { operation_id }
                if operation_id != &pending.request.request_id =>
            {
                return Err(EngineError("controller_receipt_identity_mismatch".into()));
            }
            OwnerResponse::Read { .. } => {
                return Err(EngineError("controller_mutation_response".into()));
            }
            _ => {}
        }
        Ok(response)
    }

    /// Explicitly retries the exact retained original mutation.
    /// Takes exclusive state without rebasing revision or changing content.
    /// Returns the actual replay or mutation outcome.
    pub async fn retry_pending(&mut self) -> Result<OwnerResponse, EngineError> {
        self.send_pending().await
    }

    /// Reads an actual typed owner query over the concrete client.
    /// Takes one exclusive mutable borrow and a bounded query.
    /// Returns the native state or actual owner error.
    pub async fn read(&mut self, query: OwnerRead) -> Result<OwnerReadReply, EngineError> {
        if let Some(view) = &mut self.view {
            view.fresh = false;
        }
        let request = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(crate::engine::owner::random_id()?)
                .map_err(|e| EngineError(e.to_string()))?,
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read { query },
        };
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?;
        let reply = client.request(request).await?;
        match reply.response {
            OwnerResponse::Read { state } => {
                self.view = Some(ControllerView {
                    fresh: !matches!(state.data, OwnerData::ResyncRequired),
                    state: state.clone(),
                });
                Ok(state)
            }
            OwnerResponse::Rejected { code } => Err(EngineError(code)),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads one actual page with a native cursor.
    /// Takes a view, optional cursor and optional u8 limit.
    /// Defaults to sixteen rows; owner validation enforces thirty-two.
    pub async fn read_page(
        &mut self,
        view: OwnerView,
        cursor: Option<OwnerPageCursor>,
        limit: Option<u8>,
    ) -> Result<OwnerReadReply, EngineError> {
        self.read(OwnerRead::Page {
            view,
            cursor,
            limit: limit.unwrap_or(16),
        })
        .await
    }
}
