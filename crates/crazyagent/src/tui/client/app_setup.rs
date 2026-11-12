use super::settings::{applied, problem};
use super::*;
use crate::cleanup_settings::CleanupSettings;
use crate::github::GitHubStatus;
use crate::master_frame_limit::MasterFrameSettings;
use crate::master_step_limit::MasterStepSettings;
use crate::master_time_limit::MasterTimeSettings;
use crate::projects_home::ProjectsHome;
use crate::quit_settings::QuitSettings;
use crate::sandbox_settings::SandboxSettings;
use crate::setup::SetupStatus;

/// One request from the setup page: setup status and runs, default-pack changes,
/// the projects home folder, the GitHub account, the sandbox settings, when finished
/// agents' worktrees go, what quitting does to team agents and the master step, time and
/// frame size limits.
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
    Sandbox,
    SetSandbox {
        settings: SandboxSettings,
        generation: u64,
    },
    Cleanup,
    SetCleanup {
        settings: CleanupSettings,
        generation: u64,
    },
    Quit,
    SetQuit {
        settings: QuitSettings,
        generation: u64,
    },
    MasterSteps,
    SetMasterSteps {
        settings: MasterStepSettings,
        generation: u64,
    },
    MasterTime,
    SetMasterTime {
        settings: MasterTimeSettings,
        generation: u64,
    },
    MasterFrames,
    SetMasterFrames {
        settings: MasterFrameSettings,
        generation: u64,
    },
}

/// One change to an installed default-pack entry at the pack version the page showed.
/// Input: built by the Settings setup page. Output: one engine change.
pub(in crate::tui) enum PackChange {
    Switch { id: String, on: bool, version: u64 },
    Remove { id: String, version: u64 },
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
    Sandbox(Result<SandboxSettings, String>),
    SandboxSet(Result<(), String>),
    Cleanup(Result<CleanupSettings, String>),
    CleanupSet(Result<(), String>),
    Quit(Result<QuitSettings, String>),
    QuitSet(Result<(), String>),
    MasterSteps(Result<MasterStepSettings, String>),
    MasterStepsSet(Result<(), String>),
    MasterTime(Result<MasterTimeSettings, String>),
    MasterTimeSet(Result<(), String>),
    MasterFrames(Result<MasterFrameSettings, String>),
    MasterFramesSet(Result<(), String>),
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
            Self::Sandbox => "Sandbox",
            Self::SetSandbox { .. } => "SetSandbox",
            Self::Cleanup => "Cleanup",
            Self::SetCleanup { .. } => "SetCleanup",
            Self::Quit => "Quit",
            Self::SetQuit { .. } => "SetQuit",
            Self::MasterSteps => "MasterSteps",
            Self::SetMasterSteps { .. } => "SetMasterSteps",
            Self::MasterTime => "MasterTime",
            Self::SetMasterTime { .. } => "SetMasterTime",
            Self::MasterFrames => "MasterFrames",
            Self::SetMasterFrames { .. } => "SetMasterFrames",
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
            AppSetupJob::Sandbox => Self::Sandbox(Err(problem)),
            AppSetupJob::SetSandbox { .. } => Self::SandboxSet(Err(problem)),
            AppSetupJob::Cleanup => Self::Cleanup(Err(problem)),
            AppSetupJob::SetCleanup { .. } => Self::CleanupSet(Err(problem)),
            AppSetupJob::Quit => Self::Quit(Err(problem)),
            AppSetupJob::SetQuit { .. } => Self::QuitSet(Err(problem)),
            AppSetupJob::MasterSteps => Self::MasterSteps(Err(problem)),
            AppSetupJob::SetMasterSteps { .. } => Self::MasterStepsSet(Err(problem)),
            AppSetupJob::MasterTime => Self::MasterTime(Err(problem)),
            AppSetupJob::SetMasterTime { .. } => Self::MasterTimeSet(Err(problem)),
            AppSetupJob::MasterFrames => Self::MasterFrames(Err(problem)),
            AppSetupJob::SetMasterFrames { .. } => Self::MasterFramesSet(Err(problem)),
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
                    PackChange::Switch { id, on, version } => {
                        controller.set_pack_entry(id, on, version, generation).await
                    }
                    PackChange::Remove { id, version } => {
                        controller.remove_pack_entry(id, version, generation).await
                    }
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
            } => AppSetupEvent::HomeSet(applied(
                controller
                    .move_projects_home(path, move_projects, generation)
                    .await,
                "home_outcome_unknown",
                lost,
            )),
            AppSetupJob::FolderSize { path } => {
                let size = controller
                    .folder_size(path.clone())
                    .await
                    .map(|measured| measured.bytes)
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
            AppSetupJob::Sandbox => AppSetupEvent::Sandbox(
                controller
                    .sandbox_settings()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::SetSandbox {
                settings,
                generation,
            } => AppSetupEvent::SandboxSet(applied(
                controller.set_sandbox_settings(settings, generation).await,
                "sandbox_outcome_unknown",
                lost,
            )),
            AppSetupJob::Cleanup => AppSetupEvent::Cleanup(
                controller
                    .cleanup_settings()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::SetCleanup {
                settings,
                generation,
            } => AppSetupEvent::CleanupSet(applied(
                controller.set_cleanup_settings(settings, generation).await,
                "cleanup_settings_outcome_unknown",
                lost,
            )),
            AppSetupJob::Quit => AppSetupEvent::Quit(
                controller
                    .quit_settings()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::SetQuit {
                settings,
                generation,
            } => AppSetupEvent::QuitSet(applied(
                controller.set_quit_settings(settings, generation).await,
                "quit_settings_outcome_unknown",
                lost,
            )),
            AppSetupJob::MasterSteps => AppSetupEvent::MasterSteps(
                controller
                    .master_step_settings()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::SetMasterSteps {
                settings,
                generation,
            } => AppSetupEvent::MasterStepsSet(applied(
                controller
                    .set_master_step_settings(settings, generation)
                    .await,
                "master_step_settings_outcome_unknown",
                lost,
            )),
            AppSetupJob::MasterTime => AppSetupEvent::MasterTime(
                controller
                    .master_time_settings()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::SetMasterTime {
                settings,
                generation,
            } => AppSetupEvent::MasterTimeSet(applied(
                controller
                    .set_master_time_settings(settings, generation)
                    .await,
                "master_time_settings_outcome_unknown",
                lost,
            )),
            AppSetupJob::MasterFrames => AppSetupEvent::MasterFrames(
                controller
                    .master_frame_settings()
                    .await
                    .map_err(|error| problem(&error, lost)),
            ),
            AppSetupJob::SetMasterFrames {
                settings,
                generation,
            } => AppSetupEvent::MasterFramesSet(applied(
                controller
                    .set_master_frame_settings(settings, generation)
                    .await,
                "master_frame_settings_outcome_unknown",
                lost,
            )),
        }
    }
}

#[cfg(all(test, unix))]
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
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        #[cfg(target_os = "macos")]
        let _keychain = {
            let mut keychain = crate::credentials::test_keychain::TestKeychain::temporary();
            keychain.serve(&crate::github::namespace(root.path()));
            keychain
        };
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
        let beside = tempfile::Builder::new()
            .prefix("ca-tsetup-home-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let folder = std::fs::canonicalize(beside.path())
            .unwrap()
            .join("Projects");
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

        let refused = ask(
            &mut native,
            AppSetupJob::Pack {
                change: PackChange::Switch {
                    id: status.pack[0].id.clone(),
                    on: true,
                    version: status.version,
                },
                generation: 2,
            },
        )
        .await;
        assert!(
            matches!(&refused, AppSetupEvent::PackChanged(Err(code)) if code == "pack_not_installed"),
            "a refused pack change keeps the engine's own code"
        );

        let AppSetupEvent::Sandbox(Ok(sandbox)) = ask(&mut native, AppSetupJob::Sandbox).await
        else {
            panic!("sandbox settings required")
        };
        assert_eq!(sandbox.linux_sockets, crate::sandbox::LinuxSockets::TcpOnly);
        for (generation, linux_sockets) in [
            (3, crate::sandbox::LinuxSockets::TcpPorts),
            (4, crate::sandbox::LinuxSockets::Refuse),
            (5, crate::sandbox::LinuxSockets::TcpOnly),
        ] {
            let settings = SandboxSettings { linux_sockets };
            let set = ask(
                &mut native,
                AppSetupJob::SetSandbox {
                    settings,
                    generation,
                },
            )
            .await;
            assert!(matches!(set, AppSetupEvent::SandboxSet(Ok(()))));
            let AppSetupEvent::Sandbox(Ok(read)) = ask(&mut native, AppSetupJob::Sandbox).await
            else {
                panic!("sandbox settings required")
            };
            assert_eq!(read, settings);
        }

        let AppSetupEvent::Quit(Ok(quit)) = ask(&mut native, AppSetupJob::Quit).await else {
            panic!("quitting setting required")
        };
        assert_eq!(quit, QuitSettings::default());
        for (generation, settings) in [
            (
                6,
                QuitSettings {
                    on_quit: crate::quit_settings::OnQuit::Stop,
                    left_running: crate::quit_settings::LeftRunning::AfterHours,
                    hours: 1,
                },
            ),
            (
                7,
                QuitSettings {
                    on_quit: crate::quit_settings::OnQuit::KeepRunning,
                    left_running: crate::quit_settings::LeftRunning::Never,
                    hours: 168,
                },
            ),
        ] {
            let set = ask(
                &mut native,
                AppSetupJob::SetQuit {
                    settings,
                    generation,
                },
            )
            .await;
            assert!(matches!(set, AppSetupEvent::QuitSet(Ok(()))));
            let AppSetupEvent::Quit(Ok(read)) = ask(&mut native, AppSetupJob::Quit).await else {
                panic!("quitting setting required")
            };
            assert_eq!(read, settings);
        }
        let refused = ask(
            &mut native,
            AppSetupJob::SetQuit {
                settings: QuitSettings {
                    hours: 0,
                    ..QuitSettings::default()
                },
                generation: 8,
            },
        )
        .await;
        assert!(
            matches!(&refused, AppSetupEvent::QuitSet(Err(code)) if code == "quit_settings_invalid"),
            "hours outside 1 to 168 keep the engine's own code"
        );

        let read_steps = AppSetupJob::MasterSteps;
        let AppSetupEvent::MasterSteps(Ok(steps)) = ask(&mut native, read_steps).await else {
            panic!("master step limit required")
        };
        assert_eq!(steps, MasterStepSettings::default());
        assert_eq!(steps.limit, crate::master_step_limit::StepLimit::NoLimit);
        let limits = crate::master_step_limit::StepLimit::ALL;
        for (generation, limit) in (9..).zip(limits.into_iter().rev()) {
            let settings = MasterStepSettings { limit };
            let set = ask(
                &mut native,
                AppSetupJob::SetMasterSteps {
                    settings,
                    generation,
                },
            )
            .await;
            assert!(matches!(set, AppSetupEvent::MasterStepsSet(Ok(()))));
            let read_steps = AppSetupJob::MasterSteps;
            let AppSetupEvent::MasterSteps(Ok(read)) = ask(&mut native, read_steps).await else {
                panic!("master step limit required")
            };
            assert_eq!(read, settings, "{limit:?} is stored as picked");
        }

        let read_time = AppSetupJob::MasterTime;
        let AppSetupEvent::MasterTime(Ok(time)) = ask(&mut native, read_time).await else {
            panic!("master time limit required")
        };
        assert_eq!(time, MasterTimeSettings::default());
        assert_eq!(time.limit, crate::master_time_limit::TimeLimit::NoLimit);
        let limits = crate::master_time_limit::TimeLimit::ALL;
        for (generation, limit) in (13..).zip(limits.into_iter().rev()) {
            let settings = MasterTimeSettings { limit };
            let set = ask(
                &mut native,
                AppSetupJob::SetMasterTime {
                    settings,
                    generation,
                },
            )
            .await;
            assert!(matches!(set, AppSetupEvent::MasterTimeSet(Ok(()))));
            let read_time = AppSetupJob::MasterTime;
            let AppSetupEvent::MasterTime(Ok(read)) = ask(&mut native, read_time).await else {
                panic!("master time limit required")
            };
            assert_eq!(read, settings, "{limit:?} is stored as picked");
        }

        let read_frames = AppSetupJob::MasterFrames;
        let AppSetupEvent::MasterFrames(Ok(frames)) = ask(&mut native, read_frames).await else {
            panic!("master frame size limit required")
        };
        assert_eq!(frames, MasterFrameSettings::default());
        assert_eq!(frames.limit, crate::master_frame_limit::FrameSize::Kib64);
        let limits = crate::master_frame_limit::FrameSize::ALL;
        for (generation, limit) in (17..).zip(limits.into_iter().rev()) {
            let settings = MasterFrameSettings { limit };
            let set = ask(
                &mut native,
                AppSetupJob::SetMasterFrames {
                    settings,
                    generation,
                },
            )
            .await;
            assert!(matches!(set, AppSetupEvent::MasterFramesSet(Ok(()))));
            let read_frames = AppSetupJob::MasterFrames;
            let AppSetupEvent::MasterFrames(Ok(read)) = ask(&mut native, read_frames).await else {
                panic!("master frame size limit required")
            };
            assert_eq!(read, settings, "{limit:?} is stored as picked");
        }

        drop(native);
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}
