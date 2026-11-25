mod owner_git_rows {
    use crate::sandbox::windows_farm as farm;
    use crazyagent_win_sandbox::request::{PROFILE_FLAG, SEPARATOR};
    use kernel_sandbox::paths::canonical;
    use kernel_sandbox::windows::GRANTS_FOLDER;
    use kernel_sandbox::windows::owner_git::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct Made {
        _temp: tempfile::TempDir,
        root: PathBuf,
        project: PathBuf,
        folder: PathBuf,
        git: PathBuf,
        grants: PathBuf,
    }

    /// A project, an owner-git folder, a grants folder and a stand-in for Git for Windows'
    /// `cmd\git.exe` beside its `mingw64\bin\git.exe`, all in one temp folder. Output: the folders.
    fn made() -> Made {
        let temp = tempfile::tempdir().unwrap();
        let root = canonical(temp.path()).unwrap();
        let (project, folder) = (root.join("project"), root.join(OWNER_GIT_FOLDER));
        let (git, grants) = (
            root.join("Git").join("cmd").join("git.exe"),
            root.join(GRANTS_FOLDER),
        );
        let platform = root.join("Git").join("mingw64").join("bin").join("git.exe");
        for made in [
            &project,
            git.parent().unwrap(),
            &grants,
            platform.parent().unwrap(),
        ] {
            fs::create_dir_all(made).unwrap();
        }
        fs::write(&git, "").unwrap();
        fs::write(&platform, "").unwrap();
        Made {
            _temp: temp,
            root,
            project,
            folder,
            git,
            grants,
        }
    }

    fn installed(made: &Made, project: &Path) -> OwnerGit {
        let helper = std::env::current_exe().unwrap();
        OwnerGit::install(
            &crate::sandbox::PROFILE_HOOKS,
            &made.folder,
            project,
            &made.git,
            &helper,
            &made.grants,
        )
        .unwrap()
    }

    #[test]
    fn the_git_the_engine_finds_on_the_path_is_one_owner_git_installs_for() {
        let git = crate::github::git_program(std::env::var_os("PATH").as_deref())
            .expect("git on the runner's PATH");
        let root = install_root(&git);
        assert!(
            root.is_some_and(marked),
            "git {} gives the install folder {root:?}",
            git.display()
        );
    }

    /// A share row waits while another holds the runner's sessions, so no
    /// reset or session read of one row falls inside another's; the waiting hold sees the
    /// first row's work already finished.
    #[test]
    fn a_share_row_waits_while_another_holds_the_runner_sessions() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let inside = Arc::new(AtomicBool::new(true));
        let first = farm::hold();
        let waiting = {
            let inside = inside.clone();
            std::thread::spawn(move || {
                let _second = farm::hold();
                inside.load(Ordering::SeqCst)
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        inside.store(false, Ordering::SeqCst);
        drop(first);
        assert!(
            !waiting.join().unwrap(),
            "the second hold began while the first row was inside"
        );
    }

    #[test]
    fn a_wrapped_git_starts_through_the_helper_in_its_own_home_with_no_global_config() {
        let made = made();
        let owner = installed(&made, &made.project);
        let folder = canonical(&made.folder).unwrap();
        let places = places(&folder, &made.project);
        let mut git = Command::new(&made.git);
        git.env("LC_ALL", "C")
            .env(GLOBAL_CONFIG, kernel_common::names::NULL_DEVICE)
            .env("GIT_COMMON_DIR", made.project.join(".git"))
            .envs(crate::research::runner::GIT_SAFE_ENV)
            .env("HOME", made.root.join("publish-home"))
            .env("SystemRoot", r"C:\Elsewhere")
            .env_remove("GIT_DIR")
            .current_dir(&made.project)
            .args(["ls-files", "-z", "--cached"]);
        let wrapped = owner.wrap(&git).unwrap();
        let args: Vec<&OsStr> = wrapped.get_args().collect();
        assert_eq!(
            args,
            [
                OsStr::new(PROFILE_FLAG),
                owner.profile().as_os_str(),
                OsStr::new(SEPARATOR),
                made.git.as_os_str(),
                OsStr::new("ls-files"),
                OsStr::new("-z"),
                OsStr::new("--cached"),
            ]
        );
        assert_eq!(wrapped.get_current_dir(), Some(made.project.as_path()));
        let env = |name: &str| {
            wrapped
                .get_envs()
                .find(|(known, _)| *known == OsStr::new(name))
                .and_then(|(_, value)| value)
        };
        assert_eq!(env("HOME"), Some(places.home.as_os_str()));
        assert_eq!(env("USERPROFILE"), Some(places.home.as_os_str()));
        assert_eq!(env("TEMP"), Some(places.tmp.as_os_str()));
        assert_eq!(env(GLOBAL_CONFIG), Some(OsStr::new("/dev/null")));
        assert_eq!(
            env("GIT_COMMON_DIR"),
            Some(made.project.join(".git").as_os_str())
        );
        assert_eq!(env("GIT_NO_LAZY_FETCH"), Some(OsStr::new("1")));
        assert_eq!(env("LC_ALL"), Some(OsStr::new("C")));
        assert_ne!(env("SystemRoot"), Some(OsStr::new(r"C:\Elsewhere")));
        assert_eq!(env("GIT_DIR"), None);
    }
}
