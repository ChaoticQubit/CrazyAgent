#[cfg(test)]
#[path = "master_listed_tests.rs"]
mod tests;

use super::{EngineError, failure};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::credentials::SecretString;
use crate::engine::claude_host::MasterTools;
use crate::harnesses::accounts::Handover;
use crate::harnesses::launch::{
    LaunchError, LaunchEvent, LaunchLimits, LaunchRequest, RUN_TIME_MAX, Session, listed_command,
};
use crate::harnesses::registry::Harness;
use crate::launch_context::EngineServer;
use crate::model_gateway::relay::Relay;
use crate::plugins::LaunchServer;
use crate::usage::capture::Tokens;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// The code of a listed master turn its harness ended as failed, without a rate limit.
pub(super) const TURN_FAILED: &str = "listed_master_turn_failed";
/// The code of a listed master turn whose harness exited before the turn ended.
pub(super) const PROCESS_EXITED: &str = "listed_master_process_exited";
/// The code of a listed master turn its harness ended as interrupted.
pub(super) const CANCELLED: &str = "listed_master_cancelled";
/// The code of a listed master turn whose harness is found nowhere the catalog looks.
pub(super) const NOT_INSTALLED: &str = "listed_master_not_installed";
/// The code of a listed master turn whose installed harness is older than its manifest's
/// `version.minimum`, which a client words as "needs X or newer".
pub(super) const TOO_OLD: &str = "listed_master_harness_too_old";
/// The code of a listed master launch whose manifest's render or home block does not read.
const UNSUPPORTED: &str = "master_runtime_backend_unsupported";
/// The most reply text one turn gathers; the rest is counted, and the stored reply is cut
/// to [`crate::master_session::REPLY_BYTES_MAX`] with its cut note.
const GATHERED_BYTES_MAX: usize = 4 * crate::master_session::REPLY_BYTES_MAX;
/// How long one wait for a listed master's next event lasts before the turn reads its stop
/// check again.
const EVENT_WAIT: Duration = Duration::from_millis(100);

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

/// Builds one listed master turn's launch: the manifest's ACP arguments with its tools-off
/// switch when tools are off, the hand-over variables through the agent sandbox's guard, the
/// team server for its session when tools are on, and an MCP config naming no server of the
/// engine's or the owner's, beside the instruction file. Input: the turn's launch. Output:
/// the launch, or the refusal.
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
    let arguments: Vec<OsString> = crate::harnesses::drivers::acp::arguments(
        manifest,
        home,
        &request.working_dir,
        request.model.as_deref(),
        request.tools,
    )
    .map_err(engine_error)?
    .into_iter()
    .map(OsString::from)
    .collect();
    let renders = crate::harnesses::render::read(manifest).map_err(|_| failure(UNSUPPORTED))?;
    let none = std::iter::empty::<&LaunchServer>();
    let written = crate::harnesses::render::write(&renders, home, none, spec.instructions)
        .map_err(engine_error)?;
    let server = spec.tools.map(team_server).transpose()?;
    let servers: Vec<EngineServer> = server.into_iter().collect();
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
    assert_eq!(servers.len(), usize::from(request.tools));
    Ok(ListedLaunch {
        command,
        servers,
        written,
    })
}

/// The engine's team server as a listed master's ACP session opens it, the same command and
/// arguments a native master's server gets from [`MasterTools::server_args`]. Input: the
/// tools-on parts. Output: the server, or `master_runtime_binding` for an unsafe path.
fn team_server(tools: MasterTools<'_>) -> Result<EngineServer, EngineError> {
    let engine = tools
        .engine
        .to_str()
        .filter(|text| tools.engine.is_absolute() && !text.chars().any(char::is_control))
        .ok_or_else(|| failure("master_runtime_binding"))?;
    let args = tools
        .server_args()
        .map_err(|_| failure("master_runtime_binding"))?;
    Ok(EngineServer {
        name: crate::team::tool::SERVER_NAME.into(),
        command: engine.into(),
        args,
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

/// Drives one listed master turn on its running harness: sends the prompt, then folds every
/// event until the turn ends, asking the harness once to stop the turn when `stop` says so
/// and ending it cancelled once the session's stop grace passes after that; a turn the
/// harness has already ended needs no stop. Inputs: the session, the prompt and the stop
/// check. Output: what the turn gathered, with its end set, or the refusal code.
#[cfg(test)]
pub(super) fn drive(
    session: &mut Session,
    prompt: &str,
    stop: &dyn Fn() -> bool,
) -> Result<Collected, EngineError> {
    drive_with(session, prompt, stop, &mut |_| Ok(()))
}

/// Drives one listed master turn as [`drive`] does, handing every event to `record` before
/// it is folded, so the turn's capture holds each one. Inputs: the session, the prompt, the
/// stop check and the recorder. Output: what the turn gathered, with its end set, or the
/// refusal code of the session or the recorder.
pub(super) fn drive_with(
    session: &mut Session,
    prompt: &str,
    stop: &dyn Fn() -> bool,
    record: &mut dyn FnMut(&LaunchEvent) -> Result<(), EngineError>,
) -> Result<Collected, EngineError> {
    let mut collected = Collected::default();
    let mut asked: Option<Instant> = None;
    let grace = session.stop_grace();
    session.send(prompt).map_err(engine_error)?;
    while collected.ended.is_none() {
        if asked.is_none() && stop() {
            asked = Some(Instant::now());
            match session.interrupt() {
                Ok(()) => {}
                Err(error) if error.0 == "launch_not_running" => {}
                Err(error) => return Err(engine_error(error)),
            }
        }
        if let Some(event) = session.next_event(EVENT_WAIT).map_err(engine_error)? {
            record(&event)?;
            collect(&mut collected, event);
        }
        if collected.ended.is_none() && asked.is_some_and(|asked| asked.elapsed() >= grace) {
            collected.ended = Some(Err(CANCELLED));
        }
    }
    assert!(collected.ended.is_some());
    Ok(collected)
}

/// The limits one listed master turn's harness runs under: the owner's master time limit, or
/// the longest run a launch takes when the owner chose none, so the launch's own default
/// never ends a turn early, and the owner's provider retry. Inputs: the time limit and the
/// retry. Output: the limits.
pub(super) fn turn_limits(
    time: crate::master_time_limit::TimeLimit,
    retry: crate::provider_retry::ProviderRetry,
) -> LaunchLimits {
    let limits = LaunchLimits {
        run_time: time.run_time().unwrap_or(RUN_TIME_MAX),
        provider_retry: retry,
        ..LaunchLimits::default()
    };
    assert!(!limits.run_time.is_zero() && limits.run_time <= RUN_TIME_MAX);
    limits
}

/// Makes the private home one listed master session's turns run in, below its account's
/// custody slot home: `<slot home>/<session>/<harness id>/<account>`, each folder 0700, so
/// two sessions on one account never share an instruction file, an MCP file or harness
/// state, and refuses one that holds a sign-in's record. Inputs: the slot home, the session,
/// the harness and the account. Output: the home, or the refusal.
#[cfg(unix)]
pub(super) fn listed_home(
    base: &std::path::Path,
    session: &crate::protocol::SessionId,
    harness: &Harness,
    account: &crate::protocol::CredentialRef,
) -> Result<PathBuf, EngineError> {
    crate::harnesses::home::private_folder(base, &[session.as_str()]).map_err(engine_error)?;
    let folder = base.join(session.as_str());
    let home = crate::harnesses::home::private_home(&folder, &harness.manifest().id, account)
        .map_err(engine_error)?;
    crate::harnesses::launch::holds_no_sign_in(&home).map_err(engine_error)?;
    assert!(home.starts_with(&folder) && folder.starts_with(base));
    Ok(home)
}

/// Refuses a listed master session's private home where its Unix owner and 0700 mode cannot
/// be checked. Takes the slot home, the session, the harness and the account, all unused.
/// Returns `launch_account_home_unavailable`.
#[cfg(not(unix))]
pub(super) fn listed_home(
    _base: &std::path::Path,
    _session: &crate::protocol::SessionId,
    _harness: &Harness,
    _account: &crate::protocol::CredentialRef,
) -> Result<PathBuf, EngineError> {
    Err(failure("launch_account_home_unavailable"))
}
