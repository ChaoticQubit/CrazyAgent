use super::Call;
use crate::checks::Actor;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde_json::Value;

pub const CHECKS_RUN_TOOL: &str = "checks_run";
pub const CHECKS_RESULTS_TOOL: &str = "checks_results";
/// The checks tools (V3b VE-6) a server lists after `QUESTION_TOOLS`: `checks_run` queues
/// the project's checks on the caller's own change set within the owner's hourly limit, and
/// `checks_results` reads the newest run of each check there, holdout runs outcome only.
pub const CHECKS_TOOLS: [&str; 2] = [CHECKS_RUN_TOOL, CHECKS_RESULTS_TOOL];
/// The most check ids one `checks_run` call names: every check and holdout a profile holds.
pub const CHECKS_NAMED_MAX: usize =
    crate::checks::profile::CHECKS_MAX + crate::checks::profile::HOLDOUTS_MAX;
/// The most failing results `checks_results` lists for one run.
pub const FAILURES_SHOWN_MAX: usize = 20;

const _: () = assert!(CHECKS_NAMED_MAX == 24);

/// The `checks_run` tool's listing. Output: its name, description and input schema.
pub(super) fn run_listing() -> Value {
    todo!()
}

/// The `checks_results` tool's listing. Output: its name, description and input schema.
pub(super) fn results_listing() -> Value {
    todo!()
}

/// Reads one `checks_run` call: at most `CHECKS_NAMED_MAX` distinct check ids, none for
/// every check the profile runs on request. Input: the arguments. Output: the call, or none
/// when a key is unknown or an id is not a plain id of up to `ID_BYTES_MAX` bytes.
pub(super) fn run_call(arguments: Option<&Value>) -> Option<Call> {
    todo!()
}

/// Reads one `checks_results` call, which takes no arguments. Input: the arguments.
/// Output: the call, or none when any argument is given.
pub(super) fn results_call(arguments: Option<&Value>) -> Option<Call> {
    todo!()
}

/// Answers a master's or sub-agent's checks call on its own change set in the project: the
/// newest one whose agent is the caller, never one the call names. Inputs: the engine
/// database, the project, the caller as the engine named it and the call, `checks_run` or
/// `checks_results`. Output: the sentence the caller reads and whether it is an error.
pub(in crate::team) fn answer(
    db: &Connection,
    project: &ProjectId,
    caller: Actor,
    call: Call,
) -> (String, bool) {
    todo!()
}

#[cfg(all(test, target_os = "macos"))]
#[path = "checks/tests.rs"]
mod tests;
