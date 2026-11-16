//! Connectors: app plugins of kind `connector` that let a project's master and agents use
//! a vendor's MCP server through the engine. This module holds who may use which connector
//! in which project, the app-wide connector settings and the owner page's view. A key is
//! never part of a view; only where it is bound is.

use crate::app_plugins::store;
use crate::app_plugins::{Origin, PluginKind, PluginStatus, RunsView, SecretView};
use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub mod approvals;
pub mod audit;
pub mod callback;
pub mod gateway;
pub mod grants;
pub mod launch;
pub mod mcp_client;
pub mod oauth;
pub mod settings;
pub mod shim;
pub mod sign_in;

pub use grants::{Grant, GrantedConnector, Role, Who};
pub use settings::{ConnectorSettings, KeepLog, NativeConnectors, SizeCap, Wait, WritePolicy};

/// The refusal for a project id that names no project.
pub const PROJECT_UNKNOWN: &str = "connector_project_unknown";

/// How the owner signs a connector in today: with a key, or not yet, for a connector whose
/// only way in is an OAuth sign-in this build does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignIn {
    Key,
    NotYet,
}

/// One connector as the owner's page lists it: what it is, where it came from, whether it
/// is trusted and has its key, what it reaches and where each secret is bound. A refused
/// file whose kind reads as connector has no `runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorView {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub origin: Origin,
    pub status: PluginStatus,
    pub sign_in: SignIn,
    pub runs: Option<RunsView>,
    pub secrets: Vec<SecretView>,
    pub sha256: String,
}

/// The owner's Connectors page: every connector, bundled first, whether the plugins folder
/// held more files than are listed, the settings with their version, and for the project
/// asked about its switches. A connector with no switch row is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorsView {
    pub connectors: Vec<ConnectorView>,
    pub more: bool,
    pub settings: ConnectorSettings,
    pub version: u64,
    pub project: Option<String>,
    pub grants: Vec<Grant>,
}

const PLAIN: &[(&str, &str)] = &[
    (PROJECT_UNKNOWN, "that project is not set up here any more."),
    (
        "connector_request_invalid",
        "the request could not be read.",
    ),
    ("connector_request_bounds", "the request is too large."),
    (
        "connector_busy",
        "too many connector calls are waiting; try again shortly.",
    ),
    (
        "connector_gateway_unavailable",
        "the engine did not answer the connector call.",
    ),
    (
        "connector_not_granted",
        "this connector is not turned on for this project and agent.",
    ),
    (
        "connector_reply_withheld",
        "the vendor's reply held the connector's key, so it was not passed on.",
    ),
    (
        "connector_tools_changed",
        "the vendor changed this connector's tools; the owner reviews them first.",
    ),
    (
        "connector_tools_too_large",
        "this connector's tool list is larger than the engine passes on.",
    ),
    (
        "connector_writes_not_yet",
        "this tool can change things at the vendor; writes arrive with the approval step.",
    ),
    (
        "connector_writes_off",
        "connector writes are set to Read-only, so this tool cannot run.",
    ),
    (
        "connector_write_denied",
        "the owner denied this call, so nothing was sent to the vendor.",
    ),
    (
        "connector_write_expired",
        "nobody answered this call in time, so nothing was sent to the vendor.",
    ),
    (
        approvals::WAITING_FULL,
        "too many connector writes are waiting for the owner; try again later.",
    ),
    (approvals::UNKNOWN, "that approval is not here."),
    (
        approvals::CLOSED,
        "that approval was already answered or ended.",
    ),
    (
        audit::QUIET_CAP_REACHED,
        "this launch made more calls that reached no vendor than the log keeps; later ones are not listed.",
    ),
    (
        "connector_outcome_unknown",
        "the engine did not answer in time; check the list again.",
    ),
    ("connectors_unavailable", "the engine could not be reached."),
    (
        "connector_address_refused",
        "the connector's address is not one the engine calls.",
    ),
    (
        "connector_arguments_invalid",
        "the tool's arguments could not be read.",
    ),
    ("connector_call_failed", "the vendor could not be reached."),
    (
        "connector_host_private",
        "the vendor's name points at a private network address, so the engine did not call it.",
    ),
    (
        "connector_host_unknown",
        "the vendor's name could not be found; check the network.",
    ),
    (
        "connector_key_missing",
        "the connector's key is gone; enter it again.",
    ),
    (
        "connector_needs_input",
        "the vendor asked a question this build cannot pass on.",
    ),
    (
        "connector_protocol_unsupported",
        "the vendor speaks a protocol version this build does not.",
    ),
    (
        "connector_redirect_refused",
        "the vendor sent the call elsewhere, which the engine never follows.",
    ),
    (
        "connector_reply_invalid",
        "the vendor's reply could not be read.",
    ),
    (
        "connector_reply_too_large",
        "the vendor's reply was too large to read.",
    ),
    (
        "connector_request_too_large",
        "the call is too large to send.",
    ),
    (
        "connector_sign_in_needed",
        "the vendor refused the key or it lacks access; check the key.",
    ),
    ("connector_timeout", "the vendor took too long to answer."),
    (
        "connector_tool_name_invalid",
        "that tool name cannot be called.",
    ),
    (
        "connector_tool_unknown",
        "the vendor does not list that tool.",
    ),
    (
        "connector_tools_limit",
        "the vendor lists more tools than the engine takes.",
    ),
    (
        "connector_unavailable_here",
        "connectors are not available on this system.",
    ),
    (
        "connector_vendor_error",
        "the vendor answered with an error.",
    ),
    (
        "connector_oauth_client_invalid",
        "the sign-in app's id or secret is not usable; save it again.",
    ),
    (
        "connector_oauth_code_invalid",
        "the sign-in answer held no usable code; sign in again.",
    ),
    (
        "connector_oauth_issuer_mismatch",
        "the sign-in answer came from another server than the one asked, so it was not used.",
    ),
    (
        "connector_oauth_key_store_unavailable",
        "the engine's key store could not be read or written.",
    ),
    (
        "connector_oauth_metadata_invalid",
        "the vendor's sign-in details could not be read.",
    ),
    (
        "connector_oauth_needs_own_app",
        "this service needs your own OAuth app; add its client id in the connector's settings.",
    ),
    (
        "connector_oauth_not_offered",
        "this connector does not offer a sign-in.",
    ),
    (
        "connector_oauth_pkce_unsupported",
        "the vendor's sign-in lacks the secure code check the engine requires (PKCE S256).",
    ),
    (
        "connector_oauth_redirect_invalid",
        "a sign-in can only come back to this computer.",
    ),
    ("connector_oauth_refused", "the vendor refused the sign-in."),
    (
        "connector_oauth_scope_undeclared",
        "the vendor asked for access the connector's file does not declare, or for writes while they are off.",
    ),
    (
        "connector_oauth_state_unknown",
        "this sign-in has ended or was already answered; start it again.",
    ),
    (
        "connector_oauth_step_up_limit",
        "the vendor still wants more access after two sign-ins; check the connector's settings.",
    ),
    (
        "connector_oauth_token_invalid",
        "the vendor's sign-in answer held no usable token.",
    ),
    (
        "connector_oauth_trust_changed",
        "the connector's file changed since this sign-in began; start it again.",
    ),
];

/// The plain sentence for a code the Connectors page can show: this module's own, or an
/// app plugin's. Input: the code. Output: the sentence, or none for a code of neither.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    PLAIN
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, plain)| *plain)
        .or_else(|| crate::app_plugins::plain_problem(code))
}

pub(crate) fn project_known(db: &Connection, project: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
        [project],
        |row| row.get(0),
    )
    .map_err(|_| EngineError("connectors_unavailable".into()))
}

/// Reads the Connectors page, with each refused file whose kind reads as connector listed
/// under its file name with its reason. Inputs: the engine database and root, and the
/// project whose switches to include. Output: the page, `connector_project_unknown`, or
/// the store's refusal when the plugins folder cannot be read.
pub fn view(
    db: &Connection,
    root: &Path,
    project: Option<&str>,
) -> Result<ConnectorsView, EngineError> {
    let grants = match project {
        None => Vec::new(),
        Some(project) if project_known(db, project)? => grants::of_project(db, project)?,
        Some(_) => return Err(EngineError(PROJECT_UNKNOWN.into())),
    };
    let listing = store::list(db, root)?;
    let connectors: Vec<ConnectorView> = listing
        .plugins
        .into_iter()
        .filter(|plugin| plugin.kind == PluginKind::Connector)
        .map(|plugin| {
            assert!(
                plugin.runs.is_some() || matches!(plugin.status, PluginStatus::Refused { .. }),
                "only a refused file has no address"
            );
            ConnectorView {
                sign_in: if plugin.secrets.is_empty() {
                    SignIn::NotYet
                } else {
                    SignIn::Key
                },
                runs: plugin.runs,
                name: plugin.name,
                title: plugin.title,
                version: plugin.version,
                description: plugin.description,
                origin: plugin.origin,
                status: plugin.status,
                secrets: plugin.secrets,
                sha256: plugin.sha256,
            }
        })
        .collect();
    assert!(connectors.len() <= store::PLUGINS_MAX);
    let (settings, version) = settings::load(db)?;
    Ok(ConnectorsView {
        connectors,
        more: listing.more,
        settings,
        version,
        project: project.map(str::to_owned),
        grants,
    })
}

#[cfg(all(test, unix))]
mod tests;
