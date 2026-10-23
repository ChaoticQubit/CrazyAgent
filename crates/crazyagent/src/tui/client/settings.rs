use super::accounts::{AccountsReply, AccountsRequest};
use super::*;
use crate::accounts::AccountView;
use crate::config::ModelAccessEdit;
use crate::engine::EngineError;
use crate::engine::owner::{
    AccountSecret, ClaudeAuthInput, ModelAccessCollection, ModelAccessRecord, NativeAuthAccount,
    NativeAuthMethod, NativeAuthPhase, NativeAuthStart,
};
use crate::harnesses::HarnessEntry;
use crate::memory::library::{
    LIBRARY_ITEMS_MAX, MemoryScope, PAGE_ITEMS_MAX, SCOPE_ITEMS_MAX, SavedMemory,
};
use crate::plugins::{Scope, ServerSpec, ServerView, SkillView};
use crate::protocol::CredentialRef;

const POOL_JOINS_MAX: usize = 64;
const DISPLAY_BYTES_MAX: usize = 4096;
/// Accounts read per engine page when every account is loaded.
pub(in crate::tui) const ACCOUNTS_PAGE: u16 = 200;
/// Most accounts one full load returns; the rest stay in the engine.
pub(in crate::tui) const ACCOUNTS_LOADED_MAX: usize = 5000;

/// One Settings request the settings screens send through the shell.
/// Input: built by the Accounts, Add account, Harnesses and Memory screens.
/// Output: run on the owner worker; the reply arrives as `OwnerEvent::Settings`.
pub(in crate::tui) enum SettingsJob {
    Accounts(AccountsRequest),
    AllAccounts,
    Harnesses,
    SignIn(SignInJob),
    JoinPools {
        account: CredentialRef,
        pools: Vec<(CredentialRef, u64)>,
    },
    Memories {
        scopes: Vec<MemoryScope>,
    },
    DeleteMemory {
        memory: RequestId,
        generation: u64,
    },
    Plugins,
    ProjectPlugins {
        project: ProjectId,
    },
    Plugin(PluginChange),
}

/// One change to skills and MCP servers, from Settings or from a project's
/// settings. Switch turns an every-project item on or off for one project.
/// Input: built by the plugins pages. Output: one plugins call on the engine.
pub(in crate::tui) enum PluginChange {
    AddSkill {
        scope: Scope,
        folder: String,
    },
    SetSkill {
        id: String,
        enabled: bool,
    },
    RemoveSkill {
        id: String,
    },
    AddServer {
        scope: Scope,
        name: String,
        spec: ServerSpec,
        secrets: Vec<(String, AccountSecret)>,
    },
    Approve {
        id: String,
        spec: ServerSpec,
    },
    SetServer {
        id: String,
        enabled: bool,
    },
    RemoveServer {
        id: String,
    },
    CheckServer {
        id: String,
    },
    Update {
        id: String,
        spec: ServerSpec,
    },
    SetSecret {
        id: String,
        name: String,
        value: AccountSecret,
    },
    RemoveSecret {
        id: String,
        name: String,
    },
    Switch {
        project: ProjectId,
        id: String,
        on: bool,
    },
}

impl std::fmt::Debug for SettingsJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Accounts(_) => "Accounts",
            Self::AllAccounts => "AllAccounts",
            Self::Harnesses => "Harnesses",
            Self::SignIn(_) => "SignIn",
            Self::JoinPools { .. } => "JoinPools",
            Self::Memories { .. } => "Memories",
            Self::DeleteMemory { .. } => "DeleteMemory",
            Self::Plugins => "Plugins",
            Self::ProjectPlugins { .. } => "ProjectPlugins",
            Self::Plugin(_) => "Plugin",
        })
    }
}

/// One step of a managed Codex or Claude sign-in for an account.
/// Input: the account to sign in and its native method, or the ticket of a
/// running sign-in. Output: the engine's fresh sign-in state.
pub(in crate::tui) enum SignInJob {
    Start {
        account: CredentialRef,
        method: NativeAuthMethod,
    },
    Read {
        ticket: SignInTicket,
    },
    Cancel {
        ticket: SignInTicket,
    },
    ClaudeCode {
        ticket: SignInTicket,
        generation: u64,
        prompt: u64,
        code: AccountSecret,
    },
}

/// The engine identity of one running sign-in, kept so later reads and the
/// cancel address the same operation.
/// Input: returned with every sign-in reply. Output: passed back unchanged.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::tui) struct SignInTicket {
    scope: crate::engine::owner::NativeAuthScope,
    operation: RequestId,
}

impl std::fmt::Debug for SignInTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SignInTicket")
    }
}

/// What a sign-in step shows, in plain parts only.
/// Input: built from the engine's native sign-in reply. Output: the phase,
/// the page and code to show, and who it signed in as.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::tui) struct SignInView {
    pub phase: NativeAuthPhase,
    pub problem: Option<String>,
    pub generation: u64,
    pub page: Option<String>,
    pub device_code: Option<String>,
    pub claude_prompt: Option<u64>,
    pub email: Option<String>,
    pub plan: Option<String>,
}

impl std::fmt::Debug for SignInView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignInView")
            .field("phase", &self.phase)
            .field("problem", &self.problem)
            .finish_non_exhaustive()
    }
}

/// The engine's reply to one Settings job.
/// Input: produced on the owner worker. Output: read by the settings screens.
pub(in crate::tui) enum SettingsEvent {
    Accounts(Result<AccountsReply, String>),
    AllAccounts(Result<Vec<AccountView>, String>),
    Harnesses(Result<Vec<HarnessEntry>, String>),
    SignIn(Result<(SignInTicket, SignInView), String>),
    PoolsJoined(Result<usize, String>),
    Memories(Result<Vec<SavedMemory>, String>),
    MemoryDeleted(Result<(), String>),
    Plugins(Result<(Vec<SkillView>, Vec<ServerView>), String>),
    ProjectPlugins(ProjectId, Result<(Vec<SkillView>, Vec<ServerView>), String>),
    PluginChanged(Result<Option<ServerView>, String>),
}

impl SettingsEvent {
    /// The failure event for a job that reached no trusted connection.
    /// Input: the job. Output: its event carrying the plain problem code.
    pub(in crate::tui) fn unavailable(job: &SettingsJob) -> Self {
        let problem = "trusted_connection_unavailable".to_owned();
        match job {
            SettingsJob::Accounts(_) => Self::Accounts(Err(problem)),
            SettingsJob::AllAccounts => Self::AllAccounts(Err(problem)),
            SettingsJob::Harnesses => Self::Harnesses(Err(problem)),
            SettingsJob::SignIn(_) => Self::SignIn(Err(problem)),
            SettingsJob::JoinPools { .. } => Self::PoolsJoined(Err(problem)),
            SettingsJob::Memories { .. } => Self::Memories(Err(problem)),
            SettingsJob::DeleteMemory { .. } => Self::MemoryDeleted(Err(problem)),
            SettingsJob::Plugins => Self::Plugins(Err(problem)),
            SettingsJob::ProjectPlugins { project } => {
                Self::ProjectPlugins(project.clone(), Err(problem))
            }
            SettingsJob::Plugin(_) => Self::PluginChanged(Err(problem)),
        }
    }
}

impl Connection {
    /// Queues one Settings job on the paired owner worker.
    /// Input: the job. Output: queued, or why not while another job runs.
    pub(in crate::tui) fn settings(&mut self, job: SettingsJob) -> Result<(), String> {
        self.owner_job(Job::Settings(Box::new(job)))
    }
}

fn shown(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= DISPLAY_BYTES_MAX && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

impl SignInView {
    /// Builds the plain view of one native sign-in reply.
    /// Input: the reply. Output: phase, page, device code, Claude prompt and
    /// the signed-in email and plan when the engine observed them.
    pub(in crate::tui) fn of(reply: &crate::engine::owner::NativeAuthReply) -> Self {
        let mut view = Self {
            phase: reply.fact.phase,
            problem: reply.fact.code.as_deref().map(bounded_problem),
            generation: reply.fact.generation,
            page: None,
            device_code: None,
            claude_prompt: None,
            email: None,
            plan: None,
        };
        if let Some(challenge) = &reply.challenge {
            match challenge.codex() {
                crate::adapters::codex::LoginChallenge::Browser { auth_url, .. } => {
                    view.page = shown(auth_url.expose());
                }
                crate::adapters::codex::LoginChallenge::DeviceCode {
                    user_code,
                    verification_url,
                    ..
                } => {
                    view.page = shown(verification_url.expose());
                    view.device_code = shown(user_code.expose());
                }
            }
        }
        if let Some(challenge) = &reply.claude_challenge {
            view.page = shown(challenge.url());
            view.claude_prompt = challenge.prompt_id();
        }
        match &reply.account {
            Some(NativeAuthAccount::Codex(crate::adapters::codex::AuthAccount::Chatgpt {
                email,
                plan_type,
            })) => {
                view.email = email.as_ref().and_then(|email| shown(email.expose()));
                view.plan = shown(plan_type);
            }
            Some(NativeAuthAccount::Claude(status)) => {
                let field = |field: &crate::adapters::claude::AuthStatusField| match field {
                    crate::adapters::claude::AuthStatusField::Value(text) => shown(text.as_str()),
                    _ => None,
                };
                view.email = field(&status.email);
                view.plan = field(&status.subscription_type);
            }
            _ => {}
        }
        view
    }
}

impl NativeOwner {
    /// Runs one Settings job through the owner controller.
    /// Input: the job. Output: the event; engine errors become plain codes and
    /// a transport failure detaches the connection so the shell reconnects.
    pub(super) async fn settings(&mut self, job: SettingsJob) -> SettingsEvent {
        let mut lost = false;
        let event = self.settings_event(job, &mut lost).await;
        if lost {
            self.detach();
        }
        event
    }

    async fn settings_event(&mut self, job: SettingsJob, lost: &mut bool) -> SettingsEvent {
        match job {
            SettingsJob::Accounts(request) => {
                SettingsEvent::Accounts(self.accounts(request).await.result)
            }
            SettingsJob::AllAccounts => {
                SettingsEvent::AllAccounts(self.all_accounts(ACCOUNTS_PAGE, lost).await)
            }
            SettingsJob::Harnesses => SettingsEvent::Harnesses(match self.settings_controller() {
                Ok(controller) => controller
                    .harness_catalog()
                    .await
                    .map_err(|error| problem(&error, lost)),
                Err(error) => Err(error),
            }),
            SettingsJob::SignIn(job) => SettingsEvent::SignIn(self.sign_in(job, lost).await),
            SettingsJob::JoinPools { account, pools } => {
                SettingsEvent::PoolsJoined(self.join_pools(account, pools, lost).await)
            }
            SettingsJob::Memories { scopes } => {
                SettingsEvent::Memories(self.all_memories(scopes, lost).await)
            }
            SettingsJob::Plugins => SettingsEvent::Plugins(self.plugins(None, lost).await),
            SettingsJob::ProjectPlugins { project } => SettingsEvent::ProjectPlugins(
                project.clone(),
                self.plugins(Some(project), lost).await,
            ),
            SettingsJob::Plugin(change) => {
                SettingsEvent::PluginChanged(self.plugin_change(change, lost).await)
            }
            SettingsJob::DeleteMemory { memory, generation } => {
                SettingsEvent::MemoryDeleted(match self.settings_controller() {
                    Ok(controller) => match controller.delete_memory(memory, generation).await {
                        Ok(OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }) => {
                            Ok(())
                        }
                        Ok(OwnerResponse::Rejected { code }) => Err(bounded_problem(&code)),
                        Ok(_) => Err("memory_outcome_unknown".into()),
                        Err(error) => Err(problem(&error, lost)),
                    },
                    Err(error) => Err(error),
                })
            }
        }
    }

    /// Reads the skills and MCP servers added for every project, or the ones one
    /// project's agents can get with whether each every-project item is on there.
    /// Inputs: the project, if any, and the lost-connection flag. Output: both
    /// lists, or the plain problem code.
    pub(super) async fn plugins(
        &mut self,
        project: Option<ProjectId>,
        lost: &mut bool,
    ) -> Result<(Vec<SkillView>, Vec<ServerView>), String> {
        let controller = self.settings_controller()?;
        let skills = controller
            .plugin_skills(project.clone())
            .await
            .map_err(|error| problem(&error, lost))?;
        let servers = controller
            .plugin_servers(project)
            .await
            .map_err(|error| problem(&error, lost))?;
        Ok((skills, servers))
    }

    /// Runs one plugins change.
    /// Inputs: the change and the lost-connection flag. Output: the changed MCP
    /// server when the change was about one, or the plain problem code.
    pub(super) async fn plugin_change(
        &mut self,
        change: PluginChange,
        lost: &mut bool,
    ) -> Result<Option<ServerView>, String> {
        let controller = self.settings_controller()?;
        let done = match change {
            PluginChange::AddSkill { scope, folder } => {
                controller.add_skill(scope, folder).await.map(|_| None)
            }
            PluginChange::SetSkill { id, enabled } => controller
                .set_skill_enabled(id, enabled)
                .await
                .map(|_| None),
            PluginChange::RemoveSkill { id } => controller.remove_skill(id).await.map(|()| None),
            PluginChange::AddServer {
                scope,
                name,
                spec,
                secrets,
            } => controller
                .add_mcp_server(scope, name, spec, secrets)
                .await
                .map(Some),
            PluginChange::Approve { id, spec } => {
                controller.approve_mcp_server(id, spec).await.map(Some)
            }
            PluginChange::SetServer { id, enabled } => controller
                .set_mcp_server_enabled(id, enabled)
                .await
                .map(Some),
            PluginChange::RemoveServer { id } => {
                controller.remove_mcp_server(id).await.map(|()| None)
            }
            PluginChange::CheckServer { id } => controller.check_mcp_server(id).await.map(Some),
            PluginChange::Update { id, spec } => {
                controller.update_mcp_server(id, spec).await.map(Some)
            }
            PluginChange::SetSecret { id, name, value } => {
                controller.set_mcp_secret(id, name, value).await.map(Some)
            }
            PluginChange::RemoveSecret { id, name } => {
                controller.remove_mcp_secret(id, name).await.map(Some)
            }
            PluginChange::Switch { project, id, on } => controller
                .switch_plugin(project, id, on)
                .await
                .map(|()| None),
        };
        done.map_err(|error| problem(&error, lost))
    }

    /// Reads the saved memories of several scopes, newest first overall.
    /// Inputs: the scopes and the lost-connection flag. Output: at most the
    /// library's limit of memories, or the first plain problem code.
    pub(super) async fn all_memories(
        &mut self,
        scopes: Vec<MemoryScope>,
        lost: &mut bool,
    ) -> Result<Vec<SavedMemory>, String> {
        let mut memories: Vec<SavedMemory> = Vec::new();
        for scope in scopes {
            if memories.len() >= LIBRARY_ITEMS_MAX {
                break;
            }
            memories.extend(self.saved_memories(scope, lost).await?);
        }
        memories.sort_by_key(|memory| std::cmp::Reverse(memory.ordinal));
        memories.truncate(LIBRARY_ITEMS_MAX);
        Ok(memories)
    }

    /// Reads every saved memory in one scope, newest first, page by page.
    /// Inputs: the scope and the lost-connection flag. Output: at most one
    /// scope's worth of memories, or the plain problem code.
    pub(super) async fn saved_memories(
        &mut self,
        scope: MemoryScope,
        lost: &mut bool,
    ) -> Result<Vec<SavedMemory>, String> {
        let controller = self.settings_controller()?;
        let mut memories: Vec<SavedMemory> = Vec::new();
        let mut before = None;
        let pages_max = SCOPE_ITEMS_MAX.div_ceil(usize::from(PAGE_ITEMS_MAX)) + 1;
        for _ in 0..pages_max {
            let page = controller
                .memories(scope.clone(), None, before)
                .await
                .map_err(|error| problem(&error, lost))?;
            let room = SCOPE_ITEMS_MAX - memories.len();
            memories.extend(page.items.into_iter().take(room));
            assert!(memories.len() <= SCOPE_ITEMS_MAX);
            match page.next {
                Some(next) if memories.len() < SCOPE_ITEMS_MAX => before = Some(next),
                _ => return Ok(memories),
            }
        }
        Err("memory_pages_unbounded".into())
    }

    /// Reads every account page by page, stopping at the load limit.
    /// Inputs: the page size and the lost-connection flag. Output: the
    /// accounts in engine order, or the plain problem code.
    pub(super) async fn all_accounts(
        &mut self,
        page: u16,
        lost: &mut bool,
    ) -> Result<Vec<AccountView>, String> {
        assert!(page > 0);
        let mut accounts: Vec<AccountView> = Vec::new();
        let mut after = None;
        let pages_max = ACCOUNTS_LOADED_MAX.div_ceil(usize::from(page)) + 1;
        for _ in 0..pages_max {
            let read = match self
                .accounts(AccountsRequest::List { after, limit: page })
                .await
                .result
            {
                Ok(AccountsReply::Page(read)) => read,
                Ok(_) => return Err("account_reply_unexpected".into()),
                Err(code) => {
                    *lost |= connection_lost(&code);
                    return Err(code);
                }
            };
            let room = ACCOUNTS_LOADED_MAX - accounts.len();
            accounts.extend(read.accounts.into_iter().take(room));
            assert!(accounts.len() <= ACCOUNTS_LOADED_MAX);
            match read.next {
                Some(next) if accounts.len() < ACCOUNTS_LOADED_MAX => after = Some(next),
                _ => return Ok(accounts),
            }
        }
        Err("account_pages_unbounded".into())
    }

    fn settings_controller(&mut self) -> Result<&mut OwnerController, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        self.controller
            .as_mut()
            .ok_or_else(|| "trusted_connection_unavailable".into())
    }

    async fn sign_in(
        &mut self,
        job: SignInJob,
        lost: &mut bool,
    ) -> Result<(SignInTicket, SignInView), String> {
        let controller = self.settings_controller()?;
        let (ticket, reply) = match job {
            SignInJob::Start { account, method } => {
                let read = controller
                    .read(OwnerRead::ModelAccessRecord {
                        collection: ModelAccessCollection::Connections,
                        id: account.clone(),
                    })
                    .await
                    .map_err(|error| problem(&error, lost))?;
                let OwnerData::ModelAccessRecord {
                    source_hash,
                    writable,
                    record: ModelAccessRecord::Connection { .. },
                } = read.data
                else {
                    return Err("native_auth_connection_missing".into());
                };
                if !writable {
                    return Err("model_access_read_only".into());
                }
                let scope = crate::engine::owner::NativeAuthScope {
                    expected_database_instance: controller.identity().database_instance().into(),
                    connection: account,
                    expected_config_id: read.config_id,
                    expected_source_hash: source_hash,
                };
                let request_id = fresh_id()?;
                let ticket = SignInTicket {
                    scope: scope.clone(),
                    operation: request_id.clone(),
                };
                let reply = controller
                    .start_native_auth(NativeAuthStart {
                        request_id,
                        scope,
                        method,
                    })
                    .await;
                (ticket, reply)
            }
            SignInJob::Read { ticket } => {
                let reply = controller
                    .read_native_auth(crate::engine::owner::NativeAuthQuery {
                        request_id: fresh_id()?,
                        scope: ticket.scope.clone(),
                        operation_id: ticket.operation.clone(),
                    })
                    .await;
                (ticket, reply)
            }
            SignInJob::Cancel { ticket } => {
                let reply = controller
                    .cancel_native_auth(crate::engine::owner::NativeAuthCancel {
                        request_id: fresh_id()?,
                        scope: ticket.scope.clone(),
                        operation_id: ticket.operation.clone(),
                    })
                    .await;
                (ticket, reply)
            }
            SignInJob::ClaudeCode {
                ticket,
                generation,
                prompt,
                code,
            } => {
                let query = crate::engine::owner::NativeAuthQuery {
                    request_id: fresh_id()?,
                    scope: ticket.scope.clone(),
                    operation_id: ticket.operation.clone(),
                };
                controller
                    .submit_claude_auth_code(
                        ClaudeAuthInput {
                            query,
                            generation,
                            prompt_id: prompt,
                        },
                        code,
                    )
                    .await
                    .map_err(|_| "native_auth_claude_input_outcome_unknown".to_owned())?;
                let reply = controller
                    .read_native_auth(crate::engine::owner::NativeAuthQuery {
                        request_id: fresh_id()?,
                        scope: ticket.scope.clone(),
                        operation_id: ticket.operation.clone(),
                    })
                    .await;
                (ticket, reply)
            }
        };
        let reply = reply.map_err(|error| bounded_problem(&error.0))?;
        Ok((ticket, SignInView::of(&reply)))
    }

    async fn join_pools(
        &mut self,
        account: CredentialRef,
        pools: Vec<(CredentialRef, u64)>,
        lost: &mut bool,
    ) -> Result<usize, String> {
        if pools.len() > POOL_JOINS_MAX {
            return Err("pool_join_bounds".into());
        }
        let controller = self.settings_controller()?;
        let mut joined = 0;
        for (pool_id, generation) in pools {
            if controller.pending().is_some() {
                return Err("controller_pending_intent".into());
            }
            let read = controller
                .read(OwnerRead::ModelAccessRecord {
                    collection: ModelAccessCollection::Pools,
                    id: pool_id,
                })
                .await
                .map_err(|error| problem(&error, lost))?;
            let OwnerData::ModelAccessRecord {
                source_hash,
                writable,
                record: ModelAccessRecord::Pool { mut pool },
            } = read.data
            else {
                return Err("pool_read_failed".into());
            };
            if !writable {
                return Err("model_access_read_only".into());
            }
            if pool.members.contains(&account) {
                joined += 1;
                continue;
            }
            if pool.members.len() >= crate::config::POOL_MEMBERS_MAX {
                return Err("pool_member_limit".into());
            }
            pool.members.push(account.clone());
            controller
                .begin_mutation(
                    OwnerMutation::ModelAccess {
                        expected_config_id: read.config_id,
                        expected_source_hash: source_hash,
                        edit: ModelAccessEdit::ReplacePool { pool },
                    },
                    generation,
                )
                .map_err(|error| bounded_problem(&error.0))?;
            match controller.send_pending().await {
                Ok(OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }) => joined += 1,
                Ok(OwnerResponse::Rejected { code }) => return Err(bounded_problem(&code)),
                Ok(_) => return Err("pool_outcome_unknown".into()),
                Err(error) => return Err(problem(&error, lost)),
            }
        }
        Ok(joined)
    }
}

fn fresh_id() -> Result<RequestId, String> {
    let id = crate::engine::owner::random_id().map_err(|error| bounded_problem(&error.0))?;
    RequestId::new(id).map_err(|_| "native_auth_request_id_invalid".to_owned())
}

fn connection_lost(code: &str) -> bool {
    code == "controller_disconnected" || code.contains("transport")
}

fn problem(error: &EngineError, lost: &mut bool) -> String {
    if connection_lost(&error.0) {
        *lost = true;
    }
    bounded_problem(&error.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountDraft, Provider, SignInMethod};
    use crate::engine::bootstrap::EngineService;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_loads_read_every_page_and_delete_reaches_the_engine() {
        let root = tempfile::Builder::new()
            .prefix("ca-tset-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let mut native = NativeOwner::new(root.path().join("e.sock"), Duration::from_secs(5));
        let _ = native.attach().await;
        assert!(native.ready);

        for name in ["Laptop", "Mini", "Studio"] {
            let draft = AccountDraft {
                provider: Provider::Local,
                method: SignInMethod::Url,
                name: name.into(),
                url: None,
                model: Some("qwen2.5:0.5b".into()),
            };
            let added = native
                .settings(SettingsJob::Accounts(AccountsRequest::Add {
                    draft,
                    key: None,
                }))
                .await;
            assert!(matches!(
                added,
                SettingsEvent::Accounts(Ok(AccountsReply::Account(_)))
            ));
        }
        let mut lost = false;
        let accounts = native.all_accounts(2, &mut lost).await.unwrap();
        assert!(!lost);
        let mut names: Vec<&str> = accounts
            .iter()
            .map(|account| account.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["Laptop", "Mini", "Studio"]);

        let written = native
            .controller
            .as_mut()
            .unwrap()
            .add_memory(
                MemoryScope::Global,
                "Use metric units in every recipe.".into(),
                1,
            )
            .await
            .unwrap();
        assert!(matches!(
            written,
            OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
        ));
        let SettingsEvent::Memories(Ok(memories)) = native
            .settings(SettingsJob::Memories {
                scopes: vec![MemoryScope::Global],
            })
            .await
        else {
            panic!("memories required")
        };
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].text, "Use metric units in every recipe.");
        let memory = memories[0].id.clone();
        let deleted = native
            .settings(SettingsJob::DeleteMemory {
                memory: memory.clone(),
                generation: 2,
            })
            .await;
        assert!(matches!(deleted, SettingsEvent::MemoryDeleted(Ok(()))));
        let SettingsEvent::Memories(Ok(memories)) = native
            .settings(SettingsJob::Memories {
                scopes: vec![MemoryScope::Global],
            })
            .await
        else {
            panic!("memories required")
        };
        assert!(memories.is_empty());
        let again = native
            .settings(SettingsJob::DeleteMemory {
                memory,
                generation: 3,
            })
            .await;
        assert!(matches!(again, SettingsEvent::MemoryDeleted(Err(_))));

        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    fn changed(event: SettingsEvent) {
        match event {
            SettingsEvent::PluginChanged(Ok(None)) => {}
            SettingsEvent::PluginChanged(Err(code)) => panic!("plugin change refused: {code}"),
            _ => panic!("a plugin change reply without a server required"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_project_reads_its_plugins_and_switches_an_every_project_skill_off() {
        let root = tempfile::Builder::new()
            .prefix("ca-tplug-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let mut native = NativeOwner::new(root.path().join("e.sock"), Duration::from_secs(5));
        let _ = native.attach().await;
        assert!(native.ready);

        let project = ProjectId::new("tidepool").unwrap();
        let created = native
            .controller
            .as_mut()
            .unwrap()
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Tidepool".into(),
                    repositories: vec![],
                },
                crate::project_setup::ProjectSetup {
                    about: "Live dashboard for sensor buoys.".into(),
                    tech_stack: vec![],
                    platforms: vec![crate::project_setup::Platform::Website],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                1,
            )
            .await
            .unwrap();
        assert!(matches!(
            created,
            OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
        ));
        let folder = root.path().join("picked/release");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            "---\nname: release-notes\ndescription: Teaches release notes.\n---\n\nSteps.\n",
        )
        .unwrap();
        let added = native
            .settings(SettingsJob::Plugin(PluginChange::AddSkill {
                scope: Scope::Global,
                folder: folder.display().to_string(),
            }))
            .await;
        changed(added);

        let read = |event: SettingsEvent| match event {
            SettingsEvent::ProjectPlugins(read, Ok((skills, servers))) => {
                assert_eq!(read.as_str(), "tidepool");
                assert!(servers.is_empty());
                skills
                    .into_iter()
                    .map(|skill| (skill.name, skill.on_here))
                    .collect::<Vec<_>>()
            }
            SettingsEvent::ProjectPlugins(_, Err(code)) => panic!("project plugins failed: {code}"),
            _ => panic!("project plugins required"),
        };
        let here = read(
            native
                .settings(SettingsJob::ProjectPlugins {
                    project: project.clone(),
                })
                .await,
        );
        assert_eq!(here, vec![("release-notes".to_owned(), Some(true))]);
        let SettingsEvent::Plugins(Ok((skills, _))) = native.settings(SettingsJob::Plugins).await
        else {
            panic!("every-project plugins required")
        };
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].on_here, None, "Settings shows no project switch");

        let switched = native
            .settings(SettingsJob::Plugin(PluginChange::Switch {
                project: project.clone(),
                id: skills[0].id.clone(),
                on: false,
            }))
            .await;
        changed(switched);
        let here = read(
            native
                .settings(SettingsJob::ProjectPlugins {
                    project: project.clone(),
                })
                .await,
        );
        assert_eq!(here, vec![("release-notes".to_owned(), Some(false))]);
        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}
