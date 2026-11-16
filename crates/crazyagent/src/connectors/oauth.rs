//! The engine as an OAuth client of a connector's vendor (MCP authorization): it finds the
//! vendor's authorization server (RFC 9728, RFC 8414, OpenID), signs in with the
//! authorization code flow, PKCE S256 and the resource indicator (RFC 8707), and checks the
//! issuer of the answer (RFC 9207) before a code is redeemed. Tokens go to the plugin key
//! store only, named by connector and issuer.

use crate::app_plugins::connector::Registration;
use crate::app_plugins::store;
use crate::protocol::CredentialRef;
use crate::provider_reach::ProviderReach;
use crate::storage::accounts::{self, PLUGIN_KEYS};
use rusqlite::Connection;
use std::path::Path;

#[cfg(unix)]
mod sign;
#[cfg(unix)]
use sign::finish_trusted;
#[cfg(unix)]
pub(crate) use sign::begin_with;
#[cfg(all(test, unix))]
pub(crate) use sign::finish_with;

/// Every sign-in refusal code this client adds beside the connector address codes of
/// `mcp_client::MCP_CODES`, sorted.
pub const OAUTH_CODES: [&str; 13] = [
    "connector_oauth_client_invalid",
    "connector_oauth_code_invalid",
    "connector_oauth_issuer_mismatch",
    "connector_oauth_key_store_unavailable",
    "connector_oauth_metadata_invalid",
    "connector_oauth_needs_own_app",
    "connector_oauth_not_offered",
    "connector_oauth_pkce_unsupported",
    "connector_oauth_redirect_invalid",
    "connector_oauth_refused",
    "connector_oauth_state_unknown",
    "connector_oauth_token_invalid",
    "connector_oauth_trust_changed",
];

/// The longest client id of the owner's own OAuth app.
const OWN_ID_BYTES_MAX: usize = 255;

/// One sign-in waiting for the browser: the authorization server's address to open, with
/// every parameter in its query, and the `state` the answer must carry back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub authorize_url: url::Url,
    pub state: String,
}

/// Why a sign-in step gave nothing, as the code a page shows: one of `OAUTH_CODES` or of
/// `mcp_client::MCP_CODES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthError(pub &'static str);

fn not_offered() -> OAuthError {
    OAuthError("connector_oauth_not_offered")
}

fn key_store() -> OAuthError {
    OAuthError("connector_oauth_key_store_unavailable")
}

/// Whether a value is one to `bytes_max` visible ASCII bytes, the shape every id, code
/// and token this client keeps or sends must have.
fn visible(value: &str, bytes_max: usize) -> bool {
    !value.is_empty()
        && value.len() <= bytes_max
        && value.bytes().all(|byte| (0x21..0x7f).contains(&byte))
}

/// Where the client id of the owner's own OAuth app for one connector is saved.
fn own_ref(name: &str) -> Result<CredentialRef, OAuthError> {
    CredentialRef::new(format!("connector-client-{name}-own")).map_err(|_| key_store())
}

/// Saves the client id of the owner's own OAuth app for a trusted connector that lets the
/// owner bring one, replacing the one saved before. Inputs: database, engine root,
/// connector and client id. Output: Ok, or `connector_oauth_not_offered`,
/// `connector_oauth_client_invalid`, `connector_oauth_key_store_unavailable`.
pub fn set_own_client(
    db: &Connection,
    root: &Path,
    name: &str,
    id: &str,
) -> Result<(), OAuthError> {
    let manifest = store::trusted_connector(db, root, name).map_err(|_| not_offered())?;
    let declared = manifest
        .connector()
        .and_then(|spec| spec.oauth.as_ref())
        .is_some_and(|oauth| oauth.registration.contains(&Registration::Own));
    if !declared {
        return Err(not_offered());
    }
    if !visible(id, OWN_ID_BYTES_MAX) {
        return Err(OAuthError("connector_oauth_client_invalid"));
    }
    accounts::replace_key_in(root, PLUGIN_KEYS, &own_ref(name)?, id.as_bytes().to_vec())
        .map_err(|_| key_store())
}

/// Starts a sign-in to a trusted connector: finds its authorization server, picks the
/// client and keeps the verifier in engine memory for [`finish`]. Blocking. Inputs:
/// database, engine root, connector, the loopback address the browser comes back to and
/// whether writes are allowed. Output: the address to open and its state, or why not.
pub fn begin(
    db: &Connection,
    root: &Path,
    name: &str,
    redirect: &url::Url,
    writes: bool,
) -> Result<Pending, OAuthError> {
    let manifest = store::trusted_connector(db, root, name).map_err(|_| not_offered())?;
    let spec = manifest.connector().ok_or_else(not_offered)?;
    begin_with(
        root,
        name,
        &manifest.sha256,
        spec,
        redirect,
        writes,
        &ProviderReach::system(),
    )
}

/// Ends a sign-in with the browser's answer: the state must be one [`begin`] gave and not
/// yet answered, the connector's file the one trusted then, and the issuer the server's;
/// only then is the code redeemed and the tokens saved. Blocking. Inputs: database, engine
/// root, the answer's state, code and issuer. Output: Ok once the tokens are saved.
pub fn finish(
    db: &Connection,
    root: &Path,
    state: &str,
    code: &str,
    iss: Option<&str>,
) -> Result<(), OAuthError> {
    finish_trusted(db, root, state, code, iss, &ProviderReach::system())
}

/// No sign-in where curl's private request cannot be made. Output:
/// `connector_unavailable_here`.
#[cfg(not(unix))]
pub(crate) fn begin_with(
    _root: &Path,
    _name: &str,
    _sha256: &str,
    _spec: &crate::app_plugins::connector::ConnectorSpec,
    _redirect: &url::Url,
    _writes: bool,
    _reach: &ProviderReach,
) -> Result<Pending, OAuthError> {
    Err(OAuthError("connector_unavailable_here"))
}

/// No sign-in where curl's private request cannot be made. Output:
/// `connector_unavailable_here`.
#[cfg(not(unix))]
fn finish_trusted(
    _db: &Connection,
    _root: &Path,
    _state: &str,
    _code: &str,
    _iss: Option<&str>,
    _reach: &ProviderReach,
) -> Result<(), OAuthError> {
    Err(OAuthError("connector_unavailable_here"))
}

#[cfg(all(test, unix))]
#[path = "oauth_tests.rs"]
mod tests;
