use crate::config::{
    EffectiveConfig, ModelConnection, ModelEndpoint, Settings, connection_destination,
};
use crate::coordination::Role;
use crate::engine::EngineError;
use crate::engine::owner::{NativeAuthFact, NativeProfileBinding, OwnerProvenance, is_hex_id};
use crate::model_gateway::Attempt;
use crate::protocol::{ConfigId, CredentialRef, Cursor, ProjectId, RequestId, SessionId, TaskId};
use crate::routing::{RouteRequest, validate_pool_scope};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const SESSIONS_MAX: usize = 64;
pub(crate) const TURNS_MAX: usize = 1024;
pub(crate) const REBINDS_MAX: usize = 32;
pub(crate) const METADATA_BYTES_MAX: usize = 16384;
pub(crate) const PAGE_BYTES_MAX: usize = 32768;
pub(crate) const PAGE_ITEMS_MAX: u8 = 32;
/// The most a master reply spends in its stored turn, measured as JSON: 12 KiB of the
/// 16 KiB turn body (`METADATA_BYTES_MAX`, the `master_turns` CHECK), whose attempt, runtime
/// and provenance take the rest (under 2.3 KiB on the owner's turns). Clients read whole
/// turns, so this is also the most of a reply they show.
pub(crate) const REPLY_BYTES_MAX: usize = 12 * 1024;

/// Measures a reply as its stored turn holds it: the bytes of its JSON string, quotes and
/// escapes included. Input: the text. Output: the byte count.
pub(crate) fn reply_bytes(text: &str) -> usize {
    serde_json::to_string(text).map_or(usize::MAX, |json| json.len())
}

/// Fits a master's reply into its stored turn: whole when it measures within
/// `REPLY_BYTES_MAX`, otherwise its start and a last line saying how many bytes were left
/// out. Inputs: the text kept and the bytes already left out after it. Output: the reply.
pub(crate) fn fitted_reply(text: &str, dropped: usize) -> String {
    if dropped == 0 && reply_bytes(text) <= REPLY_BYTES_MAX {
        return text.to_owned();
    }
    let mut kept = text.len();
    loop {
        let fitted = format!(
            "{}\n\n[{} more bytes of this reply were left out.]",
            &text[..kept],
            text.len() - kept + dropped
        );
        let excess = reply_bytes(&fitted).saturating_sub(REPLY_BYTES_MAX);
        if excess == 0 {
            return fitted;
        }
        kept = text.floor_char_boundary(kept.saturating_sub(excess));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RcliConfiguredProfileProof {
    pub(crate) registration_id: RequestId,
    pub(crate) generation: u64,
    pub(crate) database_instance: String,
    pub(crate) engine_epoch: String,
    pub(crate) config_id: ConfigId,
    pub(crate) source_hash: String,
    pub(crate) at_revision: Cursor,
    pub(crate) connection: ModelConnection,
    pub(crate) profile_ref: CredentialRef,
    pub(crate) custody_id: String,
    pub(crate) profile_config_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum MasterBootstrapAuth {
    NativeSignIn(NativeAuthFact),
    RcliConfigured(RcliConfiguredProfileProof),
}

impl MasterBootstrapAuth {
    pub(crate) fn binding(&self) -> NativeProfileBinding {
        match self {
            Self::NativeSignIn(fact) => fact.binding.clone(),
            Self::RcliConfigured(proof) => NativeProfileBinding {
                database_instance: proof.database_instance.clone(),
                engine_epoch: proof.engine_epoch.clone(),
                config_id: proof.config_id.clone(),
                source_hash: proof.source_hash.clone(),
                at_revision: proof.at_revision.clone(),
                connection: proof.connection.clone(),
                custody_id: proof.custody_id.clone(),
            },
        }
    }
}

impl From<&NativeAuthFact> for MasterBootstrapAuth {
    fn from(fact: &NativeAuthFact) -> Self {
        Self::NativeSignIn(fact.clone())
    }
}

impl From<&MasterBootstrapAuth> for MasterBootstrapAuth {
    fn from(proof: &MasterBootstrapAuth) -> Self {
        proof.clone()
    }
}

/// The backend that runs a project's master: one of the three the engine hosts itself, or
/// another harness of the registry whose manifest names the master role, by its catalog id.
/// It is stored and sent as one word: `rcli`, `codex`, `claude`, or that catalog id.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MasterBackend {
    #[default]
    Rcli,
    Codex,
    Claude,
    Listed(&'static str),
}

impl MasterBackend {
    /// The three backends the engine hosts itself, in the order the master form lists them.
    pub const NATIVE: [Self; 3] = [Self::Rcli, Self::Codex, Self::Claude];

    /// The stored word of this backend. Takes the backend. Returns `rcli`, `codex`,
    /// `claude`, or a listed harness's catalog id.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rcli => "rcli",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Listed(id) => id,
        }
    }

    /// Reads a stored word: one of the three native words, or the exact catalog id of a
    /// listed harness whose manifest names the master role. Takes the text. Returns the
    /// backend, or None for any other word, the catalog id `claude-code` among them.
    pub fn parse(text: &str) -> Option<Self> {
        let native = Self::NATIVE
            .into_iter()
            .find(|backend| backend.as_str() == text);
        native.or_else(|| {
            crate::harnesses::registry::registry()
                .get(text)
                .and_then(Self::of)
                .filter(|backend| matches!(backend, Self::Listed(id) if *id == text))
        })
    }

    /// Names the backend that runs a master on one harness of a registry. Takes the
    /// harness. Returns Codex, Claude or RCLI for the harness the engine hosts itself,
    /// Listed with its id for any other harness whose manifest names the master role, None
    /// for a harness without that role.
    pub fn of(harness: &'static crate::harnesses::registry::Harness) -> Option<Self> {
        use crate::coord::settings::HarnessKind;
        if !harness.master() {
            return None;
        }
        match HarnessKind::of(harness)? {
            HarnessKind::Codex => Some(Self::Codex),
            HarnessKind::ClaudeCode => Some(Self::Claude),
            HarnessKind::Rcli => Some(Self::Rcli),
            HarnessKind::Listed(id) => Some(Self::Listed(id)),
        }
    }
}

impl Serialize for MasterBackend {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MasterBackend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Self::parse(&word).ok_or_else(|| serde::de::Error::custom("unknown master backend"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterSessionPhase {
    Configured,
    Parked,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterTurnPhase {
    Queued,
    Blocked,
    Prepared,
    Delivering,
    Running,
    WaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
    Uncertain,
}

impl MasterTurnPhase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Blocked => "blocked",
            Self::Prepared => "prepared",
            Self::Delivering => "delivering",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
        }
    }

    pub(crate) fn outstanding(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Prepared
                | Self::Delivering
                | Self::Running
                | Self::WaitingApproval
                | Self::Uncertain
        )
    }
}

/// The cause a master turn stops with when the engine could not write a pooled keyed RCLI
/// account's saved API key into its generated home before the launch (KY-M1).
pub const RCLI_KEY_SETUP_CAUSE: &str = "rcli_master_key_setup_required";

/// The line both apps show for a turn stopped with RCLI_KEY_SETUP_CAUSE (DEC-141): each
/// app's way to set the account up again.
pub const RCLI_KEY_SETUP_LINE: &str = "The master could not set this account's API key up for agents. Set it up again: in the app, Settings, Pools, Get ready for agents; in the terminal app, Model access, k Private profile setup.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MasterReason {
    ReadinessUnknown,
    ReadinessStale,
    InvalidAuthentication,
    QuotaUnknown,
    QuotaExhausted,
    RateLimited,
    CooldownActive,
    TransientFailure,
    ConnectionChanged,
    ScopeRevoked,
    CapacityUnavailable,
    BudgetExhausted,
    ProfileBusy,
    ApprovalRequired,
    RuntimeUnavailable,
    CaptureIncomplete,
    NoReply,
    StepLimit,
    TimeLimit,
    OutputLimit,
    UnsupportedCapability,
    RepositoryMissing,
    RepositoryConfigRefused,
    InstructionsTooLarge,
    ProtocolFault,
    FrameLimit,
    WorktreeUncertain,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterSelection {
    #[serde(default)]
    pub backend: MasterBackend,
    pub pool_ref: CredentialRef,
    pub target_profile_ref: CredentialRef,
    pub route: RouteRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterSessionView {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub bootstrap_task_id: TaskId,
    pub selection: MasterSelection,
    pub config_id: ConfigId,
    pub source_hash: String,
    pub phase: MasterSessionPhase,
    pub version: Cursor,
    pub current_turn: Option<RequestId>,
    pub owner: OwnerProvenance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rebound: Vec<MasterBinding>,
}

/// Settings and engine run an open master session was moved to after a change outside
/// its own scope or an engine restart (DEC-092, LB-23). Input: the config and source hash
/// it now runs under, the engine epoch when the send moved it to a new run, and the owner
/// revision of that send. The Open binding stays in the session itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterBinding {
    pub config_id: ConfigId,
    pub source_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_epoch: Option<String>,
    pub at: Cursor,
}

impl MasterSessionView {
    /// The settings the session runs under now: its latest rebinding, else its Open.
    /// Output: the config id and source hash a send or a turn must match.
    pub(crate) fn binding(&self) -> (&ConfigId, &str) {
        self.rebound
            .last()
            .map_or((&self.config_id, self.source_hash.as_str()), |binding| {
                (&binding.config_id, binding.source_hash.as_str())
            })
    }

    /// The engine run the session runs under now: the epoch of its latest rebinding that
    /// moved it to a new run, else the run it was opened in.
    /// Output: the engine epoch a send, a turn or a bootstrap must match.
    pub(crate) fn engine_epoch(&self) -> &str {
        self.rebound
            .iter()
            .rev()
            .find_map(|binding| binding.engine_epoch.as_deref())
            .unwrap_or(&self.owner.engine_epoch)
    }

    /// Whether a turn's attempt made under `config` belongs to this session. Input: the
    /// attempt's config and whether its turn is still live. Output: a live attempt needs the
    /// current binding; a settled one any binding the session has had.
    pub(crate) fn admits_attempt(&self, config: &ConfigId, live: bool) -> bool {
        if live {
            return self.binding().0 == config;
        }
        &self.config_id == config
            || self
                .rebound
                .iter()
                .any(|binding| &binding.config_id == config)
    }

    /// Whether settings a turn was prepared under belong to this session, by the rule of
    /// [`Self::admits_attempt`]. Input: the config, source hash and whether the turn is
    /// still live. Output: a live turn needs the current binding; a settled one any binding
    /// the session has had.
    pub(crate) fn admits_binding(&self, config: &ConfigId, source_hash: &str, live: bool) -> bool {
        if live {
            return self.binding() == (config, source_hash);
        }
        (&self.config_id, self.source_hash.as_str()) == (config, source_hash)
            || self.rebound.iter().any(|binding| {
                (&binding.config_id, binding.source_hash.as_str()) == (config, source_hash)
            })
    }

    /// Checks the stored rebindings: bounded, well formed, made after the Open in
    /// revision order up to the session's version, each a real change.
    /// Output: false when the record cannot have come from `rebind`.
    pub(crate) fn rebinds_valid(&self) -> bool {
        let mut previous = (
            &self.config_id,
            self.source_hash.as_str(),
            self.owner.engine_epoch.as_str(),
            self.owner.revision_after.value(),
        );
        if self.rebound.len() > REBINDS_MAX {
            return false;
        }
        for binding in &self.rebound {
            let epoch = binding.engine_epoch.as_deref().unwrap_or(previous.2);
            if !is_hex_id(binding.config_id.as_str())
                || !is_hex_id(&binding.source_hash)
                || !is_hex_id(epoch)
                || binding.engine_epoch.as_deref() == Some(previous.2)
                || binding.at.value() <= previous.3
                || binding.at.value() > self.version.value()
                || (&binding.config_id, binding.source_hash.as_str(), epoch)
                    == (previous.0, previous.1, previous.2)
            {
                return false;
            }
            previous = (
                &binding.config_id,
                binding.source_hash.as_str(),
                epoch,
                binding.at.value(),
            );
        }
        true
    }
}

/// The part of the settings an open master runs on (DEC-092): every setting except the
/// pools, connections, accounts and account approvals outside the session's own pool, the
/// other projects its pool serves, and, when the route pins its target, destinations that
/// are neither the master's targets, its pool's targets nor knowledge processing destinations.
/// Input: a config, the session's project and its selection. Output: the settings to compare.
pub(crate) fn master_scope(
    config: &EffectiveConfig,
    project: &ProjectId,
    selection: &MasterSelection,
) -> Settings {
    let mut settings = config.settings.clone();
    let mut destinations: Vec<CredentialRef> = [
        Some(&selection.target_profile_ref),
        selection.route.preferred_target.as_ref(),
        selection.route.required_target.as_ref(),
    ]
    .into_iter()
    .flatten()
    .chain(&settings.knowledge.processing_destinations)
    .cloned()
    .collect();
    if let Some(access) = settings.model_access.as_mut() {
        access.pools.retain(|pool| pool.id == selection.pool_ref);
        for pool in &mut access.pools {
            pool.projects
                .retain(|held| held.as_str() == project.as_str());
        }
        access.connections.retain(|connection| {
            access
                .pools
                .iter()
                .any(|pool| pool.members.contains(&connection.id))
        });
        let accounts: Vec<&CredentialRef> = access
            .connections
            .iter()
            .map(|connection| &connection.account_ref)
            .chain([&selection.route.account_ref])
            .collect();
        access
            .accounts
            .retain(|account| accounts.contains(&account));
        settings
            .routing
            .approved_accounts
            .retain(|account| accounts.contains(&account));
        destinations.extend(access.connections.iter().filter_map(connection_destination));
    }
    if selection.route.required_target.is_some() {
        for list in [
            &mut settings.allowed_destinations,
            &mut settings.revoked_destinations,
            &mut settings.routing.preference_order,
        ] {
            list.retain(|destination| destinations.contains(destination));
        }
    }
    settings
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RcliProfileRegistration {
    pub expected_session: MasterSessionView,
    pub expected_connection: ModelConnection,
    pub configured_home: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RcliProfileRegistrationMetadata {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub profile_ref: CredentialRef,
    pub registration_id: RequestId,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterRuntimeView {
    pub runtime_ref: RequestId,
    pub artifact_sha256: String,
    pub native_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterReplyProvenance {
    pub attempt_id: RequestId,
    pub runtime_ref: RequestId,
    pub capture_ref: RequestId,
    pub capture_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterReply {
    pub text: String,
    pub provenance: MasterReplyProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterTurnView {
    pub id: RequestId,
    pub session_id: SessionId,
    pub ordinal: u64,
    pub input_cursor: Cursor,
    pub phase: MasterTurnPhase,
    pub reason: Option<MasterReason>,
    pub attempt: Option<Attempt>,
    pub runtime: Option<MasterRuntimeView>,
    pub reply: Option<MasterReply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<MasterAccountState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<MasterTask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_request: Option<MasterEngineRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activity: Vec<MasterActivityLine>,
}

/// One line of what a running master turn is doing, as the engine words it from the
/// harness's stream: a fixed phrase with at most one screened project-relative path, and
/// when the engine last saw it. Held in the engine's memory while the turn runs and added
/// when turns are read; never stored with the turn (OB-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterActivityLine {
    pub at_ms: u64,
    pub text: String,
}

/// What the engine asked the master in a turn the owner started with Ask agent to update
/// (DEC-095): the change set, the plain line the owner sees in the chat and the text the
/// master receives. The engine writes it from the change set store; no client supplies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterEngineRequest {
    pub change_set: crate::landing::ChangeSetId,
    pub line: String,
    pub text: String,
}

/// One task a master turn handed to the project's team, with the engine's note on how it
/// ended once it has. Fields: the team task id and the note, none while the task is
/// still waiting or running. Filled when turns are read; never stored with the turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterTask {
    pub task: u64,
    pub note: Option<String>,
}

impl MasterTask {
    /// Words this task for the conversation: the engine's note once it ended, otherwise
    /// that it is still with a sub-agent. Input: none. Output: one line.
    pub fn line(&self) -> String {
        match &self.note {
            Some(note) => note.clone(),
            None => format!("Task {} is still with a sub-agent.", self.task),
        }
    }
}

/// One pool account named on a master turn that no account of its harness could run.
/// Fields: the account, its connection, the connection's harness and why it cannot run
/// the turn; a reason of None marks a ready account of another harness in the pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterAccountState {
    pub account_ref: CredentialRef,
    pub connection: CredentialRef,
    pub harness: CredentialRef,
    pub reason: Option<MasterReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterTurnPage {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub turns: Vec<MasterTurnView>,
    pub next: Option<MasterCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterCursor {
    pub db_instance: String,
    pub owner_id: String,
    pub owner_epoch: i64,
    pub engine_epoch: String,
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub config_id: ConfigId,
    pub source_hash: String,
    pub version: Cursor,
    pub after_ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MasterOwnerAction {
    Open {
        project_id: ProjectId,
        expected_config_id: ConfigId,
        expected_source_hash: String,
        selection: Box<MasterSelection>,
    },
    Submit {
        project_id: ProjectId,
        session_id: SessionId,
        text: String,
    },
    Cancel {
        project_id: ProjectId,
        session_id: SessionId,
        turn_id: RequestId,
    },
    Close {
        project_id: ProjectId,
        session_id: SessionId,
    },
    NewSession {
        project_id: ProjectId,
        expected_config_id: ConfigId,
        expected_source_hash: String,
        selection: Box<MasterSelection>,
        cancel_running: bool,
    },
    Resume {
        project_id: ProjectId,
        session_id: SessionId,
        cancel_running: bool,
    },
    Resolve {
        project_id: ProjectId,
        session_id: SessionId,
        change_set: crate::landing::ChangeSetId,
    },
    DeleteSession {
        project_id: ProjectId,
        session_id: SessionId,
    },
    RegisterRcliProfile {
        registration: Box<RcliProfileRegistration>,
    },
}

/// Turns the engine error that stopped a master turn into the code the turn stores.
/// Input: the raw error text. Output: the text itself when it is a plain code of at most
/// 192 lowercase letters, digits, `_`, `:` or `;`, else a redacted marker, so provider
/// text, paths and secrets never reach the stored turn.
pub(crate) fn cause_code(raw: &str) -> String {
    let plain = !raw.is_empty()
        && raw.len() <= CAUSE_MAX
        && raw.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b':' | b';')
        });
    let code = if plain { raw } else { CAUSE_REDACTED };
    assert!(!code.is_empty() && code.len() <= CAUSE_MAX);
    code.to_owned()
}

const CAUSE_MAX: usize = 192;
const CAUSE_REDACTED: &str = "master_cause_redacted";

pub(crate) fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

pub(crate) fn validate_action(action: &MasterOwnerAction) -> Result<(), EngineError> {
    match action {
        MasterOwnerAction::Open {
            expected_config_id,
            expected_source_hash,
            selection,
            ..
        }
        | MasterOwnerAction::NewSession {
            expected_config_id,
            expected_source_hash,
            selection,
            ..
        } => {
            if !is_hex_id(expected_config_id.as_str()) || !is_hex_id(expected_source_hash) {
                return Err(error("master_source_basis"));
            }
            selection.route.validate().map_err(error)?;
        }
        MasterOwnerAction::Submit { text, .. } => {
            if text.trim().is_empty() || text.len() > 8192 || text.contains('\0') {
                return Err(error("master_text_bounds"));
            }
        }
        MasterOwnerAction::Cancel { .. }
        | MasterOwnerAction::Close { .. }
        | MasterOwnerAction::Resume { .. }
        | MasterOwnerAction::Resolve { .. }
        | MasterOwnerAction::DeleteSession { .. }
        | MasterOwnerAction::RegisterRcliProfile { .. } => {}
    }
    Ok(())
}

pub(crate) fn validate_page(cursor: Option<&MasterCursor>, limit: u8) -> Result<(), EngineError> {
    if !(1..=PAGE_ITEMS_MAX).contains(&limit) {
        return Err(error("master_page_limit"));
    }
    if let Some(cursor) = cursor
        && (!is_hex_id(&cursor.db_instance)
            || !is_hex_id(&cursor.owner_id)
            || !is_hex_id(&cursor.engine_epoch)
            || !is_hex_id(cursor.config_id.as_str())
            || !is_hex_id(&cursor.source_hash)
            || cursor.owner_epoch < 1
            || cursor.version.value() > i64::MAX.unsigned_abs()
            || cursor.after_ordinal == 0
            || cursor.after_ordinal > u64::try_from(TURNS_MAX).map_err(error)?)
    {
        return Err(error("master_cursor_bounds"));
    }
    Ok(())
}

pub(crate) fn validate_selection(
    config: &EffectiveConfig,
    project: &ProjectId,
    selection: &MasterSelection,
) -> Result<(), EngineError> {
    if let MasterBackend::Listed(_) = selection.backend {
        return Err(error("launch_harness_unsupported"));
    }
    selection.route.validate().map_err(error)?;
    validate_pool_scope(
        config,
        &selection.pool_ref,
        project,
        &Role::ProjectLeader,
        &selection.route.account_ref,
    )
    .map_err(error)?;
    let authorized = |profile: &CredentialRef| {
        config.settings.allowed_destinations.contains(profile)
            && !config.settings.revoked_destinations.contains(profile)
    };
    if !authorized(&selection.target_profile_ref)
        || [
            &selection.route.preferred_target,
            &selection.route.required_target,
        ]
        .into_iter()
        .flatten()
        .any(|profile| !authorized(profile))
        || selection
            .route
            .required_target
            .as_ref()
            .is_some_and(|profile| profile != &selection.target_profile_ref)
        || !selection
            .route
            .resources
            .fits(&config.settings.routing.task_capacity)
    {
        return Err(error("master_scope_or_budget_denied"));
    }
    let catalog = config
        .settings
        .model_access
        .as_ref()
        .ok_or_else(|| error("master_pool_missing"))?;
    let pool = catalog
        .pools
        .iter()
        .find(|pool| pool.id == selection.pool_ref)
        .ok_or_else(|| error("master_pool_missing"))?;
    if !catalog.connections.iter().any(|connection| {
        connection.account_ref == selection.route.account_ref
            && pool.members.contains(&connection.id)
            && backend_member(selection.backend, connection)
    }) {
        return Err(error("master_backend_account_denied"));
    }
    Ok(())
}

/// Finds the harness a listed backend runs a master on: one of the registry whose manifest
/// names the master role and drives it on pipes over ACP, the listed master driver the
/// engine runs (DEC-180 Task 20). Input: the catalog id. Output: the harness, or
/// `launch_harness_unsupported` for any other.
pub(crate) fn listed_master(
    id: &str,
) -> Result<&'static crate::harnesses::registry::Harness, EngineError> {
    let _ = id;
    Err(error("launch_harness_unsupported"))
}

/// Tells whether a connection can run a master of this backend.
/// Inputs: the backend and the connection. Output: true for an RCLI HTTP connection, or
/// for a native connection of the Codex or Claude harness the backend names; false for a
/// listed backend, which no connection runs until the engine hosts a master on it.
pub(crate) fn backend_member(backend: MasterBackend, connection: &ModelConnection) -> bool {
    match backend {
        MasterBackend::Rcli => {
            connection.harness.as_str() == "rcli"
                && matches!(connection.endpoint, ModelEndpoint::Http { .. })
        }
        MasterBackend::Codex => {
            connection.harness.as_str() == "codex"
                && connection.protocol.as_str() == "native"
                && matches!(connection.endpoint, ModelEndpoint::Native {})
        }
        MasterBackend::Claude => {
            matches!(connection.harness.as_str(), "claude" | "claude-code")
                && connection.protocol.as_str() == "native"
                && matches!(connection.endpoint, ModelEndpoint::Native {})
        }
        MasterBackend::Listed(_) => false,
    }
}

#[cfg(test)]
mod cause_code_tests {
    use super::cause_code;

    #[test]
    fn only_a_plain_bounded_code_is_stored_and_anything_else_is_redacted() {
        for plain in [
            "rcli_profile_configuration_required",
            "repository_config_refused:core_hookspath",
            "rcli_master_cleanup_incomplete;rcli_master_wait_failed",
            &"a".repeat(192),
        ] {
            assert_eq!(cause_code(plain), plain);
        }
        for private in [
            "",
            "Error: token sk-live-123 rejected",
            "/private/tmp/owner/repo: permission denied",
            "gateway rate limited",
            "Rcli_profile",
            "caf\u{e9}_code",
            &"a".repeat(193),
        ] {
            assert_eq!(cause_code(private), "master_cause_redacted", "{private}");
        }
    }
}

#[cfg(test)]
mod backend_tests {
    use super::{MasterBackend, MasterSelection, backend_member, error, listed_master};
    use crate::config::ModelConnection;
    use crate::protocol::CredentialRef;
    use crate::routing::{ResourceBudget, RouteRequest};

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    fn selection(backend: MasterBackend) -> MasterSelection {
        MasterSelection {
            backend,
            pool_ref: reference("pool"),
            target_profile_ref: reference("connection"),
            route: RouteRequest {
                preferred_target: Some(reference("connection")),
                required_target: Some(reference("connection")),
                required_capabilities: vec![],
                account_ref: reference("account"),
                budget_ref: reference("conversation-budget"),
                resources: ResourceBudget::unit(),
            },
        }
    }

    /// DEC-180 Phase 1: a master's backend is stored and sent as `rcli`, `codex` or
    /// `claude`, a selection stored without one reads as RCLI, and no other word is taken:
    /// not the catalog id `claude-code`, and none of the 15 other catalog harnesses.
    #[test]
    fn a_master_backend_is_stored_as_rcli_codex_or_claude_and_as_no_other_word() {
        for (backend, word) in [
            (MasterBackend::Rcli, "rcli"),
            (MasterBackend::Codex, "codex"),
            (MasterBackend::Claude, "claude"),
        ] {
            let stored = format!("\"{word}\"");
            assert_eq!(serde_json::to_string(&backend).unwrap(), stored);
            assert_eq!(
                serde_json::from_str::<MasterBackend>(&stored).unwrap(),
                backend
            );
            let mut value = serde_json::to_value(selection(backend)).unwrap();
            assert_eq!(value["backend"], serde_json::json!(word));
            assert_eq!(
                serde_json::from_value::<MasterSelection>(value.clone()).unwrap(),
                selection(backend)
            );
            value.as_object_mut().unwrap().remove("backend");
            assert_eq!(
                serde_json::from_value::<MasterSelection>(value)
                    .unwrap()
                    .backend,
                MasterBackend::Rcli
            );
        }
        assert_eq!(MasterBackend::default(), MasterBackend::Rcli);
        for word in [
            "claude-code",
            "opencode",
            "gemini-cli",
            "fx",
            "openclaw",
            "pi",
            "goose",
            "aider",
            "qwen-code",
            "crush",
            "droid",
            "copilot",
            "cline",
            "continue",
            "openhands",
            "kimi",
            "",
            "Codex",
            "listed",
            "no-such-harness",
        ] {
            let sent = serde_json::to_string(word).unwrap();
            assert!(
                serde_json::from_str::<MasterBackend>(&sent).is_err(),
                "{word}"
            );
            let mut value = serde_json::to_value(selection(MasterBackend::Codex)).unwrap();
            value["backend"] = serde_json::json!(word);
            assert!(
                serde_json::from_value::<MasterSelection>(value).is_err(),
                "{word}"
            );
        }
    }

    fn manifest(id: &str, role: &str) -> String {
        format!(
            "schema: 1\nkind: harness\nid: \"{id}\"\nname: \"Sample\"\nbinaries:\n  - \"{id}-bin\"\nheadless: \"sample --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\nroles:\n  - \"{role}\"\n"
        )
    }

    /// DEC-180 Phase 1: the backend of a master follows the manifest of its harness. A
    /// harness that is not Codex, Claude Code or RCLI gets a listed backend, stored under
    /// its own id, the moment its manifest names the master role, and no bundled manifest
    /// does that yet, so a team plan's master maps as it did.
    #[test]
    fn a_harness_gets_a_listed_backend_when_its_manifest_names_the_master_role() {
        use crate::harnesses::registry::{Registry, registry};
        let files = [
            ("master-only", manifest("master-only", "master")),
            ("agent-only", manifest("agent-only", "agent")),
            ("codex", manifest("codex", "master")),
            ("claude-code", manifest("claude-code", "master")),
            ("rcli", manifest("rcli", "master")),
        ];
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(file, text)| (*file, text.as_str()))
            .collect();
        let built: &'static Registry = Box::leak(Box::new(Registry::build(&borrowed).unwrap()));
        let backend = |id: &str| MasterBackend::of(built.get(id).unwrap());
        let listed = MasterBackend::Listed("master-only");
        assert_eq!(backend("master-only"), Some(listed));
        assert_eq!(backend("agent-only"), None);
        assert_eq!(backend("codex"), Some(MasterBackend::Codex));
        assert_eq!(backend("claude-code"), Some(MasterBackend::Claude));
        assert_eq!(backend("rcli"), Some(MasterBackend::Rcli));
        assert_eq!(listed.as_str(), "master-only");
        assert_eq!(serde_json::to_string(&listed).unwrap(), "\"master-only\"");
        assert_eq!(
            serde_json::to_value(selection(listed)).unwrap()["backend"],
            serde_json::json!("master-only")
        );
        assert_eq!(MasterBackend::parse("master-only"), None);
        assert_eq!(
            MasterBackend::NATIVE.map(MasterBackend::as_str),
            ["rcli", "codex", "claude"]
        );

        let mut masters = Vec::new();
        for harness in registry().all() {
            let id = harness.id();
            let named = MasterBackend::of(harness);
            assert_eq!(named.is_some(), harness.master(), "{id}");
            assert_eq!(crate::team::master_backend(id), named, "{id}");
            assert!(!matches!(named, Some(MasterBackend::Listed(_))), "{id}");
            masters.extend(named);
        }
        assert_eq!(
            masters,
            [
                MasterBackend::Codex,
                MasterBackend::Claude,
                MasterBackend::Rcli
            ]
        );
        assert_eq!(
            crate::team::master_backend("claude"),
            Some(MasterBackend::Claude)
        );
    }

    fn connection(harness: &str, native: bool) -> ModelConnection {
        let (protocol, endpoint, auth) = if native {
            (
                "native",
                serde_json::json!({"kind": "native"}),
                serde_json::json!({"method": "native_managed", "profile_ref": "profile"}),
            )
        } else {
            (
                "openai-chat",
                serde_json::json!({"kind": "http", "url": "http://127.0.0.1:11434/v1"}),
                serde_json::json!({"method": "none"}),
            )
        };
        serde_json::from_value(serde_json::json!({
            "id": "connection",
            "provider": "provider",
            "harness": harness,
            "protocol": protocol,
            "account_ref": "account",
            "endpoint": endpoint,
            "model": {"id": "model", "author": "author"},
            "auth": auth,
        }))
        .unwrap()
    }

    /// DEC-180 Task 20: a connection runs a master on a listed backend exactly when the
    /// harness fits its account: an endpoint account of a wire format it speaks, whatever
    /// harness stored it, never a sign-in; RCLI, Codex and Claude keep the connections they
    /// ran on, and a listed harness without the master role runs on none.
    #[test]
    fn a_connection_runs_a_master_on_a_listed_backend_when_the_harness_fits_its_account() {
        let connections = [
            connection("rcli", false),
            connection("codex", true),
            connection("claude-code", true),
            connection("claude", true),
            connection("opencode", true),
            connection("opencode", false),
        ];
        let runs =
            |backend: MasterBackend| connections.each_ref().map(|c| backend_member(backend, c));
        assert_eq!(
            runs(MasterBackend::Rcli),
            [true, false, false, false, false, false]
        );
        assert_eq!(
            runs(MasterBackend::Codex),
            [false, true, false, false, false, false]
        );
        assert_eq!(
            runs(MasterBackend::Claude),
            [false, false, true, true, false, false]
        );
        assert_eq!(
            runs(MasterBackend::Listed("standin-acp")),
            [true, false, false, false, false, true]
        );
        assert_eq!(runs(MasterBackend::Listed("opencode")), [false; 6]);
        assert_eq!(runs(MasterBackend::Listed("no-such-harness")), [false; 6]);
    }

    /// DEC-180 Task 20: a listed backend runs a master once its harness's manifest names
    /// the master role and drives it over ACP; a harness without the role, a terminal
    /// stand-in with no piped driver, or a name of no registry, is refused with its code.
    #[test]
    fn a_listed_backend_runs_a_master_when_its_manifest_names_the_role_and_an_acp_driver() {
        let found = listed_master("standin-acp").map(|harness| harness.id());
        assert_eq!(found, Ok("standin-acp"));
        let refused = Some(error("launch_harness_unsupported"));
        for id in ["opencode", "standin-screen", "no-such-harness", "codex"] {
            assert_eq!(listed_master(id).err(), refused, "{id}");
        }
        let parsed = MasterBackend::parse("standin-acp");
        assert_eq!(parsed, Some(MasterBackend::Listed("standin-acp")));
    }
}
