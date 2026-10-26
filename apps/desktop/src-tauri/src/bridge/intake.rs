use super::{ActionOutcome, BridgeError, IntentReceipt, NativeBridge, Unlocked, bounded, failure};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{AccountSecret, OwnerClient, OwnerData, OwnerMutation, OwnerRead};
use crazyagent::github::{CloneReceipt, GitHubDeviceCode, GitHubRepoPage, GitHubStatus};
use crazyagent::onboarding::OnboardingView;
use crazyagent::projects_home::{AddMode, FoundProject, ProjectsHome};
use crazyagent::protocol::{CredentialRef, ProjectId};
use crazyagent::setup::SetupStatus;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

pub const FOUND_PROJECTS_MAX: usize = 500;
pub const GIT_CONFIG_BYTES_MAX: u64 = 64 * 1024;
pub const GITHUB_NAME_PART_MAX: usize = 100;
pub const GITHUB_PAGE_MAX: u32 = 100;
pub const PACK_TICKS_MAX: usize = 64;
pub const PACK_ID_BYTES_MAX: usize = 64;

pub const INTAKE_CODES: [&str; 50] = [
    "projects_home_unset",
    "projects_home_path_invalid",
    "projects_home_missing",
    "projects_home_not_folder",
    "projects_home_unsafe",
    "projects_home_unreadable",
    "projects_home_watch_failed",
    "home_source_invalid",
    "home_source_missing",
    "home_source_not_folder",
    "home_source_in_home",
    "home_source_unsafe",
    "home_source_hidden",
    "home_source_name",
    "home_destination_exists",
    "home_copy_special_file",
    "home_copy_too_large",
    "home_copy_failed",
    "home_copy_unverified",
    "home_move_failed",
    "home_move_source_left",
    "home_move_same",
    "home_move_nested",
    "home_move_partial",
    "home_move_interrupted",
    "home_add_unverified",
    "home_add_interrupted",
    "home_record_invalid",
    "folder_size_path_invalid",
    "folder_size_unsafe",
    "folder_size_unreadable",
    "folder_size_timeout",
    "folder_size_too_large",
    "folder_size_interrupted",
    "crazyagent_folder_invalid",
    "crazyagent_folder_unwritable",
    "git_exclude_invalid",
    "owner_home_unknown",
    "onboarding_folder_missing",
    "onboarding_folder_unsafe",
    "onboarding_layout_incomplete",
    "onboarding_path_invalid",
    "onboarding_path_not_canonical",
    "onboarding_project_mismatch",
    "onboarding_step_invalid",
    "project_exists",
    "project_setup_missing",
    "project_missing",
    "project_folder_missing",
    "pool_missing",
];

const GITHUB_CODES: [&str; 35] = [
    "github_not_signed_in",
    "github_token_invalid",
    "github_token_refused",
    "github_sign_in_expired",
    "github_sign_in_denied",
    "github_sign_in_failed",
    "github_sign_in_cancelled",
    "github_device_flow_refused",
    "github_client_id_not_configured",
    "github_client_id_refused",
    "github_cli_missing",
    "github_cli_not_signed_in",
    "github_unreachable",
    "github_reply_invalid",
    "github_config_unsafe",
    "github_config_invalid",
    "github_state_invalid",
    "github_state_unavailable",
    "credential_missing",
    "credential_store_unavailable",
    "github_repo_name_invalid",
    "github_repo_page_invalid",
    "clone_destination_exists",
    "github_clone_failed",
    "github_clone_timed_out",
    "projects_home_not_set",
    "projects_home_unset",
    "github_outcome_unknown",
    "github_owner_unavailable",
    "github_request_bounds",
    "github_reply_bounds",
    "github_request_invalid",
    "github_token_frame_invalid",
    "github_token_frame_mismatch",
    "not_connected",
];

const SETUP_CODES: [&str; 33] = [
    "owner_outcome_unknown_reconcile_same_identity",
    "setup_checksum_mismatch",
    "setup_download_failed",
    "setup_homebrew_missing",
    "setup_install_failed",
    "setup_timeout",
    "setup_version_mismatch",
    "setup_version_unreadable",
    "setup_uv_missing",
    "setup_store_unavailable",
    "setup_pin_invalid",
    "setup_busy",
    "setup_readonly",
    "setup_tool_unsupported",
    "setup_request_bounds",
    "setup_request_invalid",
    "setup_reply_bounds",
    "setup_owner_unavailable",
    "setup_outcome_unknown",
    "pack_commit_mismatch",
    "pack_fetch_failed",
    "pack_no_skills",
    "pack_server_invalid",
    "pack_server_command_missing",
    "pack_bundled_unavailable",
    "pack_unknown",
    "pack_name_taken",
    "pack_not_installed",
    "pack_id_invalid",
    "pack_too_many",
    "not_connected",
    "busy",
    "owner_unavailable",
];

/// A folder's size before it is copied in, and the GitHub owner/name of its `origin`
/// remote when that remote is on github.com, so the window can suggest a clone instead.
#[derive(Debug, Serialize)]
pub struct FolderSize {
    pub bytes: u64,
    pub github: Option<String>,
}

fn known(code: &str, list: &[&'static str], fallback: &'static str) -> BridgeError {
    let code = if code == "controller_disconnected" {
        "not_connected"
    } else {
        code
    };
    failure(
        list.iter()
            .copied()
            .find(|item| *item == code)
            .unwrap_or(fallback),
    )
}

/// Names a projects home or onboarding refusal the window has a sentence for.
/// Input: an engine code. Output: the same code as a static name, or None.
pub fn intake_code(code: &str) -> Option<&'static str> {
    INTAKE_CODES.iter().copied().find(|item| *item == code)
}

fn intake_failure(error: EngineError) -> BridgeError {
    known(&error.0, &INTAKE_CODES, "owner_read_failed")
}

fn github_failure(error: EngineError) -> BridgeError {
    known(&error.0, &GITHUB_CODES, "github_failed")
}

fn setup_failure(error: EngineError) -> BridgeError {
    known(&error.0, &SETUP_CODES, "setup_failed")
}

fn project(id: &str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("invalid_project"))
}

fn pack_ids(pack: Vec<String>) -> Result<Vec<String>, BridgeError> {
    if pack.len() > PACK_TICKS_MAX {
        return Err(failure("pack_too_many"));
    }
    if pack.iter().any(|id| {
        id.is_empty()
            || id.len() > PACK_ID_BYTES_MAX
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    }) {
        return Err(failure("pack_id_invalid"));
    }
    Ok(pack)
}

fn plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_dir())
}

fn without_user(rest: &str) -> &str {
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    }
}

fn name_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= GITHUB_NAME_PART_MAX
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Finds the GitHub owner/name in one remote URL, dropping any user name or token in it.
/// Input: the URL text from a git config. Output: "owner/name" for a github.com remote over
/// https, ssh or scp syntax, or None for anything else.
pub fn github_name(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        without_user(rest)
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        without_user(rest)
    } else {
        url.strip_prefix("git@")?
    };
    let path = rest
        .strip_prefix("github.com/")
        .or_else(|| rest.strip_prefix("github.com:"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if !name_part(owner) || !name_part(name) {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Reads the `origin` remote of a folder's own git config and names its GitHub repository.
/// Input: the folder. Output: "owner/name" when `.git/config` is a plain bounded file whose
/// `[remote "origin"]` url points at github.com, otherwise None.
pub fn origin_on_github(folder: &Path) -> Option<String> {
    let git = folder.join(".git");
    if !plain_dir(&git) {
        return None;
    }
    let text = crazyagent::platform::read_plain_file(&git.join("config"), GIT_CONFIG_BYTES_MAX)?;
    let mut origin = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            origin = line == "[remote \"origin\"]";
            continue;
        }
        if !origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "url" {
            return github_name(value);
        }
    }
    None
}

impl NativeBridge {
    async fn owner_read(&mut self, query: OwnerRead) -> Result<OwnerData, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        Ok(self
            .controller()?
            .read(query)
            .await
            .map_err(intake_failure)?
            .data)
    }

    async fn home_change(
        &mut self,
        mutation: OwnerMutation,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        self.ready_for_intent(generation)?;
        self.owner_read(OwnerRead::ProjectsHome).await?;
        self.controller()?
            .begin_mutation(mutation, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        self.deliver_intent(false).await
    }

    /// Reads the projects home folder and whether the engine watches it.
    /// Input: none. Output: the folder, None before the owner picked one.
    pub async fn projects_home(&mut self) -> Result<ProjectsHome, BridgeError> {
        match self.owner_read(OwnerRead::ProjectsHome).await? {
            OwnerData::ProjectsHome { home } => bounded(home),
            _ => Err(failure("owner_read_failed")),
        }
    }

    /// Picks the projects home folder; nothing already on disk moves.
    /// Inputs: an absolute folder path and the client generation. Output: the receipt.
    pub async fn set_projects_home(
        &mut self,
        path: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        crazyagent::projects_home::path_text(&path, "projects_home_path_invalid")
            .map_err(intake_failure)?;
        self.home_change(OwnerMutation::SetProjectsHome { path }, generation)
            .await
    }

    /// Lists the folders in the projects home and set-up projects whose folder vanished.
    /// Input: none. Output: at most FOUND_PROJECTS_MAX entries in engine order.
    pub async fn found_projects(&mut self) -> Result<Vec<FoundProject>, BridgeError> {
        match self.owner_read(OwnerRead::FoundProjects).await? {
            OwnerData::FoundProjects { mut projects } => {
                projects.truncate(FOUND_PROJECTS_MAX);
                bounded(projects)
            }
            _ => Err(failure("owner_read_failed")),
        }
    }

    /// Measures a folder before Copy and names its GitHub repository when it has one.
    /// Input: an absolute folder path. Output: its size and the GitHub owner/name, if any.
    pub async fn folder_size(&mut self, path: String) -> Result<FolderSize, BridgeError> {
        crazyagent::projects_home::path_text(&path, "folder_size_path_invalid")
            .map_err(intake_failure)?;
        let github = origin_on_github(Path::new(&path));
        match self.owner_read(OwnerRead::FolderSize { path }).await? {
            OwnerData::FolderSize { bytes } => bounded(FolderSize { bytes, github }),
            _ => Err(failure("owner_read_failed")),
        }
    }

    /// Moves or copies an existing folder into the projects home.
    /// Inputs: the absolute source folder, move or copy and the client generation.
    /// Output: the receipt; the folder then shows as found.
    pub async fn add_existing_folder(
        &mut self,
        source: String,
        mode: AddMode,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        crazyagent::projects_home::path_text(&source, "home_source_invalid")
            .map_err(intake_failure)?;
        self.home_change(
            OwnerMutation::AddExistingFolder { source, mode },
            generation,
        )
        .await
    }

    /// Starts onboarding a found folder on a pool: graph first, then the mapping agent.
    /// Inputs: the folder path, the pool id and the client generation. Output: the receipt.
    pub async fn start_onboarding(
        &mut self,
        path: String,
        pool: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        crazyagent::projects_home::path_text(&path, "onboarding_path_invalid")
            .map_err(intake_failure)?;
        let pool = CredentialRef::new(pool).map_err(|_| failure("pool_missing"))?;
        self.home_change(OwnerMutation::StartOnboarding { path, pool }, generation)
            .await
    }

    /// Reads one project's onboarding as the engine serves it.
    /// Input: the project id. Output: the steps, the draft, the facts, what is missing and
    /// the mapping agent's overview once it exists.
    pub async fn onboarding(&mut self, id: &str) -> Result<OnboardingView, BridgeError> {
        let project_id = project(id)?;
        match self
            .owner_read(OwnerRead::Onboarding { project_id })
            .await?
        {
            OwnerData::Onboarding { onboarding } => bounded(*onboarding),
            _ => Err(failure("owner_read_failed")),
        }
    }

    fn github_client(&mut self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller()?.shared_client().map_err(github_failure)
    }

    /// Reads the GitHub sign-in: signed in or not, login, method and any plain problem.
    pub fn github_status(&mut self) -> Result<Unlocked<GitHubStatus>, BridgeError> {
        let client = self.github_client()?;
        Ok(Box::pin(async move {
            bounded(client.github_status().await.map_err(github_failure)?)
        }))
    }

    /// Starts the GitHub device sign-in. Output: the code, its page and lifetime.
    pub fn github_sign_in(&mut self) -> Result<Unlocked<GitHubDeviceCode>, BridgeError> {
        let client = self.github_client()?;
        Ok(Box::pin(async move {
            bounded(client.github_sign_in().await.map_err(github_failure)?)
        }))
    }

    /// Sends a pasted GitHub token once through the engine's secret channel; it is never
    /// logged or returned. Input: the token. Output: the status after GitHub checked it.
    pub fn github_token(&mut self, token: String) -> Result<Unlocked<GitHubStatus>, BridgeError> {
        let token = token.trim();
        if token.is_empty() || token.len() > 255 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(failure("github_token_invalid"));
        }
        let secret = AccountSecret::new(token.as_bytes().to_vec())
            .map_err(|_| failure("github_token_invalid"))?;
        let client = self.github_client()?;
        Ok(Box::pin(async move {
            bounded(client.github_token(secret).await.map_err(github_failure)?)
        }))
    }

    /// Reuses a signed-in GitHub CLI on this machine. Output: the new status.
    pub fn github_use_cli(&mut self) -> Result<Unlocked<GitHubStatus>, BridgeError> {
        let client = self.github_client()?;
        Ok(Box::pin(async move {
            bounded(client.github_use_cli().await.map_err(github_failure)?)
        }))
    }

    /// Signs CrazyAgent out of GitHub and deletes its keychain item. Output: the new status.
    pub fn github_sign_out(&mut self) -> Result<Unlocked<GitHubStatus>, BridgeError> {
        let client = self.github_client()?;
        Ok(Box::pin(async move {
            bounded(client.github_sign_out().await.map_err(github_failure)?)
        }))
    }

    /// Lists one page of the signed-in owner's repositories. Input: the page, from 1.
    pub fn github_repos(&mut self, page: u32) -> Result<Unlocked<GitHubRepoPage>, BridgeError> {
        if page == 0 || page > GITHUB_PAGE_MAX {
            return Err(failure("github_repo_page_invalid"));
        }
        let client = self.github_client()?;
        Ok(Box::pin(async move {
            bounded(client.github_repos(page).await.map_err(github_failure)?)
        }))
    }

    /// Clones one repository into the projects home through the engine's credential helper.
    /// Input: "owner/name". Output: the repository and the folder it landed in.
    pub fn clone_repo(&mut self, full_name: String) -> Result<Unlocked<CloneReceipt>, BridgeError> {
        let valid = full_name
            .split_once('/')
            .is_some_and(|(owner, name)| name_part(owner) && name_part(name));
        if !valid {
            return Err(failure("github_repo_name_invalid"));
        }
        let client = self.github_client()?;
        Ok(Box::pin(async move {
            bounded(client.clone_repo(full_name).await.map_err(github_failure)?)
        }))
    }

    async fn setup_change(
        &mut self,
        mutation: OwnerMutation,
        generation: u64,
    ) -> Result<SetupStatus, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.ready_for_intent(generation)?;
        self.controller()?
            .setup_status()
            .await
            .map_err(setup_failure)?;
        self.controller()?
            .begin_mutation(mutation, generation)
            .map_err(|_| failure("owner_not_ready"))?;
        let receipt = self.deliver_intent(false).await?;
        match receipt.outcome {
            ActionOutcome::Applied => self.setup_status().await,
            ActionOutcome::Rejected => Err(known(
                receipt.rejection.unwrap_or("setup_failed"),
                &SETUP_CODES,
                "setup_failed",
            )),
            _ => Err(failure("setup_outcome_unknown")),
        }
    }

    /// Reads setup: each dependency, each default-pack entry and whether setup is running.
    pub async fn setup_status(&mut self) -> Result<SetupStatus, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let status = self
            .controller()?
            .setup_status()
            .await
            .map_err(setup_failure)?;
        bounded(status)
    }

    /// Starts setup with the ticked pack entries as one owner intent; the ticked MCP servers
    /// count as approved. Inputs: the ticked entry ids and a fresh draft generation.
    /// Output: the status as the run starts; a refusal comes back as its plain code.
    pub async fn run_setup(
        &mut self,
        pack: Vec<String>,
        generation: u64,
    ) -> Result<SetupStatus, BridgeError> {
        let pack = pack_ids(pack)?;
        self.setup_change(OwnerMutation::RunSetup { pack }, generation)
            .await
    }

    /// Switches one installed pack entry on or off as one owner intent.
    /// Inputs: its id, the state and a fresh draft generation. Output: the new status.
    pub async fn set_pack_entry(
        &mut self,
        id: String,
        on: bool,
        generation: u64,
    ) -> Result<SetupStatus, BridgeError> {
        let id = pack_ids(vec![id])?.remove(0);
        self.setup_change(OwnerMutation::SetPackEntry { id, on }, generation)
            .await
    }

    /// Removes one installed pack entry and what it added, as one owner intent.
    /// Inputs: its id and a fresh draft generation. Output: the new status.
    pub async fn remove_pack_entry(
        &mut self,
        id: String,
        generation: u64,
    ) -> Result<SetupStatus, BridgeError> {
        let id = pack_ids(vec![id])?.remove(0);
        self.setup_change(OwnerMutation::RemovePackEntry { id }, generation)
            .await
    }

    /// Moves every project folder from the current projects home into a new one, after the
    /// owner saw the size; SetProjectsHome alone moves nothing. Inputs: the new absolute
    /// folder and the client generation. Output: the receipt.
    pub async fn move_projects_home(
        &mut self,
        path: String,
        generation: u64,
    ) -> Result<IntentReceipt, BridgeError> {
        crazyagent::projects_home::path_text(&path, "projects_home_path_invalid")
            .map_err(intake_failure)?;
        self.home_change(
            OwnerMutation::MoveProjectsHome {
                path,
                move_projects: true,
            },
            generation,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn github_names_come_from_https_ssh_and_scp_remotes_without_credentials() {
        assert_eq!(
            github_name("https://github.com/mira/plant-shop.git").as_deref(),
            Some("mira/plant-shop")
        );
        assert_eq!(
            github_name("https://github.com/mira/plant-shop").as_deref(),
            Some("mira/plant-shop")
        );
        assert_eq!(
            github_name("https://x-access-token:ghp_secret@github.com/mira/shop.git").as_deref(),
            Some("mira/shop")
        );
        assert_eq!(
            github_name("git@github.com:mira/shop.git").as_deref(),
            Some("mira/shop")
        );
        assert_eq!(
            github_name("ssh://git@github.com/mira/shop").as_deref(),
            Some("mira/shop")
        );
        assert_eq!(github_name("https://gitlab.com/mira/shop.git"), None);
        assert_eq!(github_name("https://github.com.evil.test/mira/shop"), None);
        assert_eq!(github_name("https://github.com/mira"), None);
        assert_eq!(github_name("https://github.com/mira/../etc"), None);
        assert_eq!(github_name("https://github.com/mi ra/shop"), None);
        assert_eq!(github_name("https://evil.test/@github.com/mira/shop"), None);
        assert_eq!(github_name("file:///Users/mira/shop"), None);
    }

    #[test]
    fn only_the_origin_remote_of_a_real_git_config_names_the_repository() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(origin_on_github(folder.path()), None);
        fs::create_dir(folder.path().join(".git")).unwrap();
        fs::write(
            folder.path().join(".git/config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://github.com/other/shop.git\n[remote \"origin\"]\n\turl = git@github.com:mira/shop.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();
        assert_eq!(
            origin_on_github(folder.path()).as_deref(),
            Some("mira/shop")
        );
        fs::write(
            folder.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = https://example.com/mira/shop.git\n",
        )
        .unwrap();
        assert_eq!(origin_on_github(folder.path()), None);
        fs::write(
            folder.path().join(".git/config"),
            vec![b'#'; (GIT_CONFIG_BYTES_MAX + 1) as usize],
        )
        .unwrap();
        assert_eq!(origin_on_github(folder.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_git_config_swapped_for_a_link_after_the_check_is_never_followed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let folder = tempfile::tempdir().unwrap();
        let git = folder.path().join(".git");
        fs::create_dir(&git).unwrap();
        let elsewhere = folder.path().join("elsewhere.conf");
        fs::write(
            &elsewhere,
            "[remote \"origin\"]\n\turl = https://github.com/other/secret.git\n",
        )
        .unwrap();
        let plain = "[remote \"origin\"]\n\turl = https://github.com/mira/shop.git\n";
        let config = git.join("config");
        fs::write(&config, plain).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let stop = Arc::clone(&stop);
            let (git, config, elsewhere) = (git.clone(), config.clone(), elsewhere.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let link = git.join("staged-link");
                    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
                    fs::rename(&link, &config).unwrap();
                    let file = git.join("staged-plain");
                    fs::write(&file, plain).unwrap();
                    fs::rename(&file, &config).unwrap();
                }
            })
        };
        let (mut read, mut followed) = (0, 0);
        for _ in 0..20_000 {
            match origin_on_github(folder.path()).as_deref() {
                Some("mira/shop") => read += 1,
                Some("other/secret") => followed += 1,
                _ => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();
        assert!(read > 0, "the plain config was read while the swap ran");
        assert_eq!(
            followed, 0,
            "a link swapped in after the check is never followed ({read} plain reads)"
        );
    }

    #[test]
    fn a_started_onboarding_names_the_project_the_engine_registers() {
        assert_eq!(
            crate::bridge::onboarded_project("/Users/mira/Projects/Plant Shop").as_deref(),
            Some("plant-shop")
        );
        assert_eq!(
            crate::bridge::onboarded_project("/Users/mira/Projects/---").as_deref(),
            Some("project")
        );
        assert_eq!(crate::bridge::onboarded_project("/"), None);
    }

    #[test]
    fn pack_ids_are_bounded_plain_names() {
        assert_eq!(
            pack_ids(vec!["context-mode".into(), "i_have_adhd".into()])
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            pack_ids(vec!["".into()]).unwrap_err().code,
            "pack_id_invalid"
        );
        assert_eq!(
            pack_ids(vec!["../x".into()]).unwrap_err().code,
            "pack_id_invalid"
        );
        assert_eq!(
            pack_ids(vec!["Upper".into()]).unwrap_err().code,
            "pack_id_invalid"
        );
        assert_eq!(
            pack_ids(vec!["a".repeat(65)]).unwrap_err().code,
            "pack_id_invalid"
        );
        assert_eq!(
            pack_ids(vec!["a".into(); 65]).unwrap_err().code,
            "pack_too_many"
        );
    }

    #[test]
    fn engine_codes_map_to_known_plain_codes_only() {
        assert_eq!(
            setup_failure(EngineError("setup_busy".into())).code,
            "setup_busy"
        );
        assert_eq!(
            setup_failure(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        assert_eq!(
            setup_failure(EngineError("raw /Users/mira/token".into())).code,
            "setup_failed"
        );
        assert_eq!(
            github_failure(EngineError("github_sign_in_expired".into())).code,
            "github_sign_in_expired"
        );
        assert_eq!(
            github_failure(EngineError("ghp_leaked".into())).code,
            "github_failed"
        );
        assert_eq!(
            intake_failure(EngineError("home_destination_exists".into())).code,
            "home_destination_exists"
        );
        assert_eq!(
            intake_failure(EngineError("weird".into())).code,
            "owner_read_failed"
        );
        assert_eq!(intake_code("home_move_partial"), Some("home_move_partial"));
        assert_eq!(intake_code("folder_name_taken"), None);
        assert_eq!(
            github_failure(EngineError("clone_destination_exists".into())).code,
            "clone_destination_exists"
        );
    }
}
