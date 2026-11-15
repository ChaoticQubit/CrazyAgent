use crate::connectors::mcp_client::VendorTool;
use crate::engine::EngineError;
use rusqlite::Connection;

/// Which of a connector's two tool lists a pin is for: the one at its normal address, or
/// the one its vendor read-only address or header gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Normal,
    ReadOnly,
}

/// What a list is against its pin: the list the owner's connector was pinned to (made now
/// when there was none), or a different one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pin {
    Same,
    Changed,
}

/// Checks a tool list against the connector's pin for one endpoint and keeps a changed
/// list for the owner to review. Inputs: the engine database, the connector's name and
/// title, the endpoint and the cleaned list. Output: how the list stands, or
/// `connectors_unavailable`.
pub(crate) fn check(
    _db: &Connection,
    _connector: &str,
    _title: &str,
    _endpoint: Endpoint,
    _tools: &[VendorTool],
) -> Result<Pin, EngineError> {
    Err(EngineError("connectors_unavailable".into()))
}

/// Whether a connector is stopped because one of its tool lists changed and the owner has
/// not accepted it. Inputs: the engine database and the connector. Output: the answer.
pub fn stopped(_db: &Connection, _connector: &str) -> Result<bool, EngineError> {
    Err(EngineError("connectors_unavailable".into()))
}

/// Accepts a connector's changed tool lists as its new pins, which starts it again.
/// Inputs: the engine database and the connector. Output: whether anything was accepted.
pub fn accept_tools(_db: &Connection, _connector: &str) -> Result<bool, EngineError> {
    Err(EngineError("connectors_unavailable".into()))
}

/// Deletes a connector's pins: when it is removed or trusted again. Inputs: the engine
/// database and the connector. Output: Ok, or `connectors_unavailable`.
pub fn forget_connector(_db: &Connection, _connector: &str) -> Result<(), EngineError> {
    Err(EngineError("connectors_unavailable".into()))
}
