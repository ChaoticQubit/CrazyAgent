#[rustfmt::skip]
pub(super) async fn route(state: &super::Shared, command: &str, args: serde_json::Value) -> super::Routed {
    use super::{lock, parse, read, routed, Routed};
    use crazyagent_desktop::bridge::BridgeError;
    use crazyagent::accounts::AccountDraft;
    use crazyagent::cleanup_settings::CleanupSettings;
    use crazyagent::config::ModelAccessEdit;
    use crazyagent::coord::settings::CoordSettings;
    use crazyagent::coord::settings::Delivery;
    use crazyagent::coord::settings::HarnessKind;
    use crazyagent::coord::settings::SubagentMode;
    use crazyagent::coord::tool_switches::Switch;
    use crazyagent::engine::owner::ModelAccessCollection;
    use crazyagent::engine::owner::NativeAuthMethod;
    use crazyagent::ide::bridge::run_command::RunMode;
    use crazyagent::learning::MarkStep;
    use crazyagent::master_frame_limit::MasterFrameSettings;
    use crazyagent::master_step_limit::MasterStepSettings;
    use crazyagent::master_time_limit::MasterTimeSettings;
    use crazyagent::memory::ReviewDecision;
    use crazyagent::owner_profile::OwnerProfile;
    use crazyagent::plugins::Scope;
    use crazyagent::plugins::ServerSpec;
    use crazyagent::project_overview::Apply;
    use crazyagent::project_overview::Tab;
    use crazyagent::project_setup::KeptFile;
    use crazyagent::project_setup::ProjectSetup;
    use crazyagent::project_setup::ResearchKind;
    use crazyagent::projects_home::AddMode;
    use crazyagent::projects_home::AddOverride;
    use crazyagent::protocol::ProjectSpec;
    use crazyagent::quit_settings::QuitSettings;
    use crazyagent::roadmap_status::Refresh;
    use crazyagent::sandbox_settings::SandboxSettings;
    use crazyagent::scheduling::ScheduleChange;
    use crazyagent::team::TerminalsPerWindow;
    use crazyagent::usage::money::MoneySettings;
    use crazyagent::usage::spend::SpendSettings;
    match command {
        "desktop_connect" => {
            let result: Result<_, BridgeError> = async { let mut bridge = lock(state).await?; bridge.connect().await }.await;
            routed(result)
        }
        "desktop_status" => {
            let result: Result<_, BridgeError> = async { Ok(read(state).await?.status()) }.await;
            routed(result)
        }
        "desktop_harness_catalog" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                check: Option<bool>,
            }
            let Args { check } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state) .await? .harness_catalog(check.unwrap_or(false)) .await }.await;
            routed(result)
        }
        "desktop_harness_versions" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                look: bool,
            }
            let Args { id, look } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.harness_versions(&id, look).await }.await;
            routed(result)
        }
        "desktop_harness_update" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.harness_update(&id).await }.await;
            routed(result)
        }
        "desktop_harness_install" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.harness_install(&id).await }.await;
            routed(result)
        }
        "desktop_harness_install_many" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ids: Vec<String>,
            }
            let Args { ids } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.harness_install_many(&ids).await }.await;
            routed(result)
        }
        "desktop_harness_queued" => {
            let result: Result<_, BridgeError> = async { read(state).await?.harness_queued().await }.await;
            routed(result)
        }
        "desktop_harness_find" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.harness_find(&id).await }.await;
            routed(result)
        }
        "desktop_harness_choose_path" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                path: Option<String>,
            }
            let Args { id, path } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state) .await? .harness_choose_path(&id, path.as_deref()) .await }.await;
            routed(result)
        }
        "desktop_harness_update_settings" => {
            let result: Result<_, BridgeError> = async { read(state).await?.harness_update_settings().await }.await;
            routed(result)
        }
        "desktop_harness_set_update_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: crazyagent::harnesses::update::UpdateSettings,
            }
            let Args { settings } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state) .await? .harness_set_update_settings(settings) .await }.await;
            routed(result)
        }
        "desktop_baselines" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                next: bool,
            }
            let Args { next } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.baselines(next).await }.await;
            routed(result)
        }
        "desktop_inspect_baseline" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.inspect(&id).await }.await;
            routed(result)
        }
        "desktop_approve_baseline" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.approve(&ticket).await }.await;
            routed(result)
        }
        "desktop_revoke_baseline" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.revoke(&ticket).await }.await;
            routed(result)
        }
        "desktop_reconcile" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.reconcile().await }.await;
            routed(result)
        }
        "desktop_retry" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.retry().await }.await;
            routed(result)
        }
        "desktop_projects" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                next: bool,
            }
            let Args { next } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.projects(next).await }.await;
            routed(result)
        }
        "desktop_project_list" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.project_list().await }.await;
            routed(result)
        }
        "desktop_create_project" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                spec: ProjectSpec,
                generation: u64,
            }
            let Args { spec, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.create_project(spec, generation).await }.await;
            routed(result)
        }
        "desktop_project_catalog" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                pool: Option<String>,
                research: Vec<ResearchKind>,
            }
            let Args { pool, research } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.project_catalog(pool, research).await }.await;
            routed(result)
        }
        "desktop_setup_project" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                spec: ProjectSpec,
                setup: ProjectSetup,
                generation: u64,
            }
            let Args { spec, setup, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .setup_project(spec, setup, generation) .await }.await;
            routed(result)
        }
        "desktop_project_setup" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.project_setup(&id).await }.await;
            routed(result)
        }
        "desktop_research_jobs" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.research_jobs(&id).await }.await;
            routed(result)
        }
        "desktop_project_activity" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                limit: usize,
            }
            let Args { id, limit } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.project_activity(&id, limit).await }.await;
            routed(result)
        }
        "desktop_project_context" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.project_context(&id).await }.await;
            routed(result)
        }
        "desktop_project_context_item" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                path: String,
                offset: u64,
            }
            let Args { id, path, offset } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .project_context_item(&id, path, offset) .await }.await;
            routed(result)
        }
        "desktop_save_research_to_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                research: ResearchKind,
                section: Option<String>,
                generation: u64,
            }
            let Args { id, research, section, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .save_research_to_memory(&id, research, section, generation) .await }.await;
            routed(result)
        }
        "desktop_add_project_documents" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                documents: Vec<String>,
                generation: u64,
            }
            let Args { id, documents, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .add_project_documents(&id, documents, generation) .await }.await;
            routed(result)
        }
        "desktop_project_memories" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                after: Option<String>,
            }
            let Args { id, after } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state) .await? .project_memories(&id, after.as_deref()) .await }.await;
            routed(result)
        }
        "desktop_review_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                memory: String,
                version: u64,
                decision: ReviewDecision,
                generation: u64,
            }
            let Args { id, memory, version, decision, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .review_memory(&id, &memory, version, decision, generation) .await }.await;
            routed(result)
        }
        "desktop_memories" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: Option<String>,
                search: Option<String>,
                before: Option<u64>,
            }
            let Args { project, search, before } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .memories(project.as_deref(), search.as_deref(), before) .await }.await;
            routed(result)
        }
        "desktop_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.memory(&id).await }.await;
            routed(result)
        }
        "desktop_approved_memories" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: String,
            }
            let Args { project } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.approved_memories(&project).await }.await;
            routed(result)
        }
        "desktop_add_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: Option<String>,
                text: String,
                generation: u64,
            }
            let Args { project, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .add_memory(project.as_deref(), text, generation) .await }.await;
            routed(result)
        }
        "desktop_import_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: String,
                folder: String,
                generation: u64,
            }
            let Args { project, folder, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .import_memory(&project, &folder, generation) .await }.await;
            routed(result)
        }
        "desktop_promote_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: String,
                memory: String,
                version: u64,
                generation: u64,
            }
            let Args { project, memory, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .promote_memory(&project, &memory, version, generation) .await }.await;
            routed(result)
        }
        "desktop_delete_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                generation: u64,
            }
            let Args { id, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.delete_memory(&id, generation).await }.await;
            routed(result)
        }
        "desktop_approve_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                generation: u64,
            }
            let Args { id, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.approve_memory(&id, generation).await }.await;
            routed(result)
        }
        "desktop_set_agent_memory_writes" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                writes: crazyagent::memory::library::AgentWrites,
                generation: u64,
            }
            let Args { writes, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_agent_memory_writes(writes, generation) .await }.await;
            routed(result)
        }
        "desktop_move_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                project: String,
                generation: u64,
            }
            let Args { id, project, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .move_memory(&id, &project, generation) .await }.await;
            routed(result)
        }
        "desktop_set_agent_memory_scope" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                scope: crazyagent::memory::library::AgentScope,
                generation: u64,
            }
            let Args { scope, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_agent_memory_scope(scope, generation) .await }.await;
            routed(result)
        }
        "desktop_forget_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: String,
                memory: String,
                version: u64,
                generation: u64,
            }
            let Args { project, memory, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .forget_memory(&project, &memory, version, generation) .await }.await;
            routed(result)
        }
        "desktop_save_selection_to_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                path: String,
                text: String,
                generation: u64,
            }
            let Args { id, path, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .save_selection_to_memory(&id, path, text, generation) .await }.await;
            routed(result)
        }
        "desktop_remember_research" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: String,
                research: ResearchKind,
                section: Option<String>,
                generation: u64,
            }
            let Args { project, research, section, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .remember_research(&project, research, section, generation) .await }.await;
            routed(result)
        }
        "desktop_project_read_paths" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.project_read_paths(&id).await }.await;
            routed(result)
        }
        "desktop_project_hidden_paths" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.project_hidden_paths(&id).await }.await;
            routed(result)
        }
        "desktop_project_agent_policy" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.project_agent_policy(&id).await }.await;
            routed(result)
        }
        "desktop_set_project_agent_policy" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                version: u64,
                policy: crazyagent::sandbox::policy::AgentPolicy,
                generation: u64,
            }
            let Args { id, version, policy, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_project_agent_policy(&id, version, policy, generation) .await }.await;
            routed(result)
        }
        "desktop_project_files" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                query: String,
            }
            let Args { id, query } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { state.project_files(&id, &query).await }.await;
            routed(result)
        }
        "desktop_set_project_read_paths" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                version: u64,
                paths: Vec<String>,
                generation: u64,
            }
            let Args { id, version, paths, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_project_read_paths(&id, version, paths, generation) .await }.await;
            routed(result)
        }
        "desktop_set_project_hidden_paths" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                paths: Vec<String>,
                generation: u64,
            }
            let Args { id, paths, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_project_hidden_paths(&id, paths, generation) .await }.await;
            routed(result)
        }
        "desktop_delete_project_context" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                path: String,
                generation: u64,
            }
            let Args { id, path, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .delete_project_context(&id, path, generation) .await }.await;
            routed(result)
        }
        "desktop_team_status" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.team_status(&id).await }.await;
            routed(result)
        }
        "desktop_team_agent_messages" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                agent: i64,
                before: Option<crazyagent::team::agent_messages::Cursor>,
                limit: Option<u16>,
            }
            let Args { id, agent, before, limit } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state) .await? .team_agent_messages(&id, agent, before, limit) .await }.await;
            routed(result)
        }
        "desktop_team_message_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                settings: crazyagent::team::agent_messages::MessageSettings,
                generation: u64,
            }
            let Args { id, settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_message_settings(&id, settings, generation) .await }.await;
            routed(result)
        }
        "desktop_team_message" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                text: String,
                generation: u64,
            }
            let Args { id, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_message(&id, text, generation) .await }.await;
            routed(result)
        }
        "desktop_team_delegate" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                harness: Option<String>,
                prompt: String,
                generation: u64,
            }
            let Args { id, harness, prompt, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_delegate(&id, harness, prompt, generation) .await }.await;
            routed(result)
        }
        "desktop_team_cancel" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                task: Option<i64>,
                generation: u64,
            }
            let Args { id, task, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.team_cancel(&id, task, generation).await }.await;
            routed(result)
        }
        "desktop_team_keep_warm" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                on: bool,
                version: u64,
                generation: u64,
            }
            let Args { id, on, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_keep_warm(&id, on, version, generation) .await }.await;
            routed(result)
        }
        "desktop_team_time_limits" => {
            #[derive(serde::Deserialize)]
            struct TeamTimeLimits {
                #[serde(rename = "turnSecs")]
                turn_secs: Option<u32>,
                #[serde(rename = "sessionSecs")]
                session_secs: Option<u32>,
                #[serde(rename = "idleSecs")]
                idle_secs: Option<u32>,
            }
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                #[serde(flatten)]
                limits: TeamTimeLimits,
                version: u64,
                generation: u64,
            }
            let Args { id, limits, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let TeamTimeLimits { turn_secs, session_secs, idle_secs, } = limits; lock(state) .await? .team_time_limits(&id, turn_secs, session_secs, idle_secs, version, generation) .await }.await;
            routed(result)
        }
        "desktop_team_on_harness_switch" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: String,
                version: u64,
                generation: u64,
            }
            let Args { id, mode, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_on_harness_switch(&id, &mode, version, generation) .await }.await;
            routed(result)
        }
        "desktop_team_apply_harness_switch" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: String,
                version: u64,
                generation: u64,
            }
            let Args { id, mode, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_apply_harness_switch(&id, &mode, version, generation) .await }.await;
            routed(result)
        }
        "desktop_team_sub_agent_depth" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                depth: u8,
                generation: u64,
            }
            let Args { id, depth, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_sub_agent_depth(&id, depth, generation) .await }.await;
            routed(result)
        }
        "desktop_team_child_landing" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: String,
                generation: u64,
            }
            let Args { id, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_child_landing(&id, &mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_master_branch" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                branch: Option<String>,
                generation: u64,
            }
            let Args { id, branch, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_master_branch(&id, branch, generation) .await }.await;
            routed(result)
        }
        "desktop_team_land_on" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                branch: Option<String>,
                generation: u64,
            }
            let Args { id, branch, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_land_on(&id, branch, generation) .await }.await;
            routed(result)
        }
        "desktop_team_background" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                choice: crazyagent::team::BackgroundAtEnd,
                generation: u64,
            }
            let Args { id, choice, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_background(&id, choice, generation) .await }.await;
            routed(result)
        }
        "desktop_team_leftovers" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: crazyagent::team::LeftoverMode,
                generation: u64,
            }
            let Args { id, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_leftovers(&id, mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_leftover_hold" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                key: u64,
                end: bool,
                generation: u64,
            }
            let Args { id, key, end, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_leftover_hold(&id, key, end, generation) .await }.await;
            routed(result)
        }
        "desktop_team_ask_before_merge" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                on: bool,
                generation: u64,
            }
            let Args { id, on, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_ask_before_merge(&id, on, generation) .await }.await;
            routed(result)
        }
        "desktop_team_master_lands" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: crazyagent::team::MasterLands,
                generation: u64,
            }
            let Args { id, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_master_lands(&id, mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_reask_empty_reply" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                on: bool,
                generation: u64,
            }
            let Args { id, on, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_reask_empty_reply(&id, on, generation) .await }.await;
            routed(result)
        }
        "desktop_team_done_quiet" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                secs: Option<u32>,
                generation: u64,
            }
            let Args { id, secs, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_done_quiet(&id, secs, generation) .await }.await;
            routed(result)
        }
        "desktop_team_catch_up_main" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: String,
                generation: u64,
            }
            let Args { id, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_catch_up_main(&id, &mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_master_idle_stop" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                choice: String,
                generation: u64,
            }
            let Args { id, choice, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_master_idle_stop(&id, &choice, generation) .await }.await;
            routed(result)
        }
        "desktop_team_master_live_terminal" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                on: bool,
                generation: u64,
            }
            let Args { id, on, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_master_live_terminal(&id, on, generation) .await }.await;
            routed(result)
        }
        "desktop_team_reuse_idle" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: String,
                generation: u64,
            }
            let Args { id, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_reuse_idle(&id, &mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_landing_mode" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: String,
                generation: u64,
            }
            let Args { id, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_landing_mode(&id, &mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_new_folder_limit" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                files: u32,
                generation: u64,
            }
            let Args { id, files, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_new_folder_limit(&id, files, generation) .await }.await;
            routed(result)
        }
        "desktop_team_coordination" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                settings: CoordSettings,
                generation: u64,
            }
            let Args { id, settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_coordination(&id, settings, generation) .await }.await;
            routed(result)
        }
        "desktop_team_harness_mode" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                harness: HarnessKind,
                mode: SubagentMode,
                generation: u64,
            }
            let Args { id, harness, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_harness_mode(&id, harness, mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_harness_delivery" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                harness: HarnessKind,
                delivery: Delivery,
                generation: u64,
            }
            let Args { id, harness, delivery, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_harness_delivery(&id, harness, delivery, generation) .await }.await;
            routed(result)
        }
        "desktop_team_agent_branches" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: String,
                generation: u64,
            }
            let Args { id, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_agent_branches(&id, &mode, generation) .await }.await;
            routed(result)
        }
        "desktop_team_finished_agents" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                show: bool,
                version: u64,
                generation: u64,
            }
            let Args { id, show, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_finished_agents(&id, show, version, generation) .await }.await;
            routed(result)
        }
        "desktop_team_live_master" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                on: bool,
                version: u64,
                generation: u64,
            }
            let Args { id, on, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_live_master(&id, on, version, generation) .await }.await;
            routed(result)
        }
        "desktop_team_pause" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                paused: bool,
                generation: u64,
            }
            let Args { id, paused, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_pause(&id, paused, generation) .await }.await;
            routed(result)
        }
        "desktop_team_hand_off" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                reply: i64,
                step: u8,
                generation: u64,
            }
            let Args { id, reply, step, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_hand_off(&id, reply, step, generation) .await }.await;
            routed(result)
        }
        "desktop_team_events" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.team_events().await }.await;
            routed(result)
        }
        "desktop_team_wake_rule" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                target: crazyagent::team::events::RuleTarget,
                rule: Option<crazyagent::team::events::WakeRule>,
                version: u64,
                generation: u64,
            }
            let Args { target, rule, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_wake_rule(target, rule, version, generation) .await }.await;
            routed(result)
        }
        "desktop_team_rename" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                agent: i64,
                handle: String,
                generation: u64,
            }
            let Args { id, agent, handle, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_rename(&id, agent, handle, generation) .await }.await;
            routed(result)
        }
        "desktop_team_stop_agent" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                agent: i64,
                generation: u64,
            }
            let Args { id, agent, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_stop_agent(&id, agent, generation) .await }.await;
            routed(result)
        }
        "desktop_coord_send" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                agent: i64,
                text: String,
                generation: u64,
            }
            let Args { id, agent, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .coord_send(&id, agent, text, generation) .await }.await;
            routed(result)
        }
        "desktop_team_stop_teammate" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                agent: i64,
                pane: String,
                generation: u64,
            }
            let Args { id, agent, pane, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_stop_teammate(&id, agent, pane, generation) .await }.await;
            routed(result)
        }
        "desktop_team_max_harnesses" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                max: Option<u8>,
                generation: u64,
            }
            let Args { id, max, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_max_harnesses(&id, max, generation) .await }.await;
            routed(result)
        }
        "desktop_team_max_teammates" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                max: Option<u8>,
                generation: u64,
            }
            let Args { id, max, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_max_teammates(&id, max, generation) .await }.await;
            routed(result)
        }
        "desktop_team_master_rounds" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                max: Option<u8>,
                generation: u64,
            }
            let Args { id, max, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_master_rounds(&id, max, generation) .await }.await;
            routed(result)
        }
        "desktop_team_master_run_time" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                secs: Option<u32>,
                generation: u64,
            }
            let Args { id, secs, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_master_run_time(&id, secs, generation) .await }.await;
            routed(result)
        }
        "desktop_team_token_budget" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                tokens: Option<u64>,
                generation: u64,
            }
            let Args { id, tokens, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_token_budget(&id, tokens, generation) .await }.await;
            routed(result)
        }
        "desktop_team_spread_accounts" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                on: bool,
                generation: u64,
            }
            let Args { id, on, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_spread_accounts(&id, on, generation) .await }.await;
            routed(result)
        }
        "desktop_team_wake_agent" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                agent: i64,
                generation: u64,
            }
            let Args { id, agent, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_wake_agent(&id, agent, generation) .await }.await;
            routed(result)
        }
        "desktop_team_seen" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                upto: i64,
                generation: u64,
            }
            let Args { id, upto, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.team_seen(&id, upto, generation).await }.await;
            routed(result)
        }
        "desktop_coord_questions" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.coord_questions(&id).await }.await;
            routed(result)
        }
        "desktop_coord_answer" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                message: i64,
                text: String,
                generation: u64,
            }
            let Args { id, message, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .coord_answer(&id, message, text, generation) .await }.await;
            routed(result)
        }
        "desktop_team_graph" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.team_graph(&id).await }.await;
            routed(result)
        }
        "desktop_team_terminals_per_window" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                per_window: TerminalsPerWindow,
                generation: u64,
            }
            let Args { id, per_window, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_terminals_per_window(&id, per_window, generation) .await }.await;
            routed(result)
        }
        "desktop_terminals_per_window" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.terminals_per_window(&id).await }.await;
            routed(result)
        }
        "desktop_update_project_setup" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                setup: ProjectSetup,
                name: Option<String>,
                revision: Option<u64>,
                generation: u64,
                kept: Option<Vec<KeptFile>>,
            }
            let Args { id, setup, name, revision, generation, kept } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .update_project_setup(&id, setup, name, revision, kept, generation) .await }.await;
            routed(result)
        }
        "desktop_move_project_home" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                mode: AddMode,
                overrides: Vec<AddOverride>,
                generation: u64,
            }
            let Args { id, mode, overrides, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .move_project_home(&id, mode, overrides, generation) .await }.await;
            routed(result)
        }
        "desktop_remove_project" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                generation: u64,
                delete_name: Option<String>,
            }
            let Args { id, generation, delete_name } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .remove_project(&id, generation, delete_name) .await }.await;
            routed(result)
        }
        "desktop_project_demo" => {
            let result: Result<_, BridgeError> = async { read(state).await?.project_demo().await }.await;
            routed(result)
        }
        "desktop_create_demo" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                pool: String,
                generation: u64,
            }
            let Args { pool, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.create_demo(pool, generation).await }.await;
            routed(result)
        }
        "desktop_project_removal" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.removal_preview(id)?; call.await }.await;
            routed(result)
        }
        "desktop_cancel_research" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                job: i64,
                generation: u64,
            }
            let Args { id, job, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .cancel_research(&id, job, generation) .await }.await;
            routed(result)
        }
        "desktop_retry_research" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                job: i64,
                generation: u64,
            }
            let Args { id, job, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .retry_research(&id, job, generation) .await }.await;
            routed(result)
        }
        "desktop_plan_roadmap" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                generation: u64,
            }
            let Args { id, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.plan_roadmap(&id, generation).await }.await;
            routed(result)
        }
        "desktop_roadmap_status" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = read(state).await?.roadmap_status(id)?; call.await }.await;
            routed(result)
        }
        "desktop_refresh_roadmap_status" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.refresh_roadmap_status(id)?; call.await }.await;
            routed(result)
        }
        "desktop_set_roadmap_refresh" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                refresh: String,
            }
            let Args { id, refresh } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.set_roadmap_refresh(id, refresh)?; call.await }.await;
            routed(result)
        }
        "desktop_research_log" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                job: i64,
            }
            let Args { id, job } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.research_log(&id, job).await }.await;
            routed(result)
        }
        "desktop_projects_home" => {
            let result: Result<_, BridgeError> = async { read(state).await?.projects_home().await }.await;
            routed(result)
        }
        "desktop_set_projects_home" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                path: String,
                generation: u64,
            }
            let Args { path, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_projects_home(path, generation) .await }.await;
            routed(result)
        }
        "desktop_move_projects_home" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                path: String,
                generation: u64,
            }
            let Args { path, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .move_projects_home(path, generation) .await }.await;
            routed(result)
        }
        "desktop_found_projects" => {
            let result: Result<_, BridgeError> = async { read(state).await?.found_projects().await }.await;
            routed(result)
        }
        "desktop_folder_size" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                path: String,
            }
            let Args { path } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.folder_size(path).await }.await;
            routed(result)
        }
        "desktop_add_existing_folder" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                source: String,
                mode: AddMode,
                overrides: Vec<AddOverride>,
                generation: u64,
            }
            let Args { source, mode, overrides, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .add_existing_folder(source, mode, overrides, generation) .await }.await;
            routed(result)
        }
        "desktop_start_onboarding" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                path: String,
                pool: String,
                generation: u64,
            }
            let Args { path, pool, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .start_onboarding(path, pool, generation) .await }.await;
            routed(result)
        }
        "desktop_onboarding" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.onboarding(&id).await }.await;
            routed(result)
        }
        "desktop_github_status" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.github_status()?; call.await }.await;
            routed(result)
        }
        "desktop_github_sign_in" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.github_sign_in()?; call.await }.await;
            routed(result)
        }
        "desktop_github_token" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                token: String,
            }
            let Args { token } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.github_token(token)?; call.await }.await;
            routed(result)
        }
        "desktop_github_use_cli" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.github_use_cli()?; call.await }.await;
            routed(result)
        }
        "desktop_github_sign_out" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.github_sign_out()?; call.await }.await;
            routed(result)
        }
        "desktop_github_repos" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                page: u32,
            }
            let Args { page } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.github_repos(page)?; call.await }.await;
            routed(result)
        }
        "desktop_clone_repo" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                full_name: String,
            }
            let Args { full_name } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.clone_repo(full_name)?; call.await }.await;
            routed(result)
        }
        "desktop_icloud_download" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                path: String,
            }
            let Args { path } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.icloud_download(path)?; call.await }.await;
            routed(result)
        }
        "desktop_icloud_progress" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.icloud_progress()?; call.await }.await;
            routed(result)
        }
        "desktop_icloud_cancel" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.icloud_cancel()?; call.await }.await;
            routed(result)
        }
        "desktop_setup_status" => {
            let result: Result<_, BridgeError> = async { read(state).await?.setup_status().await }.await;
            routed(result)
        }
        "desktop_run_setup" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                pack: Vec<String>,
                generation: u64,
            }
            let Args { pack, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.run_setup(pack, generation).await }.await;
            routed(result)
        }
        "desktop_install_dependency" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                name: crazyagent::setup::Dependency,
                generation: u64,
            }
            let Args { name, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .install_dependency(name, generation) .await }.await;
            routed(result)
        }
        "desktop_fix_tool_permissions" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                name: crazyagent::setup::Dependency,
                tree: String,
                generation: u64,
            }
            let Args { name, tree, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .fix_tool_permissions(name, tree, generation) .await }.await;
            routed(result)
        }
        "desktop_set_refused_tool" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                choice: crazyagent::setup::RefusedTool,
                generation: u64,
            }
            let Args { choice, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_refused_tool(choice, generation) .await }.await;
            routed(result)
        }
        "desktop_set_pack_entry" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                on: bool,
                version: u64,
                generation: u64,
            }
            let Args { id, on, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_pack_entry(id, on, version, generation) .await }.await;
            routed(result)
        }
        "desktop_remove_pack_entry" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                version: u64,
                generation: u64,
            }
            let Args { id, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .remove_pack_entry(id, version, generation) .await }.await;
            routed(result)
        }
        "desktop_open_project" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.open_project(&id).await }.await;
            routed(result)
        }
        "desktop_history" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                next: bool,
            }
            let Args { next } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.history(next).await }.await;
            routed(result)
        }
        "desktop_send_message" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                text: String,
                generation: u64,
            }
            let Args { ticket, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .send_message(&ticket, &text, generation) .await }.await;
            routed(result)
        }
        "desktop_master_state" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.master_state(&ticket).await }.await;
            routed(result)
        }
        "desktop_master_turns" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                cursor: Option<crazyagent::master_session::MasterCursor>,
            }
            let Args { ticket, session_id, cursor } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .master_turns(&ticket, &session_id, cursor) .await }.await;
            routed(result)
        }
        "desktop_open_master" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                generation: u64,
            }
            let Args { ticket, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .open_default_master(&ticket, generation) .await }.await;
            routed(result)
        }
        "desktop_close_master" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                generation: u64,
            }
            let Args { ticket, session_id, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .close_master(&ticket, &session_id, generation) .await }.await;
            routed(result)
        }
        "desktop_master_sessions" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.master_sessions(&ticket).await }.await;
            routed(result)
        }
        "desktop_master_inputs" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                cursors: Vec<String>,
            }
            let Args { ticket, session_id, cursors } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .master_inputs(&ticket, &session_id, &cursors) .await }.await;
            routed(result)
        }
        "desktop_master_messages" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                before: Option<u64>,
                limit: u8,
            }
            let Args { ticket, session_id, before, limit } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .master_messages(&ticket, &session_id, before, limit) .await }.await;
            routed(result)
        }
        "desktop_master_session_turns" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                cursor: Option<crazyagent::master_session::MasterCursor>,
            }
            let Args { ticket, session_id, cursor } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .session_turns(&ticket, &session_id, cursor) .await }.await;
            routed(result)
        }
        "desktop_new_master_session" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                cancel_running: bool,
                generation: u64,
            }
            let Args { ticket, cancel_running, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .new_master_session(&ticket, cancel_running, generation) .await }.await;
            routed(result)
        }
        "desktop_resume_master_session" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                cancel_running: bool,
                generation: u64,
            }
            let Args { ticket, session_id, cancel_running, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .resume_master_session(&ticket, &session_id, cancel_running, generation) .await }.await;
            routed(result)
        }
        "desktop_delete_master_session" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                generation: u64,
            }
            let Args { ticket, session_id, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .delete_master_session(&ticket, &session_id, generation) .await }.await;
            routed(result)
        }
        "desktop_cancel_master_turn" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session: String,
                turn: String,
                generation: u64,
            }
            let Args { ticket, session, turn, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .cancel_master_turn(&ticket, &session, &turn, generation) .await }.await;
            routed(result)
        }
        "desktop_submit_master" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                session_id: String,
                text: String,
                generation: u64,
            }
            let Args { ticket, session_id, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .submit_master(&ticket, &session_id, &text, generation) .await }.await;
            routed(result)
        }
        "desktop_replace_brief" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                text: String,
                generation: u64,
            }
            let Args { ticket, text, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .replace_brief(&ticket, &text, generation) .await }.await;
            routed(result)
        }
        "desktop_pending_intent" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.pending_intent() }.await;
            routed(result)
        }
        "desktop_reconcile_intent" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.reconcile_intent().await }.await;
            routed(result)
        }
        "desktop_retry_intent" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.retry_intent().await }.await;
            routed(result)
        }
        "desktop_drop_intent" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.drop_intent() }.await;
            routed(result)
        }
        "desktop_prepare_managed_account" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                provider: String,
                label: String,
            }
            let Args { provider, label } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .prepare_managed_account(&provider, &label) .await }.await;
            routed(result)
        }
        "desktop_prepare_configured_account" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                provider: String,
                label: String,
                endpoint: String,
                model: String,
            }
            let Args { provider, label, endpoint, model } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .prepare_configured_account(&provider, &label, &endpoint, &model) .await }.await;
            routed(result)
        }
        "desktop_account_auth_capabilities" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.account_auth_capabilities(&ticket).await }.await;
            routed(result)
        }
        "desktop_read_account_profile" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.read_account_profile().await }.await;
            routed(result)
        }
        "desktop_owner_profile" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.owner_profile().await }.await;
            routed(result)
        }
        "desktop_save_owner_profile" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                profile: OwnerProfile,
                generation: u64,
            }
            let Args { profile, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .save_owner_profile(profile, generation) .await }.await;
            routed(result)
        }
        "desktop_sandbox_settings" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.sandbox_settings().await }.await;
            routed(result)
        }
        "desktop_set_sandbox_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: SandboxSettings,
                generation: u64,
            }
            let Args { settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_sandbox_settings(settings, generation) .await }.await;
            routed(result)
        }
        "desktop_cleanup_settings" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.cleanup_settings().await }.await;
            routed(result)
        }
        "desktop_set_cleanup_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: CleanupSettings,
                generation: u64,
            }
            let Args { settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_cleanup_settings(settings, generation) .await }.await;
            routed(result)
        }
        "desktop_quit_settings" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.quit_settings().await }.await;
            routed(result)
        }
        "desktop_set_quit_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: QuitSettings,
                generation: u64,
            }
            let Args { settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_quit_settings(settings, generation) .await }.await;
            routed(result)
        }
        "desktop_learning_progress" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.learning_progress().await }.await;
            routed(result)
        }
        "desktop_set_learning_mark" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                step: MarkStep,
                on: bool,
                generation: u64,
            }
            let Args { step, on, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_learning_mark(step, on, generation) .await }.await;
            routed(result)
        }
        "desktop_master_step_settings" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.master_step_settings().await }.await;
            routed(result)
        }
        "desktop_set_master_step_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: MasterStepSettings,
                generation: u64,
            }
            let Args { settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_master_step_settings(settings, generation) .await }.await;
            routed(result)
        }
        "desktop_master_time_settings" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.master_time_settings().await }.await;
            routed(result)
        }
        "desktop_set_master_time_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: MasterTimeSettings,
                generation: u64,
            }
            let Args { settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_master_time_settings(settings, generation) .await }.await;
            routed(result)
        }
        "desktop_master_frame_settings" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.master_frame_settings().await }.await;
            routed(result)
        }
        "desktop_set_master_frame_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: MasterFrameSettings,
                generation: u64,
            }
            let Args { settings, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_master_frame_settings(settings, generation) .await }.await;
            routed(result)
        }
        "desktop_pool_summaries" => {
            let result: Result<_, BridgeError> = async { read(state).await?.pool_summaries().await }.await;
            routed(result)
        }
        "desktop_account_schedule" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.account_schedule().await }.await;
            routed(result)
        }
        "desktop_account_ranking" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.account_ranking().await }.await;
            routed(result)
        }
        "desktop_set_account_schedule" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                change: ScheduleChange,
                version: u64,
                generation: u64,
            }
            let Args { change, version, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_account_schedule(change, version, generation) .await }.await;
            routed(result)
        }
        "desktop_account_catalog" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.account_catalog().await }.await;
            routed(result)
        }
        "desktop_list_accounts" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                after: Option<String>,
                limit: u16,
            }
            let Args { after, limit } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.list_accounts(after, limit).await }.await;
            routed(result)
        }
        "desktop_add_account" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                draft: AccountDraft,
                key: Option<String>,
            }
            let Args { draft, key } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.add_account(draft, key).await }.await;
            routed(result)
        }
        "desktop_rename_account" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                name: String,
            }
            let Args { id, name } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.rename_account(&id, name).await }.await;
            routed(result)
        }
        "desktop_remove_account" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_account(&id).await }.await;
            routed(result)
        }
        "desktop_check_account" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.check_account(&id).await }.await;
            routed(result)
        }
        "desktop_local_models" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                url: String,
            }
            let Args { url } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.local_models(url).await }.await;
            routed(result)
        }
        "desktop_plugin_skills" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.plugin_skills(project_id).await }.await;
            routed(result)
        }
        "desktop_plugin_servers" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.plugin_servers(project_id).await }.await;
            routed(result)
        }
        "desktop_plugin_hooks" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { read(state).await?.plugin_hooks(project_id).await }.await;
            routed(result)
        }
        "desktop_add_hooks" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                scope: Scope,
                folder: String,
            }
            let Args { scope, folder } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.add_hooks(scope, folder).await }.await;
            routed(result)
        }
        "desktop_set_hook_enabled" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                enabled: bool,
            }
            let Args { id, enabled } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.set_hook_enabled(id, enabled).await }.await;
            routed(result)
        }
        "desktop_add_skill" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                scope: Scope,
                folder: String,
            }
            let Args { scope, folder } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.add_skill(scope, folder).await }.await;
            routed(result)
        }
        "desktop_set_skill_enabled" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                enabled: bool,
                version: u64,
            }
            let Args { id, enabled, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_skill_enabled(id, enabled, version) .await }.await;
            routed(result)
        }
        "desktop_skill_detail" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                project_id: Option<String>,
            }
            let Args { id, project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.skill_detail(id, project_id).await }.await;
            routed(result)
        }
        "desktop_skill_import_scan" => {
            let result: Result<_, BridgeError> = async { let call = read(state).await?.skill_import_scan()?; call.await }.await;
            routed(result)
        }
        "desktop_import_skill" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                folder: String,
                replace: Option<String>,
            }
            let Args { folder, replace } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = read(state).await?.import_skill(folder, replace)?; call.await }.await;
            routed(result)
        }
        "desktop_remove_skill" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                version: u64,
            }
            let Args { id, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_skill(id, version).await }.await;
            routed(result)
        }
        "desktop_add_mcp_server" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                scope: Scope,
                name: String,
                spec: ServerSpec,
                secrets: Vec<(String, String)>,
            }
            let Args { scope, name, spec, secrets } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = read(state) .await? .add_mcp_server(scope, name, spec, secrets)?; call.await }.await;
            routed(result)
        }
        "desktop_update_mcp_server" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                spec: ServerSpec,
                version: u64,
            }
            let Args { id, spec, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .update_mcp_server(id, spec, version) .await }.await;
            routed(result)
        }
        "desktop_approve_mcp_server" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                spec: ServerSpec,
            }
            let Args { id, spec } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.approve_mcp_server(id, spec).await }.await;
            routed(result)
        }
        "desktop_set_mcp_server_enabled" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                enabled: bool,
                version: u64,
            }
            let Args { id, enabled, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_mcp_server_enabled(id, enabled, version) .await }.await;
            routed(result)
        }
        "desktop_remove_mcp_server" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                version: u64,
            }
            let Args { id, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_mcp_server(id, version).await }.await;
            routed(result)
        }
        "desktop_switch_plugin" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                id: String,
                on: bool,
                version: u64,
            }
            let Args { project_id, id, on, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .switch_plugin(project_id, id, on, version) .await }.await;
            routed(result)
        }
        "desktop_set_mcp_secret" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                name: String,
                value: String,
            }
            let Args { id, name, value } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.set_mcp_secret(id, name, value).await }.await;
            routed(result)
        }
        "desktop_remove_mcp_secret" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                name: String,
            }
            let Args { id, name } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_mcp_secret(id, name).await }.await;
            routed(result)
        }
        "desktop_check_mcp_server" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.check_mcp_server(id)?; call.await }.await;
            routed(result)
        }
        "desktop_project_plugins" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.project_plugins(project_id).await }.await;
            routed(result)
        }
        "desktop_tool_switches" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
                server_id: Option<String>,
            }
            let Args { project_id, server_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .tool_switches(project_id, server_id) .await }.await;
            routed(result)
        }
        "desktop_set_tool_switch" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
                server_id: Option<String>,
                tool: String,
                value: Switch,
            }
            let Args { project_id, server_id, tool, value } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_tool_switch(project_id, server_id, tool, value) .await }.await;
            routed(result)
        }
        "desktop_restore_tool_switches" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
                server_id: Option<String>,
            }
            let Args { project_id, server_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .restore_tool_switches(project_id, server_id) .await }.await;
            routed(result)
        }
        "desktop_agents" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.agents(project_id).await }.await;
            routed(result)
        }
        "desktop_stop_agent" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.stop_agent(id)?; call.await }.await;
            routed(result)
        }
        "desktop_account_use" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                account_id: String,
            }
            let Args { account_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.account_use(account_id).await }.await;
            routed(result)
        }
        "desktop_release_account" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                account_id: String,
                generation: u64,
            }
            let Args { account_id, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let stop = lock(state) .await? .stop_account_agents(account_id.clone())?; let holders = stop.await?; lock(state).await?.end_holders(holders, generation).await?; let wait = lock(state).await?.await_account_free(account_id)?; wait.await }.await;
            routed(result)
        }
        "desktop_interrupt_agent" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.interrupt_agent(id)?; call.await }.await;
            routed(result)
        }
        "desktop_restart_agent" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.restart_agent(id)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_report" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
                since_ms: Option<i64>,
                offset_minutes: i32,
            }
            let Args { project_id, since_ms, offset_minutes } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .usage_report(project_id, since_ms, offset_minutes)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_metrics" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
                since_ms: i64,
            }
            let Args { project_id, since_ms } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.usage_metrics(project_id, since_ms)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_extras" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                detail: bool,
            }
            let Args { detail } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.usage_extras(detail)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_refresh" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.usage_refresh()?; call.await }.await;
            routed(result)
        }
        "desktop_usage_notices" => {
            let result: Result<_, BridgeError> = async { Ok(crazyagent::usage::THIRD_PARTY_NOTICES) }.await;
            routed(result)
        }
        "desktop_usage_set_money" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: MoneySettings,
            }
            let Args { settings } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.usage_set_money(settings)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_spend" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                account: String,
                confirmed: bool,
            }
            let Args { account, confirmed } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.usage_spend(account, confirmed)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_set_spend" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: SpendSettings,
            }
            let Args { settings } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.usage_set_spend(settings)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_decide_recount" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                use_new: bool,
            }
            let Args { use_new } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.usage_decide_recount(use_new)?; call.await }.await;
            routed(result)
        }
        "desktop_ide_run" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                command: String,
            }
            let Args { project_id, command } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.ide_run(project_id, command)?; call.await }.await;
            routed(result)
        }
        "desktop_ide_run_mode" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.ide_run_mode(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_ide_set_run_mode" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                mode: RunMode,
            }
            let Args { project_id, mode } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.ide_set_run_mode(project_id, mode)?; call.await }.await;
            routed(result)
        }
        "desktop_overview_read" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.overview_read(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_overview_refresh" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.overview_refresh(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_overview_save" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                tab: Tab,
                text: String,
                version: String,
            }
            let Args { project_id, tab, text, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .overview_save(project_id, tab, text, version)?; call.await }.await;
            routed(result)
        }
        "desktop_overview_accept" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                tab: Tab,
                version: String,
                proposal_version: String,
            }
            let Args { project_id, tab, version, proposal_version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .overview_accept(project_id, tab, version, proposal_version)?; call.await }.await;
            routed(result)
        }
        "desktop_overview_reject" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                tab: Tab,
            }
            let Args { project_id, tab } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.overview_reject(project_id, tab)?; call.await }.await;
            routed(result)
        }
        "desktop_overview_set_refresh" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                refresh: Refresh,
            }
            let Args { project_id, refresh } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .overview_set_refresh(project_id, refresh)?; call.await }.await;
            routed(result)
        }
        "desktop_overview_set_apply" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                apply: Apply,
            }
            let Args { project_id, apply } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.overview_set_apply(project_id, apply)?; call.await }.await;
            routed(result)
        }
        "desktop_usage_set_reset" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                account: String,
                window_kind: String,
                at_ms: Option<i64>,
            }
            let Args { account, window_kind, at_ms } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .usage_set_reset(account, window_kind, at_ms)?; call.await }.await;
            routed(result)
        }
        "desktop_ide_extension_approve" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                id: String,
                version: String,
            }
            let Args { project_id, id, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .ide_extension_approve(project_id, id, version)?; call.await }.await;
            routed(result)
        }
        "desktop_ide_extension_decline" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                id: String,
                version: String,
            }
            let Args { project_id, id, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .ide_extension_decline(project_id, id, version)?; call.await }.await;
            routed(result)
        }
        "desktop_finished_agents" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.finished_agents(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_remove_finished_agents" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                agents: Vec<i64>,
            }
            let Args { project_id, agents } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .remove_finished_agents(project_id, agents)?; call.await }.await;
            routed(result)
        }
        "desktop_changes" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.change_sets(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_change_diff" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                head: String,
                page: u16,
            }
            let Args { id, head, page } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.change_diff(id, head, page)?; call.await }.await;
            routed(result)
        }
        "desktop_accept_change" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                expected_head: String,
                message: Option<String>,
                target: Option<String>,
            }
            let Args { id, expected_head, message, target } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .accept_change(id, expected_head, message, target)?; call.await }.await;
            routed(result)
        }
        "desktop_change_branches" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.change_branches(id)?; call.await }.await;
            routed(result)
        }
        "desktop_change_owner_dirty" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                target: Option<String>,
            }
            let Args { id, target } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.change_owner_dirty(id, target)?; call.await }.await;
            routed(result)
        }
        "desktop_commit_owner_files" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                files: Vec<String>,
                message: String,
                target: Option<String>,
            }
            let Args { id, files, message, target } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .commit_owner_files(id, files, message, target)?; call.await }.await;
            routed(result)
        }
        "desktop_discard_change" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                expected_head: String,
            }
            let Args { id, expected_head } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.discard_change(id, expected_head)?; call.await }.await;
            routed(result)
        }
        "desktop_restore_change" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.restore_change(id)?; call.await }.await;
            routed(result)
        }
        "desktop_undo_change" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.undo_change(id)?; call.await }.await;
            routed(result)
        }
        "desktop_update_change" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.update_change(id)?; call.await }.await;
            routed(result)
        }
        "desktop_questions" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                after: Option<u64>,
            }
            let Args { project_id, after } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = read(state).await?.questions(project_id, after)?; call.await }.await;
            routed(result)
        }
        "desktop_answer_question" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: u64,
                version: u64,
                picks: Vec<u8>,
                comment: Option<String>,
            }
            let Args { id, version, picks, comment } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .answer_question(id, version, picks, comment)?; call.await }.await;
            routed(result)
        }
        "desktop_dismiss_question" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: u64,
                version: u64,
            }
            let Args { id, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.dismiss_question(id, version)?; call.await }.await;
            routed(result)
        }
        "desktop_dismiss_questions" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                items: Vec<(u64, u64)>,
            }
            let Args { items } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.dismiss_questions(items)?; call.await }.await;
            routed(result)
        }
        "desktop_question_counts" => {
            let result: Result<_, BridgeError> = async { let call = read(state).await?.question_counts()?; call.await }.await;
            routed(result)
        }
        "desktop_process_text" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.process_text(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_save_process_text" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
                text: String,
                version: u64,
                opened: Option<u64>,
            }
            let Args { project_id, text, version, opened } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .save_process_text(project_id, text, version, opened)?; call.await }.await;
            routed(result)
        }
        "desktop_ide_state" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                retry: bool,
            }
            let Args { retry } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = read(state).await?.ide_state(retry)?; call.await }.await;
            routed(result)
        }
        "desktop_instruction_import" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.instruction_import(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_confirm_instruction_import" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.confirm_instruction_import(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_uncommitted_instruction_import" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .uncommitted_instruction_import(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_commit_instruction_import" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.commit_instruction_import(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_agent_instructions" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                agent: Option<i64>,
            }
            let Args { project_id, agent } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.agent_instructions(project_id, agent)?; call.await }.await;
            routed(result)
        }
        "desktop_recovery_banner" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.recovery_banner(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_instruction_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.instruction_settings(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_process_propose" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: Option<String>,
                pool: Option<String>,
            }
            let Args { project_id, pool } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.propose_process(project_id, pool)?; call.await }.await;
            routed(result)
        }
        "desktop_app_instruction_settings" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.app_instruction_settings()?; call.await }.await;
            routed(result)
        }
        "desktop_save_app_instruction_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                settings: crazyagent::engine::owner::AppInstructionSettings,
            }
            let Args { settings } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .save_app_instruction_settings(settings)?; call.await }.await;
            routed(result)
        }
        "desktop_retry_recovery" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                agent: i64,
            }
            let Args { agent } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.retry_recovery(agent)?; call.await }.await;
            routed(result)
        }
        "desktop_dismiss_recovery" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                action_kind: crazyagent::ledger::ActionKind,
                id: i64,
            }
            let Args { action_kind, id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.dismiss_recovery(action_kind, id)?; call.await }.await;
            routed(result)
        }
        "desktop_adoption" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.adoption(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_save_instruction_settings" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                settings: crazyagent::engine::owner::InstructionSettings,
                version: Option<u64>,
            }
            let Args { project_id, settings, version } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state) .await? .save_instruction_settings(project_id, settings, version)?; call.await }.await;
            routed(result)
        }
        "desktop_default_process" => {
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.default_process()?; call.await }.await;
            routed(result)
        }
        "desktop_process_given" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
            }
            let Args { project_id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { let call = lock(state).await?.given_text(project_id)?; call.await }.await;
            routed(result)
        }
        "desktop_reveal_project_folder" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.reveal_project_folder(&id).await }.await;
            routed(result)
        }
        "desktop_open_context_item" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
                path: String,
            }
            let Args { id, path } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.open_context_item(&id, &path).await }.await;
            routed(result)
        }
        "desktop_model_access_page" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                collection: ModelAccessCollection,
                next: bool,
            }
            let Args { collection, next } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .model_access_page(collection, next) .await }.await;
            routed(result)
        }
        "desktop_inspect_model_access" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                collection: ModelAccessCollection,
                id: String,
            }
            let Args { collection, id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .inspect_model_access(collection, &id) .await }.await;
            routed(result)
        }
        "desktop_edit_model_access" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                edit: ModelAccessEdit,
                generation: u64,
            }
            let Args { ticket, edit, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .edit_model_access(&ticket, edit, generation) .await }.await;
            routed(result)
        }
        "desktop_start_native_auth" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
                method: NativeAuthMethod,
            }
            let Args { ticket, method } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.start_native_auth(&ticket, method).await }.await;
            routed(result)
        }
        "desktop_read_native_auth" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.read_native_auth(&ticket).await }.await;
            routed(result)
        }
        "desktop_cancel_native_auth" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.cancel_native_auth(&ticket).await }.await;
            routed(result)
        }
        "desktop_clear_native_auth" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.clear_native_auth().await }.await;
            routed(result)
        }
        "desktop_logout_native_auth" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                ticket: String,
            }
            let Args { ticket } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.logout_native_auth(&ticket).await }.await;
            routed(result)
        }
        _ => Routed::Unknown,
    }
}
