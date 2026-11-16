#[cfg(test)]
#[path = "master_listed_tests.rs"]
mod tests;

use super::harness_catalog::listed_install;
use super::master_runtime::{Capture, timed_out};
use super::{EngineError, OwnerRuntimeHandle, failure};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::credentials::SecretString;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::claude_host::MasterTools;
use crate::harnesses::accounts::Handover;
use crate::harnesses::launch::{
    AccountBinding, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest, RUN_TIME_MAX, Session,
    listed_command,
};
use crate::harnesses::registry::Harness;
use crate::launch_context::EngineServer;
use crate::master_session::{
    MasterBackend, MasterReply, MasterReplyProvenance, MasterRuntimeView, MasterTurnPhase,
    MasterTurnView, fitted_reply,
};
use crate::model_gateway::relay::Relay;
use crate::plugins::LaunchServer;
use crate::protocol::RequestId;
use crate::storage::{MasterRuntimeObservation, PreparedMasterTurn};
use crate::usage::capture::Tokens;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// How often a running listed master turn reads its stored turn again, for an owner's stop
/// and a revoked scope.
const INSPECT_INTERVAL: Duration = Duration::from_millis(100);
/// The cause of a listed master turn whose harness the engine could not be sure it stopped,
/// which stores the turn as disconnected.
const STOP_UNCERTAIN: &str = "master_runtime_cleanup_uncertain:listed_master_stop";
/// The cause of a listed master turn whose process thread ended without handing back what
/// its harness left, which stores the turn as disconnected.
const THREAD_LOST: &str = "master_runtime_cleanup_uncertain:listed_master_thread";

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

/// The real file a listed master turn runs: the one its harness's catalog entry leads to now,
/// as a team agent's launch resolves it, refused when the harness is found nowhere or its
/// installed version is below the manifest's `version.minimum`. Inputs: the harness, its
/// catalog entry, if found, and its installed version, if known. Output: the file, or
/// [`NOT_INSTALLED`] or [`TOO_OLD`].
pub(super) fn listed_program(
    harness: &Harness,
    entry: Option<&std::path::Path>,
    installed: Option<&str>,
) -> Result<PathBuf, EngineError> {
    let entry = entry
        .filter(|entry| entry.is_absolute())
        .ok_or_else(|| failure(NOT_INSTALLED))?;
    let path = std::env::var_os("PATH");
    let program = crate::paths::launch_executable(entry, path.as_deref())
        .map_err(|_| failure(NOT_INSTALLED))?;
    if crate::harnesses::too_old(harness, installed) {
        return Err(failure(TOO_OLD));
    }
    assert!(program.is_absolute());
    Ok(program)
}

/// What one listed master turn its harness ended stores: the reply, fitted to its room with
/// the bytes left out counted, under the provenance given, for a turn that answered;
/// cancelled for a turn stopped; else the turn's stop code, as for a turn with no end or a
/// reply holding a NUL byte. Inputs: what the turn gathered and the reply's provenance.
/// Output: the observation, or the stop code.
pub(super) fn turn_outcome(
    collected: Collected,
    provenance: MasterReplyProvenance,
) -> Result<MasterRuntimeObservation, EngineError> {
    let Some(ended) = collected.ended else {
        return Err(failure(TURN_FAILED));
    };
    match ended {
        Ok(()) if collected.text.contains('\0') => Err(failure("master_capture_incomplete")),
        Ok(()) if collected.text.trim().is_empty() => Err(failure("master_empty_reply")),
        Ok(()) => {
            let text = fitted_reply(&collected.text, collected.dropped);
            assert!(!text.trim().is_empty());
            Ok(MasterRuntimeObservation::Response {
                reply: MasterReply { text, provenance },
                delegations: Vec::new(),
            })
        }
        Err(CANCELLED) => Ok(MasterRuntimeObservation::Cancelled),
        Err(code) => Err(failure(code)),
    }
}

/// What a listed master turn stores for the error that ended it: disconnected when the
/// engine could not be sure its harness stopped, else failed for the reason every master
/// stops for. Input: the error. Output: the observation.
fn failure_observation(error: &EngineError) -> MasterRuntimeObservation {
    if error.0.starts_with("master_runtime_cleanup_uncertain:") {
        return MasterRuntimeObservation::Disconnected;
    }
    MasterRuntimeObservation::Failed {
        reason: super::master_runtime::reason_for_error(error),
        cause: error.0.clone(),
    }
}

/// The SHA-256 of the program a listed master turn runs, the artifact its stored run names,
/// read as a stream just before the harness starts. Input: the program. Output: 64
/// lowercase hex characters, or [`NOT_INSTALLED`] for a file that cannot be read.
#[cfg(unix)]
fn program_digest(program: &std::path::Path) -> Result<String, EngineError> {
    crate::setup::process::file_sha256(program).map_err(|_| failure(NOT_INSTALLED))
}

/// Refuses the digest of a listed master's program where no listed master runs, as a
/// listed master's home and launch need a Unix owner and mode to check. Takes the program,
/// unused. Returns [`UNSUPPORTED`].
#[cfg(not(unix))]
fn program_digest(_program: &std::path::Path) -> Result<String, EngineError> {
    Err(failure(UNSUPPORTED))
}

/// Everything one listed master turn's process needs once its account is handed over and
/// its sandbox is set: the harness, the checked request whose home is the session's private
/// home, the hand-over and its relay, if keyed, the instruction text, the prompt and the
/// turn's capture.
struct ListedTurn {
    harness: &'static Harness,
    request: LaunchRequest,
    handover: Handover,
    relay: Option<Relay>,
    instructions: String,
    prompt: String,
    capture: Capture,
}

/// What one listed master turn's process left once its thread stopped it: what the turn
/// gathered or why it ended without, how the stop went, [`STOP_UNCERTAIN`] when the engine
/// could not be sure, and the turn's capture with every event recorded.
struct ListedRun {
    driven: Result<Collected, EngineError>,
    stopped: Result<(), EngineError>,
    capture: Capture,
}

/// How a listed master turn's thread and its async side talk: the stop flag the async side
/// sets for an owner's stop or a revoked scope, the start report the thread sends once its
/// harness runs, with its process group and program digest, and the go signal it waits on
/// before the prompt.
struct Signals {
    stop: Arc<AtomicBool>,
    started: tokio::sync::oneshot::Sender<Result<(u32, String), EngineError>>,
    go: std::sync::mpsc::Receiver<bool>,
}

/// Opens one listed master turn's process: takes the private home's lock, writes the
/// turn's files there under it, reads the program's digest and starts the harness holding
/// the relay. Inputs: the harness, the request, the hand-over, the relay and the instruction
/// text. Output: the lock, the running session and the digest, or the refusal.
fn open_listed(
    harness: &'static Harness,
    request: LaunchRequest,
    handover: &Handover,
    relay: Option<Relay>,
    instructions: &str,
) -> Result<(crate::launch_context::Installed, Session, String), EngineError> {
    let home = request.account.home();
    let lock = crate::launch_context::lock_only(home, &harness.manifest().id)
        .map_err(engine_error)?;
    let launch = listed_launch(&ListedSpec {
        harness,
        request: &request,
        handover,
        tools: None,
        instructions,
    })?;
    let artifact = program_digest(&request.executable)?;
    let session = Session::start_listed(request, launch.command, relay, &launch.servers)
        .map_err(engine_error)?;
    assert!(crate::engine::owner::is_hex_id(&artifact));
    Ok((lock, session, artifact))
}

/// Runs one listed master turn's process from start to stop on the thread it is called on,
/// so the harness never outlives the turn: opens it, reports its process group and digest,
/// drives the prompt once told to go, recording every event, then stops the harness whatever
/// happened. Inputs: the turn and the signals. Output: what the process left.
fn run_listed(turn: ListedTurn, signals: Signals) -> ListedRun {
    let ListedTurn {
        harness,
        request,
        handover,
        relay,
        instructions,
        prompt,
        mut capture,
    } = turn;
    let opened = open_listed(harness, request, &handover, relay, &instructions);
    let (_lock, mut session, artifact) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let _ = signals.started.send(Err(error.clone()));
            return ListedRun {
                driven: Err(error),
                stopped: Ok(()),
                capture,
            };
        }
    };
    let _ = signals.started.send(Ok((session.process_group(), artifact)));
    let driven = match signals.go.recv() {
        Ok(true) => {
            let stop = || signals.stop.load(Ordering::SeqCst);
            let mut record = |event: &LaunchEvent| capture.record(event);
            drive_with(&mut session, &prompt, &stop, &mut record)
        }
        Ok(false) | Err(_) => Err(failure(CANCELLED)),
    };
    let stopped = session
        .stop()
        .map(|_| ())
        .map_err(|_| failure(STOP_UNCERTAIN));
    ListedRun {
        driven,
        stopped,
        capture,
    }
}

impl OwnerRuntimeHandle {
    /// Runs one prepared listed master turn and stores how it ended (DEC-180 Task 20): its
    /// reply, cancelled, or the reason it failed, a harness the engine could not be sure it
    /// stopped stored as disconnected. Inputs: the prepared turn and its id. Output: the
    /// stored turn, or the error that ended it.
    pub(super) async fn launch_listed_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
    ) -> Result<MasterTurnView, EngineError> {
        match self.run_listed_master_turn(prepared, turn_id).await {
            Ok(observation) => {
                self.observe_master_turn(prepared, turn_id, observation)
                    .await
            }
            Err(error) if error.0 == "master_runtime_profile_not_acquired" => {
                self.inspect_master_turn(
                    &prepared.session.project_id,
                    &prepared.session.session_id,
                    turn_id,
                )
                .await
            }
            Err(error) => {
                let error = timed_out(error, prepared.time);
                self.observe_master_turn(prepared, turn_id, failure_observation(&error))
                    .await?;
                Err(error)
            }
        }
    }

    /// Sets up one listed master turn on this side: its binding, budget and program, its
    /// account's custody use, the session's private home, the hand-over with the relay of a
    /// keyed account, the capture and the sandbox, which reaches only the hand-over's address.
    /// Inputs: the prepared turn and its id. Output: the observation to store, or the code.
    async fn run_listed_master_turn(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
    ) -> Result<MasterRuntimeObservation, EngineError> {
        let backend = prepared.session.selection.backend;
        let connection = &prepared.attempt.connection;
        let MasterBackend::Listed(listed) = backend else {
            return Err(failure("master_runtime_binding"));
        };
        if prepared.session.current_turn.as_ref() != Some(turn_id)
            || prepared.attempt.request.project_id != prepared.session.project_id
            || !prepared.attempt_on_current_turn()
            || *connection != prepared.profile.binding().connection
            || !crate::master_session::backend_member(backend, connection)
            || prepared.tools
        {
            return Err(failure("master_runtime_binding"));
        }
        let harness = crate::master_session::listed_master(listed)?;
        let resources = &prepared.attempt.request.route.resources;
        resources
            .validate()
            .map_err(|_| failure("master_runtime_budget_exhausted"))?;
        let root = super::changes::engine_root(&self.authority)?;
        let id = harness.manifest().id.as_str();
        let search = self.harness_search()?;
        let (entry, installed) = listed_install(&self.engine, root.clone(), search, id)
            .await;
        let program = listed_program(harness, entry.as_deref(), installed.as_deref())?;
        let (custody, _hold) = self.acquire_master_profile(prepared, turn_id).await?;
        let current = self
            .inspect_master_turn(
                &prepared.session.project_id,
                &prepared.session.session_id,
                turn_id,
            )
            .await?;
        super::master_rcli::validate_turn(prepared, turn_id, &current)?;
        if current.phase == MasterTurnPhase::Cancelled {
            return Ok(MasterRuntimeObservation::Cancelled);
        }
        let generation = current.ordinal;
        custody.revalidate(
            &self.identity(),
            prepared.profile.binding(),
            turn_id,
            generation,
            NativeProfileUsePurpose::Execution,
        )?;
        let slot = custody.profile_path()?;
        let session_id = &prepared.session.session_id;
        let home = listed_home(&slot, session_id, harness, &connection.id)?;
        let key = match &connection.auth {
            ModelAuthentication::ApiKey { credential_ref } => Some(
                crate::storage::accounts::read_key_file(&root, credential_ref)?
                    .ok_or_else(|| failure("account_key_missing"))?,
            ),
            _ => None,
        };
        let (handover, relay) = hand_over(harness, connection, key)?;
        let given = crate::agent_home::text(&prepared.agent_project)
            .map_err(|problem| EngineError(problem.0))?;
        let instructions = instruction_text(&prepared.instructions, given);
        let (runtime_ref, capture_ref, file) = self.allocate_master_capture()?;
        let _folder =
            super::master_host_launch::MasterRuntimeFolder::new(&self.authority.root, &runtime_ref);
        let endpoint = ModelEndpoint::Http {
            url: handover.base_url().to_owned(),
        };
        let sandbox = super::master_host_launch::master_sandbox(
            &self.authority.root,
            &runtime_ref,
            &prepared.repository,
            &prepared.cwd,
            &home,
            &program,
            &endpoint,
            &prepared.agent_policy,
            &prepared.hidden_paths,
        )?;
        if sandbox.is_unconfined() {
            return Err(failure("launch_environment_refused"));
        }
        let capture = Capture::new(file, resources.output_bytes)?;
        let request = LaunchRequest {
            harness: id.to_owned(),
            executable: program,
            working_dir: prepared.cwd.clone(),
            account: AccountBinding::Url {
                home,
                base_url: handover.base_url().to_owned(),
                key: None,
            },
            model: Some(handover.model().to_owned()),
            tools: false,
            sandbox_profile: Some(sandbox.profile().to_path_buf()),
            unconfined: false,
            git_identity: None,
            limits: turn_limits(prepared.time, prepared.provider_retry),
        };
        let turn = ListedTurn {
            harness,
            request,
            handover,
            relay,
            instructions,
            prompt: prepared.text.clone(),
            capture,
        };
        let refs = (runtime_ref, capture_ref);
        self.drive_listed(prepared, turn_id, &custody, generation, turn, refs)
            .await
    }

    /// Runs one listed master turn's process on a thread of its own and waits for it: once
    /// the harness runs, its group and the turn as running are stored and the prompt goes,
    /// and while it works the stored turn is read again, telling the thread to stop for an
    /// owner's stop or a revoked scope. Every path waits for the thread to end. Inputs: the
    /// turn's parts. Output: the observation to store, or the stop code.
    async fn drive_listed(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        custody: &NativeProfileUse,
        generation: u64,
        turn: ListedTurn,
        (runtime_ref, capture_ref): (RequestId, RequestId),
    ) -> Result<MasterRuntimeObservation, EngineError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (started, opened) = tokio::sync::oneshot::channel();
        let (go, going) = std::sync::mpsc::channel();
        let signals = Signals {
            stop: Arc::clone(&stop),
            started,
            go: going,
        };
        let mut running = tokio::task::spawn_blocking(move || run_listed(turn, signals));
        let opened = opened.await.unwrap_or_else(|_| Err(failure(THREAD_LOST)));
        let begun = match &opened {
            Ok((group, artifact)) => {
                let runtime = MasterRuntimeView {
                    runtime_ref: runtime_ref.clone(),
                    artifact_sha256: artifact.clone(),
                    native_session_id: None,
                };
                self.begin_listed(prepared, turn_id, *group, runtime).await
            }
            Err(_) => Ok(false),
        };
        let _ = go.send(matches!(begun, Ok(true)));
        let mut cancelled = matches!(begun, Ok(false));
        let mut scope = None;
        let joined = loop {
            tokio::select! {
                joined = &mut running => break joined,
                () = tokio::time::sleep(INSPECT_INTERVAL), if !stop.load(Ordering::SeqCst) => {
                    let read = self.inspect_listed(prepared, turn_id, generation, custody);
                    match read.await {
                        Ok(current) => cancelled |= current.phase == MasterTurnPhase::Cancelled,
                        Err(error) => scope = Some(error),
                    }
                    stop.store(cancelled || scope.is_some(), Ordering::SeqCst);
                }
            }
        };
        let run = joined.map_err(|_| failure(THREAD_LOST))?;
        opened?;
        let ListedRun {
            driven,
            stopped,
            capture,
        } = run;
        let tokens = driven.as_ref().ok().and_then(|collected| collected.tokens);
        self.count_master_usage(prepared, None, tokens.unwrap_or_default())
            .await;
        let finished = if capture.sequence() > 0 {
            capture.finish()
        } else {
            Ok(())
        };
        stopped?;
        begun?;
        if let Some(error) = scope {
            return Err(error);
        }
        finished?;
        if cancelled {
            return Ok(MasterRuntimeObservation::Cancelled);
        }
        let provenance = MasterReplyProvenance {
            attempt_id: prepared.attempt.request.id.clone(),
            runtime_ref,
            capture_ref,
            capture_sequence: capture.sequence(),
        };
        let outcome = turn_outcome(driven?, provenance)?;
        let replied = matches!(outcome, MasterRuntimeObservation::Response { .. });
        assert!(!replied || capture.sequence() > 0);
        Ok(outcome)
    }

    /// Stores one listed master turn as running once its harness runs: its process group
    /// first, then its runtime. Inputs: the prepared turn, its id, the group and the runtime.
    /// Output: true to send the prompt, false for a turn the owner stopped first, or the
    /// refusal that ends the turn.
    async fn begin_listed(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        group: u32,
        runtime: MasterRuntimeView,
    ) -> Result<bool, EngineError> {
        self.record_master_child(prepared, turn_id, Some(group))
            .await?;
        let running = self
            .observe_master_turn(
                prepared,
                turn_id,
                MasterRuntimeObservation::Running { runtime },
            )
            .await?;
        Ok(running.phase != MasterTurnPhase::Cancelled)
    }

    /// Reads one running listed master turn again, refused once its turn, attempt or
    /// generation changed or its custody use no longer holds. Inputs: the prepared turn, its
    /// id, the generation and the custody use. Output: the stored turn, or the refusal.
    async fn inspect_listed(
        &self,
        prepared: &PreparedMasterTurn,
        turn_id: &RequestId,
        generation: u64,
        custody: &NativeProfileUse,
    ) -> Result<MasterTurnView, EngineError> {
        let current = self
            .inspect_master_turn(
                &prepared.session.project_id,
                &prepared.session.session_id,
                turn_id,
            )
            .await?;
        super::master_rcli::validate_turn(prepared, turn_id, &current)?;
        if current.ordinal != generation {
            return Err(failure("master_runtime_scope_revoked"));
        }
        if current.phase != MasterTurnPhase::Cancelled {
            custody.revalidate(
                &self.identity(),
                prepared.profile.binding(),
                turn_id,
                generation,
                NativeProfileUsePurpose::Execution,
            )?;
        }
        Ok(current)
    }
}
