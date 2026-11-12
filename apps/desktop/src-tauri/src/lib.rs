pub mod bridge;
pub mod ide;
mod updates;

#[cfg(feature = "native-webdriver")]
mod ide_probe;
#[cfg(feature = "native-webdriver")]
mod native_refresh_observation;

use bridge::intake::FolderSize;
use bridge::memory::{MemoryImport, MemoryList, MemoryNote};
use bridge::notify::Notices;
use bridge::picker::{FolderPurpose, Pick};
use bridge::project_setup::{IdeAskItem, MemoryItem, ResearchLog};
use bridge::questions::{QuestionCount, QuestionPage, QuestionsDismissed};
use bridge::terminal::Terminals;
use bridge::{
    AccountProfileView, ActionReceipt, BaselineDetail, BaselinePage, BridgeError, BridgeQueue,
    ConfiguredAccountPlan, ConnectionStatus, HistoryPage, IntentReceipt, ManagedAccountPlan,
    ModelAccessDetail, ModelAccessPage, NativeAuthView, NativeBridge, PendingIntent, ProjectDetail,
    ProjectList, ProjectPage,
};
use crazyagent::accounts::{AccountDraft, AccountView, ProviderEntry};
use crazyagent::agents::{AccountUse, AgentView};
use crazyagent::cleanup_settings::CleanupSettings;
use crazyagent::config::ModelAccessEdit;
use crazyagent::coord::settings::{CoordSettings, Delivery, HarnessKind, SubagentMode};
use crazyagent::coord::tool_switches::Switch;
use crazyagent::engine::owner::{
    AccountAuthCapabilitiesReply, AccountCheck, AccountPage, AccountSecret, ModelAccessCollection,
    NativeAuthMethod,
};
use crazyagent::github::{CloneReceipt, GitHubDeviceCode, GitHubRepoPage, GitHubStatus};
use crazyagent::harnesses::HarnessEntry;
use crazyagent::harnesses::update::HarnessVersions;
use crazyagent::ide::bridge::run_command::RunMode;
use crazyagent::ide::extensions::{Approved, AskView};
use crazyagent::landing::{AcceptOutcome, ChangeDiff, ChangeSetView, OwnerDirty, TargetBranches};
use crazyagent::master_frame_limit::MasterFrameSettings;
use crazyagent::master_step_limit::MasterStepSettings;
use crazyagent::master_time_limit::MasterTimeSettings;
use crazyagent::memory::ReviewDecision;
use crazyagent::memory::library::ApprovedMemories;
use crazyagent::onboarding::OnboardingView;
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::plugins::{
    EffectivePlugins, HookView, Scope, ServerSpec, ServerView, SkillView, ToolView,
};
use crazyagent::pools::PoolSummary;
use crazyagent::project_overview::{Apply, Overview, Tab};
use crazyagent::project_setup::{
    ContextChunk, KeptFile, ProjectCatalog, ProjectContext, ProjectSetup, ResearchKind, StoredSetup,
};
use crazyagent::projects_home::{AddMode, AddOverride, FoundProject, ProjectsHome};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::quit_settings::QuitSettings;
use crazyagent::roadmap_status::{Refresh, RoadmapStatus};
use crazyagent::sandbox_settings::SandboxSettings;
use crazyagent::scheduling::{PoolRanking, Schedule, ScheduleChange};
use crazyagent::setup::SetupStatus;
use crazyagent::team::cleanup::FinishedAgent;
use crazyagent::team::{TeamGraph, TeamStatus, TerminalsPerWindow};
use crazyagent::terminal::TerminalRequest;
use crazyagent::usage::metrics::Metrics;
use crazyagent::usage::money::MoneySettings;
use crazyagent::usage::report::UsageReport;
use std::sync::Arc;
use tauri::ipc::{Channel, Invoke, InvokeResponseBody};
use tauri::{AppHandle, Manager, Runtime, State, Webview, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};
use updates::{AvailableUpdate, InstallOutcome, UpdateProgress, UpdateSettings, UpdateState};

pub struct NativeState(BridgeQueue);

pub struct TerminalState(Arc<Terminals>);

fn main_window<R: Runtime>(webview: &Webview<R>) -> Result<(), BridgeError> {
    if webview.label() != "main" {
        return Err(BridgeError {
            code: "window_denied",
            cause: None,
            account: None,
        });
    }
    Ok(())
}

/// Admits the main window and the terminal wall windows to the terminal, graph, stop and
/// Terminals per window commands; any other window gets `window_denied`. Takes the calling
/// page. Returns unit or the refusal.
fn terminal_window(webview: &Webview) -> Result<(), BridgeError> {
    if !bridge::terminal::window_label_valid(webview.label()) {
        return Err(engine_failure("window_denied"));
    }
    Ok(())
}

/// Admits the main window to one project's terminal, graph, stop and setting commands, and a
/// terminal wall window only for the project its label was made for; any other window or
/// project gets `window_denied`. Takes the calling page and the project id. Returns unit or
/// the refusal.
fn terminal_window_for(webview: &Webview, id: &str) -> Result<(), BridgeError> {
    terminal_window(webview)?;
    let label = webview.label();
    let admitted = label == "main"
        || ProjectId::new(id).is_ok_and(|project| bridge::terminal::window_admits(label, &project));
    if !admitted {
        return Err(engine_failure("window_denied"));
    }
    Ok(())
}

async fn lock(state: &NativeState) -> Result<RwLockWriteGuard<'_, NativeBridge>, BridgeError> {
    state.0.enter().await
}

async fn read(state: &NativeState) -> Result<RwLockReadGuard<'_, NativeBridge>, BridgeError> {
    state.0.read().await
}

const SOCKET_OVERRIDE: &str = "CRAZYAGENT_DESKTOP_SOCKET";

fn engine_failure(code: &'static str) -> BridgeError {
    BridgeError {
        code,
        cause: None,
        account: None,
    }
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

/// Installs the service when it is missing, when `reinstall` asks or when it is
/// [`crazyagent::service::Service::stale`], else starts it as it is, then waits for the
/// engine with the shared bound. The service runs the sidecar, or its private copy when
/// the sidecar sits in the AppImage mount folder; a sidecar other users can change is
/// refused. Inputs: the layout, the service and whether to replace the definition with
/// this app's sidecar. Output: Ok once it answers.
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
    let appdir = std::env::var_os("APPDIR").map(std::path::PathBuf::from);
    let bundled = &service.executable;
    let service = crazyagent::service::Service {
        executable: crazyagent::service::persistable(bundled, layout, appdir.as_deref())
            .map_err(|_| engine_failure("engine_service_failed"))?,
        ..service.clone()
    };
    if reinstall || !installed || service.stale(bundled) {
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
        .map_err(|_| engine_failure(start_failure(layout)))
}

/// The code of an engine start that did not answer: `engine_backup_failed` when the engine's
/// record says its upgrade backup failed (F36), else `engine_start_failed`. Input: the
/// layout. Output: the code.
fn start_failure(layout: &crazyagent::paths::Layout) -> &'static str {
    if crazyagent::service::backup_failure(layout).is_some() {
        "engine_backup_failed"
    } else {
        "engine_start_failed"
    }
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
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&webview)?;
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
async fn desktop_engine_versions(webview: Webview) -> Result<EngineVersions, BridgeError> {
    main_window(&webview)?;
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
/// app's sidecar so the engine and the app share one version, then reconnects. Refuses
/// `busy` while an update check or install holds the update lock, and holds it itself.
#[tauri::command]
async fn desktop_restart_engine(
    webview: Webview,
    state: State<'_, NativeState>,
    update: State<'_, UpdateState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&webview)?;
    if std::env::var_os(SOCKET_OVERRIDE).is_some_and(|socket| !socket.is_empty()) {
        return Err(engine_failure("engine_restart_unavailable"));
    }
    let _update = updates::hold(&update)?;
    let mut bridge = lock(&state).await?;
    let (layout, service) = default_engine()?;
    start_engine(&layout, &service, true).await?;
    ensure_default_engine().await?;
    bridge.connect().await
}

#[tauri::command]
async fn desktop_status(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&webview)?;
    Ok(read(&state).await?.status())
}

#[tauri::command]
async fn desktop_harness_catalog(
    webview: Webview,
    state: State<'_, NativeState>,
    check: Option<bool>,
) -> Result<Vec<HarnessEntry>, BridgeError> {
    main_window(&webview)?;
    read(&state)
        .await?
        .harness_catalog(check.unwrap_or(false))
        .await
}

#[tauri::command]
async fn desktop_harness_versions(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    look: bool,
) -> Result<HarnessVersions, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.harness_versions(&id, look).await
}

#[tauri::command]
async fn desktop_harness_update(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    read(&state).await?.harness_update(&id).await
}

#[tauri::command]
async fn desktop_harness_install(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    read(&state).await?.harness_install(&id).await
}

#[tauri::command]
async fn desktop_harness_find(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<crazyagent::harnesses::finding::Found, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.harness_find(&id).await
}

#[tauri::command]
async fn desktop_harness_choose_path(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    path: Option<String>,
) -> Result<crazyagent::harnesses::finding::Found, BridgeError> {
    main_window(&webview)?;
    read(&state)
        .await?
        .harness_choose_path(&id, path.as_deref())
        .await
}

#[tauri::command]
async fn desktop_pick_harness_file(webview: Webview) -> Result<Option<String>, BridgeError> {
    main_window(&webview)?;
    Ok(
        match bridge::picker::choose(bridge::picker::HARNESS_FILE_SCRIPT).await? {
            Pick::Canceled => None,
            Pick::Paths(mut paths) => {
                assert!(!paths.is_empty());
                Some(paths.swap_remove(0))
            }
        },
    )
}

#[tauri::command]
async fn desktop_harness_update_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<crazyagent::harnesses::update::UpdateSettings, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.harness_update_settings().await
}

#[tauri::command]
async fn desktop_harness_set_update_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: crazyagent::harnesses::update::UpdateSettings,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    read(&state)
        .await?
        .harness_set_update_settings(settings)
        .await
}

#[tauri::command]
async fn desktop_baselines(
    webview: Webview,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<BaselinePage, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.baselines(next).await
}

#[tauri::command]
async fn desktop_inspect_baseline(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<BaselineDetail, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.inspect(&id).await
}

#[tauri::command]
async fn desktop_approve_baseline(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.approve(&ticket).await
}

#[tauri::command]
async fn desktop_revoke_baseline(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.revoke(&ticket).await
}

#[tauri::command]
async fn desktop_reconcile(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.reconcile().await
}

#[tauri::command]
async fn desktop_retry(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.retry().await
}

#[tauri::command]
async fn desktop_projects(
    webview: Webview,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<ProjectPage, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.projects(next).await
}

#[tauri::command]
async fn desktop_project_list(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<ProjectList, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.project_list().await
}

#[tauri::command]
async fn desktop_create_project(
    webview: Webview,
    state: State<'_, NativeState>,
    spec: ProjectSpec,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.create_project(spec, generation).await
}

#[tauri::command]
async fn desktop_project_catalog(
    webview: Webview,
    state: State<'_, NativeState>,
    pool: Option<String>,
    research: Vec<ResearchKind>,
) -> Result<ProjectCatalog, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.project_catalog(pool, research).await
}

#[tauri::command]
async fn desktop_setup_project(
    webview: Webview,
    state: State<'_, NativeState>,
    spec: ProjectSpec,
    setup: ProjectSetup,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .setup_project(spec, setup, generation)
        .await
}

#[tauri::command]
async fn desktop_project_setup(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<Option<StoredSetup>, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.project_setup(&id).await
}

#[tauri::command]
async fn desktop_research_jobs(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<bridge::project_setup::ResearchJobs, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.research_jobs(&id).await
}

#[tauri::command]
async fn desktop_project_activity(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    limit: usize,
) -> Result<bridge::project_setup::ProjectActivity, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.project_activity(&id, limit).await
}

#[tauri::command]
async fn desktop_project_context(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ProjectContext, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.project_context(&id).await
}

#[tauri::command]
async fn desktop_project_context_item(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    path: String,
    offset: u64,
) -> Result<ContextChunk, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .project_context_item(&id, path, offset)
        .await
}

#[tauri::command]
async fn desktop_save_research_to_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    research: ResearchKind,
    section: Option<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .save_research_to_memory(&id, research, section, generation)
        .await
}

#[tauri::command]
async fn desktop_add_project_documents(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    documents: Vec<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .add_project_documents(&id, documents, generation)
        .await
}

#[tauri::command]
async fn desktop_project_memories(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    after: Option<String>,
) -> Result<Vec<MemoryItem>, BridgeError> {
    main_window(&webview)?;
    read(&state)
        .await?
        .project_memories(&id, after.as_deref())
        .await
}

#[tauri::command]
async fn desktop_ide_asks(
    webview: Webview,
    state: State<'_, NativeState>,
    views: State<'_, ide::IdeViews>,
    id: String,
) -> Result<Vec<IdeAskItem>, BridgeError> {
    main_window(&webview)?;
    if !views.any_loaded() {
        return Ok(Vec::new());
    }
    read(&state).await?.ide_asks(&id).await
}

#[tauri::command]
async fn desktop_review_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    memory: String,
    version: u64,
    decision: ReviewDecision,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .review_memory(&id, &memory, version, decision, generation)
        .await
}

#[tauri::command]
async fn desktop_memories(
    webview: Webview,
    state: State<'_, NativeState>,
    project: Option<String>,
    search: Option<String>,
    before: Option<u64>,
) -> Result<MemoryList, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .memories(project.as_deref(), search.as_deref(), before)
        .await
}

#[tauri::command]
async fn desktop_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<Option<MemoryNote>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.memory(&id).await
}

#[tauri::command]
async fn desktop_approved_memories(
    webview: Webview,
    state: State<'_, NativeState>,
    project: String,
) -> Result<ApprovedMemories, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.approved_memories(&project).await
}

#[tauri::command]
async fn desktop_add_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    project: Option<String>,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .add_memory(project.as_deref(), text, generation)
        .await
}

#[tauri::command]
async fn desktop_import_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    project: String,
    folder: String,
    generation: u64,
) -> Result<MemoryImport, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .import_memory(&project, &folder, generation)
        .await
}

#[tauri::command]
async fn desktop_promote_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    project: String,
    memory: String,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .promote_memory(&project, &memory, version, generation)
        .await
}

#[tauri::command]
async fn desktop_delete_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.delete_memory(&id, generation).await
}

#[tauri::command]
async fn desktop_approve_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.approve_memory(&id, generation).await
}

#[tauri::command]
async fn desktop_set_agent_memory_writes(
    webview: Webview,
    state: State<'_, NativeState>,
    writes: crazyagent::memory::library::AgentWrites,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_agent_memory_writes(writes, generation)
        .await
}

#[tauri::command]
async fn desktop_move_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    project: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .move_memory(&id, &project, generation)
        .await
}

#[tauri::command]
async fn desktop_set_agent_memory_scope(
    webview: Webview,
    state: State<'_, NativeState>,
    scope: crazyagent::memory::library::AgentScope,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_agent_memory_scope(scope, generation)
        .await
}

#[tauri::command]
async fn desktop_forget_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    project: String,
    memory: String,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .forget_memory(&project, &memory, version, generation)
        .await
}

#[tauri::command]
async fn desktop_save_selection_to_memory(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    path: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .save_selection_to_memory(&id, path, text, generation)
        .await
}

#[tauri::command]
async fn desktop_remember_research(
    webview: Webview,
    state: State<'_, NativeState>,
    project: String,
    research: ResearchKind,
    section: Option<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .remember_research(&project, research, section, generation)
        .await
}

#[tauri::command]
async fn desktop_project_read_paths(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<crazyagent::project_setup::ReadPaths, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.project_read_paths(&id).await
}

#[tauri::command]
async fn desktop_project_hidden_paths(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.project_hidden_paths(&id).await
}

#[tauri::command]
async fn desktop_project_agent_policy(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<crazyagent::sandbox::policy::PolicyView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.project_agent_policy(&id).await
}

#[tauri::command]
async fn desktop_set_project_agent_policy(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    version: u64,
    policy: crazyagent::sandbox::policy::AgentPolicy,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_project_agent_policy(&id, version, policy, generation)
        .await
}

#[tauri::command]
async fn desktop_project_files(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    query: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&webview)?;
    state.0.project_files(&id, &query).await
}

#[tauri::command]
async fn desktop_set_project_read_paths(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    version: u64,
    paths: Vec<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_project_read_paths(&id, version, paths, generation)
        .await
}

#[tauri::command]
async fn desktop_set_project_hidden_paths(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    paths: Vec<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_project_hidden_paths(&id, paths, generation)
        .await
}

#[tauri::command]
async fn desktop_delete_project_context(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    path: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .delete_project_context(&id, path, generation)
        .await
}

#[tauri::command]
async fn desktop_team_status(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<TeamStatus, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.team_status(&id).await
}

#[tauri::command]
async fn desktop_team_agent_messages(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    agent: i64,
    before: Option<crazyagent::team::agent_messages::Cursor>,
    limit: Option<u16>,
) -> Result<crazyagent::team::agent_messages::AgentMessages, BridgeError> {
    main_window(&webview)?;
    read(&state)
        .await?
        .team_agent_messages(&id, agent, before, limit)
        .await
}

#[tauri::command]
async fn desktop_team_message_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    settings: crazyagent::team::agent_messages::MessageSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_message_settings(&id, settings, generation)
        .await
}

#[tauri::command]
async fn desktop_team_message(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_message(&id, text, generation)
        .await
}

#[tauri::command]
async fn desktop_team_delegate(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    harness: Option<String>,
    prompt: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_delegate(&id, harness, prompt, generation)
        .await
}

#[tauri::command]
async fn desktop_team_cancel(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    task: Option<i64>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.team_cancel(&id, task, generation).await
}

#[tauri::command]
async fn desktop_team_keep_warm(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_keep_warm(&id, on, version, generation)
        .await
}

/// The three Team settings time limits one change sets: seconds for a task turn, an agent
/// session and a sub-agent's idle stop, each from the listed choices or none for no limit.
/// The window sends them as the separate `turnSecs`, `sessionSecs` and `idleSecs` keys.
struct TeamTimeLimits {
    turn_secs: Option<u32>,
    session_secs: Option<u32>,
    idle_secs: Option<u32>,
}

impl<'de, R: tauri::Runtime> tauri::ipc::CommandArg<'de, R> for TeamTimeLimits {
    /// Reads each limit from its own key of the command call, the way Tauri reads an optional
    /// argument: a missing key or null is no limit, any other non-number is refused. Input:
    /// the command item. Output: the limits, or Tauri's invalid-argument error for that key.
    fn from_command(
        command: tauri::ipc::CommandItem<'de, R>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        let tauri::ipc::CommandItem {
            plugin,
            name,
            message,
            acl,
            ..
        } = command;
        let read = |key| {
            <Option<u32> as tauri::ipc::CommandArg<'de, R>>::from_command(tauri::ipc::CommandItem {
                plugin,
                name,
                key,
                message,
                acl,
            })
        };
        Ok(Self {
            turn_secs: read("turnSecs")?,
            session_secs: read("sessionSecs")?,
            idle_secs: read("idleSecs")?,
        })
    }
}

/// Sets a project team's task and session time limits and sub-agent idle stop from Team
/// settings: seconds from the listed choices, or none for no limit. Output: the receipt.
#[tauri::command]
async fn desktop_team_time_limits(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    limits: TeamTimeLimits,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    let TeamTimeLimits {
        turn_secs,
        session_secs,
        idle_secs,
    } = limits;
    lock(&state)
        .await?
        .team_time_limits(&id, turn_secs, session_secs, idle_secs, version, generation)
        .await
}

#[tauri::command]
async fn desktop_team_on_harness_switch(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: String,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_on_harness_switch(&id, &mode, version, generation)
        .await
}

#[tauri::command]
async fn desktop_team_apply_harness_switch(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: String,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_apply_harness_switch(&id, &mode, version, generation)
        .await
}

#[tauri::command]
async fn desktop_team_sub_agent_depth(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    depth: u8,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_sub_agent_depth(&id, depth, generation)
        .await
}

#[tauri::command]
async fn desktop_team_child_landing(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_child_landing(&id, &mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_master_branch(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    branch: Option<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_master_branch(&id, branch, generation)
        .await
}

#[tauri::command]
async fn desktop_team_land_on(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    branch: Option<String>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_land_on(&id, branch, generation)
        .await
}

#[tauri::command]
async fn desktop_team_background(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    choice: crazyagent::team::BackgroundAtEnd,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_background(&id, choice, generation)
        .await
}

#[tauri::command]
async fn desktop_team_leftovers(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: crazyagent::team::LeftoverMode,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_leftovers(&id, mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_leftover_hold(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    key: u64,
    end: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_leftover_hold(&id, key, end, generation)
        .await
}

#[tauri::command]
async fn desktop_team_ask_before_merge(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_ask_before_merge(&id, on, generation)
        .await
}

#[tauri::command]
async fn desktop_team_master_lands(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: crazyagent::team::MasterLands,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_master_lands(&id, mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_reask_empty_reply(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_reask_empty_reply(&id, on, generation)
        .await
}

#[tauri::command]
async fn desktop_team_done_quiet(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    secs: Option<u32>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_done_quiet(&id, secs, generation)
        .await
}

#[tauri::command]
async fn desktop_team_catch_up_main(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_catch_up_main(&id, &mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_master_idle_stop(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    choice: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_master_idle_stop(&id, &choice, generation)
        .await
}

#[tauri::command]
async fn desktop_team_master_live_terminal(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_master_live_terminal(&id, on, generation)
        .await
}

#[tauri::command]
async fn desktop_team_reuse_idle(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_reuse_idle(&id, &mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_landing_mode(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_landing_mode(&id, &mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_new_folder_limit(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    files: u32,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_new_folder_limit(&id, files, generation)
        .await
}

#[tauri::command]
async fn desktop_team_coordination(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    settings: CoordSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_coordination(&id, settings, generation)
        .await
}

#[tauri::command]
async fn desktop_team_harness_mode(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    harness: HarnessKind,
    mode: SubagentMode,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_harness_mode(&id, harness, mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_harness_delivery(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    harness: HarnessKind,
    delivery: Delivery,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_harness_delivery(&id, harness, delivery, generation)
        .await
}

#[tauri::command]
async fn desktop_team_agent_branches(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_agent_branches(&id, &mode, generation)
        .await
}

#[tauri::command]
async fn desktop_team_finished_agents(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    show: bool,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_finished_agents(&id, show, version, generation)
        .await
}

#[tauri::command]
async fn desktop_team_live_master(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_live_master(&id, on, version, generation)
        .await
}

#[tauri::command]
async fn desktop_team_pause(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    paused: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_pause(&id, paused, generation)
        .await
}

#[tauri::command]
async fn desktop_team_hand_off(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    reply: i64,
    step: u8,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_hand_off(&id, reply, step, generation)
        .await
}

#[tauri::command]
async fn desktop_team_events(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<crazyagent::team::events::AppEvents, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.team_events().await
}

#[tauri::command]
async fn desktop_team_wake_rule(
    webview: Webview,
    state: State<'_, NativeState>,
    target: crazyagent::team::events::RuleTarget,
    rule: Option<crazyagent::team::events::WakeRule>,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_wake_rule(target, rule, version, generation)
        .await
}

#[tauri::command]
async fn desktop_team_rename(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    agent: i64,
    handle: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_rename(&id, agent, handle, generation)
        .await
}

#[tauri::command]
async fn desktop_team_stop_agent(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    agent: i64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    terminal_window_for(&webview, &id)?;
    lock(&state)
        .await?
        .team_stop_agent(&id, agent, generation)
        .await
}

#[tauri::command]
async fn desktop_coord_send(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    agent: i64,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .coord_send(&id, agent, text, generation)
        .await
}

#[tauri::command]
async fn desktop_team_stop_teammate(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    agent: i64,
    pane: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    terminal_window_for(&webview, &id)?;
    lock(&state)
        .await?
        .team_stop_teammate(&id, agent, pane, generation)
        .await
}

#[tauri::command]
async fn desktop_team_max_harnesses(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    max: Option<u8>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_max_harnesses(&id, max, generation)
        .await
}

#[tauri::command]
async fn desktop_team_max_teammates(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    max: Option<u8>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_max_teammates(&id, max, generation)
        .await
}

#[tauri::command]
async fn desktop_team_master_rounds(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    max: Option<u8>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_master_rounds(&id, max, generation)
        .await
}

#[tauri::command]
async fn desktop_team_master_run_time(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    secs: Option<u32>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_master_run_time(&id, secs, generation)
        .await
}

#[tauri::command]
async fn desktop_team_token_budget(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    tokens: Option<u64>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_token_budget(&id, tokens, generation)
        .await
}

#[tauri::command]
async fn desktop_team_spread_accounts(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_spread_accounts(&id, on, generation)
        .await
}

#[tauri::command]
async fn desktop_team_wake_agent(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    agent: i64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_wake_agent(&id, agent, generation)
        .await
}

#[tauri::command]
async fn desktop_team_seen(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    upto: i64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.team_seen(&id, upto, generation).await
}

#[tauri::command]
async fn desktop_coord_questions(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<crazyagent::team::questions::Inbox, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.coord_questions(&id).await
}

#[tauri::command]
async fn desktop_coord_answer(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    message: i64,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .coord_answer(&id, message, text, generation)
        .await
}

#[tauri::command]
async fn desktop_team_attention(
    webview: Webview,
    unseen: u32,
    alert: bool,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    bridge::attention(&webview.window(), unseen, alert)
}

#[tauri::command]
async fn desktop_team_graph(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<TeamGraph, BridgeError> {
    terminal_window_for(&webview, &id)?;
    read(&state).await?.team_graph(&id).await
}

#[tauri::command]
async fn desktop_terminal_attach(
    webview: Webview,
    state: State<'_, NativeState>,
    terminals: State<'_, TerminalState>,
    agent: String,
    pane: Option<String>,
    id: Option<String>,
    output: Channel<InvokeResponseBody>,
) -> Result<u64, BridgeError> {
    terminal_window(&webview)?;
    let agent = bridge::terminal::agent(&agent)?;
    let pane = pane.as_deref().map(bridge::terminal::pane).transpose()?;
    if webview.label() != "main" {
        let id = id.ok_or_else(|| engine_failure("window_denied"))?;
        terminal_window_for(&webview, &id)?;
        let graph = read(&state).await?.team_graph(&id).await?;
        if !bridge::terminal::graph_shows(&graph, &agent) {
            return Err(engine_failure("window_denied"));
        }
    }
    let room = terminals.0.room(webview.label(), &agent)?;
    let session = lock(&state).await?.open_terminal(agent, pane).await?;
    room.attach(session, output)
}

/// Types a view's keys into an attached agent terminal. Inputs: the window, the terminals,
/// the terminal name, the bytes and whether the view made them from the owner's mouse
/// wheel, which never counts as the owner typing. Output: ok once queued, or a bridge error.
#[tauri::command]
async fn desktop_terminal_input(
    webview: Webview,
    terminals: State<'_, TerminalState>,
    agent: String,
    bytes: Vec<u8>,
    wheel: Option<bool>,
) -> Result<(), BridgeError> {
    terminal_window(&webview)?;
    let agent = bridge::terminal::agent(&agent)?;
    let request = bridge::terminal::input_request(bytes, wheel.unwrap_or(false));
    terminals.0.send(webview.label(), &agent, request).await
}

#[tauri::command]
async fn desktop_terminal_prompt(
    webview: Webview,
    terminals: State<'_, TerminalState>,
    agent: String,
    text: String,
) -> Result<(), BridgeError> {
    terminal_window(&webview)?;
    let agent = bridge::terminal::agent(&agent)?;
    terminals
        .0
        .send(webview.label(), &agent, TerminalRequest::Prompt { text })
        .await
}

#[tauri::command]
async fn desktop_terminal_resize(
    webview: Webview,
    terminals: State<'_, TerminalState>,
    agent: String,
    cols: u16,
    rows: u16,
) -> Result<(), BridgeError> {
    terminal_window(&webview)?;
    let agent = bridge::terminal::agent(&agent)?;
    terminals
        .0
        .send(
            webview.label(),
            &agent,
            TerminalRequest::Resize { cols, rows },
        )
        .await
}

#[tauri::command]
fn desktop_terminal_detach(
    webview: Webview,
    terminals: State<'_, TerminalState>,
    agent: String,
    serial: u64,
) -> Result<(), BridgeError> {
    terminal_window(&webview)?;
    let agent = bridge::terminal::agent(&agent)?;
    terminals.0.detach(webview.label(), &agent, serial)
}

#[tauri::command]
async fn desktop_team_terminals_per_window(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    per_window: TerminalsPerWindow,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .team_terminals_per_window(&id, per_window, generation)
        .await
}

#[tauri::command]
async fn desktop_terminals_per_window(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<TerminalsPerWindow, BridgeError> {
    terminal_window_for(&webview, &id)?;
    read(&state).await?.terminals_per_window(&id).await
}

#[tauri::command]
async fn desktop_open_terminal_windows(
    webview: Webview,
    app: AppHandle,
    id: String,
    name: String,
    count: usize,
    focus: bool,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let project = ProjectId::new(id).map_err(|_| engine_failure("invalid_project"))?;
    open_walls(&app, &project, &wall_name(&name), count, focus)
}

/// Lets a terminal wall window open its project's next windows as the team grows, titled
/// with the project name its own title shows; it never closes one. Main and other projects'
/// windows get `window_denied`. Inputs: the calling wall window, the app, the project id and
/// the windows its team needs. Output: unit or the `open_walls` failure.
#[tauri::command]
async fn desktop_extend_terminal_windows(
    webview: Webview,
    app: AppHandle,
    id: String,
    count: usize,
) -> Result<(), BridgeError> {
    terminal_window_for(&webview, &id)?;
    if webview.label() == "main" {
        return Err(engine_failure("window_denied"));
    }
    let project = ProjectId::new(id).map_err(|_| engine_failure("invalid_project"))?;
    let title = webview
        .window()
        .title()
        .map_err(|_| engine_failure("terminal_window_failed"))?;
    let name = wall_name(&bridge::terminal::wall_title_name(&title));
    open_walls(&app, &project, &name, count.max(1), false)
}

/// Brings forward, for a terminal wall window, the main window on its project's Agents tab or
/// another wall window of its own project, as the wall's Back to Agents and Window switch do.
/// Main and other projects' windows get `window_denied`. Inputs: the calling wall window, the
/// app, the project id and the target, "main" or a page. Output: unit, `window_denied`, or
/// `terminal_window_failed` when that window is not open or does not come forward.
#[tauri::command]
fn desktop_focus_terminal_window(
    webview: Webview,
    app: AppHandle,
    id: String,
    target: String,
) -> Result<(), BridgeError> {
    terminal_window_for(&webview, &id)?;
    let project = ProjectId::new(id).map_err(|_| engine_failure("invalid_project"))?;
    let failed = || engine_failure("terminal_window_failed");
    match bridge::terminal::wall_focus(webview.label(), &project, &target)? {
        bridge::terminal::WallFocus::Main { hash } => {
            assert!(
                hash.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:/#".contains(&b))
            );
            let main = app.get_webview("main").ok_or_else(failed)?;
            main.eval(format!("window.location.hash = \"{hash}\";"))
                .map_err(|_| failed())?;
            main.window().set_focus().map_err(|_| failed())
        }
        bridge::terminal::WallFocus::Wall { label } => app
            .get_webview_window(&label)
            .ok_or_else(failed)?
            .set_focus()
            .map_err(|_| failed()),
    }
}

/// Closes one project's terminal wall windows: for the main window once it removed the
/// project, and for a wall window of that project once its read finds the project gone.
/// Inputs: the calling window, the app and the project id. Output: unit, `window_denied`
/// from any other window or a wall window of another project, or `invalid_project`.
#[tauri::command]
fn desktop_close_project_walls(
    webview: Webview,
    app: AppHandle,
    id: String,
) -> Result<(), BridgeError> {
    terminal_window_for(&webview, &id)?;
    let project = ProjectId::new(id).map_err(|_| engine_failure("invalid_project"))?;
    close_walls(&app, Some(&project));
    ide::shut_own_windows(app, Some(project));
    Ok(())
}

/// Closes the terminal wall windows: every one, or only one project's. Inputs: the app and
/// the project, if any. Output: none; a window that refuses to close stays open.
fn close_walls(app: &AppHandle, project: Option<&ProjectId>) {
    let open = app.webview_windows();
    for label in bridge::terminal::walls_to_close(open.keys().map(String::as_str), project) {
        if let Some(wall) = open.get(label) {
            let _ = wall.close();
        }
    }
}

/// Whether a webview may load a URL: the app's own pages only, and the dev server in a
/// debug build. Takes the URL. Returns the answer.
fn app_page(url: &tauri::Url) -> bool {
    (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port() == Some(1420))
}

/// The project name a terminal wall window shows in its title: the shown name without
/// control characters and with every other hidden character as its visible escape, at most
/// WALL_NAME_CHARS_MAX characters, or "Project" when blank. Takes the name the main window
/// sent. Returns the name to show.
fn wall_name(name: &str) -> String {
    let plain: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(WALL_NAME_CHARS_MAX)
        .collect();
    let shown: String = crazyagent::memory::visible_text(&plain)
        .chars()
        .take(WALL_NAME_CHARS_MAX)
        .collect();
    let shown = shown.trim();
    if shown.is_empty() {
        "Project".to_owned()
    } else {
        shown.to_owned()
    }
}

const WALL_NAME_CHARS_MAX: usize = 80;

/// Opens one project's terminal wall windows, one call at a time: builds each of pages 1 to
/// `count` not yet open, retitles the open ones and brings them forward when asked, and
/// closes the project's windows past `count`. Without `focus` a wall is growing: it changes
/// nothing while none of the project's wall windows is open and never closes one, so a
/// growing team only extends a wall the owner opened. Inputs: the app, the project, its
/// shown name, the page count and whether to focus. Output: unit, `terminal_windows_full`
/// past WINDOWS_MAX windows in all, or `terminal_window_failed`.
fn open_walls(
    app: &AppHandle,
    project: &ProjectId,
    name: &str,
    count: usize,
    focus: bool,
) -> Result<(), BridgeError> {
    static WALLS_OPENING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _opening = WALLS_OPENING
        .lock()
        .map_err(|_| engine_failure("terminal_window_failed"))?;
    let prefix = bridge::terminal::wall_prefix(project);
    let open = app.webview_windows();
    let pages: Vec<usize> = open
        .keys()
        .filter_map(|label| label.strip_prefix(&prefix)?.parse().ok())
        .collect();
    let mine = pages.len();
    let count = if focus {
        count
    } else {
        pages.iter().copied().fold(count, usize::max)
    };
    let others = open
        .keys()
        .filter(|label| {
            label.as_str() != "main"
                && !label.starts_with(&prefix)
                && bridge::terminal::window_label_valid(label)
        })
        .count();
    if count > bridge::terminal::WINDOWS_MAX || others + count > bridge::terminal::WINDOWS_MAX {
        return Err(engine_failure("terminal_windows_full"));
    }
    if !focus && mine == 0 {
        return Ok(());
    }
    for (label, wall) in &open {
        let past = label
            .strip_prefix(&prefix)
            .is_some_and(|page| !page.parse::<usize>().is_ok_and(|page| page <= count));
        if past {
            let _ = wall.close();
        }
    }
    for page in 1..=count {
        let label = bridge::terminal::wall_label(project, page);
        let title = format!("{name} \u{b7} Terminals {page} of {count}");
        match open.get(&label) {
            Some(wall) => {
                let _ = wall.set_title(&title);
                if focus {
                    let _ = wall.set_focus();
                }
            }
            None => build_wall(app, &label, project, page, name, &title)?,
        }
    }
    assert!(count <= bridge::terminal::WINDOWS_MAX);
    Ok(())
}

/// Builds one terminal wall window on the wall page of its project, at the address naming
/// its project name, with the main window's navigation rule, a page load that clears only its
/// own terminal views, and a close that forgets them. Inputs: the app, the window label, the
/// project, the page, the shown name and the title. Output: unit or `terminal_window_failed`.
fn build_wall(
    app: &AppHandle,
    label: &str,
    project: &ProjectId,
    page: usize,
    name: &str,
    title: &str,
) -> Result<(), BridgeError> {
    assert!(bridge::terminal::window_label_valid(label));
    let url = bridge::terminal::wall_url(project, page, name);
    let wall = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(1280.0, 820.0)
        .min_inner_size(640.0, 420.0)
        .on_navigation(app_page)
        .on_page_load(|window, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
                window
                    .state::<TerminalState>()
                    .0
                    .page_started(window.label());
            }
        })
        .build()
        .map_err(|_| engine_failure("terminal_window_failed"))?;
    let terminals = Arc::clone(&app.state::<TerminalState>().0);
    let closed = label.to_owned();
    wall.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            terminals.window_closed(&closed);
        }
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn desktop_update_project_setup(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    setup: ProjectSetup,
    name: Option<String>,
    revision: Option<u64>,
    generation: u64,
    kept: Option<Vec<KeptFile>>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .update_project_setup(&id, setup, name, revision, kept, generation)
        .await
}

#[tauri::command]
async fn desktop_move_project_home(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    mode: AddMode,
    overrides: Vec<AddOverride>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .move_project_home(&id, mode, overrides, generation)
        .await
}

#[tauri::command]
async fn desktop_remove_project(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    generation: u64,
    delete_name: Option<String>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .remove_project(&id, generation, delete_name)
        .await
}

#[tauri::command]
async fn desktop_project_removal(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<crazyagent::team::cleanup::RemovalPreview, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.removal_preview(id)?;
    call.await
}

#[tauri::command]
async fn desktop_cancel_research(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    job: i64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .cancel_research(&id, job, generation)
        .await
}

#[tauri::command]
async fn desktop_retry_research(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    job: i64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .retry_research(&id, job, generation)
        .await
}

#[tauri::command]
async fn desktop_plan_roadmap(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.plan_roadmap(&id, generation).await
}

#[tauri::command]
async fn desktop_roadmap_status(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<RoadmapStatus, BridgeError> {
    main_window(&webview)?;
    let call = read(&state).await?.roadmap_status(id)?;
    call.await
}

#[tauri::command]
async fn desktop_refresh_roadmap_status(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.refresh_roadmap_status(id)?;
    call.await
}

#[tauri::command]
async fn desktop_set_roadmap_refresh(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    refresh: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.set_roadmap_refresh(id, refresh)?;
    call.await
}

#[tauri::command]
async fn desktop_research_log(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    job: i64,
) -> Result<ResearchLog, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.research_log(&id, job).await
}

#[tauri::command]
async fn desktop_projects_home(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<ProjectsHome, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.projects_home().await
}

#[tauri::command]
async fn desktop_set_projects_home(
    webview: Webview,
    state: State<'_, NativeState>,
    path: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_projects_home(path, generation)
        .await
}

#[tauri::command]
async fn desktop_move_projects_home(
    webview: Webview,
    state: State<'_, NativeState>,
    path: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .move_projects_home(path, generation)
        .await
}

#[tauri::command]
async fn desktop_found_projects(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Vec<FoundProject>, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.found_projects().await
}

#[tauri::command]
async fn desktop_folder_size(
    webview: Webview,
    state: State<'_, NativeState>,
    path: String,
) -> Result<FolderSize, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.folder_size(path).await
}

#[tauri::command]
async fn desktop_add_existing_folder(
    webview: Webview,
    state: State<'_, NativeState>,
    source: String,
    mode: AddMode,
    overrides: Vec<AddOverride>,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .add_existing_folder(source, mode, overrides, generation)
        .await
}

#[tauri::command]
async fn desktop_start_onboarding(
    webview: Webview,
    state: State<'_, NativeState>,
    path: String,
    pool: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .start_onboarding(path, pool, generation)
        .await
}

#[tauri::command]
async fn desktop_onboarding(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<OnboardingView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.onboarding(&id).await
}

#[tauri::command]
async fn desktop_github_status(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.github_status()?;
    call.await
}

#[tauri::command]
async fn desktop_github_sign_in(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<GitHubDeviceCode, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.github_sign_in()?;
    call.await
}

#[tauri::command]
async fn desktop_github_token(
    webview: Webview,
    state: State<'_, NativeState>,
    token: String,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.github_token(token)?;
    call.await
}

#[tauri::command]
async fn desktop_github_use_cli(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.github_use_cli()?;
    call.await
}

#[tauri::command]
async fn desktop_github_sign_out(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<GitHubStatus, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.github_sign_out()?;
    call.await
}

#[tauri::command]
async fn desktop_github_repos(
    webview: Webview,
    state: State<'_, NativeState>,
    page: u32,
) -> Result<GitHubRepoPage, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.github_repos(page)?;
    call.await
}

#[tauri::command]
async fn desktop_clone_repo(
    webview: Webview,
    state: State<'_, NativeState>,
    full_name: String,
) -> Result<CloneReceipt, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.clone_repo(full_name)?;
    call.await
}

#[tauri::command]
async fn desktop_icloud_download(
    webview: Webview,
    state: State<'_, NativeState>,
    path: String,
) -> Result<u64, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.icloud_download(path)?;
    call.await
}

#[tauri::command]
async fn desktop_icloud_progress(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Option<crazyagent::projects_home::icloud::Progress>, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.icloud_progress()?;
    call.await
}

#[tauri::command]
async fn desktop_icloud_cancel(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<bool, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.icloud_cancel()?;
    call.await
}

#[tauri::command]
async fn desktop_setup_status(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<SetupStatus, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.setup_status().await
}

#[tauri::command]
async fn desktop_run_setup(
    webview: Webview,
    state: State<'_, NativeState>,
    pack: Vec<String>,
    generation: u64,
) -> Result<SetupStatus, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.run_setup(pack, generation).await
}

#[tauri::command]
async fn desktop_set_pack_entry(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    on: bool,
    version: u64,
    generation: u64,
) -> Result<SetupStatus, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_pack_entry(id, on, version, generation)
        .await
}

#[tauri::command]
async fn desktop_remove_pack_entry(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    version: u64,
    generation: u64,
) -> Result<SetupStatus, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .remove_pack_entry(id, version, generation)
        .await
}

#[tauri::command]
async fn desktop_open_project(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ProjectDetail, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.open_project(&id).await
}

#[tauri::command]
async fn desktop_history(
    webview: Webview,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<HistoryPage, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.history(next).await
}

#[tauri::command]
async fn desktop_send_message(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .send_message(&ticket, &text, generation)
        .await
}

#[tauri::command]
async fn desktop_master_state(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<bridge::master::MasterState, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.master_state(&ticket).await
}

#[tauri::command]
async fn desktop_master_turns(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    cursor: Option<crazyagent::master_session::MasterCursor>,
) -> Result<bridge::master::MasterTurns, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .master_turns(&ticket, &session_id, cursor)
        .await
}

#[tauri::command]
async fn desktop_open_master(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .open_default_master(&ticket, generation)
        .await
}

#[tauri::command]
async fn desktop_close_master(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .close_master(&ticket, &session_id, generation)
        .await
}

#[tauri::command]
async fn desktop_master_sessions(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<bridge::master::MasterSessions, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.master_sessions(&ticket).await
}

#[tauri::command]
async fn desktop_master_inputs(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    cursors: Vec<String>,
) -> Result<bridge::master::MasterInputs, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .master_inputs(&ticket, &session_id, &cursors)
        .await
}

#[tauri::command]
async fn desktop_master_messages(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    before: Option<u64>,
    limit: u8,
) -> Result<bridge::master::MasterMessages, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .master_messages(&ticket, &session_id, before, limit)
        .await
}

#[tauri::command]
async fn desktop_master_session_turns(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    cursor: Option<crazyagent::master_session::MasterCursor>,
) -> Result<bridge::master::MasterTurns, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .session_turns(&ticket, &session_id, cursor)
        .await
}

#[tauri::command]
async fn desktop_new_master_session(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    cancel_running: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .new_master_session(&ticket, cancel_running, generation)
        .await
}

#[tauri::command]
async fn desktop_resume_master_session(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    cancel_running: bool,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .resume_master_session(&ticket, &session_id, cancel_running, generation)
        .await
}

#[tauri::command]
async fn desktop_delete_master_session(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .delete_master_session(&ticket, &session_id, generation)
        .await
}

#[tauri::command]
async fn desktop_cancel_master_turn(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session: String,
    turn: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .cancel_master_turn(&ticket, &session, &turn, generation)
        .await
}

#[tauri::command]
async fn desktop_submit_master(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .submit_master(&ticket, &session_id, &text, generation)
        .await
}

#[tauri::command]
async fn desktop_replace_brief(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    text: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .replace_brief(&ticket, &text, generation)
        .await
}

#[tauri::command]
async fn desktop_pending_intent(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Option<PendingIntent>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.pending_intent()
}

#[tauri::command]
async fn desktop_reconcile_intent(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.reconcile_intent().await
}

#[tauri::command]
async fn desktop_retry_intent(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.retry_intent().await
}

#[tauri::command]
async fn desktop_drop_intent(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.drop_intent()
}

#[tauri::command]
async fn desktop_prepare_managed_account(
    webview: Webview,
    state: State<'_, NativeState>,
    provider: String,
    label: String,
) -> Result<ManagedAccountPlan, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .prepare_managed_account(&provider, &label)
        .await
}

#[tauri::command]
async fn desktop_prepare_configured_account(
    webview: Webview,
    state: State<'_, NativeState>,
    provider: String,
    label: String,
    endpoint: String,
    model: String,
) -> Result<ConfiguredAccountPlan, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .prepare_configured_account(&provider, &label, &endpoint, &model)
        .await
}

#[tauri::command]
async fn desktop_account_auth_capabilities(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<AccountAuthCapabilitiesReply, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.account_auth_capabilities(&ticket).await
}

#[tauri::command]
async fn desktop_configure_account_profile(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    secret: Option<String>,
    generation: u64,
) -> Result<AccountProfileView, BridgeError> {
    main_window(&webview)?;
    let secret = secret
        .map(|value| AccountSecret::new(value.into_bytes()))
        .transpose()
        .map_err(|_| BridgeError {
            code: "account_secret_invalid",
            cause: None,
            account: None,
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
        account: None,
    })?
}

#[tauri::command]
async fn desktop_read_account_profile(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<AccountProfileView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.read_account_profile().await
}

#[tauri::command]
async fn desktop_owner_profile(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Option<OwnerProfile>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.owner_profile().await
}

#[tauri::command]
async fn desktop_save_owner_profile(
    webview: Webview,
    state: State<'_, NativeState>,
    profile: OwnerProfile,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .save_owner_profile(profile, generation)
        .await
}

#[tauri::command]
async fn desktop_sandbox_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<SandboxSettings, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.sandbox_settings().await
}

#[tauri::command]
async fn desktop_set_sandbox_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: SandboxSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_sandbox_settings(settings, generation)
        .await
}

#[tauri::command]
async fn desktop_cleanup_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<CleanupSettings, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.cleanup_settings().await
}

#[tauri::command]
async fn desktop_set_cleanup_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: CleanupSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_cleanup_settings(settings, generation)
        .await
}

#[tauri::command]
async fn desktop_quit_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<QuitSettings, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.quit_settings().await
}

#[tauri::command]
async fn desktop_set_quit_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: QuitSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_quit_settings(settings, generation)
        .await
}

#[tauri::command]
async fn desktop_master_step_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<MasterStepSettings, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.master_step_settings().await
}

#[tauri::command]
async fn desktop_set_master_step_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: MasterStepSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_master_step_settings(settings, generation)
        .await
}

#[tauri::command]
async fn desktop_master_time_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<MasterTimeSettings, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.master_time_settings().await
}

#[tauri::command]
async fn desktop_set_master_time_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: MasterTimeSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_master_time_settings(settings, generation)
        .await
}

#[tauri::command]
async fn desktop_master_frame_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<MasterFrameSettings, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.master_frame_settings().await
}

#[tauri::command]
async fn desktop_set_master_frame_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: MasterFrameSettings,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_master_frame_settings(settings, generation)
        .await
}

#[tauri::command]
async fn desktop_pool_summaries(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Vec<PoolSummary>, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.pool_summaries().await
}

#[tauri::command]
async fn desktop_account_schedule(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Schedule, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.account_schedule().await
}

#[tauri::command]
async fn desktop_account_ranking(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Vec<PoolRanking>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.account_ranking().await
}

#[tauri::command]
async fn desktop_set_account_schedule(
    webview: Webview,
    state: State<'_, NativeState>,
    change: ScheduleChange,
    version: u64,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_account_schedule(change, version, generation)
        .await
}

#[tauri::command]
async fn desktop_account_catalog(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Vec<ProviderEntry>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.account_catalog().await
}

#[tauri::command]
async fn desktop_list_accounts(
    webview: Webview,
    state: State<'_, NativeState>,
    after: Option<String>,
    limit: u16,
) -> Result<AccountPage, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.list_accounts(after, limit).await
}

#[tauri::command]
async fn desktop_add_account(
    webview: Webview,
    state: State<'_, NativeState>,
    draft: AccountDraft,
    key: Option<String>,
) -> Result<AccountView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.add_account(draft, key).await
}

#[tauri::command]
async fn desktop_rename_account(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    name: String,
) -> Result<AccountView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.rename_account(&id, name).await
}

#[tauri::command]
async fn desktop_remove_account(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.remove_account(&id).await
}

#[tauri::command]
async fn desktop_check_account(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<AccountCheck, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.check_account(&id).await
}

#[tauri::command]
async fn desktop_local_models(
    webview: Webview,
    state: State<'_, NativeState>,
    url: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.local_models(url).await
}

#[tauri::command]
async fn desktop_plugin_skills(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<SkillView>, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.plugin_skills(project_id).await
}

#[tauri::command]
async fn desktop_plugin_servers(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<ServerView>, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.plugin_servers(project_id).await
}

#[tauri::command]
async fn desktop_plugin_hooks(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<HookView>, BridgeError> {
    main_window(&webview)?;
    read(&state).await?.plugin_hooks(project_id).await
}

#[tauri::command]
async fn desktop_add_hooks(
    webview: Webview,
    state: State<'_, NativeState>,
    scope: Scope,
    folder: String,
) -> Result<Vec<HookView>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.add_hooks(scope, folder).await
}

#[tauri::command]
async fn desktop_set_hook_enabled(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    enabled: bool,
) -> Result<HookView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.set_hook_enabled(id, enabled).await
}

#[tauri::command]
async fn desktop_add_skill(
    webview: Webview,
    state: State<'_, NativeState>,
    scope: Scope,
    folder: String,
) -> Result<SkillView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.add_skill(scope, folder).await
}

#[tauri::command]
async fn desktop_set_skill_enabled(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    enabled: bool,
    version: u64,
) -> Result<SkillView, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_skill_enabled(id, enabled, version)
        .await
}

#[tauri::command]
async fn desktop_skill_detail(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    project_id: Option<String>,
) -> Result<crazyagent::skills::SkillDetail, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.skill_detail(id, project_id).await
}

#[tauri::command]
async fn desktop_skill_import_scan(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<crazyagent::skills::import::ImportList, BridgeError> {
    main_window(&webview)?;
    let call = read(&state).await?.skill_import_scan()?;
    call.await
}

#[tauri::command]
async fn desktop_import_skill(
    webview: Webview,
    state: State<'_, NativeState>,
    folder: String,
    replace: Option<String>,
) -> Result<crazyagent::skills::import::Imported, BridgeError> {
    main_window(&webview)?;
    let call = read(&state).await?.import_skill(folder, replace)?;
    call.await
}

#[tauri::command]
async fn desktop_remove_skill(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    version: u64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.remove_skill(id, version).await
}

#[tauri::command]
async fn desktop_add_mcp_server(
    webview: Webview,
    state: State<'_, NativeState>,
    scope: Scope,
    name: String,
    spec: ServerSpec,
    secrets: Vec<(String, String)>,
) -> Result<ServerView, BridgeError> {
    main_window(&webview)?;
    let call = read(&state)
        .await?
        .add_mcp_server(scope, name, spec, secrets)?;
    call.await
}

#[tauri::command]
async fn desktop_update_mcp_server(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    spec: ServerSpec,
    version: u64,
) -> Result<ServerView, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .update_mcp_server(id, spec, version)
        .await
}

#[tauri::command]
async fn desktop_approve_mcp_server(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    spec: ServerSpec,
) -> Result<ServerView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.approve_mcp_server(id, spec).await
}

#[tauri::command]
async fn desktop_set_mcp_server_enabled(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    enabled: bool,
    version: u64,
) -> Result<ServerView, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_mcp_server_enabled(id, enabled, version)
        .await
}

#[tauri::command]
async fn desktop_remove_mcp_server(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    version: u64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.remove_mcp_server(id, version).await
}

#[tauri::command]
async fn desktop_switch_plugin(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    id: String,
    on: bool,
    version: u64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .switch_plugin(project_id, id, on, version)
        .await
}

#[tauri::command]
async fn desktop_set_mcp_secret(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    name: String,
    value: String,
) -> Result<ServerView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.set_mcp_secret(id, name, value).await
}

#[tauri::command]
async fn desktop_remove_mcp_secret(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    name: String,
) -> Result<ServerView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.remove_mcp_secret(id, name).await
}

#[tauri::command]
async fn desktop_check_mcp_server(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ServerView, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.check_mcp_server(id)?;
    call.await
}

#[tauri::command]
async fn desktop_project_plugins(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<EffectivePlugins, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.project_plugins(project_id).await
}

#[tauri::command]
async fn desktop_tool_switches(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    server_id: Option<String>,
) -> Result<Vec<ToolView>, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .tool_switches(project_id, server_id)
        .await
}

#[tauri::command]
async fn desktop_set_tool_switch(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    server_id: Option<String>,
    tool: String,
    value: Switch,
) -> Result<Vec<ToolView>, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .set_tool_switch(project_id, server_id, tool, value)
        .await
}

#[tauri::command]
async fn desktop_restore_tool_switches(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    server_id: Option<String>,
) -> Result<Vec<ToolView>, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .restore_tool_switches(project_id, server_id)
        .await
}

#[tauri::command]
async fn desktop_agents(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<AgentView>, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.agents(project_id).await
}

#[tauri::command]
async fn desktop_stop_agent(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<AgentView, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.stop_agent(id)?;
    call.await
}

#[tauri::command]
async fn desktop_account_use(
    webview: Webview,
    state: State<'_, NativeState>,
    account_id: String,
) -> Result<AccountUse, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.account_use(account_id).await
}

#[tauri::command]
async fn desktop_release_account(
    webview: Webview,
    state: State<'_, NativeState>,
    account_id: String,
    generation: u64,
) -> Result<AccountUse, BridgeError> {
    main_window(&webview)?;
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
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.interrupt_agent(id)?;
    call.await
}

#[tauri::command]
async fn desktop_restart_agent(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<AgentView, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.restart_agent(id)?;
    call.await
}

#[tauri::command]
async fn desktop_usage_report(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    since_ms: Option<i64>,
    offset_minutes: i32,
) -> Result<UsageReport, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .usage_report(project_id, since_ms, offset_minutes)?;
    call.await
}

#[tauri::command]
async fn desktop_usage_metrics(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    since_ms: i64,
) -> Result<Metrics, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.usage_metrics(project_id, since_ms)?;
    call.await
}

#[tauri::command]
async fn desktop_usage_refresh(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.usage_refresh()?;
    call.await
}

#[tauri::command]
fn desktop_usage_notices(webview: Webview) -> Result<&'static str, BridgeError> {
    main_window(&webview)?;
    Ok(crazyagent::usage::THIRD_PARTY_NOTICES)
}

#[tauri::command]
async fn desktop_usage_set_money(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: MoneySettings,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.usage_set_money(settings)?;
    call.await
}

#[tauri::command]
async fn desktop_usage_decide_recount(
    webview: Webview,
    state: State<'_, NativeState>,
    use_new: bool,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.usage_decide_recount(use_new)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_focus(
    webview: Webview,
    app: AppHandle,
    state: State<'_, NativeState>,
    views: State<'_, ide::IdeViews>,
) -> Result<Option<crazyagent::ide::bridge::reveal::RevealFocus>, BridgeError> {
    main_window(&webview)?;
    if !views.any_loaded() {
        return Ok(None);
    }
    let call = lock(&state).await?.ide_focus()?;
    let focus = call.await?;
    if let Some(revealed) = &focus
        && !ide::focus_own_window(&app, &views, &revealed.project).unwrap_or(false)
    {
        let window = webview.window();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    Ok(focus)
}

#[tauri::command]
async fn desktop_ide_run(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    command: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.ide_run(project_id, command)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_run_mode(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<RunMode, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.ide_run_mode(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_set_run_mode(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    mode: RunMode,
) -> Result<RunMode, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.ide_set_run_mode(project_id, mode)?;
    call.await
}

#[tauri::command]
async fn desktop_overview_read(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<Overview, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.overview_read(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_overview_refresh(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.overview_refresh(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_overview_save(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    tab: Tab,
    text: String,
    version: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .overview_save(project_id, tab, text, version)?;
    call.await
}

#[tauri::command]
async fn desktop_overview_accept(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    tab: Tab,
    version: String,
    proposal_version: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .overview_accept(project_id, tab, version, proposal_version)?;
    call.await
}

#[tauri::command]
async fn desktop_overview_reject(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    tab: Tab,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.overview_reject(project_id, tab)?;
    call.await
}

#[tauri::command]
async fn desktop_overview_set_refresh(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    refresh: Refresh,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .overview_set_refresh(project_id, refresh)?;
    call.await
}

#[tauri::command]
async fn desktop_overview_set_apply(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    apply: Apply,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.overview_set_apply(project_id, apply)?;
    call.await
}

#[tauri::command]
async fn desktop_usage_set_reset(
    webview: Webview,
    state: State<'_, NativeState>,
    account: String,
    window_kind: String,
    at_ms: Option<i64>,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .usage_set_reset(account, window_kind, at_ms)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_extension_asks(
    webview: Webview,
    state: State<'_, NativeState>,
    views: State<'_, ide::IdeViews>,
    project_id: String,
) -> Result<Vec<AskView>, BridgeError> {
    main_window(&webview)?;
    if !views.any_loaded() {
        return Ok(Vec::new());
    }
    let call = lock(&state).await?.ide_extension_asks(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_extension_approve(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    id: String,
    version: String,
) -> Result<Approved, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .ide_extension_approve(project_id, id, version)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_extension_decline(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    id: String,
    version: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .ide_extension_decline(project_id, id, version)?;
    call.await
}

#[tauri::command]
async fn desktop_finished_agents(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<Vec<FinishedAgent>, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.finished_agents(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_remove_finished_agents(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    agents: Vec<i64>,
) -> Result<crazyagent::team::cleanup::Removal, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .remove_finished_agents(project_id, agents)?;
    call.await
}

#[tauri::command]
async fn desktop_changes(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<Vec<ChangeSetView>, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.change_sets(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_change_diff(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    head: String,
    page: u16,
) -> Result<ChangeDiff, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.change_diff(id, head, page)?;
    call.await
}

#[tauri::command]
async fn desktop_accept_change(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    expected_head: String,
    message: Option<String>,
    target: Option<String>,
) -> Result<AcceptOutcome, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .accept_change(id, expected_head, message, target)?;
    call.await
}

#[tauri::command]
async fn desktop_change_branches(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<TargetBranches, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.change_branches(id)?;
    call.await
}

#[tauri::command]
async fn desktop_change_owner_dirty(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    target: Option<String>,
) -> Result<OwnerDirty, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.change_owner_dirty(id, target)?;
    call.await
}

#[tauri::command]
async fn desktop_commit_owner_files(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    files: Vec<String>,
    message: String,
    target: Option<String>,
) -> Result<(String, String), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .commit_owner_files(id, files, message, target)?;
    call.await
}

#[tauri::command]
async fn desktop_discard_change(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    expected_head: String,
) -> Result<u64, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.discard_change(id, expected_head)?;
    call.await
}

#[tauri::command]
async fn desktop_restore_change(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.restore_change(id)?;
    call.await
}

#[tauri::command]
async fn desktop_undo_change(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<crazyagent::landing::AcceptOutcome, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.undo_change(id)?;
    call.await
}

#[tauri::command]
async fn desktop_update_change(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.update_change(id)?;
    call.await
}

/// Shows one macOS notification for a pending question while the window is in the
/// background, at most once per question. The text is built here from the engine's record
/// and the project's name; the window names only the project and the question.
#[tauri::command]
async fn desktop_notify_question(
    webview: Webview,
    state: State<'_, NativeState>,
    notices: State<'_, Notices>,
    project_id: String,
    question_id: u64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    bridge::notify::available()?;
    if webview.window().is_focused().unwrap_or(false) {
        return Ok(());
    }
    let call = read(&state)
        .await?
        .notified_question(project_id.clone(), question_id)?;
    let question = call.await?;
    let setup = read(&state).await?.project_setup(&project_id).await;
    let name = setup.ok().flatten().map(|setup| setup.name);
    let notice = bridge::notify::notice(name.as_deref(), &question);
    let Some(id) = notices.claim(question.id, question.project.as_str())? else {
        return Ok(());
    };
    bridge::notify::show(tauri::Manager::app_handle(&webview), id, &notice)
        .inspect_err(|_| notices.release(question.id, id))
}

#[tauri::command]
async fn desktop_notification_target(
    webview: Webview,
    notices: State<'_, Notices>,
) -> Result<Option<String>, BridgeError> {
    main_window(&webview)?;
    Ok(notices.take())
}

#[tauri::command]
async fn desktop_system_notifications(webview: Webview) -> Result<&'static str, BridgeError> {
    main_window(&webview)?;
    Ok(bridge::notify::support())
}

#[tauri::command]
async fn desktop_questions(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    after: Option<u64>,
) -> Result<QuestionPage, BridgeError> {
    main_window(&webview)?;
    let call = read(&state).await?.questions(project_id, after)?;
    call.await
}

#[tauri::command]
async fn desktop_answer_question(
    webview: Webview,
    state: State<'_, NativeState>,
    id: u64,
    version: u64,
    picks: Vec<u8>,
    comment: Option<String>,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .answer_question(id, version, picks, comment)?;
    call.await
}

#[tauri::command]
async fn desktop_dismiss_question(
    webview: Webview,
    state: State<'_, NativeState>,
    id: u64,
    version: u64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.dismiss_question(id, version)?;
    call.await
}

#[tauri::command]
async fn desktop_dismiss_questions(
    webview: Webview,
    state: State<'_, NativeState>,
    items: Vec<(u64, u64)>,
) -> Result<QuestionsDismissed, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.dismiss_questions(items)?;
    call.await
}

#[tauri::command]
async fn desktop_question_counts(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Vec<QuestionCount>, BridgeError> {
    main_window(&webview)?;
    let call = read(&state).await?.question_counts()?;
    call.await
}

#[tauri::command]
async fn desktop_process_text(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<crazyagent::engine::owner::ProcessText, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.process_text(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_save_process_text(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    text: String,
    version: u64,
    opened: Option<u64>,
) -> Result<crazyagent::engine::owner::SavedText, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .save_process_text(project_id, text, version, opened)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_state(
    webview: Webview,
    state: State<'_, NativeState>,
    retry: bool,
) -> Result<bridge::ide::IdeView, BridgeError> {
    main_window(&webview)?;
    let call = read(&state).await?.ide_state(retry)?;
    call.await
}

#[tauri::command]
async fn desktop_ide_open(
    webview: Webview,
    app: AppHandle,
    state: State<'_, NativeState>,
    views: State<'_, ide::IdeViews>,
    project_id: String,
    bounds: ide::Bounds,
) -> Result<ide::Opened, BridgeError> {
    main_window(&webview)?;
    let project = bridge::ide::ide_project(&project_id)?;
    let bounds = bounds.checked()?;
    let (ticket, turn) = views.begin(&project);
    let opened = {
        let _turn = turn.lock().await;
        let call = read(&state).await?.ide_open(project.clone())?;
        call.await?
    };
    let left = opened.left.clone();
    let workbench = ide::workbench(&opened)?;
    let window = webview.window();
    let bounds = bounds.in_window(&window)?;
    let own_window = ide::show(&app, &window, &views, &project, workbench, bounds, ticket)?;
    Ok(ide::Opened { own_window, left })
}

#[tauri::command]
async fn desktop_ide_place<R: Runtime>(
    webview: Webview<R>,
    app: AppHandle<R>,
    views: State<'_, ide::IdeViews>,
    project_id: String,
    bounds: ide::Bounds,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let project = bridge::ide::ide_project(&project_id)?;
    let bounds = bounds.checked()?.in_window(&webview.window())?;
    ide::place(&app, &views, &project, bounds)
}

#[tauri::command]
async fn desktop_ide_hide<R: Runtime>(
    webview: Webview<R>,
    app: AppHandle<R>,
    views: State<'_, ide::IdeViews>,
    project_id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    ide::hide(&app, &views, &bridge::ide::ide_project(&project_id)?)
}

#[tauri::command]
async fn desktop_ide_close<R: Runtime>(
    webview: Webview<R>,
    app: AppHandle<R>,
    state: State<'_, NativeState>,
    views: State<'_, ide::IdeViews>,
    project_id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let project = bridge::ide::ide_project(&project_id)?;
    if views.keep_for_own_window(&project) {
        return Ok(());
    }
    close_ide(&app, &state, &views, &project).await
}

/// Closes a project's IDE webview and stops its server, in the project's turn. Inputs: the app,
/// the engine bridge, the views and the project. Output: Ok, or the engine's refusal.
async fn close_ide<R: Runtime>(
    app: &AppHandle<R>,
    state: &NativeState,
    views: &ide::IdeViews,
    project: &ProjectId,
) -> Result<(), BridgeError> {
    let closing = views.closing(project);
    ide::close(app, views, project)?;
    let Some(closing) = closing else {
        return Ok(());
    };
    views
        .stop_closed(project, closing, || async {
            let call = read(state).await?.ide_stop(project.clone())?;
            call.await
        })
        .await
        .map(|_| ())
}

/// Moves a project's loaded IDE into a maximized window of its own. Input: the project.
/// Output: Ok, or the refusal.
#[tauri::command]
async fn desktop_ide_window_open(
    webview: Webview,
    app: AppHandle,
    views: State<'_, ide::IdeViews>,
    project_id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let project = bridge::ide::ide_project(&project_id)?;
    ide::move_out(&app, &webview.window(), &views, &project)
}

/// Brings a project's own IDE window forward. Input: the project. Output: Ok, also when the
/// project has none, or the refusal.
#[tauri::command]
async fn desktop_ide_window_focus(
    webview: Webview,
    app: AppHandle,
    views: State<'_, ide::IdeViews>,
    project_id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let project = bridge::ide::ide_project(&project_id)?;
    ide::focus_own_window(&app, &views, &project).map(|_| ())
}

/// Whether a project's IDE sits in its own window. Input: the project. Output: true then.
#[tauri::command]
async fn desktop_ide_window_out(
    webview: Webview,
    views: State<'_, ide::IdeViews>,
    project_id: String,
) -> Result<bool, BridgeError> {
    main_window(&webview)?;
    Ok(views.in_own_window(&bridge::ide::ide_project(&project_id)?))
}

#[tauri::command]
async fn desktop_ide_touch(
    webview: Webview,
    state: State<'_, NativeState>,
    views: State<'_, ide::IdeViews>,
    project_id: String,
) -> Result<bool, BridgeError> {
    main_window(&webview)?;
    let project = bridge::ide::ide_project(&project_id)?;
    if !views.loaded(&project) {
        return Ok(false);
    }
    let call = read(&state).await?.ide_touch(project)?;
    call.await
}

#[tauri::command]
async fn desktop_import_instructions_file(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<Option<String>, BridgeError> {
    main_window(&webview)?;
    let path = match bridge::picker::choose(bridge::picker::INSTRUCTIONS_SCRIPT).await? {
        Pick::Canceled => return Ok(None),
        Pick::Paths(mut paths) => {
            assert!(!paths.is_empty());
            paths.swap_remove(0)
        }
    };
    let call = lock(&state).await?.read_instruction_file(path)?;
    call.await.map(Some)
}

#[tauri::command]
async fn desktop_instruction_import(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<Vec<crazyagent::engine::owner::ImportFile>, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.instruction_import(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_confirm_instruction_import(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<Vec<crazyagent::engine::owner::ImportFile>, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.confirm_instruction_import(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_uncommitted_instruction_import(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<crazyagent::engine::owner::UncommittedImport, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .uncommitted_instruction_import(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_commit_instruction_import(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<crazyagent::engine::owner::ImportCommit, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.commit_instruction_import(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_agent_instructions(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    agent: Option<i64>,
) -> Result<crazyagent::engine::owner::AgentInstructions, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.agent_instructions(project_id, agent)?;
    call.await
}

#[tauri::command]
async fn desktop_recovery_banner(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<crazyagent::engine::owner::RecoveryBanner, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.recovery_banner(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_instruction_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<crazyagent::engine::owner::InstructionView, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.instruction_settings(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_process_propose(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: Option<String>,
    pool: Option<String>,
) -> Result<crazyagent::engine::owner::Proposal, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.propose_process(project_id, pool)?;
    call.await
}

#[tauri::command]
async fn desktop_app_instruction_settings(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<crazyagent::engine::owner::AppInstructionSettings, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.app_instruction_settings()?;
    call.await
}

#[tauri::command]
async fn desktop_save_app_instruction_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    settings: crazyagent::engine::owner::AppInstructionSettings,
) -> Result<crazyagent::engine::owner::AppInstructionSettings, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .save_app_instruction_settings(settings)?;
    call.await
}

#[tauri::command]
async fn desktop_retry_recovery(
    webview: Webview,
    state: State<'_, NativeState>,
    agent: i64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.retry_recovery(agent)?;
    call.await
}

#[tauri::command]
async fn desktop_dismiss_recovery(
    webview: Webview,
    state: State<'_, NativeState>,
    action_kind: crazyagent::ledger::ActionKind,
    id: i64,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.dismiss_recovery(action_kind, id)?;
    call.await
}

#[tauri::command]
async fn desktop_adoption(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<crazyagent::engine::owner::Adoption, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.adoption(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_save_instruction_settings(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
    settings: crazyagent::engine::owner::InstructionSettings,
    version: Option<u64>,
) -> Result<crazyagent::engine::owner::InstructionView, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state)
        .await?
        .save_instruction_settings(project_id, settings, version)?;
    call.await
}

#[tauri::command]
async fn desktop_default_process(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<String, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.default_process()?;
    call.await
}

#[tauri::command]
async fn desktop_process_given(
    webview: Webview,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<String, BridgeError> {
    main_window(&webview)?;
    let call = lock(&state).await?.given_text(project_id)?;
    call.await
}

#[tauri::command]
async fn desktop_pick_folder(
    webview: Webview,
    purpose: FolderPurpose,
) -> Result<Option<String>, BridgeError> {
    main_window(&webview)?;
    #[cfg(all(unix, feature = "native-webdriver"))]
    if let Some(picked) = bridge::trial_pick::take(purpose)? {
        return Ok(Some(picked));
    }
    Ok(match bridge::picker::choose(purpose.script()).await? {
        Pick::Canceled => None,
        Pick::Paths(mut paths) => {
            assert!(!paths.is_empty());
            Some(paths.swap_remove(0))
        }
    })
}

#[tauri::command]
async fn desktop_pick_documents(webview: Webview) -> Result<Vec<String>, BridgeError> {
    main_window(&webview)?;
    Ok(
        match bridge::picker::choose(bridge::picker::DOCUMENTS_SCRIPT).await? {
            Pick::Canceled => Vec::new(),
            Pick::Paths(paths) => paths,
        },
    )
}

#[tauri::command]
async fn desktop_open_link(webview: Webview, url: String) -> Result<(), BridgeError> {
    main_window(&webview)?;
    bridge::open::open_link(&url).await
}

#[tauri::command]
async fn desktop_reveal_project_folder(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.reveal_project_folder(&id).await
}

#[tauri::command]
async fn desktop_open_context_item(
    webview: Webview,
    state: State<'_, NativeState>,
    id: String,
    path: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.open_context_item(&id, &path).await
}

#[tauri::command]
async fn desktop_model_access_page(
    webview: Webview,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    next: bool,
) -> Result<ModelAccessPage, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .model_access_page(collection, next)
        .await
}

#[tauri::command]
async fn desktop_inspect_model_access(
    webview: Webview,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    id: String,
) -> Result<ModelAccessDetail, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .inspect_model_access(collection, &id)
        .await
}

#[tauri::command]
async fn desktop_edit_model_access(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    edit: ModelAccessEdit,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&webview)?;
    lock(&state)
        .await?
        .edit_model_access(&ticket, edit, generation)
        .await
}

#[tauri::command]
async fn desktop_start_native_auth(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    method: NativeAuthMethod,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.start_native_auth(&ticket, method).await
}

#[tauri::command]
async fn desktop_read_native_auth(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.read_native_auth(&ticket).await
}

#[tauri::command]
async fn desktop_cancel_native_auth(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.cancel_native_auth(&ticket).await
}

#[tauri::command]
async fn desktop_clear_native_auth(
    webview: Webview,
    state: State<'_, NativeState>,
) -> Result<bool, BridgeError> {
    main_window(&webview)?;
    lock(&state).await?.clear_native_auth().await
}

#[tauri::command]
async fn desktop_submit_claude_auth_code(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
    code: String,
) -> Result<(), BridgeError> {
    main_window(&webview)?;
    let secret = NativeBridge::claude_auth_secret(code)?;
    let mut bridge = state.0.enter_owned().await?;
    tauri::async_runtime::spawn(
        async move { bridge.submit_claude_auth_code(&ticket, secret).await },
    )
    .await
    .map_err(|_| BridgeError {
        code: "native_auth_claude_input_outcome_unknown",
        cause: None,
        account: None,
    })?
}

#[tauri::command]
async fn desktop_logout_native_auth(
    webview: Webview,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&webview)?;
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
    webview: Webview,
    app: AppHandle,
) -> Result<UpdateSettings, BridgeError> {
    main_window(&webview)?;
    updates::settings(&app)
}

#[tauri::command]
fn desktop_set_update_channel(
    webview: Webview,
    app: AppHandle,
    state: State<'_, UpdateState>,
    channel: String,
) -> Result<UpdateSettings, BridgeError> {
    main_window(&webview)?;
    updates::set_channel(&app, &state, &channel)
}

#[tauri::command]
async fn desktop_check_update(
    webview: Webview,
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<Option<AvailableUpdate>, BridgeError> {
    main_window(&webview)?;
    updates::check(&app, &state).await
}

#[tauri::command]
async fn desktop_install_update(
    webview: Webview,
    state: State<'_, UpdateState>,
    progress: Channel<UpdateProgress>,
) -> Result<InstallOutcome, BridgeError> {
    main_window(&webview)?;
    updates::install(&state, progress).await
}

#[tauri::command]
fn desktop_restart_app(webview: Webview, app: AppHandle) -> Result<(), BridgeError> {
    main_window(&webview)?;
    app.request_restart();
    Ok(())
}

#[tauri::command]
async fn desktop_link_cli(webview: Webview) -> Result<String, BridgeError> {
    main_window(&webview)?;
    updates::link_cli().await
}

/// Puts one check behind Tauri's capability check and in front of every native command. The
/// capability check refuses the real IDE page first, because its address is not a local one.
/// Any page that passes it reaches a command only under the main or a terminal wall label, and
/// any other label gets `window_denied` before its arguments are read. Input: the generated
/// handler. Output: the handler to register.
fn admit_pages<R: Runtime>(
    handler: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        if bridge::terminal::window_label_valid(invoke.message.webview_ref().label()) {
            return handler(invoke);
        }
        invoke.resolver.reject(engine_failure("window_denied"));
        true
    }
}

/// Declares the native commands once: their names for whoever must walk them and their
/// handler behind the page check. Input: the command functions. Output: `NATIVE_COMMANDS`
/// and `native_handler`.
macro_rules! native_commands {
    ($($command:ident,)+) => {
        #[cfg(test)]
        const NATIVE_COMMANDS: &[&str] = &[$(stringify!($command),)+];

        fn native_handler() -> impl Fn(Invoke) -> bool + Send + Sync + 'static {
            admit_pages::<tauri::Wry>(tauri::generate_handler![$($command,)+])
        }
    };
}

native_commands! {
    desktop_connect,
    desktop_status,
    desktop_harness_catalog,
    desktop_harness_versions,
    desktop_harness_update,
    desktop_harness_install,
    desktop_harness_find,
    desktop_harness_choose_path,
    desktop_pick_harness_file,
    desktop_harness_update_settings,
    desktop_harness_set_update_settings,
    desktop_baselines,
    desktop_inspect_baseline,
    desktop_approve_baseline,
    desktop_revoke_baseline,
    desktop_reconcile,
    desktop_retry,
    desktop_projects,
    desktop_project_list,
    desktop_create_project,
    desktop_project_catalog,
    desktop_setup_project,
    desktop_project_setup,
    desktop_research_jobs,
    desktop_project_activity,
    desktop_project_context,
    desktop_project_context_item,
    desktop_save_research_to_memory,
    desktop_add_project_documents,
    desktop_delete_project_context,
    desktop_project_read_paths,
    desktop_project_hidden_paths,
    desktop_project_files,
    desktop_set_project_read_paths,
    desktop_set_project_hidden_paths,
    desktop_project_agent_policy,
    desktop_set_project_agent_policy,
    desktop_team_status,
    desktop_team_agent_messages,
    desktop_team_message_settings,
    desktop_team_message,
    desktop_team_delegate,
    desktop_team_cancel,
    desktop_team_keep_warm,
    desktop_team_time_limits,
    desktop_team_on_harness_switch,
    desktop_team_apply_harness_switch,
    desktop_team_sub_agent_depth,
    desktop_team_child_landing,
    desktop_team_finished_agents,
    desktop_team_agent_branches,
    desktop_team_master_branch,
    desktop_team_land_on,
    desktop_team_background,
    desktop_team_leftovers,
    desktop_team_leftover_hold,
    desktop_team_ask_before_merge,
    desktop_team_master_lands,
    desktop_team_reask_empty_reply,
    desktop_team_done_quiet,
    desktop_team_catch_up_main,
    desktop_team_master_idle_stop,
    desktop_team_master_live_terminal,
    desktop_team_reuse_idle,
    desktop_team_landing_mode,
    desktop_team_new_folder_limit,
    desktop_team_coordination,
    desktop_team_harness_mode,
    desktop_team_harness_delivery,
    desktop_team_live_master,
    desktop_team_pause,
    desktop_team_hand_off,
    desktop_team_rename,
    desktop_team_wake_agent,
    desktop_team_stop_agent,
    desktop_coord_send,
    desktop_team_stop_teammate,
    desktop_team_max_harnesses,
    desktop_team_max_teammates,
    desktop_team_master_rounds,
    desktop_team_master_run_time,
    desktop_team_token_budget,
    desktop_team_spread_accounts,
    desktop_team_graph,
    desktop_team_events,
    desktop_team_wake_rule,
    desktop_team_seen,
    desktop_coord_questions,
    desktop_coord_answer,
    desktop_team_attention,
    desktop_terminal_attach,
    desktop_terminal_input,
    desktop_terminal_prompt,
    desktop_terminal_resize,
    desktop_terminal_detach,
    desktop_team_terminals_per_window,
    desktop_terminals_per_window,
    desktop_open_terminal_windows,
    desktop_extend_terminal_windows,
    desktop_focus_terminal_window,
    desktop_close_project_walls,
    desktop_update_project_setup,
    desktop_remove_project,
    desktop_move_project_home,
    desktop_cancel_research,
    desktop_retry_research,
    desktop_plan_roadmap,
    desktop_roadmap_status,
    desktop_refresh_roadmap_status,
    desktop_set_roadmap_refresh,
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
    desktop_icloud_download,
    desktop_icloud_progress,
    desktop_icloud_cancel,
    desktop_setup_status,
    desktop_run_setup,
    desktop_set_pack_entry,
    desktop_remove_pack_entry,
    desktop_project_memories,
    desktop_ide_asks,
    desktop_review_memory,
    desktop_memories,
    desktop_memory,
    desktop_approved_memories,
    desktop_add_memory,
    desktop_import_memory,
    desktop_promote_memory,
    desktop_delete_memory,
    desktop_approve_memory,
    desktop_set_agent_memory_writes,
    desktop_set_agent_memory_scope,
    desktop_move_memory,
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
    desktop_master_sessions,
    desktop_master_session_turns,
    desktop_master_inputs,
    desktop_master_messages,
    desktop_new_master_session,
    desktop_resume_master_session,
    desktop_delete_master_session,
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
    desktop_clear_native_auth,
    desktop_logout_native_auth,
    desktop_submit_claude_auth_code,
    desktop_owner_profile,
    desktop_save_owner_profile,
    desktop_sandbox_settings,
    desktop_set_sandbox_settings,
    desktop_cleanup_settings,
    desktop_set_cleanup_settings,
    desktop_quit_settings,
    desktop_set_quit_settings,
    desktop_master_step_settings,
    desktop_set_master_step_settings,
    desktop_master_time_settings,
    desktop_set_master_time_settings,
    desktop_master_frame_settings,
    desktop_set_master_frame_settings,
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
    desktop_plugin_hooks,
    desktop_add_hooks,
    desktop_set_hook_enabled,
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
    desktop_tool_switches,
    desktop_set_tool_switch,
    desktop_restore_tool_switches,
    desktop_agents,
    desktop_stop_agent,
    desktop_account_use,
    desktop_release_account,
    desktop_interrupt_agent,
    desktop_restart_agent,
    desktop_changes,
    desktop_change_diff,
    desktop_accept_change,
    desktop_change_branches,
    desktop_change_owner_dirty,
    desktop_commit_owner_files,
    desktop_discard_change,
    desktop_restore_change,
    desktop_undo_change,
    desktop_update_change,
    desktop_questions,
    desktop_answer_question,
    desktop_dismiss_question,
    desktop_dismiss_questions,
    desktop_question_counts,
    desktop_notify_question,
    desktop_notification_target,
    desktop_system_notifications,
    desktop_process_text,
    desktop_save_process_text,
    desktop_import_instructions_file,
    desktop_instruction_import,
    desktop_confirm_instruction_import,
    desktop_uncommitted_instruction_import,
    desktop_commit_instruction_import,
    desktop_default_process,
    desktop_process_given,
    desktop_agent_instructions,
    desktop_recovery_banner,
    desktop_retry_recovery,
    desktop_dismiss_recovery,
    desktop_ide_state,
    desktop_ide_open,
    desktop_ide_place,
    desktop_ide_hide,
    desktop_ide_close,
    desktop_ide_touch,
    desktop_ide_window_open,
    desktop_ide_window_focus,
    desktop_ide_window_out,
    desktop_instruction_settings,
    desktop_save_instruction_settings,
    desktop_process_propose,
    desktop_app_instruction_settings,
    desktop_save_app_instruction_settings,
    desktop_adoption,
    desktop_usage_report,
    desktop_usage_metrics,
    desktop_usage_refresh,
    desktop_usage_notices,
    desktop_usage_set_money,
    desktop_usage_decide_recount,
    desktop_usage_set_reset,
    desktop_ide_extension_asks,
    desktop_ide_extension_approve,
    desktop_ide_extension_decline,
    desktop_finished_agents,
    desktop_remove_finished_agents,
    desktop_project_removal,
    desktop_pick_folder,
    desktop_pick_documents,
    desktop_open_link,
    desktop_reveal_project_folder,
    desktop_open_context_item,
    desktop_engine_versions,
    desktop_restart_engine,
    desktop_ide_focus,
    desktop_ide_run,
    desktop_ide_run_mode,
    desktop_ide_set_run_mode,
    desktop_overview_read,
    desktop_overview_refresh,
    desktop_overview_save,
    desktop_overview_accept,
    desktop_overview_reject,
    desktop_overview_set_refresh,
    desktop_overview_set_apply,
    desktop_update_settings,
    desktop_set_update_channel,
    desktop_check_update,
    desktop_install_update,
    desktop_restart_app,
    desktop_link_cli,
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
        .manage(ide::IdeViews::default())
        .manage(Notices::default())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(native_handler())
        .setup(|app| {
            let main = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("CrazyAgent")
                .inner_size(1180.0, 800.0)
                .min_inner_size(760.0, 560.0)
                .on_navigation(app_page)
                .on_page_load(|window, payload| {
                    if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
                        window
                            .state::<TerminalState>()
                            .0
                            .page_started(window.label());
                    }
                })
                .build()?;
            let handle = app.handle().clone();
            main.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    close_walls(&handle, None);
                    ide::shut_own_windows(handle.clone(), None);
                }
                if matches!(event, tauri::WindowEvent::Focused(true)) {
                    ide::main_focused(&handle);
                }
            });
            let handle = app.handle().clone();
            app.notification()
                .on_action(move |action| bridge::notify::performed(&handle, action))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::{InvokeRequest, WebviewBuilder};
    use tauri::{LogicalPosition, LogicalSize, Webview};

    struct Page(Webview<MockRuntime>);

    impl AsRef<Webview<MockRuntime>> for Page {
        fn as_ref(&self) -> &Webview<MockRuntime> {
            &self.0
        }
    }

    fn ask(page: &Webview<MockRuntime>, command: &str, args: Value) -> Result<(), Value> {
        get_ipc_response(
            &Page(page.clone()),
            InvokeRequest {
                cmd: command.into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: InvokeBody::Json(args),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            },
        )
        .map(|_| ())
    }

    #[test]
    fn a_wall_title_shows_hidden_characters_as_their_escapes_and_stays_bounded() {
        assert_eq!(
            wall_name("Lark\u{202e}spur\u{200b} \u{e0041}Booking"),
            "Lark\\u202espur\\u200b \\u{e0041}Booking"
        );
        assert_eq!(wall_name("Lark\n\tspur\u{1}"), "Larkspur");
        assert_eq!(wall_name("\u{200b}"), "\\u200b");
        assert_eq!(wall_name(" \u{1} "), "Project");
        assert_eq!(wall_name(&wall_name("Lark\u{202e}spur")), "Lark\\u202espur");
        assert_eq!(
            wall_name("Larkspur \u{2764}\u{fe0f}"),
            "Larkspur \u{2764}\u{fe0f}"
        );
        let long = wall_name(&"\u{200b}x".repeat(WALL_NAME_CHARS_MAX * 4));
        assert_eq!(long.chars().count(), WALL_NAME_CHARS_MAX);
    }

    #[test]
    fn the_app_page_keeps_its_commands_and_the_ide_page_gets_none_once_the_ide_is_in_the_window() {
        let app = mock_builder()
            .manage(NativeState(BridgeQueue::new(NativeBridge::new(None))))
            .manage(ide::IdeViews::default())
            .invoke_handler(admit_pages(tauri::generate_handler![
                desktop_ide_place,
                desktop_ide_hide,
                desktop_ide_close,
            ]))
            .build(mock_context(noop_assets()))
            .unwrap();
        let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
            .build()
            .unwrap();
        let ide_page = main
            .as_ref()
            .window()
            .add_child(
                WebviewBuilder::new("ide-70-1", WebviewUrl::default()),
                LogicalPosition::new(0, 0),
                LogicalSize::new(400, 300),
            )
            .unwrap();
        let project = json!({ "projectId": "project-one" });
        let placed = json!({
            "projectId": "project-one",
            "bounds": { "x": 0.0, "y": 80.0, "width": 900.0, "height": 600.0, "viewport": 720.0 },
        });
        let calls = [
            ("desktop_ide_place", &placed),
            ("desktop_ide_hide", &project),
            ("desktop_ide_close", &project),
        ];
        for (command, args) in calls {
            assert_eq!(
                ask(main.as_ref(), command, args.clone()),
                Ok(()),
                "{command} from the app page",
            );
            let refused = ask(&ide_page, command, args.clone()).unwrap_err();
            assert_eq!(
                refused["code"], "window_denied",
                "{command} from the IDE page",
            );
        }
    }

    #[test]
    fn every_native_command_refuses_any_page_the_app_did_not_make() {
        let reached = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let spy = {
            let reached = Arc::clone(&reached);
            move |invoke: Invoke<MockRuntime>| {
                reached
                    .lock()
                    .unwrap()
                    .push(invoke.message.command().to_owned());
                invoke.resolver.resolve(());
                true
            }
        };
        let app = mock_builder()
            .invoke_handler(admit_pages(spy))
            .build(mock_context(noop_assets()))
            .unwrap();
        let page = |label: &str| {
            WebviewWindowBuilder::new(&app, label, WebviewUrl::default())
                .build()
                .unwrap()
        };
        let main = page("main");
        let wall = page("terminals-0123456789abcdef-1");
        let ide_page = main
            .as_ref()
            .window()
            .add_child(
                WebviewBuilder::new("ide-70-1", WebviewUrl::default()),
                LogicalPosition::new(0, 0),
                LogicalSize::new(400, 300),
            )
            .unwrap();
        let strangers: Vec<Webview<MockRuntime>> = ["stranger", "Main", "terminals-zz-1"]
            .map(|label| page(label).as_ref().clone())
            .into_iter()
            .chain([ide_page])
            .collect();
        assert!(NATIVE_COMMANDS.contains(&"desktop_connect"));
        assert!(NATIVE_COMMANDS.contains(&"desktop_ide_hide"));
        let added_later = ["desktop_a_command_added_later"];
        for command in NATIVE_COMMANDS.iter().chain(&added_later) {
            for stranger in &strangers {
                let refused = ask(stranger, command, json!({})).unwrap_err();
                assert_eq!(
                    refused["code"],
                    "window_denied",
                    "{command} from {}",
                    stranger.label(),
                );
            }
        }
        assert_eq!(
            *reached.lock().unwrap(),
            Vec::<String>::new(),
            "no command ran for a page the app did not make",
        );
        for command in NATIVE_COMMANDS {
            assert_eq!(ask(main.as_ref(), command, json!({})), Ok(()), "{command}");
            assert_eq!(ask(wall.as_ref(), command, json!({})), Ok(()), "{command}");
        }
        assert_eq!(reached.lock().unwrap().len(), NATIVE_COMMANDS.len() * 2);
    }
}
