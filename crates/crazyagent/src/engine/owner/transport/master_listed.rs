#[cfg(test)]
#[path = "master_listed_tests.rs"]
mod tests;

use super::{EngineError, failure};
use crate::config::ModelConnection;
use crate::credentials::SecretString;
use crate::engine::claude_host::MasterTools;
use crate::harnesses::accounts::Handover;
use crate::harnesses::launch::{LaunchEvent, LaunchRequest};
use crate::harnesses::registry::Harness;
use crate::model_gateway::relay::Relay;
use crate::usage::capture::Tokens;
use std::path::PathBuf;
use std::process::Command;

/// The code of a listed master turn its harness ended as failed, without a rate limit.
pub(super) const TURN_FAILED: &str = "listed_master_turn_failed";
/// The code of a listed master turn whose harness exited before the turn ended.
pub(super) const PROCESS_EXITED: &str = "listed_master_process_exited";
/// The code of a listed master turn its harness ended as interrupted.
pub(super) const CANCELLED: &str = "listed_master_cancelled";

/// Gives a listed master's harness its account (rv-sa241 ruling t20-q1, condition 4): a keyed
/// account gets a fresh key relay started from this connection here, beside the hand-over
/// that names it. Inputs: the harness, the master's connection and its saved key, if keyed.
/// Output: the hand-over and the relay, which must live as long as the turn's process, or
/// the refusal.
pub(super) fn hand_over(
    harness: &Harness,
    connection: &ModelConnection,
    key: Option<SecretString>,
) -> Result<(Handover, Option<Relay>), EngineError> {
    let _ = (harness, connection, key);
    Err(failure("master_runtime_backend_unsupported"))
}

/// One listed master turn's launch as the turn has it ready: the harness, the checked
/// request whose account home is the harness's private home, the account's hand-over, the
/// team tools when they are on and the instruction text.
pub(super) struct ListedSpec<'a> {
    pub(super) harness: &'a Harness,
    pub(super) request: &'a LaunchRequest,
    pub(super) handover: &'a Handover,
    pub(super) tools: Option<MasterTools<'a>>,
    pub(super) instructions: &'a str,
}

/// The command a listed master turn spawns, exactly as built, and the files written for it
/// in the private home: the rendered MCP config and the instruction file.
pub(super) struct ListedLaunch {
    pub(super) command: Command,
    pub(super) written: Vec<PathBuf>,
}

/// Builds one listed master turn's launch: the manifest's piped arguments with its tools-off
/// switch when tools are off, the hand-over variables through the agent sandbox's guard, and
/// the MCP config, holding only the team server when tools are on, and instruction file.
/// Input: the turn's launch. Output: the launch, or the refusal.
pub(super) fn listed_launch(spec: &ListedSpec<'_>) -> Result<ListedLaunch, EngineError> {
    let _ = spec;
    Err(failure("master_runtime_backend_unsupported"))
}

/// What one listed master's instruction file holds: its startup instruction and then the
/// owner's app and project instructions, exactly as an rcli master's file does. Inputs: the
/// startup instruction and the owner's text. Output: the file text.
pub(super) fn instruction_text(startup: &str, given: crate::agent_home::Given) -> String {
    let _ = (startup, given);
    String::new()
}

/// What one listed master turn has gathered from its harness so far: the harness's session
/// id, the reply text, the tokens it reported, if it did, and how the turn ended, once it
/// has.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Collected {
    pub(super) session: Option<String>,
    pub(super) text: String,
    pub(super) tokens: Option<Tokens>,
    pub(super) ended: Option<Result<(), &'static str>>,
}

/// Folds one event of a listed master turn into what it gathered; the first end stands and
/// nothing after it counts. Inputs: what was gathered and the event. Output: nothing.
pub(super) fn collect(collected: &mut Collected, event: LaunchEvent) {
    let _ = (collected, event);
}
