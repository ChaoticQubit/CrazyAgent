//! The engine as an OAuth client of a connector's vendor (MCP authorization): it finds the
//! vendor's authorization server (RFC 9728, RFC 8414, OpenID), signs in with the
//! authorization code flow, PKCE S256 and the resource indicator (RFC 8707), and checks the
//! issuer of the answer (RFC 9207) before a code is redeemed. Tokens go to the plugin key
//! store only, named by connector and issuer.

use crate::app_plugins::connector::Registration;
use crate::app_plugins::store;
use crate::connectors::mcp_client::Credential;
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use crate::provider_reach::ProviderReach;
use crate::storage::accounts::{self, PLUGIN_KEYS};
use rusqlite::Connection;
#[cfg(unix)]
use rusqlite::params;
use std::path::Path;

#[cfg(unix)]
mod sign;
#[cfg(unix)]
pub(crate) use sign::begin_with;
#[cfg(unix)]
use sign::finish_trusted;
#[cfg(all(test, unix))]
pub(crate) use sign::finish_with;
#[cfg(unix)]
use sign::forget_keys;
#[cfg(unix)]
pub(crate) use sign::fresh_with;
#[cfg(unix)]
use sign::own_replaced;
#[cfg(unix)]
pub(crate) use sign::refused_with;
#[cfg(unix)]
pub(crate) use sign::renew_with;
#[cfg(unix)]
pub(crate) use sign::step_up_with;

/// Every sign-in refusal code this client adds beside the connector address codes of
/// `mcp_client::MCP_CODES`, sorted.
pub const OAUTH_CODES: [&str; 18] = [
    "connector_oauth_callback_expired",
    "connector_oauth_callback_flooded",
    "connector_oauth_callback_unavailable",
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
    "connector_oauth_scope_undeclared",
    "connector_oauth_state_unknown",
    "connector_oauth_step_up_limit",
    "connector_oauth_token_invalid",
    "connector_oauth_trust_changed",
];

/// Connectors whose sign-in ended because their vendor refused a renewal, each with the
/// title its notice shows and the scopes a server asked for, until a sign-in ends.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS connector_sign_ins (
    connector TEXT PRIMARY KEY CHECK(length(connector) BETWEEN 1 AND 40),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 120),
    wanted TEXT CHECK(wanted IS NULL OR length(wanted) BETWEEN 1 AND 1024),
    since_ms INTEGER NOT NULL
);";

/// The longest client id of the owner's own OAuth app or of a registered client.
const OWN_ID_BYTES_MAX: usize = 255;

/// Where the app's client metadata document is served, its client id where a server takes
/// one. None until the owner's domain exists, and CIMD is skipped meanwhile (DEC-168
/// addendum 23); Task 10 sets it.
pub const CLIENT_METADATA_URL: Option<&str> = None;

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

/// Where the client secret of the owner's own OAuth app for one connector is saved.
fn own_secret_ref(name: &str) -> Result<CredentialRef, OAuthError> {
    CredentialRef::new(format!("connector-secret-{name}-own")).map_err(|_| key_store())
}

/// Where the issuer the owner's own OAuth app was first signed in at is saved: the one
/// issuer that app is taken to until it is saved again.
fn own_issuer_ref(name: &str) -> Result<CredentialRef, OAuthError> {
    CredentialRef::new(format!("connector-issuer-{name}-own")).map_err(|_| key_store())
}

/// Saves the owner's own OAuth app for a trusted connector that lets the owner bring one:
/// its client id and, for a confidential app, its secret, replacing what was saved before
/// and freeing it from the issuer the old one was bound to. A sign-in made with the old
/// one ends and asks the owner again, and sign-ins begun with it are dropped.
/// Inputs: database, engine root, connector, client id and secret. Output: Ok, or
/// `connector_oauth_not_offered`, `_client_invalid`, `_key_store_unavailable`.
pub fn set_own_client(
    db: &Connection,
    root: &Path,
    name: &str,
    id: &str,
    secret: Option<&str>,
) -> Result<(), OAuthError> {
    let manifest = store::trusted_connector(db, root, name).map_err(|_| not_offered())?;
    let declared = manifest
        .connector()
        .and_then(|spec| spec.oauth.as_ref())
        .is_some_and(|oauth| oauth.registration.contains(&Registration::Own));
    if !declared {
        return Err(not_offered());
    }
    let secret_fits =
        secret.is_none_or(|secret| visible(secret, crate::credentials::SECRET_BYTES_MAX));
    if !visible(id, OWN_ID_BYTES_MAX) || !secret_fits {
        return Err(OAuthError("connector_oauth_client_invalid"));
    }
    own_replaced(db, root, name, &manifest.title, now_ms(), || {
        accounts::remove_key_in(root, PLUGIN_KEYS, own_issuer_ref(name)?.as_str())
            .map_err(|_| key_store())?;
        accounts::replace_key_in(root, PLUGIN_KEYS, &own_ref(name)?, id.as_bytes().to_vec())
            .map_err(|_| key_store())?;
        let secret_ref = own_secret_ref(name)?;
        match secret {
            Some(secret) => {
                accounts::replace_key_in(root, PLUGIN_KEYS, &secret_ref, secret.as_bytes().to_vec())
            }
            None => accounts::remove_key_in(root, PLUGIN_KEYS, secret_ref.as_str()),
        }
        .map_err(|_| key_store())
    })
}

/// Removes the owner's own OAuth app for a trusted connector: its client id, secret and the
/// issuer it was bound to, even when the file no longer lets the owner bring one. A sign-in
/// made with it ends and sign-ins begun with it are dropped. Inputs: database, engine root
/// and connector. Output: Ok, or `connector_oauth_not_offered`, `_key_store_unavailable`.
pub fn clear_own_client(db: &Connection, root: &Path, name: &str) -> Result<(), OAuthError> {
    let manifest = store::trusted_connector(db, root, name).map_err(|_| not_offered())?;
    own_replaced(db, root, name, &manifest.title, now_ms(), || {
        for reference in [own_ref, own_secret_ref, own_issuer_ref] {
            let key = reference(name)?;
            accounts::remove_key_in(root, PLUGIN_KEYS, key.as_str())
                .map_err(|_| key_store())?;
        }
        Ok(())
    })
}

/// Saves the owner's own app where no sign-in can be made, so none is ended. Output: the
/// save's answer.
#[cfg(not(unix))]
fn own_replaced(
    _db: &Connection,
    _root: &Path,
    _name: &str,
    _title: &str,
    _now_ms: i64,
    save: impl FnOnce() -> Result<(), OAuthError>,
) -> Result<(), OAuthError> {
    save()
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
    let document = CLIENT_METADATA_URL
        .map(url::Url::parse)
        .transpose()
        .map_err(|_| OAuthError("connector_oauth_metadata_invalid"))?;
    begin_with(
        root,
        name,
        &manifest.sha256,
        spec,
        redirect,
        writes,
        document.as_ref(),
        &ProviderReach::system(),
    )
}

/// Starts a sign-in that asks a trusted connector for more scope after its server refused a
/// call for want of it (RFC 6750 `insufficient_scope`): the union of what the last sign-in
/// asked and what the server names, within the scopes the file declares for the access
/// allowed now; at most two in a row. Blocking. Inputs: database, root, connector, the
/// challenge's scopes, the loopback address and whether writes are allowed. Output: as
/// [`begin`], or `connector_oauth_scope_undeclared`, `connector_oauth_step_up_limit`.
pub fn step_up(
    db: &Connection,
    root: &Path,
    name: &str,
    challenge: &str,
    redirect: &url::Url,
    writes: bool,
) -> Result<Pending, OAuthError> {
    let manifest = store::trusted_connector(db, root, name).map_err(|_| not_offered())?;
    let spec = manifest.connector().ok_or_else(not_offered)?;
    let document = CLIENT_METADATA_URL
        .map(url::Url::parse)
        .transpose()
        .map_err(|_| OAuthError("connector_oauth_metadata_invalid"))?;
    step_up_with(
        root,
        name,
        &manifest.sha256,
        spec,
        challenge,
        redirect,
        writes,
        document.as_ref(),
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

/// The credential a call to a trusted connector sends now, its access token renewed first
/// when it ends within a minute; a renewal the server refuses asks the owner to sign in
/// again. Blocking when it renews. Inputs: database, engine root and connector. Output:
/// the credential, or `connector_sign_in_needed` and the codes of a failed renewal.
pub fn fresh(db: &Connection, root: &Path, name: &str) -> Result<Credential, OAuthError> {
    let manifest = store::trusted_connector(db, root, name).map_err(|_| not_offered())?;
    let spec = manifest.connector().ok_or_else(not_offered)?;
    fresh_with(
        db,
        root,
        name,
        &manifest.title,
        &manifest.sha256,
        spec,
        now_ms(),
        &ProviderReach::system(),
    )
}

fn unavailable(_: rusqlite::Error) -> EngineError {
    EngineError("connectors_unavailable".into())
}

/// Marks a connector "Sign in again" after its vendor refused a renewal: one Action item
/// asks the owner to sign in until a sign-in ends or the connector is removed, and marking
/// it again changes nothing. Inputs: database, connector, its title and the time. Output:
/// Ok, or `connectors_unavailable`.
#[cfg(unix)]
fn mark_sign_in(db: &Connection, name: &str, title: &str, now_ms: i64) -> Result<(), OAuthError> {
    let title = crate::connectors::gateway::pins::short_title(title);
    db.execute_batch(SCHEMA)
        .and_then(|()| {
            db.execute(
                "INSERT INTO connector_sign_ins(connector,title,wanted,since_ms)
                 VALUES(?1,?2,NULL,?3) ON CONFLICT(connector) DO NOTHING",
                params![name, title, now_ms],
            )
        })
        .map(|_| ())
        .map_err(|_| OAuthError("connectors_unavailable"))
}

/// Clears a connector's "Sign in again" once a sign-in ended. Inputs: database and
/// connector. Output: Ok, or `connectors_unavailable`.
#[cfg(unix)]
fn signed_in(db: &Connection, name: &str) -> Result<(), OAuthError> {
    unmark(db, name).map_err(|_| OAuthError("connectors_unavailable"))
}

/// Forgets a connector's sign-ins when it is removed or trusted again with other hosts:
/// every key they saved, the sign-ins still waiting and its "Sign in again". Inputs: the
/// engine database, root and connector. Output: Ok, `connector_oauth_key_store_unavailable`
/// or `connectors_unavailable`.
pub fn forget(db: &Connection, root: &Path, name: &str) -> Result<(), EngineError> {
    forget_keys(root, name).map_err(|problem| EngineError(problem.0.into()))?;
    unmark(db, name)
}

/// No sign-in key is saved where the Unix owner-only key folder does not exist. Output: Ok.
#[cfg(not(unix))]
fn forget_keys(_root: &Path, _name: &str) -> Result<(), OAuthError> {
    Ok(())
}

/// Deletes a connector's "Sign in again". Inputs: the engine database and the connector.
/// Output: Ok, or `connectors_unavailable`.
fn unmark(db: &Connection, name: &str) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    db.execute("DELETE FROM connector_sign_ins WHERE connector=?1", [name])
        .map_err(unavailable)?;
    Ok(())
}

/// The connectors waiting for the owner to sign in again, each with its title, by name.
/// Inputs: the engine database and the most to read. Output: the rows, none when the table
/// does not exist yet, or `connectors_unavailable`.
pub(crate) fn wanting(
    db: &Connection,
    max: usize,
) -> Result<Option<Vec<(String, String)>>, EngineError> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='connector_sign_ins')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !exists {
        return Ok(None);
    }
    let rows: Vec<(String, String)> = db
        .prepare("SELECT connector, title FROM connector_sign_ins ORDER BY connector LIMIT ?1")
        .map_err(unavailable)?
        .query_map([max as i64], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    assert!(rows.len() <= max);
    Ok(Some(rows))
}

/// The scopes a connector's server last asked for that its sign-in lacked, while the
/// connector waits for the owner to sign in again for want of them. Inputs: the engine
/// database and the connector. Output: the scopes as the server named them, none, or
/// `connectors_unavailable`.
pub(crate) fn wanted(db: &Connection, name: &str) -> Result<Option<String>, EngineError> {
    use rusqlite::OptionalExtension;
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let wanted: Option<Option<String>> = db
        .query_row(
            "SELECT wanted FROM connector_sign_ins WHERE connector=?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(wanted.flatten())
}

/// This computer's clock as Unix milliseconds, 0 before 1970.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// No sign-in where curl's private request cannot be made. Output:
/// `connector_unavailable_here`.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin_with(
    _root: &Path,
    _name: &str,
    _sha256: &str,
    _spec: &crate::app_plugins::connector::ConnectorSpec,
    _redirect: &url::Url,
    _writes: bool,
    _cimd: Option<&url::Url>,
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

/// No sign-in to end where the Unix owner-only key folder does not exist. Output:
/// `connector_unavailable_here`.
#[cfg(not(unix))]
pub(crate) fn refused_with(
    _db: &Connection,
    _root: &Path,
    _name: &str,
    _title: &str,
    _refused: &Credential,
    _now_ms: i64,
) -> Result<(), OAuthError> {
    Err(OAuthError("connector_unavailable_here"))
}

/// No renewal where curl's private request cannot be made. Output:
/// `connector_unavailable_here`.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn renew_with(
    _db: &Connection,
    _root: &Path,
    _name: &str,
    _title: &str,
    _sha256: &str,
    _spec: &crate::app_plugins::connector::ConnectorSpec,
    _now_ms: i64,
    _reach: &ProviderReach,
) -> Result<Credential, OAuthError> {
    Err(OAuthError("connector_unavailable_here"))
}

/// No step-up where curl's private request cannot be made. Output:
/// `connector_unavailable_here`.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_up_with(
    _root: &Path,
    _name: &str,
    _sha256: &str,
    _spec: &crate::app_plugins::connector::ConnectorSpec,
    _challenge: &str,
    _redirect: &url::Url,
    _writes: bool,
    _cimd: Option<&url::Url>,
    _reach: &ProviderReach,
) -> Result<Pending, OAuthError> {
    Err(OAuthError("connector_unavailable_here"))
}

/// No renewal where curl's private request cannot be made. Output:
/// `connector_unavailable_here`.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn fresh_with(
    _db: &Connection,
    _root: &Path,
    _name: &str,
    _title: &str,
    _sha256: &str,
    _spec: &crate::app_plugins::connector::ConnectorSpec,
    _now_ms: i64,
    _reach: &ProviderReach,
) -> Result<Credential, OAuthError> {
    Err(OAuthError("connector_unavailable_here"))
}

#[cfg(all(test, unix))]
#[path = "oauth_tests.rs"]
mod tests;
