use super::settings::connection_lost;
use super::*;
use crate::engine::owner::{AccountSecret, OwnerResult};
use crate::github::{CloneReceipt, GitHubDeviceCode, GitHubRepoPage, GitHubStatus};
use crate::onboarding::Onboarding;
use crate::projects_home::{AddMode, FoundProject, FoundState, ProjectsHome};
use crate::protocol::CredentialRef;

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
    AddExisting {
        source: String,
        mode: AddMode,
        generation: u64,
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
    GitHubSignIn {
        generation: u64,
    },
    GitHubUseCli {
        generation: u64,
    },
    GitHubRepos {
        page: u32,
    },
    CloneRepo {
        full_name: String,
        generation: u64,
    },
}

/// One intake job for the owner worker: a plain query, or a pasted GitHub token.
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
    FolderSize(u64),
    Onboarding(Box<Onboarding>),
    OnboardingStarted(ProjectId),
    GitHub(GitHubStatus),
    DeviceCode(GitHubDeviceCode),
    Repos(GitHubRepoPage),
    Cloned(CloneReceipt),
    Changed(OwnerResult),
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
        Self {
            query: job.query(),
            result: Err("trusted_connection_unavailable".into()),
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

    async fn intake_view(&mut self, job: IntakeJob, lost: &mut bool) -> Result<IntakeView, String> {
        if !self.ready {
            return Err("trusted_connection_unavailable".into());
        }
        let controller = self
            .controller
            .as_mut()
            .ok_or("trusted_connection_unavailable")?;
        let mut problem = |error: crate::engine::EngineError| {
            if connection_lost(&error.0) {
                *lost = true;
            }
            bounded_problem(&error.0)
        };
        let query = match job {
            IntakeJob::Query(query) => query,
            IntakeJob::GitHubToken(token) => {
                controller.github_token(token).await.map_err(&mut problem)?;
                return controller
                    .github_status()
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
            IntakeQuery::AddExisting {
                source,
                mode,
                generation,
            } => IntakeView::Changed(applied(
                controller
                    .add_existing_folder(source, mode, generation)
                    .await
                    .map_err(problem)?,
            )?),
            IntakeQuery::StartOnboarding {
                path,
                pool,
                generation,
            } => {
                applied(
                    controller
                        .start_onboarding(path.clone(), pool, generation)
                        .await
                        .map_err(&mut problem)?,
                )?;
                let found = controller.found_projects().await.map_err(problem)?;
                IntakeView::OnboardingStarted(started(&found, &path)?)
            }
            IntakeQuery::Onboarding { project } => IntakeView::Onboarding(Box::new(
                controller.onboarding(project).await.map_err(problem)?,
            )),
            IntakeQuery::GitHubStatus => {
                IntakeView::GitHub(controller.github_status().await.map_err(problem)?)
            }
            IntakeQuery::GitHubSignIn { generation } => {
                match applied(
                    controller
                        .github_sign_in(generation)
                        .await
                        .map_err(problem)?,
                )? {
                    OwnerResult::GitHubDeviceCode { code } => IntakeView::DeviceCode(code),
                    _ => return Err("github_sign_in_outcome_unknown".into()),
                }
            }
            IntakeQuery::GitHubUseCli { generation } => {
                applied(
                    controller
                        .github_use_cli(generation)
                        .await
                        .map_err(&mut problem)?,
                )?;
                IntakeView::GitHub(controller.github_status().await.map_err(problem)?)
            }
            IntakeQuery::GitHubRepos { page } => {
                IntakeView::Repos(controller.github_repos(page).await.map_err(problem)?)
            }
            IntakeQuery::CloneRepo {
                full_name,
                generation,
            } => {
                match applied(
                    controller
                        .clone_repo(full_name, generation)
                        .await
                        .map_err(problem)?,
                )? {
                    OwnerResult::Cloned { receipt } => IntakeView::Cloned(receipt),
                    _ => return Err("clone_outcome_unknown".into()),
                }
            }
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

fn started(found: &[FoundProject], path: &str) -> Result<ProjectId, String> {
    found
        .iter()
        .find(|project| project.path == path && project.state == FoundState::Onboarding)
        .and_then(|project| project.project_id.clone())
        .ok_or_else(|| "onboarding_missing".into())
}
