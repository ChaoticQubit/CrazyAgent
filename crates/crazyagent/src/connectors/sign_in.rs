//! The owner's sign-in to an OAuth connector through the browser: the engine opens a
//! loopback callback, starts the sign-in with its address, answers the authorization
//! address for the owner's app to open, and waits on a thread of its own for the browser to
//! come back. At most one sign-in waits per connector, and the pages read where it stands.

use crate::connectors::oauth::{OAuthError, Pending};
use crate::engine::EngineError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// How many sign-ins wait at once per engine root: as many as the OAuth client keeps, so
/// none waits on a sign-in the client already forgot.
pub const WAITING_MAX: usize = 8;
/// How long an ended sign-in's state stays readable.
pub const ENDED_KEEP: Duration = Duration::from_secs(10 * 60);

/// Where one connector's sign-in through the browser stands, as the pages read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SignInState {
    /// No sign-in was started, or it ended long enough ago to be forgotten.
    #[serde(rename = "none")]
    Idle,
    /// The browser has not come back yet, or its answer is being redeemed.
    Waiting,
    /// The tokens are saved.
    SignedIn,
    /// The sign-in ended without tokens, for the reason `code` names.
    Failed { code: String },
    /// The owner cancelled it, or a later sign-in replaced it.
    Cancelled,
}

/// Starts a sign-in to a trusted connector through the owner's browser, asking for more
/// scope when its server last refused a call for want of it. Inputs: the engine database,
/// the engine root and the connector. Output: the address the owner's app opens.
pub fn start(_db: &Connection, _root: &Path, _name: &str) -> Result<url::Url, EngineError> {
    Err(EngineError("sign_in_not_built".into()))
}

/// Starts a sign-in with the given start and redeem steps. Inputs: the engine root, the
/// connector, how long to wait for the browser, `begin` given the loopback address and
/// `finish` given the state, code and issuer. Output: the address the owner's app opens.
pub(crate) fn start_with<B, F>(
    _root: &Path,
    _name: &str,
    _limit: Duration,
    _begin: B,
    _finish: F,
) -> Result<url::Url, EngineError>
where
    B: FnOnce(&url::Url) -> Result<Pending, OAuthError>,
    F: FnOnce(&str, &str, Option<&str>) -> Result<(), OAuthError> + Send + 'static,
{
    Err(EngineError("sign_in_not_built".into()))
}

/// Where a connector's sign-in stands. Inputs: the engine root and the connector. Output:
/// its state.
pub fn state_of(_root: &Path, _name: &str) -> SignInState {
    SignInState::Idle
}

/// Cancels a connector's waiting sign-in and closes its callback, unless the browser's
/// answer already came and is being redeemed. Inputs: the engine root and the connector.
/// Output: where it stands after.
pub fn cancel(_root: &Path, _name: &str) -> SignInState {
    SignInState::Idle
}

#[cfg(test)]
#[path = "sign_in_tests.rs"]
mod tests;
