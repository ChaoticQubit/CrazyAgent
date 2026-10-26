#[rustfmt::skip]
pub(super) async fn route(state: &super::Shared, command: &str, args: serde_json::Value) -> super::Routed {
    use super::{lock, parse, routed, Routed};
    use crazyagent_desktop::bridge::BridgeError;
    use crazyagent::accounts::AccountDraft;
    use crazyagent::config::ModelAccessEdit;
    use crazyagent::engine::owner::ModelAccessCollection;
    use crazyagent::engine::owner::NativeAuthMethod;
    use crazyagent::memory::ReviewDecision;
    use crazyagent::owner_profile::OwnerProfile;
    use crazyagent::plugins::Scope;
    use crazyagent::plugins::ServerSpec;
    use crazyagent::project_setup::ProjectSetup;
    use crazyagent::project_setup::ResearchKind;
    use crazyagent::projects_home::AddMode;
    use crazyagent::protocol::ProjectSpec;
    match command {
        "desktop_connect" => {
            let result: Result<_, BridgeError> = async { let mut bridge = lock(state).await?; bridge.connect().await }.await;
            routed(result)
        }
        "desktop_status" => {
            let result: Result<_, BridgeError> = async { Ok(lock(state).await?.status()) }.await;
            routed(result)
        }
        "desktop_harness_catalog" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.harness_catalog().await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.project_setup(&id).await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.research_jobs(&id).await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.project_context(&id).await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state) .await? .project_memories(&id, after.as_deref()) .await }.await;
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
        "desktop_forget_memory" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project: String,
                memory: String,
                generation: u64,
            }
            let Args { project, memory, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .forget_memory(&project, &memory, generation) .await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.team_status(&id).await }.await;
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
                generation: u64,
            }
            let Args { id, on, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .team_keep_warm(&id, on, generation) .await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.team_graph(&id).await }.await;
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
            }
            let Args { id, setup, name, revision, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .update_project_setup(&id, setup, name, revision, generation) .await }.await;
            routed(result)
        }
        "desktop_remove_project" => {
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
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_project(&id, generation).await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.projects_home().await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.found_projects().await }.await;
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
                generation: u64,
            }
            let Args { source, mode, generation } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .add_existing_folder(source, mode, generation) .await }.await;
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
        "desktop_setup_status" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.setup_status().await }.await;
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
        "desktop_set_pack_entry" => {
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
            let result: Result<_, BridgeError> = async { lock(state).await?.set_pack_entry(id, on, generation).await }.await;
            routed(result)
        }
        "desktop_remove_pack_entry" => {
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
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_pack_entry(id, generation).await }.await;
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
        "desktop_pool_summaries" => {
            let result: Result<_, BridgeError> = async { lock(state).await?.pool_summaries().await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.list_accounts(after, limit).await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.plugin_skills(project_id).await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state).await?.plugin_servers(project_id).await }.await;
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
            }
            let Args { id, enabled } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.set_skill_enabled(id, enabled).await }.await;
            routed(result)
        }
        "desktop_remove_skill" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_skill(id).await }.await;
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
            let result: Result<_, BridgeError> = async { lock(state) .await? .add_mcp_server(scope, name, spec, secrets) .await }.await;
            routed(result)
        }
        "desktop_update_mcp_server" => {
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
            let result: Result<_, BridgeError> = async { lock(state).await?.update_mcp_server(id, spec).await }.await;
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
            }
            let Args { id, enabled } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state) .await? .set_mcp_server_enabled(id, enabled) .await }.await;
            routed(result)
        }
        "desktop_remove_mcp_server" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                id: String,
            }
            let Args { id } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.remove_mcp_server(id).await }.await;
            routed(result)
        }
        "desktop_switch_plugin" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Args {
                project_id: String,
                id: String,
                on: bool,
            }
            let Args { project_id, id, on } = match parse(args) {
                Ok(args) => args,
                Err(invalid) => return invalid,
            };
            let result: Result<_, BridgeError> = async { lock(state).await?.switch_plugin(project_id, id, on).await }.await;
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
            let result: Result<_, BridgeError> = async {
                let stop = lock(state).await?.stop_account_agents(account_id.clone())?;
                let holders = stop.await?;
                lock(state).await?.end_holders(holders, generation).await?;
                let wait = lock(state).await?.await_account_free(account_id)?;
                wait.await
            }
            .await;
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
