use super::*;
use crate::engine::owner::AccountSecret;
use crate::github::{
    CloneReceipt, GitHubDeviceCode, GitHubOwners, GitHubPublishJob, GitHubPublishLeft,
    GitHubPublishPlan, GitHubRepoPage, GitHubStatus,
};
use crate::github::{GitHubRepoRules, GitHubRule};
use crate::onboarding::OnboardingView;
use crate::projects_home::icloud::{DOWNLOAD_TIME_MAX, Progress};
use crate::projects_home::job::{JobSubject, JobView};
use crate::projects_home::{AddMode, AddOverride, FoundProject, ProjectsHome};
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

const ICLOUD_PROGRESS_EVERY: Duration = Duration::from_secs(1);
const ICLOUD_TICKS_MAX: u64 = DOWNLOAD_TIME_MAX.as_secs() / ICLOUD_PROGRESS_EVERY.as_secs() + 120;
const _: () = assert!(ICLOUD_TICKS_MAX > DOWNLOAD_TIME_MAX.as_secs() + 60);

impl OwnerController {
    /// Reads the projects home folder and whether the engine watches it.
    /// Takes nothing; uses the attached owner connection.
    /// Returns the home; its path is none until the owner picks one.
    pub async fn projects_home(&mut self) -> Result<ProjectsHome, EngineError> {
        match self.read(OwnerRead::ProjectsHome).await?.data {
            OwnerData::ProjectsHome { home } => Ok(home),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Sets the projects home folder at first run or from Settings; nothing is moved.
    /// Takes the absolute folder path and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn set_projects_home(
        &mut self,
        path: String,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.projects_home().await?;
        self.begin_mutation(OwnerMutation::SetProjectsHome { path }, generation)?;
        self.send_pending().await
    }

    /// Lists the folders in the projects home with their Found, Missing, Onboarding or Ready state.
    /// Takes nothing.
    /// Returns the folders in the engine's order.
    pub async fn found_projects(&mut self) -> Result<Vec<FoundProject>, EngineError> {
        match self.read(OwnerRead::FoundProjects).await?.data {
            OwnerData::FoundProjects { projects } => Ok(projects),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Measures a folder before the owner copies it into the home.
    /// Takes the absolute folder path.
    /// Returns its size in bytes and how many of its entries are only in iCloud.
    pub async fn folder_size(
        &mut self,
        path: String,
    ) -> Result<crate::projects_home::Measured, EngineError> {
        match self.read(OwnerRead::FolderSize { path }).await?.data {
            OwnerData::FolderSize {
                bytes,
                placeholders,
            } => Ok(crate::projects_home::Measured {
                bytes,
                placeholders,
            }),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Moves or copies an existing folder into the projects home, where it then shows as Found.
    /// Takes the absolute source folder, Move or Copy, the soft refusals the owner chose to go
    /// past for this add, and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn add_existing_folder(
        &mut self,
        source: String,
        mode: AddMode,
        overrides: Vec<AddOverride>,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.found_projects().await?;
        self.begin_mutation(
            OwnerMutation::AddExistingFolder {
                source,
                mode,
                overrides,
            },
            generation,
        )?;
        self.send_pending().await
    }

    /// Starts onboarding a found or cloned folder: the graph builds and the mapping agent runs on the pool.
    /// Takes the folder path in the home, the pool and the client generation.
    /// Returns the actual owner response; an applied start carries the new project id.
    pub async fn start_onboarding(
        &mut self,
        path: String,
        pool: CredentialRef,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.found_projects().await?;
        self.begin_mutation(OwnerMutation::StartOnboarding { path, pool }, generation)?;
        self.send_pending().await
    }

    /// Reads one onboarding: the graph and mapping steps, the drafted setup and what is still missing.
    /// Takes the project id the folder was registered as.
    /// Returns the onboarding; the owner confirms its draft with `update_project_setup`.
    pub async fn onboarding(
        &mut self,
        project_id: ProjectId,
    ) -> Result<OnboardingView, EngineError> {
        match self.read(OwnerRead::Onboarding { project_id }).await?.data {
            OwnerData::Onboarding { onboarding } => Ok(*onboarding),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads whether CrazyAgent is signed in to GitHub, as whom, how, and any plain problem.
    /// Takes nothing.
    /// Returns the status; no token ever leaves the engine.
    pub async fn github_status(&mut self) -> Result<GitHubStatus, EngineError> {
        self.owner_client()?.github_status().await
    }

    /// Starts the GitHub device sign-in; the engine finishes it once the owner enters the code.
    /// Takes nothing.
    /// Returns the code and page the owner uses on github.com; `github_status` shows waiting until then.
    pub async fn github_sign_in(&mut self) -> Result<GitHubDeviceCode, EngineError> {
        self.owner_client()?.github_sign_in().await
    }

    /// Hands a pasted GitHub token to the engine in its own secret frame, never a plain field.
    /// Takes the token as an account secret.
    /// Returns the status after the engine checked the token with GitHub.
    pub async fn github_token(
        &mut self,
        token: AccountSecret,
    ) -> Result<GitHubStatus, EngineError> {
        self.owner_client()?.github_token(token).await
    }

    /// Reuses the GitHub sign-in of an installed and signed-in `gh` CLI.
    /// Takes nothing.
    /// Returns the new status.
    pub async fn github_use_cli(&mut self) -> Result<GitHubStatus, EngineError> {
        self.owner_client()?.github_use_cli().await
    }

    /// Lists one page of the signed-in owner's GitHub repositories, most recently pushed first.
    /// Takes the page number, from 1 to `github::REPO_PAGE_MAX`.
    /// Returns the page with the next page number, none after the last page.
    pub async fn github_repos(&mut self, page: u32) -> Result<GitHubRepoPage, EngineError> {
        if page == 0 || page > crate::github::REPO_PAGE_MAX {
            return Err(EngineError("github_page_invalid".into()));
        }
        self.owner_client()?.github_repos(page).await
    }

    /// Lists the GitHub accounts a published project's repository can go under.
    /// Takes nothing.
    /// Returns the accounts, the personal one first, and a plain problem, if any.
    pub async fn github_owners(&mut self) -> Result<GitHubOwners, EngineError> {
        self.owner_client()?.github_owners().await
    }

    /// Starts publishing a project on GitHub; the engine checks the plan, the sign-in and the git.
    /// Takes the project id and the plan the New Project publish block built.
    /// Returns the job at its .gitignore lines.
    pub async fn github_publish_start(
        &mut self,
        project: ProjectId,
        plan: GitHubPublishPlan,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.owner_client()?
            .github_publish_start(project, plan)
            .await
    }

    /// Reads where a project's GitHub publish stands. Takes the project.
    /// Returns the job, or none when the project has no publish.
    pub async fn github_publish_status(
        &mut self,
        project: ProjectId,
    ) -> Result<Option<GitHubPublishJob>, EngineError> {
        self.owner_client()?.github_publish_status(project).await
    }

    /// Approves a project's .gitignore lines and runs its publish as far as it goes.
    /// Takes the project and the lines. Returns the job where it stands.
    pub async fn github_publish_approve(
        &mut self,
        project: ProjectId,
        lines: Vec<String>,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.owner_client()?
            .github_publish_approve(project, lines)
            .await
    }

    /// Leaves a file that looks like it holds a secret out of a project's publish and runs on.
    /// Takes the project and the file's path. Returns the job where it stands.
    pub async fn github_publish_exclude(
        &mut self,
        project: ProjectId,
        path: String,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.owner_client()?
            .github_publish_exclude(project, path)
            .await
    }

    /// Checks a project's files for secrets again. Takes the project.
    /// Returns the job where it stands.
    pub async fn github_publish_recheck(
        &mut self,
        project: ProjectId,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.owner_client()?.github_publish_recheck(project).await
    }

    /// Runs a stopped publish on from the step it reached. Takes the project.
    /// Returns the job where it stands.
    pub async fn github_publish_continue(
        &mut self,
        project: ProjectId,
    ) -> Result<GitHubPublishJob, EngineError> {
        self.owner_client()?.github_publish_continue(project).await
    }

    /// Says whether the Overview asks to publish a project that has no GitHub publish: it
    /// names no git remote and the owner said neither Not now in the last seven days nor
    /// Don't ask again. Takes the project. Returns the answer.
    pub async fn github_publish_prompt(&mut self, project: ProjectId) -> Result<bool, EngineError> {
        self.owner_client()?.github_publish_prompt(project).await
    }

    /// Keeps the owner's Not now or Don't ask again for a project's publish question.
    /// Takes the project and the answer. Returns nothing.
    pub async fn github_publish_prompt_set(
        &mut self,
        project: ProjectId,
        answer: crate::github::GitHubPromptAnswer,
    ) -> Result<(), EngineError> {
        self.owner_client()?
            .github_publish_prompt_set(project, answer)
            .await
    }

    /// Forgets a project's publish before its push. Takes the project.
    /// Returns the repository GitHub made or may have made, if any.
    pub async fn github_publish_discard(
        &mut self,
        project: ProjectId,
    ) -> Result<Option<GitHubPublishLeft>, EngineError> {
        self.owner_client()?.github_publish_discard(project).await
    }

    /// Clones a GitHub repository into the projects home, where it then shows as Found.
    /// Takes the repository as owner/name.
    /// Returns the receipt with the new folder.
    pub async fn clone_repo(&mut self, full_name: String) -> Result<CloneReceipt, EngineError> {
        self.owner_client()?.clone_repo(full_name).await
    }

    /// Starts the engine's one job, which adds a folder or moves a project into the projects
    /// home apart from this client after it downloads what is only in iCloud, or attaches to
    /// the job already doing that. Takes the start's fresh id, which a job it makes carries,
    /// the subject, Move or Copy and the overrides. Returns the job, or `job_busy` while
    /// another job runs.
    pub async fn job_start(
        &mut self,
        id: RequestId,
        subject: JobSubject,
        mode: AddMode,
        overrides: Vec<AddOverride>,
    ) -> Result<JobView, EngineError> {
        let view = self
            .owner_client()?
            .job_start(id, subject, mode, overrides)
            .await?;
        assert!(!view.source.is_empty());
        Ok(view)
    }

    /// Reads the engine's job, running or last ended. Returns it, or None.
    pub async fn job_status(&mut self) -> Result<Option<JobView>, EngineError> {
        self.owner_client()?.job_status().await
    }

    /// Asks the running job to stop between files. Takes its id. Returns whether it ran.
    pub async fn job_stop(&mut self, id: RequestId) -> Result<bool, EngineError> {
        self.owner_client()?.job_stop(id).await
    }

    /// Forgets an ended job. Takes its id. Returns whether it was forgotten.
    pub async fn job_dismiss(&mut self, id: RequestId) -> Result<bool, EngineError> {
        self.owner_client()?.job_dismiss(id).await
    }

    /// Downloads every file of a folder that is only in iCloud, reading the progress about
    /// once a second while it runs and asking the engine to stop once `cancel` is set.
    /// Takes the absolute folder, the cancel flag and the progress callback.
    /// Returns how many placeholders were downloaded, or `home_icloud_not_downloaded` after
    /// a cancel, a stall or the time limit.
    pub async fn icloud_download(
        &mut self,
        path: String,
        cancel: &AtomicBool,
        mut report: impl FnMut(Progress),
    ) -> Result<u64, EngineError> {
        let client = self.owner_client()?;
        let download = client.icloud_download(path);
        tokio::pin!(download);
        let mut tick = tokio::time::interval(ICLOUD_PROGRESS_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut asked = false;
        for _ in 0..ICLOUD_TICKS_MAX {
            tokio::select! {
                outcome = &mut download => return outcome,
                _ = tick.tick() => {}
            }
            if cancel.load(Ordering::Relaxed) && !asked {
                asked = matches!(client.icloud_cancel().await, Ok(true));
            }
            if let Ok(Some(progress)) = client.icloud_progress().await {
                assert!(progress.done <= progress.total);
                report(progress);
            }
        }
        Err(EngineError("icloud_outcome_unknown".into()))
    }

    /// Reads the rules a new GitHub repository starts with. Takes nothing.
    /// Returns the rules on, DEC-182's standard setup while the owner saved none.
    pub async fn github_rule_defaults(&mut self) -> Result<BTreeSet<GitHubRule>, EngineError> {
        self.owner_client()?.github_rule_defaults().await
    }

    /// Saves the rules a new GitHub repository starts with. Takes the rules on.
    /// Returns the rules the engine read back.
    pub async fn github_rule_defaults_set(
        &mut self,
        rules: BTreeSet<GitHubRule>,
    ) -> Result<BTreeSet<GitHubRule>, EngineError> {
        self.owner_client()?.github_rule_defaults_set(rules).await
    }

    /// Reads the live rules of the repository a project was published to; sends nothing
    /// that changes it. Takes the project. Returns the page.
    pub async fn github_rules(
        &mut self,
        project: ProjectId,
    ) -> Result<GitHubRepoRules, EngineError> {
        self.owner_client()?.github_rules(project).await
    }

    /// Turns one rule on or off on the repository a project was published to, with the
    /// owner's sign-in. Takes the project, the `owner/name` the page showed, the rule and
    /// whether it is on. Returns the page read again after the change.
    pub async fn github_rule_set(
        &mut self,
        project: ProjectId,
        repo: String,
        rule: GitHubRule,
        on: bool,
    ) -> Result<GitHubRepoRules, EngineError> {
        self.owner_client()?
            .github_rule_set(project, repo, rule, on)
            .await
    }
}
