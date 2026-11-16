use super::settings::connection_lost;
use super::*;
use crate::engine::owner::{AccountSecret, OwnerResult};
use crate::github::{
    CloneReceipt, GitHubDeviceCode, GitHubOwners, GitHubPublishJob, GitHubPublishLeft,
    GitHubPublishPlan, GitHubRepoPage, GitHubStatus,
};
use crate::github::{GitHubRepoRules, GitHubRule};
use crate::onboarding::OnboardingView;
use crate::projects_home::job::{JobSubject, JobView};
use crate::projects_home::{AddMode, AddOverride, FoundProject, ProjectsHome};
use crate::protocol::{CredentialRef, RequestId};
use std::collections::BTreeSet;

#[cfg(all(test, unix))]
mod tests;

/// One projects home, add project, onboarding or GitHub request a TUI screen sends.
/// Built by the first run, Home, Add project and onboarding screens, and by Settings for the home folder.
/// Runs on the owner worker; the reply arrives as `OwnerEvent::Intake` with this query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum IntakeQuery {
    Home,
    SetHome {
        path: String,
        generation: u64,
    },
    Found,
    FolderSize {
        path: String,
    },
    StartOnboarding {
        path: String,
        pool: CredentialRef,
        generation: u64,
    },
    Onboarding {
        project: ProjectId,
    },
    GitHubStatus,
    GitHubSignIn,
    GitHubUseCli,
    GitHubSignOut,
    GitHubRepos {
        page: u32,
    },
    GitHubOwners,
    GitHubPublishStart {
        project: ProjectId,
        plan: GitHubPublishPlan,
    },
    GitHubPublishStatus {
        project: ProjectId,
    },
    GitHubPublishApprove {
        project: ProjectId,
        lines: Vec<String>,
    },
    GitHubPublishExclude {
        project: ProjectId,
        path: String,
    },
    GitHubPublishRecheck {
        project: ProjectId,
    },
    GitHubPublishContinue {
        project: ProjectId,
    },
    GitHubPublishPrompt {
        project: ProjectId,
    },
    GitHubPublishPromptSet {
        project: ProjectId,
        answer: crate::github::GitHubPromptAnswer,
    },
    GitHubPublishDiscard {
        project: ProjectId,
    },
    CloneRepo {
        full_name: String,
    },
    JobStart {
        id: RequestId,
        subject: JobSubject,
        mode: AddMode,
        overrides: Vec<AddOverride>,
    },
    JobStatus,
    JobStop {
        id: RequestId,
    },
    JobDismiss {
        id: RequestId,
    },
    GitHubRuleDefaults,
    GitHubRuleDefaultsSet {
        rules: BTreeSet<GitHubRule>,
    },
    GitHubRules {
        project: ProjectId,
    },
    GitHubRuleSet {
        project: ProjectId,
        repo: String,
        rule: GitHubRule,
        on: bool,
    },
}

/// A fresh id for one start of the engine's intake job, which the job that start makes
/// carries, so a screen can tell that job from an older add of the same folder.
/// Output: the id, or the problem when none was made.
pub(in crate::tui) fn start_id() -> Result<RequestId, String> {
    let id = crate::engine::owner::random_id().map_err(|error| bounded_problem(&error.0))?;
    assert!(!id.is_empty());
    RequestId::new(id).map_err(|_| "intake_failed".to_owned())
}

/// One intake job for the owner worker: a plain query or a pasted GitHub token.
/// The token travels only here, never in a query, so no reply, log or Debug output can echo it.
/// The token job answers as a `GitHubStatus` query.
pub(in crate::tui) enum IntakeJob {
    Query(IntakeQuery),
    GitHubToken(AccountSecret),
}

/// What the engine answered to one intake request.
/// Reads carry their data; changes carry the result, the device code, the clone or the new project.
/// Never holds a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) enum IntakeView {
    Home(ProjectsHome),
    Found(Vec<FoundProject>),
    FolderSize(crate::projects_home::Measured),
    Onboarding(Box<OnboardingView>),
    OnboardingStarted(ProjectId),
    GitHub(GitHubStatus),
    DeviceCode(GitHubDeviceCode),
    Repos(GitHubRepoPage),
    Owners(GitHubOwners),
    Publish(Box<GitHubPublishJob>),
    PublishStatus(Option<Box<GitHubPublishJob>>),
    Prompt(bool),
    PromptKept,
    Discarded(Option<GitHubPublishLeft>),
    Cloned(CloneReceipt),
    Changed(OwnerResult),
    Job(Option<JobView>),
    JobStopped(bool),
    JobDismissed(bool),
    RuleChanging,
    RuleDefaults(BTreeSet<GitHubRule>),
    Rules(Box<GitHubRepoRules>),
}

/// One intake reply for the screens.
/// Holds the query it answers and the view, or a bounded plain problem code.
/// Screens apply it only when the query is one they sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tui) struct IntakeEvent {
    pub query: IntakeQuery,
    pub result: Result<IntakeView, String>,
}

impl IntakeEvent {
    /// The reply for a job the worker could not run because the trusted connection is gone.
    /// Takes the job.
    /// Returns the event for its query, or for the status read a token job answers as.
    pub(in crate::tui) fn unavailable(job: &IntakeJob) -> Self {
        Self::failed(job, "trusted_connection_unavailable")
    }

    /// The reply for a job that failed before it reached the engine.
    /// Takes the job and the problem code.
    /// Returns the event for its query, or for the status read a token job answers as.
    pub(in crate::tui) fn failed(job: &IntakeJob, code: &str) -> Self {
        Self {
            query: job.query(),
            result: Err(code.to_owned()),
        }
    }
}

impl IntakeJob {
    /// The query the reply to this job carries.
    /// Takes nothing.
    /// Returns the query itself, or the status read for a token job.
    pub(in crate::tui) fn query(&self) -> IntakeQuery {
        match self {
            Self::Query(query) => query.clone(),
            Self::GitHubToken(_) => IntakeQuery::GitHubStatus,
        }
    }
}

impl Connection {
    /// Queues one intake job on the paired owner worker.
    /// Takes the job.
    /// Returns an error while another native request is pending.
    pub(in crate::tui) fn intake(&mut self, job: IntakeJob) -> Result<(), String> {
        self.owner_job(Job::Intake(Box::new(job)))
    }
}

impl NativeOwner {
    /// Runs one intake job through the trusted owner controller.
    /// Takes the job.
    /// Returns the event; a transport failure detaches the connection so the shell reconnects.
    pub(super) async fn intake(&mut self, job: IntakeJob) -> IntakeEvent {
        let query = job.query();
        let mut lost = false;
        let result = self.intake_view(job, &mut lost).await;
        if lost {
            self.detach();
        }
        IntakeEvent { query, result }
    }

    /// Starts one GitHub rule switch on a lane thread of its own, so the worker takes other
    /// requests while GitHub answers, which can take minutes. It first sends the worker's
    /// finished answer, `RuleChanging` or the refusal, `github_rule_busy` while a switch
    /// still runs; the switch's result comes after as an unfinished owner event. Takes the
    /// switch job and the reply sender. Returns false once nobody reads the replies.
    pub(super) fn rule_lane(&mut self, job: IntakeJob, replies: &SyncSender<OwnerReply>) -> bool {
        let query = job.query();
        let IntakeJob::Query(IntakeQuery::GitHubRuleSet {
            project,
            repo,
            rule,
            on,
        }) = job
        else {
            unreachable!("the rule lane runs only a rule switch");
        };
        let reply = |finished, result| OwnerReply {
            finished,
            event: OwnerEvent::Intake(Box::new(IntakeEvent {
                query: query.clone(),
                result,
            })),
        };
        if self
            .rule_lane
            .as_ref()
            .is_some_and(|lane| !lane.is_finished())
        {
            return replies
                .send(reply(true, Err("github_rule_busy".into())))
                .is_ok();
        }
        let shared = self
            .controller
            .as_ref()
            .filter(|_| self.ready)
            .and_then(|controller| controller.shared_client().ok());
        let Some(client) = shared else {
            return replies
                .send(reply(true, Err("trusted_connection_unavailable".into())))
                .is_ok();
        };
        if replies
            .send(reply(true, Ok(IntakeView::RuleChanging)))
            .is_err()
        {
            return false;
        }
        let lane_replies = replies.clone();
        let asked = query.clone();
        let spawned = std::thread::Builder::new()
            .name("tui-github".into())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime
                        .block_on(client.github_rule_set(project, repo, rule, on))
                        .map(|page| IntakeView::Rules(Box::new(page)))
                        .map_err(|error| bounded_problem(&error.0)),
                    Err(_) => Err("github_rule_unstarted".to_owned()),
                };
                let _ = lane_replies.send(OwnerReply {
                    finished: false,
                    event: OwnerEvent::Intake(Box::new(IntakeEvent {
                        query: asked,
                        result,
                    })),
                });
            });
        match spawned {
            Ok(lane) => {
                self.rule_lane = Some(lane);
                true
            }
            Err(_) => replies
                .send(reply(false, Err("github_rule_unstarted".into())))
                .is_ok(),
        }
    }

    async fn intake_view(&mut self, job: IntakeJob, lost: &mut bool) -> Result<IntakeView, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        let problem = |error: crate::engine::EngineError| {
            if connection_lost(&error.0) {
                *lost = true;
            }
            bounded_problem(&error.0)
        };
        let query = match job {
            IntakeJob::Query(query) => query,
            IntakeJob::GitHubToken(token) => {
                return controller
                    .github_token(token)
                    .await
                    .map(IntakeView::GitHub)
                    .map_err(problem);
            }
        };
        Ok(match query {
            IntakeQuery::Home => {
                IntakeView::Home(controller.projects_home().await.map_err(problem)?)
            }
            IntakeQuery::SetHome { path, generation } => IntakeView::Changed(applied(
                controller
                    .set_projects_home(path, generation)
                    .await
                    .map_err(problem)?,
            )?),
            IntakeQuery::Found => {
                IntakeView::Found(controller.found_projects().await.map_err(problem)?)
            }
            IntakeQuery::FolderSize { path } => {
                IntakeView::FolderSize(controller.folder_size(path).await.map_err(problem)?)
            }
            IntakeQuery::StartOnboarding {
                path,
                pool,
                generation,
            } => {
                let guessed = started(&path)?;
                let result = applied(
                    controller
                        .start_onboarding(path, pool, generation)
                        .await
                        .map_err(problem)?,
                )?;
                match result {
                    OwnerResult::OnboardingStarted { project_id } => {
                        IntakeView::OnboardingStarted(project_id)
                    }
                    OwnerResult::Applied => IntakeView::OnboardingStarted(guessed),
                    _ => return Err("unexpected_reply".into()),
                }
            }
            IntakeQuery::Onboarding { project } => IntakeView::Onboarding(Box::new(
                controller.onboarding(project).await.map_err(problem)?,
            )),
            IntakeQuery::GitHubStatus => {
                IntakeView::GitHub(controller.github_status().await.map_err(problem)?)
            }
            IntakeQuery::GitHubSignIn => {
                IntakeView::DeviceCode(controller.github_sign_in().await.map_err(problem)?)
            }
            IntakeQuery::GitHubUseCli => {
                IntakeView::GitHub(controller.github_use_cli().await.map_err(problem)?)
            }
            IntakeQuery::GitHubSignOut => {
                IntakeView::GitHub(controller.github_sign_out().await.map_err(problem)?)
            }
            IntakeQuery::GitHubRepos { page } => {
                IntakeView::Repos(controller.github_repos(page).await.map_err(problem)?)
            }
            IntakeQuery::GitHubOwners => {
                IntakeView::Owners(controller.github_owners().await.map_err(problem)?)
            }
            IntakeQuery::GitHubPublishStart { project, plan } => IntakeView::Publish(Box::new(
                controller
                    .github_publish_start(project, plan)
                    .await
                    .map_err(problem)?,
            )),
            IntakeQuery::GitHubPublishStatus { project } => IntakeView::PublishStatus(
                controller
                    .github_publish_status(project)
                    .await
                    .map_err(problem)?
                    .map(Box::new),
            ),
            IntakeQuery::GitHubPublishApprove { project, lines } => IntakeView::Publish(Box::new(
                controller
                    .github_publish_approve(project, lines)
                    .await
                    .map_err(problem)?,
            )),
            IntakeQuery::GitHubPublishExclude { project, path } => IntakeView::Publish(Box::new(
                controller
                    .github_publish_exclude(project, path)
                    .await
                    .map_err(problem)?,
            )),
            IntakeQuery::GitHubPublishRecheck { project } => IntakeView::Publish(Box::new(
                controller
                    .github_publish_recheck(project)
                    .await
                    .map_err(problem)?,
            )),
            IntakeQuery::GitHubPublishContinue { project } => IntakeView::Publish(Box::new(
                controller
                    .github_publish_continue(project)
                    .await
                    .map_err(problem)?,
            )),
            IntakeQuery::GitHubPublishPrompt { project } => IntakeView::Prompt(
                controller
                    .github_publish_prompt(project)
                    .await
                    .map_err(problem)?,
            ),
            IntakeQuery::GitHubPublishPromptSet { project, answer } => {
                controller
                    .github_publish_prompt_set(project, answer)
                    .await
                    .map_err(problem)?;
                IntakeView::PromptKept
            }
            IntakeQuery::GitHubPublishDiscard { project } => IntakeView::Discarded(
                controller
                    .github_publish_discard(project)
                    .await
                    .map_err(problem)?,
            ),
            IntakeQuery::CloneRepo { full_name } => {
                IntakeView::Cloned(controller.clone_repo(full_name).await.map_err(problem)?)
            }
            IntakeQuery::JobStart {
                id,
                subject,
                mode,
                overrides,
            } => IntakeView::Job(Some(
                controller
                    .job_start(id, subject, mode, overrides)
                    .await
                    .map_err(problem)?,
            )),
            IntakeQuery::JobStatus => {
                IntakeView::Job(controller.job_status().await.map_err(problem)?)
            }
            IntakeQuery::JobStop { id } => {
                IntakeView::JobStopped(controller.job_stop(id).await.map_err(problem)?)
            }
            IntakeQuery::JobDismiss { id } => {
                IntakeView::JobDismissed(controller.job_dismiss(id).await.map_err(problem)?)
            }
            IntakeQuery::GitHubRuleDefaults => {
                IntakeView::RuleDefaults(controller.github_rule_defaults().await.map_err(problem)?)
            }
            IntakeQuery::GitHubRuleDefaultsSet { rules } => IntakeView::RuleDefaults(
                controller
                    .github_rule_defaults_set(rules)
                    .await
                    .map_err(problem)?,
            ),
            IntakeQuery::GitHubRules { project } => IntakeView::Rules(Box::new(
                controller.github_rules(project).await.map_err(problem)?,
            )),
            IntakeQuery::GitHubRuleSet {
                project,
                repo,
                rule,
                on,
            } => IntakeView::Rules(Box::new(
                controller
                    .github_rule_set(project, repo, rule, on)
                    .await
                    .map_err(problem)?,
            )),
        })
    }
}

fn applied(response: OwnerResponse) -> Result<OwnerResult, String> {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            match operation.result {
                OwnerResult::Rejected { code } => Err(bounded_problem(&code)),
                result => Ok(result),
            }
        }
        OwnerResponse::Rejected { code } => Err(bounded_problem(&code)),
        _ => Err("intake_change_outcome_unknown".into()),
    }
}

fn started(path: &str) -> Result<ProjectId, String> {
    crate::onboarding::project_id_for(std::path::Path::new(path))
        .map_err(|error| bounded_problem(&error.0))
}

/// Reads the project a started onboarding opens from the engine's answer: the id the engine
/// gave, or `intake_change_outcome_unknown` for a plain Applied, never an id guessed from the
/// folder. Input: the applied result. Output: the project id, or the code.
fn onboarded(result: OwnerResult) -> Result<ProjectId, String> {
    let _ = result;
    Err(String::new())
}
