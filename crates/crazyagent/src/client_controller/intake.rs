use super::*;
use crate::engine::owner::AccountSecret;
use crate::github::{GitHubRepoPage, GitHubStatus};
use crate::onboarding::Onboarding;
use crate::projects_home::{AddMode, FoundProject, ProjectsHome};
use crate::protocol::{CredentialRef, ProjectId};

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
    /// Returns its size in bytes.
    pub async fn folder_size(&mut self, path: String) -> Result<u64, EngineError> {
        match self.read(OwnerRead::FolderSize { path }).await?.data {
            OwnerData::FolderSize { bytes } => Ok(bytes),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Moves or copies an existing folder into the projects home, where it then shows as Found.
    /// Takes the absolute source folder, Move or Copy, and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn add_existing_folder(
        &mut self,
        source: String,
        mode: AddMode,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.found_projects().await?;
        self.begin_mutation(
            OwnerMutation::AddExistingFolder { source, mode },
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
    pub async fn onboarding(&mut self, project_id: ProjectId) -> Result<Onboarding, EngineError> {
        match self.read(OwnerRead::Onboarding { project_id }).await?.data {
            OwnerData::Onboarding { onboarding } => Ok(*onboarding),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads whether CrazyAgent is signed in to GitHub, as whom and how.
    /// Takes nothing.
    /// Returns the status; no token ever leaves the engine.
    pub async fn github_status(&mut self) -> Result<GitHubStatus, EngineError> {
        match self.read(OwnerRead::GitHubStatus).await?.data {
            OwnerData::GitHubStatus { status } => Ok(status),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Starts the GitHub device sign-in.
    /// Takes the client generation.
    /// Returns the actual owner response; an applied start carries the code the owner enters on github.com.
    pub async fn github_sign_in(&mut self, generation: u64) -> Result<OwnerResponse, EngineError> {
        self.github_status().await?;
        self.begin_mutation(OwnerMutation::GitHubSignIn, generation)?;
        self.send_pending().await
    }

    /// Hands a pasted GitHub token to the engine through the private secret channel, never a plain field.
    /// Takes the token as an account secret.
    /// Returns nothing; read `github_status` for the result.
    pub async fn github_token(&mut self, token: AccountSecret) -> Result<(), EngineError> {
        if self.pending.is_some() || self.pending_account_profile.is_some() {
            return Err(EngineError("controller_pending_intent".into()));
        }
        self.client
            .as_ref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .submit_github_token(token)
            .await
    }

    /// Reuses the GitHub sign-in of an installed and signed-in `gh` CLI.
    /// Takes the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn github_use_cli(&mut self, generation: u64) -> Result<OwnerResponse, EngineError> {
        self.github_status().await?;
        self.begin_mutation(OwnerMutation::GitHubUseCli, generation)?;
        self.send_pending().await
    }

    /// Lists one page of the signed-in owner's GitHub repositories.
    /// Takes the page number, from 1 to `github::REPO_PAGE_MAX`.
    /// Returns the page with the next page number, none after the last page.
    pub async fn github_repos(&mut self, page: u32) -> Result<GitHubRepoPage, EngineError> {
        if page == 0 || page > crate::github::REPO_PAGE_MAX {
            return Err(EngineError("github_page_invalid".into()));
        }
        match self.read(OwnerRead::GitHubRepos { page }).await?.data {
            OwnerData::GitHubRepos { page } => Ok(page),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Clones a GitHub repository into the projects home, where it then shows as Found.
    /// Takes the repository as owner/name and the client generation.
    /// Returns the actual owner response; a rejection carries a plain code.
    pub async fn clone_repo(
        &mut self,
        full_name: String,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        self.github_status().await?;
        self.begin_mutation(OwnerMutation::CloneRepo { full_name }, generation)?;
        self.send_pending().await
    }
}
