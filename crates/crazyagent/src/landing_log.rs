use crate::engine::EngineError;
use rusqlite::Connection;

/// The body schema of every landing event.
pub const SCHEMA: u16 = 1;
/// An agent worktree's task branch stored (`landing_agent_branches`).
pub const AGENT_BRANCH_SET: &str = "AgentBranchSet";
/// An agent worktree's task branch forgotten.
pub const AGENT_BRANCH_FORGOTTEN: &str = "AgentBranchForgotten";
/// Every landing event kind.
pub const KINDS: [&str; 2] = [AGENT_BRANCH_SET, AGENT_BRANCH_FORGOTTEN];

/// Rebuilds the landing tables from the log. Input: the store. Output: the events applied.
pub fn rebuild(_db: &Connection) -> Result<u64, EngineError> {
    todo!("V250-L9 red")
}

/// Seeds a test store for logged landing writes, as ide::log::seed does. Input: the store.
/// Output: the running host.
#[cfg(test)]
pub(crate) fn seed(db: &Connection) -> crate::engine::owner::HostStamp {
    crate::ide::log::seed(db)
}
