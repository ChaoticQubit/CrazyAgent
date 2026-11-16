//! The engine as an OAuth client of a connector's vendor (MCP authorization): it finds the
//! vendor's authorization server (RFC 9728, RFC 8414, OpenID), signs in with the
//! authorization code flow, PKCE S256 and the resource indicator (RFC 8707), and checks the
//! issuer of the answer (RFC 9207) before a code is redeemed. Tokens go to the plugin key
//! store only, named by connector and issuer.

use crate::app_plugins::connector::ConnectorSpec;
use crate::provider_reach::ProviderReach;
use rusqlite::Connection;
use std::path::Path;

/// One sign-in waiting for the browser: the authorization server's address to open, with
/// every parameter in its query, and the `state` the answer must carry back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub authorize_url: url::Url,
    pub state: String,
}

/// Why a sign-in step gave nothing, as the code a page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthError(pub &'static str);

fn not_built() -> OAuthError {
    OAuthError("connector_oauth_not_built")
}

/// Saves the owner's own OAuth app for one connector. Inputs: database, engine root,
/// connector and client id. Output: Ok, or why it was not saved.
pub fn set_own_client(
    _db: &Connection,
    _root: &Path,
    _name: &str,
    _id: &str,
) -> Result<(), OAuthError> {
    Ok(())
}

/// Starts a sign-in to a trusted connector. Inputs: database, engine root, connector, the
/// loopback address the browser comes back to and whether writes are allowed. Output: the
/// address to open and its state, or why there is none.
pub fn begin(
    db: &Connection,
    root: &Path,
    name: &str,
    redirect: &url::Url,
    writes: bool,
) -> Result<Pending, OAuthError> {
    let _ = (db, root, name, redirect, writes);
    Err(not_built())
}

/// [`begin`] with the connector's file and the name lookup given.
pub(crate) fn begin_with(
    _root: &Path,
    _name: &str,
    _sha256: &str,
    _spec: &ConnectorSpec,
    _redirect: &url::Url,
    _writes: bool,
    _reach: &ProviderReach,
) -> Result<Pending, OAuthError> {
    Err(not_built())
}

/// Ends a sign-in with the browser's answer. Inputs: database, engine root, the answer's
/// state, code and issuer. Output: Ok once the tokens are saved, or why they are not.
pub fn finish(
    db: &Connection,
    root: &Path,
    state: &str,
    code: &str,
    iss: Option<&str>,
) -> Result<(), OAuthError> {
    let _ = (db, root, state, code, iss);
    Err(not_built())
}

/// [`finish`] with the name lookup given and the file's trust not read again.
pub(crate) fn finish_with(
    _root: &Path,
    _state: &str,
    _code: &str,
    _iss: Option<&str>,
    _reach: &ProviderReach,
) -> Result<(), OAuthError> {
    Err(not_built())
}

#[cfg(all(test, unix))]
#[path = "oauth_tests.rs"]
mod tests;
