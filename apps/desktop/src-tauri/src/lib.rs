pub mod bridge;
mod updates;

#[cfg(feature = "native-webdriver")]
mod native_refresh_observation;

use bridge::intake::FolderSize;
use bridge::memory::{MemoryList, MemoryNote};
use bridge::picker::Pick;
use bridge::project_setup::{MemoryItem, ResearchLog};
use bridge::terminal::Terminals;
use bridge::{
    AccountProfileView, ActionReceipt, BaselineDetail, BaselinePage, BridgeError, BridgeQueue,
    ConfiguredAccountPlan, ConnectionStatus, HistoryPage, IntentReceipt, ManagedAccountPlan,
    ModelAccessDetail, ModelAccessPage, NativeAuthView, NativeBridge, PendingIntent, ProjectDetail,
    ProjectPage,
};
use crazyagent::accounts::{AccountDraft, AccountView, ProviderEntry};
use crazyagent::agents::{AccountUse, AgentView};
use crazyagent::config::ModelAccessEdit;
use crazyagent::engine::owner::{
    AccountAuthCapabilitiesReply, AccountCheck, AccountPage, AccountSecret, ModelAccessCollection,
    NativeAuthMethod,
};
use crazyagent::github::{CloneReceipt, GitHubDeviceCode, GitHubRepoPage, GitHubStatus};
use crazyagent::harnesses::HarnessEntry;
use crazyagent::landing::{AcceptOutcome, ChangeDiff, ChangeSetView};
use crazyagent::memory::ReviewDecision;
use crazyagent::memory::library::ApprovedMemories;
use crazyagent::onboarding::OnboardingView;
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::plugins::{EffectivePlugins, Scope, ServerSpec, ServerView, SkillView};
use crazyagent::pools::PoolSummary;
use crazyagent::project_setup::{
    ContextChunk, ProjectCatalog, ProjectContext, ProjectSetup, ResearchKind, StoredSetup,
};
use crazyagent::projects_home::{AddMode, FoundProject, ProjectsHome};
use crazyagent::protocol::ProjectSpec;
use crazyagent::scheduling::{PoolRanking, Schedule, ScheduleChange};
use crazyagent::setup::SetupStatus;
use crazyagent::team::{TeamGraph, TeamStatus};
use crazyagent::terminal::TerminalRequest;
use crazyagent::usage::report::UsageReport;
use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::MutexGuard;
use updates::{AvailableUpdate, InstallOutcome, UpdateProgress, UpdateSettings, UpdateState};

pub struct NativeState(BridgeQueue);

pub struct TerminalState(Arc<Terminals>);

fn main_window(window: &WebviewWindow) -> Result<(), BridgeError> {
    if window.label() != "main" {
        return Err(BridgeError {
            code: "window_denied",
            cause: None,
        });
    }
    Ok(())
}

async fn lock(state: &NativeState) -> Result<MutexGuard<'_, NativeBridge>, BridgeError> {
    state.0.enter().await
}

const SOCKET_OVERRIDE: &str = "CRAZYAGENT_DESKTOP_SOCKET";

fn engine_failure(code: &'static str) -> BridgeError {
    BridgeError { code, cause: None }
}

/// The `crazyagent` executable bundled beside this app's own executable as a sidecar.
/// Output: its path with links resolved when it exists, else the path where it belongs.
fn sidecar_executable() -> Result<std::path::PathBuf, BridgeError> {
    let name = if cfg!(windows) {
        "crazyagent.exe"
    } else {
        "crazyagent"
    };
    let app = std::env::current_exe().map_err(|_| engine_failure("engine_sidecar_missing"))?;
    let sidecar = std::fs::canonicalize(&app)
        .unwrap_or(app)
        .parent()
        .ok_or_else(|| engine_failure("engine_sidecar_missing"))?
        .join(name);
    Ok(std::fs::canonicalize(&sidecar).unwrap_or(sidecar))
}

/// The default engine layout and the login service that runs this app's sidecar.
fn default_engine() -> Result<(crazyagent::paths::Layout, crazyagent::service::Service), BridgeError>
{
    let layout =
        crazyagent::paths::Layout::current().map_err(|_| engine_failure("not_configured"))?;
    let service = crazyagent::service::Service::current_user(
        crazyagent::paths::Platform::CURRENT,
        sidecar_executable()?,
        &layout,
    )
    .map_err(|_| engine_failure("engine_service_failed"))?;
    Ok((layout, service))
}

/// Installs the service when it is missing or `reinstall` asks, else starts it, then
/// waits for the engine with the shared bound. Inputs: the layout, the service and
/// whether to replace the definition with this app's sidecar. Output: Ok once it answers.
async fn start_engine(
    layout: &crazyagent::paths::Layout,
    service: &crazyagent::service::Service,
    reinstall: bool,
) -> Result<(), BridgeError> {
    let since = crazyagent::service::log_length(layout);
    let installed = service
        .status()
        .map_err(|_| engine_failure("engine_service_failed"))?
        .installed;
    if reinstall || !installed {
        if !service.executable.is_file() {
            return Err(engine_failure("engine_sidecar_missing"));
        }
        if installed {
            service
                .stop()
                .map_err(|_| engine_failure("engine_service_failed"))?;
        }
        service
            .install()
            .map_err(|_| engine_failure("engine_service_failed"))?;
    } else {
        service
            .start()
            .map_err(|_| engine_failure("engine_service_failed"))?;
    }
    crazyagent::service::wait_ready(layout, crazyagent::service::READY_TIMEOUT, None, since)
        .await
        .map_err(|_| engine_failure("engine_start_failed"))
}

/// Makes sure the default engine runs before the bridge connects: when none answers it
/// starts the login service, installing it for this app's sidecar on first launch. An
/// engine of another version is refused with `engine_version_mismatch`. Skipped when
/// `CRAZYAGENT_DESKTOP_SOCKET` names an engine explicitly.
async fn ensure_default_engine() -> Result<(), BridgeError> {
    if std::env::var_os(SOCKET_OVERRIDE).is_some_and(|socket| !socket.is_empty()) {
        return Ok(());
    }
    let (layout, service) = default_engine()?;
    if !crazyagent::service::answering(&layout.socket()) {
        start_engine(&layout, &service, false).await?;
    }
    match crazyagent::service::check_engine(
        layout.root(),
        crazyagent::engine::owner::OwnerClientKind::Desktop,
        std::time::Duration::from_secs(5),
    )
    .await
    {
        crazyagent::service::EngineCheck::Mismatch { .. } => {
            Err(engine_failure("engine_version_mismatch"))
        }
        crazyagent::service::EngineCheck::Ready { .. }
        | crazyagent::service::EngineCheck::Unavailable { .. } => Ok(()),
    }
}

#[tauri::command]
async fn desktop_connect(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    let mut bridge = lock(&state).await?;
    ensure_default_engine().await?;
    bridge.connect().await
}

/// The engine and app versions for the mismatch message.
#[derive(Debug, serde::Serialize)]
pub struct EngineVersions {
    pub engine: Option<String>,
    pub app: String,
    pub message: Option<String>,
}

#[tauri::command]
async fn desktop_engine_versions(window: WebviewWindow) -> Result<EngineVersions, BridgeError> {
    main_window(&window)?;
    let (layout, _) = default_engine()?;
    let check = crazyagent::service::check_engine(
        layout.root(),
        crazyagent::engine::owner::OwnerClientKind::Desktop,
        std::time::Duration::from_secs(5),
    )
    .await;
    let (engine, message) = match check {
        crazyagent::service::EngineCheck::Ready { version } => (Some(version), None),
        crazyagent::service::EngineCheck::Mismatch { version, message } => {
            (Some(version), Some(message))
        }
        crazyagent::service::EngineCheck::Unavailable { .. } => (None, None),
    };
    Ok(EngineVersions {
        engine,
        app: crazyagent::version::VERSION.into(),
        message,
    })
}

/// Restarts the default engine through its login service, pointing the service at this
/// app's sidecar so the engine and the app share one version, then reconnects.
#[tauri::command]
async fn desktop_restart_engine(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    if std::env::var_os(SOCKET_OVERRIDE).is_some_and(|socket| !socket.is_empty()) {
        return Err(engine_failure("engine_restart_unavailable"));
    }
    let mut bridge = lock(&state).await?;
    let (layout, service) = default_engine()?;
    start_engine(&layout, &service, true).await?;
    ensure_default_engine().await?;
    bridge.connect().await
}

#[tauri::command]
async fn desktop_status(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    Ok(lock(&state).await?.status())
}

#[tauri::command]
async fn desktop_harness_catalog(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<HarnessEntry>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.harness_catalog().await
}

#[tauri::command]
async fn desktop_baselines(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<BaselinePage, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.baselines(next).await
}

#[tauri::command]
async fn desktop_inspect_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<BaselineDetail, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.inspect(&id).await
}

#[tauri::command]
async fn desktop_approve_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.approve(&ticket).await
}

#[tauri::command]
async fn desktop_revoke_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.revoke(&ticket).await
}

#[tauri::command]
async fn desktop_reconcile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.reconcile().await
}

#[tauri::command]
async fn desktop_retry(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.retry().await
}

#[tauri::command]
async fn desktop_projects(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<ProjectPage, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.projects(next).await
}

#[tauri::command]
async fn desktop_create_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    spec: ProjectSpec,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.create_project(spec, generation).await
}

#[tauri::command]
async fn desktop_project_catalog(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    pool: Option<String>,
    research: Vec<ResearchKind>,
) -> Result<ProjectCatalog, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.project_catalog(pool, research).await
}

#[tauri::command]
async fn desktop_setup_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    spec: ProjectSpec,
    setup: ProjectSetup,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .setup_project(spec, setup, generation)
        .await
}

#[tauri::command]
async fn desktop_project_setup(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<Option<StoredSetup>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.project_setup(&id).await
}

#[tauri::command]
async fn desktop_research_jobs(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<bridge::project_setup::ResearchJobs, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.research_jobs(&id).await
}

#[tauri::command]
async fn desktop_project_context(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ProjectContext, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.project_context(&id).await
}

#[tauri::command]
async fn desktop_project_context_item(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    path: String,
    offset: u64,
) -> Result<ContextChunk, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .project_context_item(&id, path, offset)
        .await
}

#[tauri::command]
async fn desktop_save_research_to_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    research: ResearchKind,
    section: Option<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .save_research_to_memory(&id, research, section, generation)
        .await
}

#[tauri::command]
async fn desktop_add_project_documents(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    documents: Vec<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .add_project_documents(&id, documents, generation)
        .await
}

#[tauri::command]
async fn desktop_project_memories(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    after: Option<String>,
) -> Result<Vec<MemoryItem>, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .project_memories(&id, after.as_deref())
        .await
}

#[tauri::command]
async fn desktop_review_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    memory: String,
    version: u64,
    decision: ReviewDecision,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .review_memory(&id, &memory, version, decision, generation)
        .await
}

#[tauri::command]
async fn desktop_memories(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project: Option<String>,
    search: Option<String>,
    before: Option<u64>,
) -> Result<MemoryList, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .memories(project.as_deref(), search.as_deref(), before)
        .await
}

#[tauri::command]
async fn desktop_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<Option<MemoryNote>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.memory(&id).await
}

#[tauri::command]
async fn desktop_approved_memories(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project: String,
) -> Result<ApprovedMemories, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.approved_memories(&project).await
}

#[tauri::command]
async fn desktop_add_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project: Option<String>,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .add_memory(project.as_deref(), text, generation)
        .await
}

#[tauri::command]
async fn desktop_promote_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project: String,
    memory: String,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .promote_memory(&project, &memory, version, generation)
        .await
}

#[tauri::command]
async fn desktop_delete_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.delete_memory(&id, generation).await
}

#[tauri::command]
async fn desktop_forget_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project: String,
    memory: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .forget_memory(&project, &memory, generation)
        .await
}

#[tauri::command]
async fn desktop_save_selection_to_memory(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    path: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .save_selection_to_memory(&id, path, text, generation)
        .await
}

#[tauri::command]
async fn desktop_remember_research(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project: String,
    research: ResearchKind,
    section: Option<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .remember_research(&project, research, section, generation)
        .await
}

#[tauri::command]
async fn desktop_project_read_paths(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.project_read_paths(&id).await
}

#[tauri::command]
async fn desktop_set_project_read_paths(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    paths: Vec<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .set_project_read_paths(&id, paths, generation)
        .await
}

#[tauri::command]
async fn desktop_delete_project_context(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    path: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .delete_project_context(&id, path, generation)
        .await
}

#[tauri::command]
async fn desktop_team_status(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<TeamStatus, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.team_status(&id).await
}

#[tauri::command]
async fn desktop_team_message(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .team_message(&id, text, generation)
        .await
}

#[tauri::command]
async fn desktop_team_delegate(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    harness: Option<String>,
    prompt: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .team_delegate(&id, harness, prompt, generation)
        .await
}

#[tauri::command]
async fn desktop_team_cancel(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    task: Option<i64>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.team_cancel(&id, task, generation).await
}

#[tauri::command]
async fn desktop_team_keep_warm(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .team_keep_warm(&id, on, generation)
        .await
}

#[tauri::command]
async fn desktop_team_pause(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    paused: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .team_pause(&id, paused, generation)
        .await
}

#[tauri::command]
async fn desktop_team_hand_off(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    reply: i64,
    step: u8,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .team_hand_off(&id, reply, step, generation)
        .await
}

#[tauri::command]
async fn desktop_team_rename(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    agent: i64,
    handle: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .team_rename(&id, agent, handle, generation)
        .await
}

#[tauri::command]
async fn desktop_team_graph(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<TeamGraph, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.team_graph(&id).await
}

#[tauri::command]
async fn desktop_terminal_attach(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    terminals: State<'_, TerminalState>,
    agent: String,
    output: Channel<InvokeResponseBody>,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let agent = bridge::terminal::agent(&agent)?;
    let room = terminals.0.room(&agent)?;
    let session = lock(&state).await?.open_terminal(agent).await?;
    room.attach(session, output)
}

#[tauri::command]
async fn desktop_terminal_input(
    window: WebviewWindow,
    terminals: State<'_, TerminalState>,
    agent: String,
    bytes: Vec<u8>,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let agent = bridge::terminal::agent(&agent)?;
    terminals
        .0
        .send(&agent, TerminalRequest::Input { bytes })
        .await
}

#[tauri::command]
async fn desktop_terminal_prompt(
    window: WebviewWindow,
    terminals: State<'_, TerminalState>,
    agent: String,
    text: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let agent = bridge::terminal::agent(&agent)?;
    terminals
        .0
        .send(&agent, TerminalRequest::Prompt { text })
        .await
}

#[tauri::command]
async fn desktop_terminal_resize(
    window: WebviewWindow,
    terminals: State<'_, TerminalState>,
    agent: String,
    cols: u16,
    rows: u16,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let agent = bridge::terminal::agent(&agent)?;
    terminals
        .0
        .send(&agent, TerminalRequest::Resize { cols, rows })
        .await
}

#[tauri::command]
fn desktop_terminal_detach(
    window: WebviewWindow,
    terminals: State<'_, TerminalState>,
    agent: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let agent = bridge::terminal::agent(&agent)?;
    terminals.0.detach(&agent)
}

#[tauri::command]
async fn desktop_update_project_setup(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    setup: ProjectSetup,
    name: Option<String>,
    revision: Option<u64>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .update_project_setup(&id, setup, name, revision, generation)
        .await
}

#[tauri::command]
async fn desktop_remove_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.remove_project(&id, generation).await
}

#[tauri::command]
async fn desktop_cancel_research(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    job: i64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .cancel_research(&id, job, generation)
        .await
}

#[tauri::command]
async fn desktop_retry_research(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    job: i64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .retry_research(&id, job, generation)
        .await
}

#[tauri::command]
async fn desktop_research_log(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    job: i64,
) -> Result<ResearchLog, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.research_log(&id, job).await
}

#[tauri::command]
async fn desktop_projects_home(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ProjectsHome, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.projects_home().await
}

#[tauri::command]
async fn desktop_set_projects_home(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    path: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .set_projects_home(path, generation)
        .await
}

#[tauri::command]
async fn desktop_move_projects_home(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    path: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .move_projects_home(path, generation)
        .await
}

#[tauri::command]
async fn desktop_found_projects(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<FoundProject>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.found_projects().await
}

#[tauri::command]
async fn desktop_folder_size(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    path: String,
) -> Result<FolderSize, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.folder_size(path).await
}

#[tauri::command]
async fn desktop_add_existing_folder(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    source: String,
    mode: AddMode,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .add_existing_folder(source, mode, generation)
        .await
}

#[tauri::command]
async fn desktop_start_onboarding(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    path: String,
    pool: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .start_onboarding(path, pool, generation)
        .await
}

#[tauri::command]
async fn desktop_onboarding(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<OnboardingView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.onboarding(&id).await
}

#[tauri::command]
async fn desktop_github_status(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.github_status()?;
    call.await
}

#[tauri::command]
async fn desktop_github_sign_in(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<GitHubDeviceCode, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.github_sign_in()?;
    call.await
}

#[tauri::command]
async fn desktop_github_token(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    token: String,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.github_token(token)?;
    call.await
}

#[tauri::command]
async fn desktop_github_use_cli(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.github_use_cli()?;
    call.await
}

#[tauri::command]
async fn desktop_github_sign_out(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.github_sign_out()?;
    call.await
}

#[tauri::command]
async fn desktop_github_repos(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    page: u32,
) -> Result<GitHubRepoPage, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.github_repos(page)?;
    call.await
}

#[tauri::command]
async fn desktop_clone_repo(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    full_name: String,
) -> Result<CloneReceipt, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.clone_repo(full_name)?;
    call.await
}

#[tauri::command]
async fn desktop_setup_status(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<SetupStatus, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.setup_status().await
}

#[tauri::command]
async fn desktop_run_setup(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    pack: Vec<String>,
    generation: u64,
) -> Result<SetupStatus, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.run_setup(pack, generation).await
}

#[tauri::command]
async fn desktop_set_pack_entry(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    generation: u64,
) -> Result<SetupStatus, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.set_pack_entry(id, on, generation).await
}

#[tauri::command]
async fn desktop_remove_pack_entry(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    generation: u64,
) -> Result<SetupStatus, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.remove_pack_entry(id, generation).await
}

#[tauri::command]
async fn desktop_open_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ProjectDetail, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.open_project(&id).await
}

#[tauri::command]
async fn desktop_history(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<HistoryPage, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.history(next).await
}

#[tauri::command]
async fn desktop_send_message(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .send_message(&ticket, &text, generation)
        .await
}

#[tauri::command]
async fn desktop_master_state(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<bridge::master::MasterState, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.master_state(&ticket).await
}

#[tauri::command]
async fn desktop_master_turns(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    cursor: Option<crazyagent::master_session::MasterCursor>,
) -> Result<bridge::master::MasterTurns, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .master_turns(&ticket, &session_id, cursor)
        .await
}

#[tauri::command]
async fn desktop_open_master(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .open_default_master(&ticket, generation)
        .await
}

#[tauri::command]
async fn desktop_close_master(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .close_master(&ticket, &session_id, generation)
        .await
}

#[tauri::command]
async fn desktop_cancel_master_turn(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    session: String,
    turn: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .cancel_master_turn(&ticket, &session, &turn, generation)
        .await
}

#[tauri::command]
async fn desktop_submit_master(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .submit_master(&ticket, &session_id, &text, generation)
        .await
}

#[tauri::command]
async fn desktop_replace_brief(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .replace_brief(&ticket, &text, generation)
        .await
}

#[tauri::command]
async fn desktop_pending_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Option<PendingIntent>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.pending_intent()
}

#[tauri::command]
async fn desktop_reconcile_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.reconcile_intent().await
}

#[tauri::command]
async fn desktop_retry_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.retry_intent().await
}

#[tauri::command]
async fn desktop_drop_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.drop_intent()
}

#[tauri::command]
async fn desktop_prepare_managed_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    provider: String,
    label: String,
) -> Result<ManagedAccountPlan, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .prepare_managed_account(&provider, &label)
        .await
}

#[tauri::command]
async fn desktop_prepare_configured_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    provider: String,
    label: String,
    endpoint: String,
    model: String,
) -> Result<ConfiguredAccountPlan, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .prepare_configured_account(&provider, &label, &endpoint, &model)
        .await
}

#[tauri::command]
async fn desktop_account_auth_capabilities(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<AccountAuthCapabilitiesReply, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.account_auth_capabilities(&ticket).await
}

#[tauri::command]
async fn desktop_configure_account_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    secret: Option<String>,
    generation: u64,
) -> Result<AccountProfileView, BridgeError> {
    main_window(&window)?;
    let secret = secret
        .map(|value| AccountSecret::new(value.into_bytes()))
        .transpose()
        .map_err(|_| BridgeError {
            code: "account_secret_invalid",
            cause: None,
        })?;
    let mut bridge = state.0.enter_owned().await?;
    tauri::async_runtime::spawn(async move {
        bridge
            .configure_account_profile(&ticket, secret, generation)
            .await
    })
    .await
    .map_err(|_| BridgeError {
        code: "account_profile_outcome_unknown_read_same_request",
        cause: None,
    })?
}

#[tauri::command]
async fn desktop_read_account_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<AccountProfileView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.read_account_profile().await
}

#[tauri::command]
async fn desktop_owner_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Option<OwnerProfile>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.owner_profile().await
}

#[tauri::command]
async fn desktop_save_owner_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    profile: OwnerProfile,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .save_owner_profile(profile, generation)
        .await
}

#[tauri::command]
async fn desktop_pool_summaries(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<PoolSummary>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.pool_summaries().await
}

#[tauri::command]
async fn desktop_account_schedule(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Schedule, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.account_schedule().await
}

#[tauri::command]
async fn desktop_account_ranking(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<PoolRanking>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.account_ranking().await
}

#[tauri::command]
async fn desktop_set_account_schedule(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    change: ScheduleChange,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .set_account_schedule(change, generation)
        .await
}

#[tauri::command]
async fn desktop_account_catalog(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<ProviderEntry>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.account_catalog().await
}

#[tauri::command]
async fn desktop_list_accounts(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    after: Option<String>,
    limit: u16,
) -> Result<AccountPage, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.list_accounts(after, limit).await
}

#[tauri::command]
async fn desktop_add_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    draft: AccountDraft,
    key: Option<String>,
) -> Result<AccountView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.add_account(draft, key).await
}

#[tauri::command]
async fn desktop_rename_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    name: String,
) -> Result<AccountView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.rename_account(&id, name).await
}

#[tauri::command]
async fn desktop_remove_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state).await?.remove_account(&id).await
}

#[tauri::command]
async fn desktop_check_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<AccountCheck, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.check_account(&id).await
}

#[tauri::command]
async fn desktop_local_models(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    url: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.local_models(url).await
}

#[tauri::command]
async fn desktop_plugin_skills(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<SkillView>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.plugin_skills(project_id).await
}

#[tauri::command]
async fn desktop_plugin_servers(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<ServerView>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.plugin_servers(project_id).await
}

#[tauri::command]
async fn desktop_add_skill(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    scope: Scope,
    folder: String,
) -> Result<SkillView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.add_skill(scope, folder).await
}

#[tauri::command]
async fn desktop_set_skill_enabled(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    enabled: bool,
) -> Result<SkillView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.set_skill_enabled(id, enabled).await
}

#[tauri::command]
async fn desktop_skill_detail(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    project_id: Option<String>,
) -> Result<crazyagent::skills::SkillDetail, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.skill_detail(id, project_id).await
}

#[tauri::command]
async fn desktop_skill_import_scan(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<crazyagent::skills::import::ImportList, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.skill_import_scan().await
}

#[tauri::command]
async fn desktop_import_skill(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    folder: String,
    replace: Option<String>,
) -> Result<crazyagent::skills::import::Imported, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.import_skill(folder, replace).await
}

#[tauri::command]
async fn desktop_remove_skill(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state).await?.remove_skill(id).await
}

#[tauri::command]
async fn desktop_add_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    scope: Scope,
    name: String,
    spec: ServerSpec,
    secrets: Vec<(String, String)>,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .add_mcp_server(scope, name, spec, secrets)
        .await
}

#[tauri::command]
async fn desktop_update_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    spec: ServerSpec,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.update_mcp_server(id, spec).await
}

#[tauri::command]
async fn desktop_approve_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    spec: ServerSpec,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.approve_mcp_server(id, spec).await
}

#[tauri::command]
async fn desktop_set_mcp_server_enabled(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    enabled: bool,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .set_mcp_server_enabled(id, enabled)
        .await
}

#[tauri::command]
async fn desktop_remove_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state).await?.remove_mcp_server(id).await
}

#[tauri::command]
async fn desktop_switch_plugin(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: String,
    id: String,
    on: bool,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state).await?.switch_plugin(project_id, id, on).await
}

#[tauri::command]
async fn desktop_set_mcp_secret(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    name: String,
    value: String,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.set_mcp_secret(id, name, value).await
}

#[tauri::command]
async fn desktop_remove_mcp_secret(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    name: String,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.remove_mcp_secret(id, name).await
}

#[tauri::command]
async fn desktop_check_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.check_mcp_server(id)?;
    call.await
}

#[tauri::command]
async fn desktop_project_plugins(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<EffectivePlugins, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.project_plugins(project_id).await
}

#[tauri::command]
async fn desktop_agents(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<AgentView>, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.agents(project_id).await
}

#[tauri::command]
async fn desktop_stop_agent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<AgentView, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.stop_agent(id)?;
    call.await
}

#[tauri::command]
async fn desktop_account_use(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    account_id: String,
) -> Result<AccountUse, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.account_use(account_id).await
}

#[tauri::command]
async fn desktop_release_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    account_id: String,
    generation: u64,
) -> Result<AccountUse, BridgeError> {
    main_window(&window)?;
    let stop = lock(&state)
        .await?
        .stop_account_agents(account_id.clone())?;
    let holders = stop.await?;
    lock(&state).await?.end_holders(holders, generation).await?;
    let wait = lock(&state).await?.await_account_free(account_id)?;
    wait.await
}

#[tauri::command]
async fn desktop_interrupt_agent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.interrupt_agent(id)?;
    call.await
}

#[tauri::command]
async fn desktop_restart_agent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<AgentView, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.restart_agent(id)?;
    call.await
}

#[tauri::command]
async fn desktop_usage_report(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    since_ms: Option<i64>,
    offset_minutes: i32,
) -> Result<UsageReport, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.usage_report(since_ms, offset_minutes)?;
    call.await
}

#[tauri::command]
async fn desktop_usage_refresh(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.usage_refresh()?;
    call.await
}

#[tauri::command]
async fn desktop_usage_set_reset(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    account: String,
    window_kind: String,
    at_ms: Option<i64>,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let call = lock(&state)
        .await?
        .usage_set_reset(account, window_kind, at_ms)?;
    call.await
}

#[tauri::command]
async fn desktop_changes(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<Vec<ChangeSetView>, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.change_sets(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_change_diff(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    page: u16,
) -> Result<ChangeDiff, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.change_diff(id, page)?;
    call.await
}

#[tauri::command]
async fn desktop_accept_change(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    expected_head: String,
    message: Option<String>,
) -> Result<AcceptOutcome, BridgeError> {
    main_window(&window)?;
    let call = lock(&state)
        .await?
        .accept_change(id, expected_head, message)?;
    call.await
}

#[tauri::command]
async fn desktop_discard_change(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    expected_head: String,
) -> Result<u64, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.discard_change(id, expected_head)?;
    call.await
}

#[tauri::command]
async fn desktop_restore_change(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.restore_change(id)?;
    call.await
}

#[tauri::command]
async fn desktop_update_change(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.update_change(id)?;
    call.await
}

#[tauri::command]
async fn desktop_process_text(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<String, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.process_text(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_save_process_text(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    text: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.save_process_text(project_id, text)?;
    call.await
}

#[tauri::command]
async fn desktop_import_global_instructions(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<String, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.import_global_instructions()?;
    call.await
}

#[tauri::command]
async fn desktop_default_process(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<String, BridgeError> {
    main_window(&window)?;
    let call = lock(&state).await?.default_process()?;
    call.await
}

#[tauri::command]
async fn desktop_pick_folder(window: WebviewWindow) -> Result<Option<String>, BridgeError> {
    main_window(&window)?;
    Ok(
        match bridge::picker::choose(bridge::picker::FOLDER_SCRIPT).await? {
            Pick::Canceled => None,
            Pick::Paths(mut paths) => {
                assert!(!paths.is_empty());
                Some(paths.swap_remove(0))
            }
        },
    )
}

#[tauri::command]
async fn desktop_pick_documents(window: WebviewWindow) -> Result<Vec<String>, BridgeError> {
    main_window(&window)?;
    Ok(
        match bridge::picker::choose(bridge::picker::DOCUMENTS_SCRIPT).await? {
            Pick::Canceled => Vec::new(),
            Pick::Paths(paths) => paths,
        },
    )
}

#[tauri::command]
async fn desktop_open_link(window: WebviewWindow, url: String) -> Result<(), BridgeError> {
    main_window(&window)?;
    bridge::open::open_link(&url).await
}

#[tauri::command]
async fn desktop_reveal_project_folder(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state).await?.reveal_project_folder(&id).await
}

#[tauri::command]
async fn desktop_open_context_item(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    path: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state).await?.open_context_item(&id, &path).await
}

#[tauri::command]
async fn desktop_model_access_page(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    next: bool,
) -> Result<ModelAccessPage, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .model_access_page(collection, next)
        .await
}

#[tauri::command]
async fn desktop_inspect_model_access(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    id: String,
) -> Result<ModelAccessDetail, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .inspect_model_access(collection, &id)
        .await
}

#[tauri::command]
async fn desktop_edit_model_access(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    edit: ModelAccessEdit,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)
        .await?
        .edit_model_access(&ticket, edit, generation)
        .await
}

#[tauri::command]
async fn desktop_start_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    method: NativeAuthMethod,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.start_native_auth(&ticket, method).await
}

#[tauri::command]
async fn desktop_read_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.read_native_auth(&ticket).await
}

#[tauri::command]
async fn desktop_cancel_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.cancel_native_auth(&ticket).await
}

#[tauri::command]
async fn desktop_submit_claude_auth_code(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    code: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    let secret = NativeBridge::claude_auth_secret(code)?;
    let mut bridge = state.0.enter_owned().await?;
    tauri::async_runtime::spawn(
        async move { bridge.submit_claude_auth_code(&ticket, secret).await },
    )
    .await
    .map_err(|_| BridgeError {
        code: "native_auth_claude_input_outcome_unknown",
        cause: None,
    })?
}

#[tauri::command]
async fn desktop_logout_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state).await?.logout_native_auth(&ticket).await
}

/// The engine socket the desktop connects to: `CRAZYAGENT_DESKTOP_SOCKET` when set, else
/// the default engine's socket in its data root, so an app opened from the Dock or Finder,
/// which starts with no such variable, still finds the engine. Inputs: the variable's value
/// and the environment values that choose the data root. Output: the socket, or none.
pub fn desktop_socket(
    explicit: Option<std::ffi::OsString>,
    env: &crazyagent::paths::RootEnvironment,
) -> Option<std::path::PathBuf> {
    use crazyagent::paths::{Layout, Platform, data_root};
    if let Some(socket) = explicit.filter(|socket| !socket.is_empty()) {
        return Some(socket.into());
    }
    let root = data_root(Platform::CURRENT, env).ok()?;
    Some(Layout::new(root).ok()?.socket())
}

#[tauri::command]
fn desktop_update_settings(
    window: WebviewWindow,
    app: AppHandle,
) -> Result<UpdateSettings, BridgeError> {
    main_window(&window)?;
    updates::settings(&app)
}

#[tauri::command]
fn desktop_set_update_channel(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, UpdateState>,
    channel: String,
) -> Result<UpdateSettings, BridgeError> {
    main_window(&window)?;
    updates::set_channel(&app, &state, &channel)
}

#[tauri::command]
async fn desktop_check_update(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<Option<AvailableUpdate>, BridgeError> {
    main_window(&window)?;
    updates::check(&app, &state).await
}

#[tauri::command]
async fn desktop_install_update(
    window: WebviewWindow,
    state: State<'_, UpdateState>,
    progress: Channel<UpdateProgress>,
) -> Result<InstallOutcome, BridgeError> {
    main_window(&window)?;
    updates::install(&state, progress).await
}

#[tauri::command]
fn desktop_restart_app(window: WebviewWindow, app: AppHandle) -> Result<(), BridgeError> {
    main_window(&window)?;
    app.request_restart();
    Ok(())
}

#[tauri::command]
async fn desktop_link_cli(window: WebviewWindow) -> Result<String, BridgeError> {
    main_window(&window)?;
    updates::link_cli().await
}

pub fn run() {
    let socket = desktop_socket(
        std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET"),
        &crazyagent::paths::RootEnvironment::current(),
    );
    let builder = tauri::Builder::default()
        .manage(NativeState(BridgeQueue::new(NativeBridge::new(socket))))
        .manage(TerminalState(Arc::new(Terminals::default())))
        .manage(UpdateState::default())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            desktop_connect,
            desktop_status,
            desktop_harness_catalog,
            desktop_baselines,
            desktop_inspect_baseline,
            desktop_approve_baseline,
            desktop_revoke_baseline,
            desktop_reconcile,
            desktop_retry,
            desktop_projects,
            desktop_create_project,
            desktop_project_catalog,
            desktop_setup_project,
            desktop_project_setup,
            desktop_research_jobs,
            desktop_project_context,
            desktop_project_context_item,
            desktop_save_research_to_memory,
            desktop_add_project_documents,
            desktop_delete_project_context,
            desktop_project_read_paths,
            desktop_set_project_read_paths,
            desktop_team_status,
            desktop_team_message,
            desktop_team_delegate,
            desktop_team_cancel,
            desktop_team_keep_warm,
            desktop_team_pause,
            desktop_team_hand_off,
            desktop_team_rename,
            desktop_team_graph,
            desktop_terminal_attach,
            desktop_terminal_input,
            desktop_terminal_prompt,
            desktop_terminal_resize,
            desktop_terminal_detach,
            desktop_update_project_setup,
            desktop_remove_project,
            desktop_cancel_research,
            desktop_retry_research,
            desktop_research_log,
            desktop_projects_home,
            desktop_set_projects_home,
            desktop_move_projects_home,
            desktop_found_projects,
            desktop_folder_size,
            desktop_add_existing_folder,
            desktop_start_onboarding,
            desktop_onboarding,
            desktop_github_status,
            desktop_github_sign_in,
            desktop_github_token,
            desktop_github_use_cli,
            desktop_github_sign_out,
            desktop_github_repos,
            desktop_clone_repo,
            desktop_setup_status,
            desktop_run_setup,
            desktop_set_pack_entry,
            desktop_remove_pack_entry,
            desktop_project_memories,
            desktop_review_memory,
            desktop_memories,
            desktop_memory,
            desktop_approved_memories,
            desktop_add_memory,
            desktop_promote_memory,
            desktop_delete_memory,
            desktop_forget_memory,
            desktop_remember_research,
            desktop_save_selection_to_memory,
            desktop_open_project,
            desktop_history,
            desktop_send_message,
            desktop_replace_brief,
            desktop_master_state,
            desktop_master_turns,
            desktop_open_master,
            desktop_submit_master,
            desktop_close_master,
            desktop_cancel_master_turn,
            desktop_pending_intent,
            desktop_reconcile_intent,
            desktop_retry_intent,
            desktop_drop_intent,
            desktop_prepare_managed_account,
            desktop_prepare_configured_account,
            desktop_account_auth_capabilities,
            desktop_configure_account_profile,
            desktop_read_account_profile,
            desktop_model_access_page,
            desktop_inspect_model_access,
            desktop_edit_model_access,
            desktop_start_native_auth,
            desktop_read_native_auth,
            desktop_cancel_native_auth,
            desktop_logout_native_auth,
            desktop_submit_claude_auth_code,
            desktop_owner_profile,
            desktop_save_owner_profile,
            desktop_pool_summaries,
            desktop_account_schedule,
            desktop_account_ranking,
            desktop_set_account_schedule,
            desktop_account_catalog,
            desktop_list_accounts,
            desktop_add_account,
            desktop_rename_account,
            desktop_remove_account,
            desktop_check_account,
            desktop_local_models,
            desktop_plugin_skills,
            desktop_plugin_servers,
            desktop_add_skill,
            desktop_set_skill_enabled,
            desktop_remove_skill,
            desktop_skill_detail,
            desktop_skill_import_scan,
            desktop_import_skill,
            desktop_add_mcp_server,
            desktop_update_mcp_server,
            desktop_approve_mcp_server,
            desktop_set_mcp_server_enabled,
            desktop_remove_mcp_server,
            desktop_switch_plugin,
            desktop_set_mcp_secret,
            desktop_remove_mcp_secret,
            desktop_check_mcp_server,
            desktop_project_plugins,
            desktop_agents,
            desktop_stop_agent,
            desktop_account_use,
            desktop_release_account,
            desktop_interrupt_agent,
            desktop_restart_agent,
            desktop_changes,
            desktop_change_diff,
            desktop_accept_change,
            desktop_discard_change,
            desktop_restore_change,
            desktop_update_change,
            desktop_process_text,
            desktop_save_process_text,
            desktop_import_global_instructions,
            desktop_default_process,
            desktop_usage_report,
            desktop_usage_refresh,
            desktop_usage_set_reset,
            desktop_pick_folder,
            desktop_pick_documents,
            desktop_open_link,
            desktop_reveal_project_folder,
            desktop_open_context_item,
            desktop_engine_versions,
            desktop_restart_engine,
            desktop_update_settings,
            desktop_set_update_channel,
            desktop_check_update,
            desktop_install_update,
            desktop_restart_app,
            desktop_link_cli,
        ])
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("CrazyAgent")
                .inner_size(1180.0, 800.0)
                .min_inner_size(760.0, 560.0)
                .on_navigation(|url| {
                    (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
                        || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
                        || (cfg!(debug_assertions)
                            && url.scheme() == "http"
                            && url.host_str() == Some("127.0.0.1")
                            && url.port() == Some(1420))
                })
                .build()?;
            Ok(())
        });
    #[cfg(feature = "native-webdriver")]
    let builder = {
        let observer = native_refresh_observation::NativeObserver::from_environment();
        builder
            .on_page_load(move |webview, payload| {
                if let Some(observer) = &observer {
                    let phase = match payload.event() {
                        tauri::webview::PageLoadEvent::Started => {
                            native_refresh_observation::Phase::Started
                        }
                        tauri::webview::PageLoadEvent::Finished => {
                            native_refresh_observation::Phase::Finished
                        }
                    };
                    observer.observe(webview.label(), payload.url().as_str(), phase);
                }
            })
            .plugin(tauri_plugin_wdio_webdriver::init())
    };
    builder
        .run(tauri::generate_context!())
        .expect("native desktop exited with an error");
}
