use super::settings::{applied, problem};
use super::*;
use crate::github::GitHubStatus;
use crate::projects_home::ProjectsHome;
use crate::setup::SetupStatus;

/// One request from the setup page: setup status and runs, default-pack changes,
/// the projects home folder and the GitHub account.
/// Input: built by the setup page. Output: run on the owner worker; the reply
/// arrives as `SettingsEvent::AppSetup`.
pub(in crate::tui) enum AppSetupJob {
    Status,
    Run {
        pack: Vec<String>,
        generation: u64,
    },
    Pack {
        change: PackChange,
        generation: u64,
    },
    Home,
    SetHome {
        path: String,
        move_projects: bool,
        generation: u64,
    },
    FolderSize {
        path: String,
    },
    GitHub,
    GitHubSignOut,
}

/// One change to an installed default-pack entry.
/// Input: built by the Settings setup page. Output: one engine change.
pub(in crate::tui) enum PackChange {
    Switch { id: String, on: bool },
    Remove { id: String },
}

/// The engine's reply to one setup page request.
/// Input: produced on the owner worker. Output: read by the setup page.
pub(in crate::tui) enum AppSetupEvent {
    Status(Result<SetupStatus, String>),
    Ran(Result<(), String>),
    PackChanged(Result<(), String>),
    Home(Result<ProjectsHome, String>),
    HomeSet(Result<(), String>),
    FolderSize(String, Result<u64, String>),
    GitHub(Result<GitHubStatus, String>),
    GitHubSignedOut(Result<GitHubStatus, String>),
}

impl AppSetupJob {
    /// The plain name of the job for debug output; never a path or an id.
    /// Input: none. Output: the name.
    pub(in crate::tui) fn name(&self) -> &'static str {
        match self {
            Self::Status => "Status",
            Self::Run { .. } => "Run",
            Self::Pack { .. } => "Pack",
            Self::Home => "Home",
            Self::SetHome { .. } => "SetHome",
            Self::FolderSize { .. } => "FolderSize",
            Self::GitHub => "GitHub",
            Self::GitHubSignOut => "GitHubSignOut",
        }
    }
}

impl AppSetupEvent {
    /// The failure event for a job that reached no trusted connection.
    /// Inputs: the job and the plain problem code. Output: its event.
    pub(in crate::tui) fn failed(job: &AppSetupJob, problem: String) -> Self {
        match job {
            AppSetupJob::Status => Self::Status(Err(problem)),
            AppSetupJob::Run { .. } => Self::Ran(Err(problem)),
            AppSetupJob::Pack { .. } => Self::PackChanged(Err(problem)),
            AppSetupJob::Home => Self::Home(Err(problem)),
            AppSetupJob::SetHome { .. } => Self::HomeSet(Err(problem)),
            AppSetupJob::FolderSize { path } => Self::FolderSize(path.clone(), Err(problem)),
            AppSetupJob::GitHub => Self::GitHub(Err(problem)),
            AppSetupJob::GitHubSignOut => Self::GitHubSignedOut(Err(problem)),
        }
    }
}

impl NativeOwner {
    /// Runs one setup page request through the owner controller.
    /// Inputs: the job and the lost-connection flag. Output: the reply event;
    /// a refused change carries the engine's plain code.
    pub(super) async fn app_setup(&mut self, job: AppSetupJob, lost: &mut bool) -> AppSetupEvent {
        let controller = match self.settings_controller() {
            Ok(controller) => controller,
            Err(code) => return AppSetupEvent::failed(&job, code),
        };
        match job {
            AppSetupJob::Status => AppSetupEvent::Status(
                controller
                    .setup_status()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::Run { pack, generation } => AppSetupEvent::Ran(applied(
                controller.run_setup(pack, generation).await,
                "setup_outcome_unknown",
                lost,
            )),
            AppSetupJob::Pack { change, generation } => {
                let reply = match change {
                    PackChange::Switch { id, on } => {
                        controller.set_pack_entry(id, on, generation).await
                    }
                    PackChange::Remove { id } => controller.remove_pack_entry(id, generation).await,
                };
                AppSetupEvent::PackChanged(applied(reply, "pack_outcome_unknown", lost))
            }
            AppSetupJob::Home => AppSetupEvent::Home(
                controller
                    .projects_home()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::SetHome {
                path,
                move_projects,
                generation,
            } => {
                let reply = if move_projects {
                    controller.move_projects_home(path, generation).await
                } else {
                    controller.set_projects_home(path, generation).await
                };
                AppSetupEvent::HomeSet(applied(reply, "home_outcome_unknown", lost))
            }
            AppSetupJob::FolderSize { path } => {
                let size = controller
                    .folder_size(path.clone())
                    .await
                    .map_err(|error| problem(&error, lost));
                AppSetupEvent::FolderSize(path, size)
            }
            AppSetupJob::GitHub => AppSetupEvent::GitHub(
                controller
                    .github_account()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::GitHubSignOut => AppSetupEvent::GitHubSignedOut(
                controller
                    .github_sign_out()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::settings::{SettingsEvent, SettingsJob};
    use super::*;
    use crate::engine::bootstrap::EngineService;
    use crate::setup::Dependency;
    use std::os::unix::fs::PermissionsExt;

    async fn ask(native: &mut NativeOwner, job: AppSetupJob) -> AppSetupEvent {
        match native.settings(SettingsJob::AppSetup(job)).await {
            SettingsEvent::AppSetup(event) => event,
            _ => panic!("a setup page reply required"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_setup_page_reads_setup_home_and_github_and_sets_the_home() {
        let root = tempfile::Builder::new()
            .prefix("ca-tsetup-")
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

        let AppSetupEvent::Status(Ok(status)) = ask(&mut native, AppSetupJob::Status).await else {
            panic!("setup status required")
        };
        let tools: Vec<Dependency> = status.dependencies.iter().map(|tool| tool.name).collect();
        assert_eq!(tools, Dependency::ALL);
        assert!(!status.pack.is_empty());
        assert!(!status.running);

        let AppSetupEvent::Home(Ok(home)) = ask(&mut native, AppSetupJob::Home).await else {
            panic!("projects home required")
        };
        assert_eq!(home.path, None);
        let folder = root.path().join("Projects");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("notes.txt"), "ten bytes!").unwrap();
        let path = folder.display().to_string();
        let set = ask(
            &mut native,
            AppSetupJob::SetHome {
                path: path.clone(),
                move_projects: false,
                generation: 1,
            },
        )
        .await;
        assert!(matches!(set, AppSetupEvent::HomeSet(Ok(()))));
        let AppSetupEvent::Home(Ok(home)) = ask(&mut native, AppSetupJob::Home).await else {
            panic!("projects home required")
        };
        assert_eq!(home.path.as_deref(), Some(path.as_str()));
        assert!(
            folder.join("notes.txt").exists(),
            "setting the home moves nothing"
        );

        let AppSetupEvent::FolderSize(measured, Ok(bytes)) =
            ask(&mut native, AppSetupJob::FolderSize { path: path.clone() }).await
        else {
            panic!("folder size required")
        };
        assert_eq!(measured, path);
        assert!(bytes >= 10);

        let AppSetupEvent::GitHub(Ok(github)) = ask(&mut native, AppSetupJob::GitHub).await else {
            panic!("GitHub status required")
        };
        assert!(!github.signed_in);
        assert_eq!(github.login, None);

        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}
