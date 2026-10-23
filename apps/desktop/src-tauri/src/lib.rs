pub mod bridge;

#[cfg(feature = "native-webdriver")]
mod native_refresh_observation;

use bridge::{
    AccountProfileView, ActionReceipt, BaselineDetail, BaselinePage, BridgeError,
    ConfiguredAccountPlan, ConnectionStatus, HistoryPage, IntentReceipt, ManagedAccountPlan,
    ModelAccessDetail, ModelAccessPage, NativeAuthView, NativeBridge, PendingIntent, ProjectDetail,
    ProjectPage,
};
use bridge::picker::Pick;
use bridge::project_setup::MemoryItem;
use bridge::memory::{MemoryList, MemoryNote};
use crazyagent::accounts::{AccountDraft, AccountView, ProviderEntry};
use crazyagent::config::ModelAccessEdit;
use crazyagent::harnesses::HarnessEntry;
use crazyagent::engine::owner::{
    AccountAuthCapabilitiesReply, AccountCheck, AccountPage, AccountSecret, ModelAccessCollection,
    NativeAuthMethod,
};
use crazyagent::memory::ReviewDecision;
use crazyagent::memory::library::ApprovedMemories;
use crazyagent::project_setup::{
    ContextChunk, ProjectCatalog, ProjectContext, ProjectSetup, ResearchKind, StoredSetup,
};
use crazyagent::owner_profile::OwnerProfile;
use crazyagent::plugins::{EffectivePlugins, Scope, ServerSpec, ServerView, SkillView};
use crazyagent::pools::PoolSummary;
use crazyagent::protocol::ProjectSpec;
use crazyagent::team::TeamStatus;
use std::sync::Arc;
use tauri::{State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::{Mutex, MutexGuard};

pub struct NativeState(Arc<Mutex<NativeBridge>>);

fn main_window(window: &WebviewWindow) -> Result<(), BridgeError> {
    if window.label() != "main" {
        return Err(BridgeError {
            code: "window_denied",
        });
    }
    Ok(())
}

fn lock(state: &NativeState) -> Result<MutexGuard<'_, NativeBridge>, BridgeError> {
    state.0.try_lock().map_err(|_| BridgeError { code: "busy" })
}

#[tauri::command]
async fn desktop_connect(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    lock(&state)?.connect().await
}

#[tauri::command]
fn desktop_status(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ConnectionStatus, BridgeError> {
    main_window(&window)?;
    Ok(lock(&state)?.status())
}

#[tauri::command]
async fn desktop_harness_catalog(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<HarnessEntry>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.harness_catalog().await
}

#[tauri::command]
async fn desktop_baselines(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<BaselinePage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.baselines(next).await
}

#[tauri::command]
async fn desktop_inspect_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<BaselineDetail, BridgeError> {
    main_window(&window)?;
    lock(&state)?.inspect(&id).await
}

#[tauri::command]
async fn desktop_approve_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.approve(&ticket).await
}

#[tauri::command]
async fn desktop_revoke_baseline(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.revoke(&ticket).await
}

#[tauri::command]
async fn desktop_reconcile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.reconcile().await
}

#[tauri::command]
async fn desktop_retry(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<ActionReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.retry().await
}

#[tauri::command]
async fn desktop_projects(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<ProjectPage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.projects(next).await
}

#[tauri::command]
async fn desktop_create_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    spec: ProjectSpec,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.create_project(spec, generation).await
}

#[tauri::command]
async fn desktop_project_catalog(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    pool: Option<String>,
    research: Vec<ResearchKind>,
) -> Result<ProjectCatalog, BridgeError> {
    main_window(&window)?;
    lock(&state)?.project_catalog(pool, research).await
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
    lock(&state)?.setup_project(spec, setup, generation).await
}

#[tauri::command]
async fn desktop_project_setup(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<Option<StoredSetup>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.project_setup(&id).await
}

#[tauri::command]
async fn desktop_research_jobs(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<bridge::project_setup::ResearchJobs, BridgeError> {
    main_window(&window)?;
    lock(&state)?.research_jobs(&id).await
}

#[tauri::command]
async fn desktop_project_context(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ProjectContext, BridgeError> {
    main_window(&window)?;
    lock(&state)?.project_context(&id).await
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
    lock(&state)?.project_context_item(&id, path, offset).await
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?.memory(&id).await
}

#[tauri::command]
async fn desktop_approved_memories(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project: String,
) -> Result<ApprovedMemories, BridgeError> {
    main_window(&window)?;
    lock(&state)?.approved_memories(&project).await
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?.delete_memory(&id, generation).await
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?
        .remember_research(&project, research, section, generation)
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
    lock(&state)?
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
    lock(&state)?.team_status(&id).await
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
    lock(&state)?.team_message(&id, text, generation).await
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
    lock(&state)?
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
    lock(&state)?.team_cancel(&id, task, generation).await
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
    lock(&state)?.team_keep_warm(&id, on, generation).await
}

#[tauri::command]
async fn desktop_open_project(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ProjectDetail, BridgeError> {
    main_window(&window)?;
    lock(&state)?.open_project(&id).await
}

#[tauri::command]
async fn desktop_history(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    next: bool,
) -> Result<HistoryPage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.history(next).await
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
    lock(&state)?.send_message(&ticket, &text, generation).await
}

#[tauri::command]
async fn desktop_master_state(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<bridge::master::MasterState, BridgeError> {
    main_window(&window)?;
    lock(&state)?.master_state(&ticket).await
}

#[tauri::command]
async fn desktop_master_turns(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    session_id: String,
) -> Result<bridge::master::MasterTurns, BridgeError> {
    main_window(&window)?;
    lock(&state)?.master_turns(&ticket, &session_id, None).await
}

#[tauri::command]
async fn desktop_open_master(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.open_default_master(&ticket, generation).await
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?
        .replace_brief(&ticket, &text, generation)
        .await
}

#[tauri::command]
fn desktop_pending_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Option<PendingIntent>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.pending_intent()
}

#[tauri::command]
async fn desktop_reconcile_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.reconcile_intent().await
}

#[tauri::command]
async fn desktop_retry_intent(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.retry_intent().await
}

#[tauri::command]
async fn desktop_prepare_managed_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    provider: String,
    label: String,
) -> Result<ManagedAccountPlan, BridgeError> {
    main_window(&window)?;
    lock(&state)?
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
    lock(&state)?
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
    lock(&state)?.account_auth_capabilities(&ticket).await
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
        })?;
    let mut bridge = Arc::clone(&state.0)
        .try_lock_owned()
        .map_err(|_| BridgeError { code: "busy" })?;
    tauri::async_runtime::spawn(async move {
        bridge
            .configure_account_profile(&ticket, secret, generation)
            .await
    })
    .await
    .map_err(|_| BridgeError {
        code: "account_profile_outcome_unknown_read_same_request",
    })?
}

#[tauri::command]
async fn desktop_read_account_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<AccountProfileView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.read_account_profile().await
}

#[tauri::command]
async fn desktop_owner_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Option<OwnerProfile>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.owner_profile().await
}

#[tauri::command]
async fn desktop_save_owner_profile(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    profile: OwnerProfile,
    generation: u64,
) -> Result<IntentReceipt, BridgeError> {
    main_window(&window)?;
    lock(&state)?.save_owner_profile(profile, generation).await
}

#[tauri::command]
async fn desktop_pool_summaries(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<PoolSummary>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.pool_summaries().await
}

#[tauri::command]
async fn desktop_account_catalog(
    window: WebviewWindow,
    state: State<'_, NativeState>,
) -> Result<Vec<ProviderEntry>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.account_catalog().await
}

#[tauri::command]
async fn desktop_list_accounts(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    after: Option<String>,
    limit: u16,
) -> Result<AccountPage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.list_accounts(after, limit).await
}

#[tauri::command]
async fn desktop_add_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    draft: AccountDraft,
    key: Option<String>,
) -> Result<AccountView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.add_account(draft, key).await
}

#[tauri::command]
async fn desktop_rename_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    name: String,
) -> Result<AccountView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.rename_account(&id, name).await
}

#[tauri::command]
async fn desktop_remove_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state)?.remove_account(&id).await
}

#[tauri::command]
async fn desktop_check_account(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<AccountCheck, BridgeError> {
    main_window(&window)?;
    lock(&state)?.check_account(&id).await
}

#[tauri::command]
async fn desktop_local_models(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    url: String,
) -> Result<Vec<String>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.local_models(url).await
}

#[tauri::command]
async fn desktop_plugin_skills(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<SkillView>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.plugin_skills(project_id).await
}

#[tauri::command]
async fn desktop_plugin_servers(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: Option<String>,
) -> Result<Vec<ServerView>, BridgeError> {
    main_window(&window)?;
    lock(&state)?.plugin_servers(project_id).await
}

#[tauri::command]
async fn desktop_add_skill(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    scope: Scope,
    folder: String,
) -> Result<SkillView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.add_skill(scope, folder).await
}

#[tauri::command]
async fn desktop_set_skill_enabled(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    enabled: bool,
) -> Result<SkillView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.set_skill_enabled(id, enabled).await
}

#[tauri::command]
async fn desktop_remove_skill(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state)?.remove_skill(id).await
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
    lock(&state)?.add_mcp_server(scope, name, spec, secrets).await
}

#[tauri::command]
async fn desktop_update_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    spec: ServerSpec,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.update_mcp_server(id, spec).await
}

#[tauri::command]
async fn desktop_approve_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    spec: ServerSpec,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.approve_mcp_server(id, spec).await
}

#[tauri::command]
async fn desktop_set_mcp_server_enabled(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    enabled: bool,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.set_mcp_server_enabled(id, enabled).await
}

#[tauri::command]
async fn desktop_remove_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state)?.remove_mcp_server(id).await
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
    lock(&state)?.switch_plugin(project_id, id, on).await
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
    lock(&state)?.set_mcp_secret(id, name, value).await
}

#[tauri::command]
async fn desktop_remove_mcp_secret(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    name: String,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.remove_mcp_secret(id, name).await
}

#[tauri::command]
async fn desktop_check_mcp_server(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
) -> Result<ServerView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.check_mcp_server(id).await
}

#[tauri::command]
async fn desktop_project_plugins(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    project_id: String,
) -> Result<EffectivePlugins, BridgeError> {
    main_window(&window)?;
    lock(&state)?.project_plugins(project_id).await
}

#[tauri::command]
async fn desktop_pick_folder(window: WebviewWindow) -> Result<Option<String>, BridgeError> {
    main_window(&window)?;
    Ok(match bridge::picker::choose(bridge::picker::FOLDER_SCRIPT).await? {
        Pick::Canceled => None,
        Pick::Paths(mut paths) => {
            assert!(!paths.is_empty());
            Some(paths.swap_remove(0))
        }
    })
}

#[tauri::command]
async fn desktop_pick_documents(window: WebviewWindow) -> Result<Vec<String>, BridgeError> {
    main_window(&window)?;
    Ok(match bridge::picker::choose(bridge::picker::DOCUMENTS_SCRIPT).await? {
        Pick::Canceled => Vec::new(),
        Pick::Paths(paths) => paths,
    })
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
    lock(&state)?.reveal_project_folder(&id).await
}

#[tauri::command]
async fn desktop_open_context_item(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    id: String,
    path: String,
) -> Result<(), BridgeError> {
    main_window(&window)?;
    lock(&state)?.open_context_item(&id, &path).await
}

#[tauri::command]
async fn desktop_model_access_page(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    next: bool,
) -> Result<ModelAccessPage, BridgeError> {
    main_window(&window)?;
    lock(&state)?.model_access_page(collection, next).await
}

#[tauri::command]
async fn desktop_inspect_model_access(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    collection: ModelAccessCollection,
    id: String,
) -> Result<ModelAccessDetail, BridgeError> {
    main_window(&window)?;
    lock(&state)?.inspect_model_access(collection, &id).await
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
    lock(&state)?
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
    lock(&state)?.start_native_auth(&ticket, method).await
}

#[tauri::command]
async fn desktop_read_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.read_native_auth(&ticket).await
}

#[tauri::command]
async fn desktop_cancel_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.cancel_native_auth(&ticket).await
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
    let mut bridge = Arc::clone(&state.0)
        .try_lock_owned()
        .map_err(|_| BridgeError { code: "busy" })?;
    tauri::async_runtime::spawn(async move {
        bridge.submit_claude_auth_code(&ticket, secret).await
    })
    .await
    .map_err(|_| BridgeError { code: "native_auth_claude_input_outcome_unknown" })?
}

#[tauri::command]
async fn desktop_logout_native_auth(
    window: WebviewWindow,
    state: State<'_, NativeState>,
    ticket: String,
) -> Result<NativeAuthView, BridgeError> {
    main_window(&window)?;
    lock(&state)?.logout_native_auth(&ticket).await
}

pub fn run() {
    let socket = std::env::var_os("CRAZYAGENT_DESKTOP_SOCKET").map(Into::into);
    let builder = tauri::Builder::default()
        .manage(NativeState(Arc::new(Mutex::new(NativeBridge::new(socket)))))
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
            desktop_team_status,
            desktop_team_message,
            desktop_team_delegate,
            desktop_team_cancel,
            desktop_team_keep_warm,
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
            desktop_pick_folder,
            desktop_pick_documents,
            desktop_open_link,
            desktop_reveal_project_folder,
            desktop_open_context_item,
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
