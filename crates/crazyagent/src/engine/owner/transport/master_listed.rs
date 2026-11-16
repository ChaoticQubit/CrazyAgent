#[cfg(test)]
#[path = "master_listed_tests.rs"]
mod tests;

use super::{EngineError, failure};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::credentials::SecretString;
use crate::engine::claude_host::MasterTools;
use crate::harnesses::accounts::Handover;
use crate::harnesses::launch::{LaunchError, LaunchEvent, LaunchRequest, listed_command};
use crate::harnesses::manifest::{DriverKind, Manifest};
use crate::harnesses::registry::Harness;
use crate::launch_context::EngineServer;
use crate::model_gateway::relay::Relay;
use crate::plugins::{LaunchServer, ServerSpec};
use crate::usage::capture::Tokens;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// The code of a listed master turn its harness ended as failed, without a rate limit.
pub(super) const TURN_FAILED: &str = "listed_master_turn_failed";
/// The code of a listed master turn whose harness exited before the turn ended.
pub(super) const PROCESS_EXITED: &str = "listed_master_process_exited";
/// The code of a listed master turn its harness ended as interrupted.
pub(super) const CANCELLED: &str = "listed_master_cancelled";
/// The code of a listed master launch its manifest cannot give: no ACP driver, a placeholder
/// it does not fill or a path that is not text.
const UNSUPPORTED: &str = "master_runtime_backend_unsupported";
/// The most reply text one turn gathers; the rest is counted, and the stored reply is cut
/// to [`crate::master_session::REPLY_BYTES_MAX`] with its cut note.
const GATHERED_BYTES_MAX: usize = 4 * crate::master_session::REPLY_BYTES_MAX;

/// Carries a launch refusal over as the engine's error with the same code. Input: the
/// refusal. Output: the error.
fn engine_error(error: LaunchError) -> EngineError {
    EngineError(error.0)
}

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
    if !harness.account_fits(connection) {
        return Err(failure("account_not_compatible"));
    }
    let relay = match (&connection.auth, key) {
        (ModelAuthentication::ApiKey { .. }, Some(key)) => {
            let ModelEndpoint::Http { url } = &connection.endpoint else {
                return Err(failure("account_not_compatible"));
            };
            let base_url = crate::harnesses::endpoint_base_url(&harness.manifest().id, url);
            Some(Relay::start(base_url, key)?)
        }
        (ModelAuthentication::ApiKey { .. }, None) => return Err(failure("account_key_missing")),
        (ModelAuthentication::None {}, None) => None,
        (ModelAuthentication::None {}, Some(_)) => return Err(failure("account_relay_unexpected")),
        _ => return Err(failure("account_not_compatible")),
    };
    let handover = crate::harnesses::accounts::handover(harness, connection, relay.as_ref())?;
    assert!(
        relay
            .as_ref()
            .is_none_or(|relay| relay.token() == handover.token())
    );
    Ok((handover, relay))
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

/// The command a listed master turn spawns, exactly as built, the engine servers its ACP
/// session opens with, and the files written for it in the private home: the rendered MCP
/// config and the instruction file.
pub(super) struct ListedLaunch {
    pub(super) command: Command,
    pub(super) servers: Vec<EngineServer>,
    pub(super) written: Vec<PathBuf>,
}

/// Builds one listed master turn's launch: the manifest's piped arguments with its tools-off
/// switch when tools are off, the hand-over variables through the agent sandbox's guard, and
/// the MCP config, holding only the team server when tools are on, and instruction file.
/// Input: the turn's launch. Output: the launch, or the refusal.
pub(super) fn listed_launch(spec: &ListedSpec<'_>) -> Result<ListedLaunch, EngineError> {
    let request = spec.request;
    if spec.tools.is_some() != request.tools {
        return Err(failure("master_runtime_binding"));
    }
    if !request.tools && !spec.harness.tools_off() {
        return Err(failure("launch_tools_off_unsupported"));
    }
    let manifest = spec.harness.manifest();
    let home = request.account.home();
    let arguments = piped_arguments(manifest, request)?;
    let renders = crate::harnesses::render::read(manifest).map_err(|_| failure(UNSUPPORTED))?;
    let server = spec.tools.map(team_server).transpose()?;
    let written = crate::harnesses::render::write(&renders, home, &server, spec.instructions)
        .map_err(engine_error)?;
    let shaped = crate::harnesses::home::read(manifest).map_err(|_| failure(UNSUPPORTED))?;
    let handover = spec.handover;
    let mut environment = shaped.environment(home);
    environment.extend(
        handover
            .env()
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    let names: Vec<&str> = handover
        .env()
        .iter()
        .filter(|(_, value)| value == handover.token())
        .map(|(name, _)| name.as_str())
        .collect();
    let handed = crate::sandbox::Handed::new(&names, handover.token())
        .map_err(|_| failure("launch_environment_refused"))?;
    let command = listed_command(request, &arguments, &environment, handed)
        .map_err(engine_error)?;
    assert!(written.iter().all(|path| path.starts_with(home)));
    Ok(ListedLaunch {
        command,
        servers: Vec::new(),
        written,
    })
}

/// One listed master turn's arguments: the manifest's `piped.argv` with `piped.tools_off`
/// placed as the per-turn drivers place it when tools are off, filling only `{home}`,
/// `{cwd}` and `{model}`, so no token, key file or prompt can reach an argument. Inputs:
/// the manifest and the request. Output: the arguments, or the refusal.
fn piped_arguments(
    manifest: &Manifest,
    request: &LaunchRequest,
) -> Result<Vec<OsString>, EngineError> {
    let refused = || failure(UNSUPPORTED);
    let piped = manifest
        .block("piped")
        .filter(|_| manifest.piped == Some(DriverKind::Acp))
        .ok_or_else(refused)?;
    let listed = |key| crate::harnesses::drivers::jsonl_turn::arguments(piped, key);
    let argv = listed("argv").map_err(|_| refused())?.unwrap_or_default();
    let off = if request.tools {
        Vec::new()
    } else {
        listed("tools_off")
            .map_err(|_| refused())?
            .unwrap_or_default()
    };
    let at = crate::harnesses::drivers::positional(&argv, &[], &off).map_err(|_| refused())?;
    let home = request.account.home().to_str().ok_or_else(refused)?;
    let cwd = request.working_dir.to_str().ok_or_else(refused)?;
    let model = request.model.as_deref();
    let value = |name: &str| match (name, model) {
        ("home", _) => Ok(home),
        ("cwd", _) => Ok(cwd),
        ("model", Some(model)) => Ok(model),
        _ => Err(LaunchError(UNSUPPORTED.into(), None)),
    };
    let arguments: Vec<OsString> = crate::harnesses::drivers::ordered(&argv, at, &[], &off)
        .map(|argument| crate::harnesses::drivers::fill(argument, value).map(OsString::from))
        .collect::<Result<_, _>>()
        .map_err(engine_error)?;
    assert_eq!(arguments.len(), argv.len() + off.len());
    Ok(arguments)
}

/// The engine's team server as a listed master's MCP config names it, the same command and
/// arguments a native master's server gets from [`MasterTools::server_args`]. Input: the
/// tools-on parts. Output: the server, or `master_runtime_binding` for an unsafe path.
fn team_server(tools: MasterTools<'_>) -> Result<LaunchServer, EngineError> {
    let engine = tools
        .engine
        .to_str()
        .filter(|text| tools.engine.is_absolute() && !text.chars().any(char::is_control))
        .ok_or_else(|| failure("master_runtime_binding"))?;
    let args = tools
        .server_args()
        .map_err(|_| failure("master_runtime_binding"))?;
    Ok(LaunchServer {
        name: crate::team::tool::SERVER_NAME.into(),
        spec: ServerSpec::Stdio {
            command: engine.into(),
            args,
        },
        secrets: Vec::new(),
    })
}

/// What one listed master's instruction file holds: its startup instruction and then the
/// owner's app and project instructions, exactly as an rcli master's file does. Inputs: the
/// startup instruction and the owner's text. Output: the file text.
pub(super) fn instruction_text(startup: &str, given: crate::agent_home::Given) -> String {
    super::master_rcli::instruction_file_text(startup, given)
}

/// What one listed master turn has gathered from its harness so far: the harness's session
/// id, the reply text up to [`GATHERED_BYTES_MAX`] and the bytes past it, the tokens it
/// reported, if it did, and how the turn ended, once it has.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Collected {
    pub(super) session: Option<String>,
    pub(super) text: String,
    pub(super) dropped: usize,
    pub(super) tokens: Option<Tokens>,
    pub(super) ended: Option<Result<(), &'static str>>,
}

/// Folds one event of a listed master turn into what it gathered; the first end stands and
/// nothing after it counts. Inputs: what was gathered and the event. Output: nothing.
pub(super) fn collect(collected: &mut Collected, event: LaunchEvent) {
    if collected.ended.is_some() {
        return;
    }
    let ended = match event {
        LaunchEvent::Ready { native_session } => {
            collected.session = native_session.or(collected.session.take());
            None
        }
        LaunchEvent::Text { text } => {
            let room = GATHERED_BYTES_MAX - collected.text.len();
            let mut kept = text.len().min(room);
            while !text.is_char_boundary(kept) {
                kept -= 1;
            }
            collected.text.push_str(&text[..kept]);
            collected.dropped += text.len() - kept;
            None
        }
        LaunchEvent::Usage { tokens } => {
            collected.tokens = Some(tokens);
            None
        }
        LaunchEvent::TurnFinished { success: true } if collected.text.trim().is_empty() => {
            Some(Err("master_empty_reply"))
        }
        LaunchEvent::TurnFinished { success: true } => Some(Ok(())),
        LaunchEvent::TurnFinished { success: false } => Some(Err(TURN_FAILED)),
        LaunchEvent::TurnError { rate_limited: true } => Some(Err("gateway_rate_limited")),
        LaunchEvent::TurnError { .. } => Some(Err(TURN_FAILED)),
        LaunchEvent::Interrupted => Some(Err(CANCELLED)),
        LaunchEvent::Exited { .. } => Some(Err(PROCESS_EXITED)),
        LaunchEvent::ToolUse { .. }
        | LaunchEvent::ApprovalDenied
        | LaunchEvent::LateOutput { .. } => None,
    };
    collected.ended = ended;
    assert!(collected.text.len() <= GATHERED_BYTES_MAX);
}
