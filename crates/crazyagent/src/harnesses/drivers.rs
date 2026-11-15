use super::launch::{LaunchError, failure};
use super::manifest::DriverKind;
use super::registry::{Harness, registry};

pub mod jsonl_turn;
pub mod rules;

/// The piped drivers the engine runs today: its own adapters for Codex and Claude Code.
/// Each shared driver kind a manifest names joins this list when its task lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piped {
    Codex,
    Claude,
    JsonlTurn,
}

/// The one dispatch point of piped launches: the driver the engine runs for a driver kind.
/// Input: the kind a manifest names. Output: the driver, or `launch_harness_unsupported`
/// for RCLI, which runs only through its master host, and for every kind whose driver has
/// not landed.
pub fn of(kind: DriverKind) -> Result<Piped, LaunchError> {
    match kind {
        DriverKind::CodexAppServer => Ok(Piped::Codex),
        DriverKind::ClaudeStreamJson => Ok(Piped::Claude),
        DriverKind::Rcli => Err(failure("launch_harness_unsupported")),
        DriverKind::Acp => Err(failure("launch_harness_unsupported")),
        DriverKind::JsonlTurn => Err(failure("launch_harness_unsupported")),
        DriverKind::JsonlRpc => Err(failure("launch_harness_unsupported")),
        DriverKind::TextTurn => Err(failure("launch_harness_unsupported")),
    }
}

/// Picks the driver a piped launch of one harness runs under, from the driver kind its
/// manifest names. Input: a catalog id or an alias, such as `claude`. Output: the driver,
/// or `launch_harness_unsupported` for a name outside the catalog, a harness with no piped
/// driver, or a kind [`of`] refuses.
pub fn piped(harness: &str) -> Result<Piped, LaunchError> {
    registry()
        .get(harness)
        .and_then(Harness::piped)
        .ok_or_else(|| failure("launch_harness_unsupported"))
        .and_then(of)
}

#[cfg(test)]
#[path = "drivers_tests.rs"]
mod tests;
