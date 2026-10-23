use super::accounts::{AccountsReply, AccountsRequest};
use super::*;
use crate::config::ModelAccessEdit;
use crate::engine::EngineError;
use crate::engine::owner::{
    AccountSecret, ClaudeAuthInput, ModelAccessCollection, ModelAccessRecord, NativeAuthAccount,
    NativeAuthMethod, NativeAuthPhase, NativeAuthStart,
};
use crate::harnesses::HarnessEntry;
use crate::memory::{KnowledgeEntry, ReviewDecision};
use crate::protocol::CredentialRef;

const POOL_JOINS_MAX: usize = 64;
const DISPLAY_BYTES_MAX: usize = 4096;

/// One Settings request the settings screens send through the shell.
/// Input: built by the Accounts, Add account, Harnesses and Memory screens.
/// Output: run on the owner worker; the reply arrives as `OwnerEvent::Settings`.
pub(in crate::tui) enum SettingsJob {
    Accounts(AccountsRequest),
    Harnesses,
    SignIn(SignInJob),
    JoinPools {
        account: CredentialRef,
        pools: Vec<(CredentialRef, u64)>,
    },
    Memories {
        project: ProjectId,
    },
    RemoveMemory {
        project: ProjectId,
        entry: RequestId,
        version: u64,
        generation: u64,
    },
}

impl std::fmt::Debug for SettingsJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Accounts(_) => "Accounts",
            Self::Harnesses => "Harnesses",
            Self::SignIn(_) => "SignIn",
            Self::JoinPools { .. } => "JoinPools",
            Self::Memories { .. } => "Memories",
            Self::RemoveMemory { .. } => "RemoveMemory",
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
    Harnesses(Result<Vec<HarnessEntry>, String>),
    SignIn(Result<(SignInTicket, SignInView), String>),
    PoolsJoined(Result<usize, String>),
    Memories {
        project: ProjectId,
        result: Result<Vec<KnowledgeEntry>, String>,
    },
    MemoryRemoved(Result<(), String>),
}

impl SettingsEvent {
    /// The failure event for a job that reached no trusted connection.
    /// Input: the job. Output: its event carrying the plain problem code.
    pub(in crate::tui) fn unavailable(job: &SettingsJob) -> Self {
        let problem = "trusted_connection_unavailable".to_owned();
        match job {
            SettingsJob::Accounts(_) => Self::Accounts(Err(problem)),
            SettingsJob::Harnesses => Self::Harnesses(Err(problem)),
            SettingsJob::SignIn(_) => Self::SignIn(Err(problem)),
            SettingsJob::JoinPools { .. } => Self::PoolsJoined(Err(problem)),
            SettingsJob::Memories { project } => Self::Memories {
                project: project.clone(),
                result: Err(problem),
            },
            SettingsJob::RemoveMemory { .. } => Self::MemoryRemoved(Err(problem)),
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
            SettingsJob::Accounts(request) => SettingsEvent::Accounts(self.accounts(request).await.result),
            SettingsJob::Harnesses => SettingsEvent::Harnesses(match self.settings_controller() {
                Ok(controller) => controller.harness_catalog().await.map_err(|error| problem(&error, lost)),
                Err(error) => Err(error),
            }),
            SettingsJob::SignIn(job) => SettingsEvent::SignIn(self.sign_in(job, lost).await),
            SettingsJob::JoinPools { account, pools } => {
                SettingsEvent::PoolsJoined(self.join_pools(account, pools, lost).await)
            }
            SettingsJob::Memories { project } => {
                let result = match self.settings_controller() {
                    Ok(controller) => controller
                        .project_memories(project.clone(), None)
                        .await
                        .map_err(|error| problem(&error, lost)),
                    Err(error) => Err(error),
                };
                SettingsEvent::Memories { project, result }
            }
            SettingsJob::RemoveMemory {
                project,
                entry,
                version,
                generation,
            } => SettingsEvent::MemoryRemoved(match self.settings_controller() {
                Ok(controller) => {
                    match controller
                        .review_memory(project, entry, version, ReviewDecision::Remove, generation)
                        .await
                    {
                        Ok(OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }) => Ok(()),
                        Ok(OwnerResponse::Rejected { code }) => Err(bounded_problem(&code)),
                        Ok(_) => Err("memory_outcome_unknown".into()),
                        Err(error) => Err(problem(&error, lost)),
                    }
                }
                Err(error) => Err(error),
            }),
        }
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
            if pool.members.len() >= crate::tui::screens::settings::pools::POOL_MEMBERS_MAX {
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

fn problem(error: &EngineError, lost: &mut bool) -> String {
    if error.0 == "controller_disconnected" || error.0.contains("transport") {
        *lost = true;
    }
    bounded_problem(&error.0)
}
