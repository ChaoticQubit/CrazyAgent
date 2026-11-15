use super::launch::{LaunchError, failure};
use super::manifest::DriverKind;

/// The piped drivers the engine runs today: its own adapters for Codex and Claude Code.
/// Each shared driver kind a manifest names joins this list when its task lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piped {
    Codex,
    Claude,
}

/// The one dispatch point of piped launches: the driver the engine runs for a driver kind.
/// Input: the kind a manifest names. Output: the driver, or `launch_harness_unsupported`
/// for RCLI, which runs only through its master host, and for every kind whose driver has
/// not landed.
pub fn of(_kind: DriverKind) -> Result<Piped, LaunchError> {
    Err(failure("launch_harness_unsupported"))
}

/// Picks the driver a piped launch of one harness runs under, from the driver kind its
/// manifest names. Input: a catalog id or an alias, such as `claude`. Output: the driver,
/// or `launch_harness_unsupported` for a name outside the catalog, a harness with no piped
/// driver, or a kind [`of`] refuses.
pub fn piped(_harness: &str) -> Result<Piped, LaunchError> {
    Err(failure("launch_harness_unsupported"))
}

#[cfg(test)]
#[path = "drivers_tests.rs"]
mod tests;
