use super::{GitHubRepoRules, GitHubRule};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::questions::Asker;
use rusqlite::{Connection, Transaction};
use std::path::Path;

#[cfg(all(test, target_os = "macos"))]
mod proposals_tests;

/// The asker name prefix of the questions agents' rule proposals raise.
pub const ASKER_PREFIX: &str = "engine:github-rule:";
/// The option an owner picks to approve a proposal, always the first.
pub const APPROVE: &str = "Approve";
/// The option an owner picks to decline a proposal.
pub const DECLINE: &str = "Decline";
/// The longest reason an agent may give with a proposal; a longer one is cut.
pub const WHY_BYTES_MAX: usize = 300;
/// The most proposals one project has open at once: waiting for the owner or being applied.
pub const OPEN_PER_PROJECT_MAX: i64 = 4;
/// The most proposals one pass applies or tells, so a pass stays bounded.
pub const PASS_MAX: i64 = 16;

const _: () = assert!(OPEN_PER_PROJECT_MAX <= PASS_MAX);
const _: () = assert!(WHY_BYTES_MAX <= crate::questions::CONTEXT_BYTES_MAX / 4);

/// How a proposal ended for the agent that made it: applied after the owner approved it,
/// refused when it could not be applied as approved, or declined by the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Applied,
    Refused,
    Declined,
}

/// Tells whether an agent may propose a rule: one GitHub's settings hold. Dependabot
/// version updates and the security policy are files in the project, so they are not.
/// Input: the rule. Output: the answer.
pub fn proposable(_rule: GitHubRule) -> bool {
    true
}

/// The rule's name as agents and the owner read it, its snake_case name. Input: the rule.
/// Output: the name.
pub fn rule_name(rule: GitHubRule) -> String {
    match serde_json::to_value(rule) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("a GitHub rule is named by a string"),
    }
}

/// Turns an agent's proposal into a real owner question (DEC-182 S7). Stub until the fix.
/// Inputs: the engine connection, the project, who asked, the rule, whether it is to be on,
/// and the reason. Output: the question id, or a refusal code.
pub fn request(
    _db: &Connection,
    _project: &ProjectId,
    _asker: &Asker,
    _rule: GitHubRule,
    _on: bool,
    _why: &str,
) -> Result<u64, String> {
    Err("github_rule_unbuilt".into())
}

/// Settles the proposal behind an engine question the owner answered or dismissed. Stub
/// until the fix. Inputs: the transaction, the project, the asker and whether approved.
/// Output: done.
pub(crate) fn decide(
    _tx: &Transaction<'_>,
    _project: &ProjectId,
    _asker: &str,
    _approved: bool,
) -> Result<(), EngineError> {
    Ok(())
}

/// Applies every approved proposal. Stub until the fix. Inputs: the engine connection and
/// the rule change. Output: how many proposals ended.
pub fn apply_approved(
    _db: &Connection,
    _set: &mut dyn FnMut(&Path, GitHubRule, bool) -> Result<GitHubRepoRules, EngineError>,
) -> Result<usize, EngineError> {
    Ok(0)
}

/// Ends every proposal an engine stop left approved or being applied. Stub until the fix.
/// Input: the start's transaction. Output: how many ended.
pub(crate) fn start(_tx: &Transaction<'_>) -> Result<usize, EngineError> {
    Ok(0)
}

/// Reads the proposal id from a rule proposal question's asker name. Stub until the fix.
/// Input: the stored asker. Output: the id, or none.
pub fn request_id(_asker: &str) -> Option<i64> {
    None
}

/// Tells whether the owner's answer approves a proposal. Stub until the fix. Input: the
/// picks. Output: the answer.
pub fn approves(_picks: &[u8]) -> bool {
    false
}
