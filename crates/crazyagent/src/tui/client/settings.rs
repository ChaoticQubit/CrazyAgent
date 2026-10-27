use super::accounts::{AccountsReply, AccountsRequest};
use super::app_setup::{AppSetupEvent, AppSetupJob};
use super::*;
use crate::accounts::AccountView;
use crate::config::ModelAccessEdit;
use crate::engine::EngineError;
use crate::engine::owner::{
    AccountSecret, ClaudeAuthInput, ModelAccessCollection, ModelAccessRecord, NativeAuthAccount,
    NativeAuthMethod, NativeAuthPhase, NativeAuthStart, OwnerResult,
};
use crate::harnesses::HarnessEntry;
use crate::memory::library::{
    LIBRARY_ITEMS_MAX, MemoryScope, PAGE_ITEMS_MAX, SCOPE_ITEMS_MAX, SavedMemory,
};
use crate::memory::{KnowledgeStatus, ReviewDecision};
use crate::plugins::{Scope, ServerSpec, ServerView, SkillView};
use crate::protocol::CredentialRef;
use crate::usage::report::UsageReport;

const POOL_JOINS_MAX: usize = 64;
const SUGGESTION_PROJECTS_MAX: usize = 50;
/// Most research suggestions the Memory page lists at once.
pub(in crate::tui) const SUGGESTIONS_MAX: usize = 200;
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
    Usage {
        since_ms: Option<i64>,
        offset_minutes: i32,
    },
    RefreshUsage,
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
    AddMemory {
        scope: MemoryScope,
        text: String,
        generation: u64,
    },
    Suggestions {
        projects: Vec<ProjectId>,
    },
    Suggestion {
        suggestion: Suggestion,
        keep: bool,
        generations: [u64; 2],
    },
    Plugins,
    ProjectPlugins {
        project: ProjectId,
    },
    SkillDetail {
        id: String,
    },
    ImportScan,
    ImportSkills {
        picks: Vec<(String, Option<String>)>,
    },
    Plugin(PluginChange),
    AppSetup(AppSetupJob),
    Schedule,
    SetSchedule {
        change: crate::scheduling::ScheduleChange,
        generation: u64,
    },
    Ranking,
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
            Self::Usage { .. } => "Usage",
            Self::RefreshUsage => "RefreshUsage",
            Self::SignIn(_) => "SignIn",
            Self::JoinPools { .. } => "JoinPools",
            Self::Memories { .. } => "Memories",
            Self::DeleteMemory { .. } => "DeleteMemory",
            Self::AddMemory { .. } => "AddMemory",
            Self::Suggestions { .. } => "Suggestions",
            Self::Suggestion { .. } => "Suggestion",
            Self::Plugins => "Plugins",
            Self::ProjectPlugins { .. } => "ProjectPlugins",
            Self::SkillDetail { .. } => "SkillDetail",
            Self::ImportScan => "ImportScan",
            Self::ImportSkills { .. } => "ImportSkills",
            Self::Plugin(_) => "Plugin",
            Self::AppSetup(job) => job.name(),
            Self::Schedule => "Schedule",
            Self::SetSchedule { .. } => "SetSchedule",
            Self::Ranking => "Ranking",
        })
    }
}

/// A research memory for the owner to act on: a suggestion to save for every
/// project or dismiss, or one kept in its project that can be used in all.
/// Input: read from a project's memory inbox. Output: the text shown, whether
/// it is kept already and the ids the change needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Suggestion {
    pub project: ProjectId,
    pub entry: RequestId,
    pub version: u64,
    pub text: String,
    pub kept: bool,
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
/// Each ticked folder with its imported skill or the plain problem code, in order.
pub(in crate::tui) type ImportOutcomes =
    Vec<(String, Result<crate::skills::import::Imported, String>)>;

/// Input: produced on the owner worker. Output: read by the settings screens.
pub(in crate::tui) enum SettingsEvent {
    Accounts(Result<AccountsReply, String>),
    AllAccounts(Result<Vec<AccountView>, String>),
    Harnesses(Result<Vec<HarnessEntry>, String>),
    Usage(Result<Box<UsageReport>, String>),
    UsageRefreshed(Result<(), String>),
    SignIn(Result<(SignInTicket, SignInView), String>),
    PoolsJoined(Result<usize, String>),
    Memories(Result<Vec<SavedMemory>, String>),
    MemoryDeleted(Result<(), String>),
    MemoryAdded(Result<(), String>),
    Suggestions(Result<Vec<Suggestion>, String>),
    SuggestionDone(Suggestion, Result<(), String>),
    Plugins(Result<(Vec<SkillView>, Vec<ServerView>), String>),
    ProjectPlugins(ProjectId, Result<(Vec<SkillView>, Vec<ServerView>), String>),
    SkillDetail(Result<crate::skills::SkillDetail, String>),
    ImportScan(Result<crate::skills::import::ImportList, String>),
    Imported(Result<ImportOutcomes, String>),
    PluginChanged(Result<Option<ServerView>, String>),
    AppSetup(AppSetupEvent),
    Schedule(Result<crate::scheduling::Schedule, String>),
    ScheduleSet(Result<(), String>),
    Ranking(Result<Vec<crate::scheduling::PoolRanking>, String>),
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
            SettingsJob::Usage { .. } => Self::Usage(Err(problem)),
            SettingsJob::RefreshUsage => Self::UsageRefreshed(Err(problem)),
            SettingsJob::SignIn(_) => Self::SignIn(Err(problem)),
            SettingsJob::JoinPools { .. } => Self::PoolsJoined(Err(problem)),
            SettingsJob::Memories { .. } => Self::Memories(Err(problem)),
            SettingsJob::DeleteMemory { .. } => Self::MemoryDeleted(Err(problem)),
            SettingsJob::AddMemory { .. } => Self::MemoryAdded(Err(problem)),
            SettingsJob::Suggestions { .. } => Self::Suggestions(Err(problem)),
            SettingsJob::Suggestion { suggestion, .. } => {
                Self::SuggestionDone(suggestion.clone(), Err(problem))
            }
            SettingsJob::Plugins => Self::Plugins(Err(problem)),
            SettingsJob::ProjectPlugins { project } => {
                Self::ProjectPlugins(project.clone(), Err(problem))
            }
            SettingsJob::SkillDetail { .. } => Self::SkillDetail(Err(problem)),
            SettingsJob::ImportScan => Self::ImportScan(Err(problem)),
            SettingsJob::ImportSkills { .. } => Self::Imported(Err(problem)),
            SettingsJob::Plugin(_) => Self::PluginChanged(Err(problem)),
            SettingsJob::AppSetup(job) => Self::AppSetup(AppSetupEvent::failed(job, problem)),
            SettingsJob::Schedule => Self::Schedule(Err(problem)),
            SettingsJob::SetSchedule { .. } => Self::ScheduleSet(Err(problem)),
            SettingsJob::Ranking => Self::Ranking(Err(problem)),
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
                let result = self.accounts(request).await;
                if let Err(code) = &result {
                    *lost |= connection_lost(code);
                }
                SettingsEvent::Accounts(result)
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
            SettingsJob::Usage {
                since_ms,
                offset_minutes,
            } => SettingsEvent::Usage(match self.settings_controller() {
                Ok(controller) => controller
                    .usage_report(since_ms, offset_minutes)
                    .await
                    .map(Box::new)
                    .map_err(|error| problem(&error, lost)),
                Err(error) => Err(error),
            }),
            SettingsJob::RefreshUsage => {
                SettingsEvent::UsageRefreshed(match self.settings_controller() {
                    Ok(controller) => controller
                        .usage_refresh()
                        .await
                        .map_err(|error| problem(&error, lost)),
                    Err(error) => Err(error),
                })
            }
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
            SettingsJob::SkillDetail { id } => {
                SettingsEvent::SkillDetail(match self.settings_controller() {
                    Ok(controller) => controller
                        .skill_detail(id, None)
                        .await
                        .map_err(|error| problem(&error, lost)),
                    Err(error) => Err(error),
                })
            }
            SettingsJob::ImportScan => {
                SettingsEvent::ImportScan(match self.settings_controller() {
                    Ok(controller) => controller
                        .skill_import_scan()
                        .await
                        .map_err(|error| problem(&error, lost)),
                    Err(error) => Err(error),
                })
            }
            SettingsJob::ImportSkills { picks } => {
                SettingsEvent::Imported(match self.settings_controller() {
                    Ok(controller) => controller
                        .import_skills(picks)
                        .await
                        .map(|done| {
                            done.into_iter()
                                .map(|(folder, result)| {
                                    (folder, result.map_err(|code| bounded_problem(&code)))
                                })
                                .collect()
                        })
                        .map_err(|error| problem(&error, lost)),
                    Err(error) => Err(error),
                })
            }
            SettingsJob::Plugin(change) => {
                SettingsEvent::PluginChanged(self.plugin_change(change, lost).await)
            }
            SettingsJob::AppSetup(job) => SettingsEvent::AppSetup(self.app_setup(job, lost).await),
            SettingsJob::Schedule => SettingsEvent::Schedule(match self.settings_controller() {
                Ok(controller) => controller
                    .account_schedule()
                    .await
                    .map_err(|error| problem(&error, lost)),
                Err(error) => Err(error),
            }),
            SettingsJob::Ranking => SettingsEvent::Ranking(match self.settings_controller() {
                Ok(controller) => controller
                    .account_ranking()
                    .await
                    .map_err(|error| problem(&error, lost)),
                Err(error) => Err(error),
            }),
            SettingsJob::SetSchedule { change, generation } => {
                SettingsEvent::ScheduleSet(match self.settings_controller() {
                    Ok(controller) => applied(
                        controller.set_account_schedule(change, generation).await,
                        "account_schedule_outcome_unknown",
                        lost,
                    ),
                    Err(error) => Err(error),
                })
            }
            SettingsJob::AddMemory {
                scope,
                text,
                generation,
            } => SettingsEvent::MemoryAdded(match self.settings_controller() {
                Ok(controller) => applied(
                    controller.add_memory(scope, text, generation).await,
                    "memory_outcome_unknown",
                    lost,
                ),
                Err(error) => Err(error),
            }),
            SettingsJob::Suggestions { projects } => {
                SettingsEvent::Suggestions(self.suggestions(projects, lost).await)
            }
            SettingsJob::Suggestion {
                suggestion,
                keep,
                generations,
            } => {
                let done = self.decide(&suggestion, keep, generations, lost).await;
                SettingsEvent::SuggestionDone(suggestion, done)
            }
            SettingsJob::DeleteMemory { memory, generation } => {
                SettingsEvent::MemoryDeleted(match self.settings_controller() {
                    Ok(controller) => applied(
                        controller.delete_memory(memory, generation).await,
                        "memory_outcome_unknown",
                        lost,
                    ),
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

    /// Reads the research memories waiting for the owner and the ones kept in a
    /// project, the first page of each project's inbox, as the desktop does.
    /// Inputs: the projects and the lost-connection flag. Output: at most
    /// SUGGESTIONS_MAX suggestions, or the first plain problem code.
    pub(super) async fn suggestions(
        &mut self,
        projects: Vec<ProjectId>,
        lost: &mut bool,
    ) -> Result<Vec<Suggestion>, String> {
        let controller = self.settings_controller()?;
        let mut found: Vec<Suggestion> = Vec::new();
        for project in projects.into_iter().take(SUGGESTION_PROJECTS_MAX) {
            let entries = controller
                .project_memories(project, None)
                .await
                .map_err(|error| problem(&error, lost))?;
            found.extend(entries.into_iter().filter_map(|entry| {
                let kept = match entry.status {
                    KnowledgeStatus::Confirmed => true,
                    KnowledgeStatus::Provisional if !entry.human_reviewed => false,
                    _ => return None,
                };
                Some(Suggestion {
                    project: entry.project_id,
                    entry: entry.id,
                    version: entry.version,
                    text: entry.text,
                    kept,
                })
            }));
            if found.len() >= SUGGESTIONS_MAX {
                found.truncate(SUGGESTIONS_MAX);
                break;
            }
        }
        assert!(found.len() <= SUGGESTIONS_MAX);
        Ok(found)
    }

    /// Saves a research suggestion to memory for every project, or dismisses it.
    /// Saving keeps it in its project first, then copies the kept version to
    /// global memory, the same two engine changes the desktop makes; a memory
    /// already kept in its project is only copied.
    /// Inputs: the suggestion, keep or dismiss, one client generation per
    /// change and the lost-connection flag. Output: done, or the plain code of
    /// the step that failed.
    pub(super) async fn decide(
        &mut self,
        suggestion: &Suggestion,
        keep: bool,
        generations: [u64; 2],
        lost: &mut bool,
    ) -> Result<(), String> {
        assert!(generations[0] > 0 && generations[0] < generations[1]);
        assert!(keep || !suggestion.kept);
        let controller = self.settings_controller()?;
        if suggestion.kept {
            return applied(
                controller
                    .promote_memory(
                        suggestion.project.clone(),
                        suggestion.entry.clone(),
                        suggestion.version,
                        generations[1],
                    )
                    .await,
                "memory_outcome_unknown",
                lost,
            );
        }
        let decision = if keep {
            ReviewDecision::Keep
        } else {
            ReviewDecision::Reject
        };
        applied(
            controller
                .review_memory(
                    suggestion.project.clone(),
                    suggestion.entry.clone(),
                    suggestion.version,
                    decision,
                    generations[0],
                )
                .await,
            "memory_outcome_unknown",
            lost,
        )?;
        if !keep {
            return Ok(());
        }
        let version = suggestion
            .version
            .checked_add(1)
            .ok_or_else(|| "memory_version_exhausted".to_owned())?;
        applied(
            controller
                .promote_memory(
                    suggestion.project.clone(),
                    suggestion.entry.clone(),
                    version,
                    generations[1],
                )
                .await,
            "memory_outcome_unknown",
            lost,
        )
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

    pub(super) fn settings_controller(&mut self) -> Result<&mut OwnerController, String> {
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
                    .map_err(|error| {
                        if connection_lost(&error.0) {
                            *lost = true;
                        }
                        "native_auth_claude_input_outcome_unknown".to_owned()
                    })?;
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
        let reply = reply.map_err(|error| problem(&error, lost))?;
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
            applied(
                controller.send_pending().await,
                "pool_outcome_unknown",
                lost,
            )?;
            joined += 1;
        }
        Ok(joined)
    }
}

fn fresh_id() -> Result<RequestId, String> {
    let id = crate::engine::owner::random_id().map_err(|error| bounded_problem(&error.0))?;
    RequestId::new(id).map_err(|_| "native_auth_request_id_invalid".to_owned())
}

/// Tells whether a failed engine call means the trusted connection is gone: the
/// socket vanished, a restarted engine replaced it, it refused the connection,
/// or the reply never came. Refusals the engine answered are not a loss.
/// Input: a raw engine error or a bounded code. Output: true when the owner
/// connection must detach and reconnect.
pub(super) fn connection_lost(code: &str) -> bool {
    let code = bounded_problem(code);
    code == "controller_disconnected"
        || code == "trusted_owner_connection_failed"
        || code.starts_with("owner_root_")
        || code.starts_with("owner_socket_")
        || code.starts_with("owner_outcome_unknown")
        || code.ends_with("_owner_unavailable")
        || code.ends_with("_outcome_unknown")
        || code.contains("transport")
}

/// Reads one owner change reply: the engine answers a refused change as an
/// applied operation whose result is the refusal.
/// Inputs: the reply, the code for an unknown outcome and the lost-connection
/// flag. Output: done only when the change took effect, else the plain code.
pub(super) fn applied(
    reply: Result<OwnerResponse, EngineError>,
    unknown: &str,
    lost: &mut bool,
) -> Result<(), String> {
    match reply {
        Ok(OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }) => {
            match operation.result {
                OwnerResult::Applied => Ok(()),
                OwnerResult::Rejected { code } => Err(bounded_problem(&code)),
                _ => Err(unknown.to_owned()),
            }
        }
        Ok(OwnerResponse::Rejected { code }) => Err(bounded_problem(&code)),
        Ok(_) => Err(unknown.to_owned()),
        Err(error) => Err(problem(&error, lost)),
    }
}

pub(super) fn problem(error: &EngineError, lost: &mut bool) -> String {
    if connection_lost(&error.0) {
        *lost = true;
    }
    bounded_problem(&error.0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::accounts::{AccountDraft, Provider, SignInMethod};
    use crate::engine::bootstrap::EngineService;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_loads_read_every_page_and_delete_reaches_the_engine() {
        let root = tempfile::Builder::new()
            .prefix("ca-tset-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n").unwrap();
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
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
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

        let note = |generation| SettingsJob::AddMemory {
            scope: MemoryScope::Global,
            text: "Use metric units in every recipe.".into(),
            generation,
        };
        let written = native.settings(note(1)).await;
        assert!(matches!(written, SettingsEvent::MemoryAdded(Ok(()))));
        let twice = native.settings(note(2)).await;
        assert!(matches!(
            twice,
            SettingsEvent::MemoryAdded(Err(code)) if code == "memory_duplicate"
        ));
        let nowhere = native
            .settings(SettingsJob::AddMemory {
                scope: MemoryScope::Project {
                    project_id: ProjectId::new("nowhere").unwrap(),
                },
                text: "A project that was never made.".into(),
                generation: 3,
            })
            .await;
        assert!(matches!(
            nowhere,
            SettingsEvent::MemoryAdded(Err(code)) if code == "memory_project_missing"
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
                generation: 4,
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
                generation: 5,
            })
            .await;
        assert!(matches!(
            again,
            SettingsEvent::MemoryDeleted(Err(code)) if code == "memory_missing"
        ));

        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deleting_selected_accounts_removes_each_on_the_engine_and_names_the_one_already_gone()
    {
        use crate::tui::client::OwnerEvent;
        use crate::tui::client::accounts::{AccountsReply, AccountsRequest};
        use crate::tui::keys::Key;
        use crate::tui::screens::Screen;
        use crate::tui::screens::settings_accounts::Accounts;
        use crate::tui::seam::{Arrival, Request};

        let root = tempfile::Builder::new()
            .prefix("ca-tdel-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (mut service, _stop) = engine(root.path()).await;
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
        let _ = native.attach().await;
        assert!(native.ready);
        for name in ["Laptop", "Mini", "Studio"] {
            let added = native
                .settings(SettingsJob::Accounts(AccountsRequest::Add {
                    draft: AccountDraft {
                        provider: Provider::Local,
                        method: SignInMethod::Url,
                        name: name.into(),
                        url: None,
                        model: Some("qwen2.5:0.5b".into()),
                    },
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
        let mut ctx = crate::tui::screens::settings_accounts::tests::ctx(80, 24);
        let mut screen = Accounts::with(accounts.clone());
        for name in ["Laptop", "Mini", "Studio"] {
            screen.key(&Key::Char('/'), &mut ctx);
            for letter in name.chars() {
                screen.key(&Key::Char(letter), &mut ctx);
            }
            screen.key(&Key::Enter, &mut ctx);
            screen.key(&Key::Char(' '), &mut ctx);
            screen.key(&Key::Esc, &mut ctx);
        }
        assert_eq!(ctx.message().unwrap().lead, "3 selected.");
        screen.key(&Key::Char('d'), &mut ctx);
        assert_eq!(
            screen
                .modal(&ctx)
                .expect("a confirm before deleting")
                .0
                .title,
            "Delete accounts"
        );
        let laptop = accounts
            .iter()
            .find(|account| account.name == "Laptop")
            .unwrap()
            .id
            .clone();
        let gone = native
            .settings(SettingsJob::Accounts(AccountsRequest::Remove {
                id: laptop,
            }))
            .await;
        assert!(matches!(
            gone,
            SettingsEvent::Accounts(Ok(AccountsReply::Removed(_)))
        ));
        screen.key(&Key::Right, &mut ctx);
        screen.key(&Key::Enter, &mut ctx);
        assert!(screen.modal(&ctx).is_none());
        assert!(screen.busy(&ctx));
        let mut sent = 0;
        for _ in 0..32 {
            let Some(request) = ctx.next_request() else {
                break;
            };
            if let Request::Settings(job) = request {
                sent += 1;
                let event = native.settings(*job).await;
                screen.arrive(
                    &Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event)))),
                    &mut ctx,
                );
            }
        }
        assert_eq!(sent, 4, "three removes one at a time, then one list reload");
        assert!(!screen.busy(&ctx));
        let message = ctx.message().unwrap();
        assert_eq!(message.lead, "Deleted 2 accounts.");
        assert_eq!(
            message.rest,
            "Could not delete Laptop: This account no longer exists."
        );
        assert!(native.all_accounts(2, &mut lost).await.unwrap().is_empty());

        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn add_account_offers_one_entry_per_company_and_adds_the_same_engine_accounts() {
        use crate::accounts::AccountStatus;
        use crate::tui::client::OwnerEvent;
        use crate::tui::ctx::Ctx;
        use crate::tui::keys::Key;
        use crate::tui::screens::Screen;
        use crate::tui::screens::settings_add_account::AddAccount;
        use crate::tui::seam::{Arrival, Request};

        async fn pump(screen: &mut AddAccount, ctx: &mut Ctx, native: &mut NativeOwner) -> usize {
            let mut held = 0;
            for _ in 0..16 {
                let Some(request) = ctx.next_request() else {
                    break;
                };
                match request {
                    Request::Settings(job) if matches!(*job, SettingsJob::Accounts(_)) => {
                        let event = native.settings(*job).await;
                        screen.arrive(
                            &Arrival::Owner(Box::new(OwnerEvent::Settings(Box::new(event)))),
                            ctx,
                        );
                    }
                    Request::Settings(_) => held += 1,
                    _ => {}
                }
            }
            held
        }
        fn text(screen: &AddAccount, ctx: &Ctx) -> Vec<String> {
            screen
                .body(76, ctx)
                .0
                .iter()
                .map(|line| line.to_string())
                .collect()
        }
        fn type_text(screen: &mut AddAccount, ctx: &mut Ctx, value: &str) {
            for letter in value.chars() {
                screen.key(&Key::Char(letter), ctx);
            }
        }

        let root = tempfile::Builder::new()
            .prefix("ca-tadd-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (mut service, _stop) = engine(root.path()).await;
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
        let _ = native.attach().await;
        assert!(native.ready);
        let mut ctx = crate::tui::screens::settings_accounts::tests::ctx(80, 24);

        let mut screen = AddAccount::new();
        screen.shown(&mut ctx);
        assert_eq!(pump(&mut screen, &mut ctx, &mut native).await, 0);
        let rows = text(&screen, &ctx);
        assert_eq!(rows.len(), 3 + 5 + 2, "five entries: {rows:#?}");
        for (row, (name, line)) in rows[3..8].iter().zip([
            ("OpenAI", "Sign in with ChatGPT or use an OpenAI API key"),
            (
                "Anthropic",
                "Sign in with Claude or use an Anthropic API key",
            ),
            ("OpenRouter", "One API key for models from many companies"),
            (
                "OpenAI-compatible URL",
                "Any server that speaks the OpenAI API",
            ),
            (
                "Local model",
                "Ollama, LM Studio or another server on this Mac",
            ),
        ]) {
            assert!(
                row.contains(&format!(" {name} ")) && row.contains(line),
                "{row}"
            );
        }
        assert!(
            rows.iter()
                .all(|row| !row.contains("ChatGPT / Codex") && !row.contains("Pay-as-you-go")),
            "{rows:#?}"
        );

        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Enter, &mut ctx);
        let methods = text(&screen, &ctx).join("\n");
        let first = methods
            .find("Sign in with your Claude subscription")
            .expect(&methods);
        let key = methods.find("Use an API key").expect(&methods);
        assert!(first < key && !methods.contains("Codex"), "{methods}");
        assert!(methods.starts_with("Anthropic\n"), "{methods}");
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Enter, &mut ctx);
        screen.key(&Key::Tab, &mut ctx);
        screen.key(
            &Key::Paste("unit-placeholder-anthropic-key-0001".into()),
            &mut ctx,
        );
        screen.key(&Key::Tab, &mut ctx);
        type_text(&mut screen, &mut ctx, "claude-sonnet-4-5");
        screen.key(&Key::Enter, &mut ctx);
        assert_eq!(pump(&mut screen, &mut ctx, &mut native).await, 0);
        assert_eq!(ctx.message().unwrap().lead, "Account added.");

        let mut screen = AddAccount::new();
        screen.shown(&mut ctx);
        pump(&mut screen, &mut ctx, &mut native).await;
        screen.key(&Key::Enter, &mut ctx);
        let methods = text(&screen, &ctx).join("\n");
        let order: Vec<usize> = [
            "Sign in with ChatGPT",
            "Use the Codex sign-in on this Mac",
            "Use an API key",
        ]
        .iter()
        .map(|title| methods.find(title).expect(&methods))
        .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{methods}");
        assert!(methods.starts_with("OpenAI\n"), "{methods}");
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Down, &mut ctx);
        screen.key(&Key::Enter, &mut ctx);
        screen.key(&Key::Tab, &mut ctx);
        screen.key(
            &Key::Paste("unit-placeholder-openai-key-00000001".into()),
            &mut ctx,
        );
        screen.key(&Key::Tab, &mut ctx);
        type_text(&mut screen, &mut ctx, "gpt-5");
        screen.key(&Key::Enter, &mut ctx);
        assert_eq!(pump(&mut screen, &mut ctx, &mut native).await, 0);

        for down in [1, 0] {
            let mut screen = AddAccount::new();
            screen.shown(&mut ctx);
            pump(&mut screen, &mut ctx, &mut native).await;
            for _ in 0..down {
                screen.key(&Key::Down, &mut ctx);
            }
            screen.key(&Key::Enter, &mut ctx);
            screen.key(&Key::Enter, &mut ctx);
            assert_eq!(
                pump(&mut screen, &mut ctx, &mut native).await,
                1,
                "the sign-in itself is held back"
            );
        }

        let mut lost = false;
        let mut added: Vec<(Provider, SignInMethod, String, String, AccountStatus)> = native
            .all_accounts(2, &mut lost)
            .await
            .unwrap()
            .into_iter()
            .map(|account| {
                (
                    account.provider,
                    account.method,
                    account.provider_name,
                    account.name,
                    account.status,
                )
            })
            .collect();
        added.sort_by_key(|account| account.0);
        assert_eq!(
            added,
            [
                (
                    Provider::Chatgpt,
                    SignInMethod::Subscription,
                    "ChatGPT / Codex".to_owned(),
                    "ChatGPT / Codex".to_owned(),
                    AccountStatus::NotSignedIn
                ),
                (
                    Provider::Claude,
                    SignInMethod::Subscription,
                    "Claude".to_owned(),
                    "Claude".to_owned(),
                    AccountStatus::NotSignedIn
                ),
                (
                    Provider::OpenaiApi,
                    SignInMethod::ApiKey,
                    "OpenAI API".to_owned(),
                    "OpenAI API".to_owned(),
                    AccountStatus::KeySaved
                ),
                (
                    Provider::AnthropicApi,
                    SignInMethod::ApiKey,
                    "Anthropic API".to_owned(),
                    "Anthropic API".to_owned(),
                    AccountStatus::KeySaved
                ),
            ]
        );

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
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n").unwrap();
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
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
        let _ = native.attach().await;
        assert!(native.ready);
        let _home =
            crate::tui::test_home::projects_home(native.controller.as_mut().unwrap(), 1).await;

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
        let SettingsEvent::Suggestions(Ok(waiting)) = native
            .settings(SettingsJob::Suggestions {
                projects: vec![project.clone()],
            })
            .await
        else {
            panic!("suggestions required")
        };
        assert!(
            waiting.is_empty(),
            "a project without research suggests nothing"
        );
        let missing = Suggestion {
            project: project.clone(),
            entry: RequestId::new("entry-never-made").unwrap(),
            version: 1,
            text: "Never researched.".into(),
            kept: false,
        };
        let refused = native
            .settings(SettingsJob::Suggestion {
                suggestion: missing.clone(),
                keep: true,
                generations: [2, 3],
            })
            .await;
        assert!(matches!(
            refused,
            SettingsEvent::SuggestionDone(done, Err(_)) if done == missing
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
        assert_eq!(skills[0].line, "Teaches release notes.");
        match native
            .settings(SettingsJob::SkillDetail {
                id: skills[0].id.clone(),
            })
            .await
        {
            SettingsEvent::SkillDetail(Ok(detail)) => {
                assert_eq!(detail.skill.id, skills[0].id);
                assert!(detail.text.ends_with("\n\nSteps.\n"), "{}", detail.text);
                let files: Vec<&str> = detail.files.iter().map(|file| file.path.as_str()).collect();
                assert_eq!(files, vec!["SKILL.md"]);
            }
            SettingsEvent::SkillDetail(Err(code)) => panic!("skill detail failed: {code}"),
            _ => panic!("a skill detail reply required"),
        }

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

        let found = root.path().join("home/.claude/skills/zeta");
        std::fs::create_dir_all(&found).unwrap();
        std::fs::write(
            found.join("SKILL.md"),
            "---\nname: zeta\ndescription: Zeta steps.\n---\n",
        )
        .unwrap();
        std::fs::write(found.join("deploy.pem"), "dummy").unwrap();
        let folder = found.display().to_string();
        match native
            .settings(SettingsJob::ImportSkills {
                picks: vec![(folder.clone(), None), (folder.clone(), None)],
            })
            .await
        {
            SettingsEvent::Imported(Ok(done)) => {
                assert_eq!(done.len(), 2);
                let first = done[0].1.as_ref().unwrap();
                assert_eq!(first.skill.name, "zeta");
                assert!(first.skill.enabled);
                assert_eq!(first.skipped, vec!["deploy.pem"]);
                assert_eq!(done[1].1.as_ref().unwrap_err(), "skill_name_taken");
            }
            SettingsEvent::Imported(Err(code)) => panic!("import failed: {code}"),
            _ => panic!("an import reply required"),
        }
        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    async fn engine(root: &std::path::Path) -> (EngineService, tokio::sync::watch::Sender<bool>) {
        let global = root.join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root,
            "e.sock",
            &global,
            &root.join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        (service, stop)
    }

    fn failure(event: &SettingsEvent) -> Option<&str> {
        match event {
            SettingsEvent::Accounts(Err(code))
            | SettingsEvent::AllAccounts(Err(code))
            | SettingsEvent::Harnesses(Err(code))
            | SettingsEvent::Memories(Err(code))
            | SettingsEvent::MemoryAdded(Err(code))
            | SettingsEvent::Plugins(Err(code)) => Some(code),
            _ => None,
        }
    }

    #[test]
    fn only_a_failed_transport_counts_as_a_lost_connection() {
        for gone in [
            "No such file or directory (os error 2)",
            "trusted_owner_connection_failed",
            "controller_disconnected",
            "accounts_owner_unavailable",
            "accounts_outcome_unknown",
            "owner_outcome_unknown_reconcile_same_identity",
            "owner_socket_replaced",
            "owner_root_replaced_or_unsafe",
        ] {
            assert!(connection_lost(gone), "{gone}");
        }
        for answered in [
            "account_not_found",
            "memory_duplicate",
            "controller_pending_intent",
            "trusted_connection_unavailable",
            "model_access_read_only",
        ] {
            assert!(!connection_lost(answered), "{answered}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stopped_or_restarted_engine_detaches_every_settings_job_and_it_reattaches() {
        type Job = (&'static str, fn() -> SettingsJob);
        let jobs: [Job; 7] = [
            ("accounts list", || {
                SettingsJob::Accounts(AccountsRequest::List {
                    after: None,
                    limit: 50,
                })
            }),
            ("accounts catalog", || {
                SettingsJob::Accounts(AccountsRequest::Catalog)
            }),
            ("all accounts", || SettingsJob::AllAccounts),
            ("harnesses", || SettingsJob::Harnesses),
            ("memories", || SettingsJob::Memories {
                scopes: vec![MemoryScope::Global],
            }),
            ("add memory", || SettingsJob::AddMemory {
                scope: MemoryScope::Global,
                text: "Written while the engine is stopped.".into(),
                generation: 1,
            }),
            ("plugins", || SettingsJob::Plugins),
        ];
        let mut kept = Vec::new();
        let mut last = None;
        for ((label, job), restart) in jobs.into_iter().flat_map(|job| [(job, false), (job, true)])
        {
            let root = tempfile::Builder::new()
                .prefix("ca-tlost-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let (mut service, _stop) = engine(root.path()).await;
            let mut native = NativeOwner::new(
                root.path().join("e.sock"),
                crate::tui::test_home::ENGINE_WAIT,
            );
            let _ = native.attach().await;
            assert!(native.ready, "{label}: attach");
            tokio::time::timeout(Duration::from_secs(10), service.shutdown())
                .await
                .unwrap()
                .unwrap();
            let restarted = if restart {
                Some(engine(root.path()).await)
            } else {
                None
            };
            let event = native.settings(job()).await;
            let code = failure(&event).map(str::to_owned);
            assert!(
                code.is_some(),
                "{label} (restart {restart}): the old session answered"
            );
            if native.ready {
                kept.push(format!("{label} (restart {restart}): {code:?}"));
            }
            if let Some((mut service, _stop)) = restarted {
                tokio::time::timeout(Duration::from_secs(10), service.shutdown())
                    .await
                    .unwrap()
                    .unwrap();
            }
            last = Some((root, native));
        }
        assert!(
            kept.is_empty(),
            "still attached after the engine went away: {kept:#?}"
        );

        let (root, mut native) = last.unwrap();
        let (mut service, _stop) = engine(root.path()).await;
        let _ = native.attach().await;
        assert!(native.ready);
        let listed = native
            .settings(SettingsJob::Accounts(AccountsRequest::List {
                after: None,
                limit: 50,
            }))
            .await;
        assert!(matches!(
            listed,
            SettingsEvent::Accounts(Ok(AccountsReply::Page(_)))
        ));
        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_sign_in_read_on_a_stopped_engine_drops_the_trusted_connection() {
        let root = tempfile::Builder::new()
            .prefix("ca-tsign-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 10000\n").unwrap();
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
        let mut native = NativeOwner::new(
            root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        );
        let _ = native.attach().await;
        assert!(native.ready);
        let controller = native.controller.as_mut().unwrap();
        let read = controller.read(OwnerRead::Capabilities).await.unwrap();
        let ticket = SignInTicket {
            scope: crate::engine::owner::NativeAuthScope {
                expected_database_instance: controller.identity().database_instance().into(),
                connection: CredentialRef::new("missing-login").unwrap(),
                expected_config_id: read.config_id,
                expected_source_hash: "a".repeat(64),
            },
            operation: fresh_id().unwrap(),
        };
        let check = || {
            SettingsJob::SignIn(SignInJob::Read {
                ticket: ticket.clone(),
            })
        };

        let refused = native.settings(check()).await;
        assert!(matches!(refused, SettingsEvent::SignIn(Err(_))));
        assert!(
            native.ready,
            "a sign-in the engine refuses keeps the connection"
        );

        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        let unanswered = native.settings(check()).await;
        assert!(matches!(unanswered, SettingsEvent::SignIn(Err(_))));
        assert!(
            !native.ready,
            "a sign-in read the stopped engine never answers drops the connection"
        );
    }
}
