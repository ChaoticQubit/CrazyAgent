mod profile_rows {
    use crate::error::SandboxError;
    use crate::net::{SandboxNetwork, SandboxRemote};
    use crate::paths::canonical;
    use crate::profile::ProjectSandbox;
    use crate::profile::command::Sandbox;
    use crate::profile::env::Handed;
    use crate::profile::installed::InstalledSandbox;
    use crate::settings::{LinuxStrict, LinuxSupervisor};
    use crate::windows::profile::*;
    use crate::windows::{APPCONTAINER_MISSING, CONTAINER_PREFIX, HELPER_NAME};
    use crazyagent_win_sandbox::profile::{
        BoundaryProfile, Capability, Guard, Guarded, check_container,
    };
    use crazyagent_win_sandbox::request::{PROFILE_FLAG, SEPARATOR};
    use std::ffi::OsStr;
    use std::fs;
    use std::io::Write;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    struct Folders {
        _temp: tempfile::TempDir,
        project: PathBuf,
        home: PathBuf,
        tmp: PathBuf,
        profiles: PathBuf,
        grants: PathBuf,
    }

    fn folders() -> Folders {
        let temp = tempfile::tempdir().unwrap();
        let root = canonical(temp.path()).unwrap();
        let project = root.join("project");
        let home = root.join("agent").join("home");
        let tmp = root.join("agent").join("tmp");
        let profiles = root.join("profiles");
        let grants = root.join("grants");
        for folder in [
            &project,
            &project.join("context"),
            &home,
            &tmp,
            &profiles,
            &grants,
        ] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(project.join("context").join("brief.md"), "brief\r\n").unwrap();
        Folders {
            _temp: temp,
            project,
            home,
            tmp,
            profiles,
            grants,
        }
    }

    fn policy(folders: &Folders, network: SandboxNetwork) -> ProjectSandbox {
        ProjectSandbox::new(
            &crate::test_hooks::STAND_IN,
            &folders.project,
            &folders.home,
            &folders.tmp,
            network,
        )
        .unwrap()
    }

    fn cmd() -> PathBuf {
        PathBuf::from(system_root())
            .join("System32")
            .join("cmd.exe")
    }

    #[test]
    fn a_host_or_port_limit_widens_to_the_internet_client_and_a_loopback_port_is_refused() {
        let client = vec![Capability::InternetClient];
        assert_eq!(capabilities(&SandboxNetwork::Open).unwrap(), client);
        assert!(
            capabilities(&SandboxNetwork::Only(Vec::new()))
                .unwrap()
                .is_empty()
        );
        for hosts in [
            vec![SandboxRemote::AnyHost(443)],
            vec![SandboxRemote::AnyHost(443), SandboxRemote::AnyHost(8443)],
        ] {
            assert_eq!(
                capabilities(&SandboxNetwork::Only(hosts)).unwrap(),
                client,
                "a host or port limit is widened to the internet client on Windows (O2a)"
            );
        }
        for remotes in [
            vec![SandboxRemote::Loopback(11_434)],
            vec![SandboxRemote::AnyHost(443), SandboxRemote::Loopback(41_300)],
        ] {
            let refusal = capabilities(&SandboxNetwork::Only(remotes)).unwrap_err();
            assert!(
                crate::lines::model_loopback_windows(&refusal),
                "expected the loopback refusal, got {refusal:?}"
            );
        }
        let model_only = crate::policy::AgentNetwork::None
            .reach("windows", true, SandboxRemote::AnyHost(443), None)
            .unwrap();
        assert_eq!(capabilities(&model_only).unwrap(), client);
    }

    #[test]
    fn the_profile_keeps_the_policy_and_each_agent_gets_its_own_container() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Open);
        let profile = boundary_profile(&sandbox, &folders.grants).unwrap();
        assert_eq!(profile.capabilities, vec![Capability::InternetClient]);
        assert_eq!(profile.grants, folders.grants.to_str().unwrap());
        assert_eq!(
            profile.write_roots,
            [&folders.project, &folders.home, &folders.tmp]
                .map(|path| path.to_str().unwrap().to_owned())
        );
        let config_lock = sandbox.project_root().join(".git").join(GIT_CONFIG_LOCK);
        for protected in sandbox.protected_paths() {
            let guarded = Guarded {
                path: protected.to_str().unwrap().to_owned(),
                guard: Guard::ReadOnly,
            };
            assert_eq!(profile.guarded.contains(&guarded), protected != config_lock);
        }
        assert!(
            profile
                .guarded
                .iter()
                .any(|item| item.guard == Guard::Hidden && item.path.ends_with(r"\.ssh"))
        );
        assert_eq!(check_container(&profile.container), Ok(()));
        assert!(profile.container.starts_with(CONTAINER_PREFIX));
        let other_home = folders.home.with_file_name("other");
        fs::create_dir_all(&other_home).unwrap();
        let other = ProjectSandbox::new(
            &crate::test_hooks::STAND_IN,
            &folders.project,
            &other_home,
            &folders.tmp,
            SandboxNetwork::Open,
        )
        .unwrap();
        assert_ne!(container_name(&other), profile.container);
        assert_eq!(container_name(&sandbox), profile.container);
    }

    #[test]
    fn install_writes_the_profile_outside_the_roots_and_refuses_a_missing_helper() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let helper = std::env::current_exe().unwrap();
        let inside = folders.project.join("agent.sb");
        assert!(matches!(
            WindowsSandbox::install(sandbox.clone(), &helper, &inside, &folders.grants),
            Err(SandboxError::Invalid(_))
        ));
        assert!(!inside.exists());
        let writable = folders.project.join("grants");
        fs::create_dir_all(&writable).unwrap();
        let beside = folders.profiles.join("c.sb");
        match WindowsSandbox::install(sandbox.clone(), &helper, &beside, &writable) {
            Err(SandboxError::Invalid(reason)) => {
                assert!(
                    reason.contains("the grants folder sits in a writable root"),
                    "{reason}"
                )
            }
            other => panic!("expected the grants folder refusal, got {other:?}"),
        }
        assert!(!beside.exists());
        let absent = folders.profiles.join("absent");
        assert!(matches!(
            WindowsSandbox::install(sandbox.clone(), &helper, &beside, &absent),
            Err(SandboxError::Invalid(_))
        ));
        let missing = folders.profiles.join("missing-helper.exe");
        match WindowsSandbox::install(
            sandbox.clone(),
            &missing,
            &folders.profiles.join("a.sb"),
            &folders.grants,
        ) {
            Err(SandboxError::Unavailable(reason)) => assert_eq!(reason, APPCONTAINER_MISSING),
            other => panic!("expected the missing helper refusal, got {other:?}"),
        }
        let path = folders.profiles.join("agent.sb");
        let installed =
            WindowsSandbox::install(sandbox.clone(), &helper, &path, &folders.grants).unwrap();
        let written = BoundaryProfile::load(&path).unwrap();
        assert_eq!(
            written,
            boundary_profile(&sandbox, &folders.grants).unwrap()
        );
        assert_eq!(installed.launcher(), helper.as_path());
        let limited = policy(
            &folders,
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        );
        let widened = folders.profiles.join("b.sb");
        let _held = WindowsSandbox::install(limited, &helper, &widened, &folders.grants).unwrap();
        assert_eq!(
            BoundaryProfile::load(&widened).unwrap().capabilities,
            vec![Capability::InternetClient]
        );
        let local = policy(
            &folders,
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(11_434)]),
        );
        match WindowsSandbox::install(
            local,
            &helper,
            &folders.profiles.join("c.sb"),
            &folders.grants,
        ) {
            Err(error) => assert!(
                crate::lines::model_loopback_windows(&error),
                "expected the loopback refusal, got {error:?}"
            ),
            Ok(_) => panic!("a loopback port is refused on Windows"),
        }
    }

    /// Install refuses a read root no app container reads and this standard
    /// user cannot change, made by an administrator and named in
    /// `CRAZYAGENT_TEST_UNCHANGEABLE_ROOT`, by that folder and before any profile is written,
    /// and installs the same policy reading a folder this user owns.
    #[test]
    #[ignore = "needs an administrator-made folder and a standard user (test farm)"]
    fn install_refuses_a_read_root_only_an_administrator_can_open_to_containers() {
        let folders = folders();
        let fixture = std::env::var_os("CRAZYAGENT_TEST_UNCHANGEABLE_ROOT").unwrap();
        let fixture = canonical(Path::new(&fixture)).unwrap();
        let helper = std::env::current_exe().unwrap();
        let path = folders.profiles.join("agent.sb");
        let refused = policy(&folders, SandboxNetwork::Only(Vec::new()))
            .allow_read(&fixture)
            .unwrap();
        match WindowsSandbox::install(refused, &helper, &path, &folders.grants) {
            Err(SandboxError::Unreadable(folder)) => assert_eq!(folder, fixture),
            other => panic!("expected the unreadable root refusal, got {other:?}"),
        }
        assert!(!path.exists());
        let own = folders.profiles.join("own");
        fs::create_dir_all(&own).unwrap();
        let allowed = policy(&folders, SandboxNetwork::Only(Vec::new()))
            .allow_read(&own)
            .unwrap();
        WindowsSandbox::install(allowed, &helper, &path, &folders.grants).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn wrap_starts_the_harness_through_the_helper_with_pinned_folders() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let helper = std::env::current_exe().unwrap();
        let installed = WindowsSandbox::install(
            sandbox,
            &helper,
            &folders.profiles.join("agent.sb"),
            &folders.grants,
        )
        .unwrap();
        let mut harness = Command::new(cmd());
        harness
            .arg("/C")
            .arg("exit 0")
            .env("userprofile", r"C:\Users\owner")
            .env("Temp", r"C:\Users\owner\AppData\Local\Temp")
            .env("EDITOR", "notepad")
            .current_dir(&folders.project);
        let wrapped = Sandbox::wrap(&installed, &harness).unwrap();
        assert_eq!(wrapped.get_program(), helper.as_os_str());
        let args: Vec<&OsStr> = wrapped.get_args().collect();
        assert_eq!(args[0], PROFILE_FLAG);
        assert_eq!(args[1], installed.profile().as_os_str());
        assert_eq!(args[2], SEPARATOR);
        assert_eq!(args[3], cmd().as_os_str());
        assert_eq!(&args[4..], [OsStr::new("/C"), OsStr::new("exit 0")]);
        let env: Vec<(String, String)> = wrapped
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        let value = |key: &str| {
            env.iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(value("USERPROFILE"), [folders.home.to_str().unwrap()]);
        assert_eq!(value("TEMP"), [folders.tmp.to_str().unwrap()]);
        assert_eq!(value("EDITOR"), ["notepad"]);
        assert_eq!(value("GIT_TERMINAL_PROMPT"), ["0"]);
        let mut outside = Command::new(cmd());
        outside.current_dir(&folders.profiles);
        assert!(Sandbox::wrap(&installed, &outside).is_err());
        let relative = Command::new("cmd.exe");
        assert!(Sandbox::wrap(&installed, &relative).is_err());
        let mut secret = Command::new(cmd());
        secret.env("GH_TOKEN", "placeholder-value");
        assert!(Sandbox::wrap(&installed, &secret).is_err());
    }

    /// The handing wrap keeps a launch's relay token under the name it hands it and refuses
    /// the same token under any other name, while the plain wrap refuses it under every name.
    #[test]
    fn wrap_handing_holds_the_token_only_under_its_names() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let helper = std::env::current_exe().unwrap();
        let installed = WindowsSandbox::install(
            sandbox,
            &helper,
            &folders.profiles.join("agent.sb"),
            &folders.grants,
        )
        .unwrap();
        let token = format!(
            "{}{}",
            kernel_common::relay_names::TOKEN_PREFIX,
            "a".repeat(kernel_common::relay_names::TOKEN_HEX)
        );
        let names = ["OPENAI_API_KEY"];
        let handed = Handed::new(&names, &token).unwrap();
        let mut harness = Command::new(cmd());
        harness
            .env("OPENAI_API_KEY", &token)
            .current_dir(&folders.project);
        let wrapped = installed.wrap_handing(&harness, handed).unwrap();
        assert_eq!(wrapped.get_program(), helper.as_os_str());
        let holding: Vec<&OsStr> = wrapped
            .get_envs()
            .filter(|(_, value)| *value == Some(OsStr::new(&token)))
            .map(|(name, _)| name)
            .collect();
        assert_eq!(holding, [OsStr::new("OPENAI_API_KEY")]);
        assert!(matches!(
            Sandbox::wrap(&installed, &harness),
            Err(SandboxError::Invalid(_))
        ));
        harness.env("OTHER_TOKEN", &token);
        assert!(matches!(
            installed.wrap_handing(&harness, handed),
            Err(SandboxError::Invalid(_))
        ));
    }

    #[test]
    fn an_installed_windows_sandbox_needs_the_helper_beside_the_engine() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let beside = std::env::current_exe().unwrap().with_file_name(HELPER_NAME);
        let profile = folders.profiles.join("agent.sb");
        match InstalledSandbox::install(sandbox, &profile, LinuxSupervisor::On, LinuxStrict::Off) {
            Ok(installed) => {
                assert!(beside.is_file());
                assert_eq!(installed.launcher(), beside.as_path());
            }
            Err(SandboxError::Unavailable(reason)) => {
                assert!(!beside.is_file());
                assert_eq!(reason, APPCONTAINER_MISSING);
            }
            Err(other) => panic!("unexpected refusal {other}"),
        }
    }

    /// A harness wrapped with a boundary profile, as the research
    /// runner wraps its agent, starts at the boundary helper beside the engine with the
    /// profile after `--profile`, inside the project. It needs the helper copied beside the
    /// test binary, where the wrap looks, so it never passes without reaching the assert.
    #[test]
    fn a_profile_wrapped_harness_starts_at_the_helper_beside_the_engine() {
        let folders = folders();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("agent.json");
        fs::write(&profile, "{}").unwrap();
        let beside = std::env::current_exe().unwrap().with_file_name(HELPER_NAME);
        assert!(
            beside.is_file(),
            "copy crazyagent-win-boundary beside the test binary first: {}",
            beside.display()
        );
        let mut harness = Command::new(cmd());
        harness
            .arg("/C")
            .arg("exit 0")
            .current_dir(&folders.project);
        let command = ProjectSandbox::wrap(&sandbox, &profile, &harness).unwrap();
        assert_eq!(command.get_program(), beside.as_os_str());
        assert_eq!(command.get_args().nth(1), Some(profile.as_os_str()));
        assert_eq!(command.get_current_dir(), Some(folders.project.as_path()));
    }

    /// The boundary program the confined asserts accept on Windows is only the helper beside
    /// the engine: not the shell, not sandbox-exec and not a helper of the same name in
    /// another folder.
    #[test]
    fn only_the_helper_beside_the_engine_is_the_boundary_program() {
        let tests = std::env::current_exe().unwrap();
        let beside = tests.with_file_name(HELPER_NAME);
        let elsewhere = tests.parent().unwrap().with_file_name(HELPER_NAME);
        assert_ne!(beside, elsewhere);
        assert!(crate::profile::command::starts_at_boundary(
            beside.as_os_str()
        ));
        for program in [
            cmd(),
            elsewhere,
            PathBuf::from(crate::profile::command::SANDBOX_EXEC),
        ] {
            assert!(
                !crate::profile::command::starts_at_boundary(program.as_os_str()),
                "{}",
                program.display()
            );
        }
    }

    #[test]
    fn a_wrapped_agent_cannot_write_a_protected_path_and_dies_with_its_helper() {
        let folders = folders();
        let tests = std::env::current_exe().unwrap();
        let helper = tests.parent().unwrap().with_file_name(HELPER_NAME);
        assert!(
            helper.is_file(),
            "build crazyagent-win-boundary first: {}",
            helper.display()
        );
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let installed = WindowsSandbox::install(
            sandbox,
            &helper,
            &folders.profiles.join("agent.sb"),
            &folders.grants,
        )
        .unwrap();
        let brief = folders.project.join("context").join("brief.md");
        let mut overwrite = Command::new(cmd());
        overwrite
            .arg("/C")
            .arg(format!("echo changed>{}", brief.display()))
            .current_dir(&folders.project);
        let status = kernel_processes::spawn_gate::status(
            Sandbox::wrap(&installed, &overwrite)
                .unwrap()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(!status.success());
        assert_eq!(fs::read_to_string(&brief).unwrap(), "brief\r\n");
        let mut shell = Command::new(cmd());
        shell.arg("/K").current_dir(&folders.project);
        let mut running = kernel_processes::spawn_gate::spawn(
            Sandbox::wrap(&installed, &shell)
                .unwrap()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let mut stdin = running.stdin.take().unwrap();
        let stdout = running.stdout.take().unwrap();
        let (events, received) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                if line.trim() == "ready" {
                    let _ = events.send("ready");
                }
                line.clear();
            }
            let _ = events.send("closed");
        });
        stdin.write_all(b"echo ready\r\n").unwrap();
        stdin.flush().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(30)), Ok("ready"));
        running.kill().unwrap();
        running.wait().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(30)), Ok("closed"));
        drop(stdin);
    }

    const CALL: &str = "BOUNDARY_FILE_CALL";

    fn helper() -> PathBuf {
        let tests = std::env::current_exe().unwrap();
        let helper = tests.parent().unwrap().with_file_name(HELPER_NAME);
        assert!(
            helper.is_file(),
            "build crazyagent-win-boundary first: {}",
            helper.display()
        );
        helper
    }

    fn quiet(installed: &WindowsSandbox, command: &Command) -> bool {
        kernel_processes::spawn_gate::status(
            Sandbox::wrap(installed, command)
                .unwrap()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .success()
    }

    fn shell(installed: &WindowsSandbox, folders: &Folders, line: &str) -> bool {
        let mut command = Command::new(cmd());
        command.arg("/C").arg(line).current_dir(&folders.project);
        quiet(installed, &command)
    }

    fn call(installed: &WindowsSandbox, folders: &Folders, step: &[&Path]) -> std::process::Output {
        assert!(!step.is_empty() && step.len() <= 2);
        let probe = folders.home.join("file-call.exe");
        if !probe.is_file() {
            fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
        }
        let line = |verb: &str| {
            let paths: Vec<&str> = step.iter().map(|path| path.to_str().unwrap()).collect();
            format!("{verb}|{}", paths.join("|"))
        };
        let verb = if step.len() == 2 { "rename" } else { "rmtree" };
        let child = module_path!().strip_prefix("kernel_sandbox::").unwrap();
        let child = format!("{child}::file_call_child");
        let mut command = Command::new(&probe);
        command
            .args([
                child.as_str(),
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CALL, line(verb))
            .current_dir(&folders.project);
        let mut wrapped = Sandbox::wrap(installed, &command).unwrap();
        kernel_processes::spawn_gate::collected(&mut wrapped).unwrap()
    }

    fn denied(output: &std::process::Output) -> bool {
        output.status.code() == Some(1)
            && String::from_utf8_lossy(&output.stdout).contains("(os error 5)")
    }

    #[test]
    fn file_call_child() {
        let Ok(line) = std::env::var(CALL) else {
            return;
        };
        let parts: Vec<&str> = line.split('|').collect();
        let done = match parts.as_slice() {
            ["rename", from, to] => fs::rename(from, to),
            ["rmtree", path] => fs::remove_dir_all(path),
            _ => panic!("unknown file call {line}"),
        };
        if let Err(error) = done {
            println!("CALL {line} => {error}");
            std::process::exit(1);
        }
    }

    #[test]
    fn install_makes_each_missing_guarded_folder_and_leaves_what_is_there() {
        let folders = folders();
        let helper = std::env::current_exe().unwrap();
        let git = folders.project.join(".git");
        let state = folders.project.join(".crazyagent");
        fs::create_dir_all(&git).unwrap();
        fs::create_dir_all(&state).unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let first = folders.profiles.join("a.sb");
        WindowsSandbox::install(sandbox.clone(), &helper, &first, &folders.grants).unwrap();
        for folder in [
            git.join("hooks"),
            git.join("modules"),
            state.join("context"),
        ] {
            assert!(
                folder.is_dir() && fs::read_dir(&folder).unwrap().next().is_none(),
                "{} is made empty",
                folder.display()
            );
        }
        fs::write(git.join("hooks").join("pre-commit"), "owner\n").unwrap();
        fs::remove_dir(git.join("modules")).unwrap();
        fs::write(git.join("modules"), "owner\n").unwrap();
        let second = folders.profiles.join("b.sb");
        WindowsSandbox::install(sandbox, &helper, &second, &folders.grants).unwrap();
        assert_eq!(
            fs::read_to_string(git.join("hooks").join("pre-commit")).unwrap(),
            "owner\n"
        );
        assert_eq!(
            fs::read_to_string(git.join("modules")).unwrap(),
            "owner\n",
            "an entry already there is never replaced"
        );
    }

    #[test]
    fn a_legacy_project_gets_no_state_folder_and_keeps_its_root_context_guarded() {
        let folders = folders();
        let helper = std::env::current_exe().unwrap();
        let project = &folders.project;
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("a.sb");
        WindowsSandbox::install(sandbox.clone(), &helper, &profile, &folders.grants).unwrap();
        assert!(
            !project.join(".git").exists(),
            "a git-less project stays git-less"
        );
        assert!(
            !project.join(".crazyagent").exists(),
            "making .crazyagent would move the project's state folder"
        );
        let guarded = |sandbox: &ProjectSandbox| -> Vec<String> {
            boundary_profile(sandbox, &folders.grants)
                .unwrap()
                .guarded
                .into_iter()
                .map(|item| item.path)
                .collect()
        };
        let root_context = drive_path(&project.join("context")).unwrap();
        assert!(guarded(&sandbox).contains(&root_context));
        fs::create_dir_all(project.join(".crazyagent")).unwrap();
        assert_eq!(
            (crate::test_hooks::STAND_IN.access.state_folder)(project),
            project.join(".crazyagent")
        );
        let moved = guarded(&sandbox);
        assert!(
            !moved.contains(&root_context),
            "the root context is unread once .crazyagent holds the state"
        );
        assert!(moved.contains(&drive_path(&project.join(".crazyagent").join("context")).unwrap()));
    }

    #[test]
    fn a_wrapped_agent_cannot_vacate_or_create_a_protected_path_but_still_edits_the_project() {
        let folders = folders();
        let helper = helper();
        let project = &folders.project;
        let git = project.join(".git");
        fs::create_dir_all(git.join("hooks")).unwrap();
        fs::write(git.join("config"), "[core]\r\n").unwrap();
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("README.md"), "readme\r\n").unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("agent.sb");
        let installed =
            WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
        let run = |line: &str| shell(&installed, &folders, line);
        let moved = call(&installed, &folders, &[&git, &project.join(".git-moved")]);
        assert!(denied(&moved), "{moved:?}");
        assert!(git.join("config").is_file() && !project.join(".git-moved").exists());
        assert!(!run(r"mkdir .crazyagent\context"));
        assert!(!project.join(".crazyagent").exists());
        assert!(run(r"echo planted>.git\config.lock"));
        let lock = git.join("config.lock");
        let replaced = call(&installed, &folders, &[&lock, &git.join("config")]);
        assert!(denied(&replaced), "{replaced:?}");
        assert_eq!(
            fs::read_to_string(git.join("config")).unwrap(),
            "[core]\r\n"
        );
        assert!(!run(r"mkdir .git\hooks\planted"));
        assert!(!git.join("hooks").join("planted").exists());
        let context = project.join("context");
        let vacated = call(&installed, &folders, &[&context, &project.join("moved")]);
        assert!(denied(&vacated), "{vacated:?}");
        assert!(context.join("brief.md").is_file());
        assert!(run(r"echo changed>src\work.txt"));
        let src = project.join("src");
        let renamed = call(
            &installed,
            &folders,
            &[&src.join("work.txt"), &src.join("done.txt")],
        );
        assert!(renamed.status.success(), "{renamed:?}");
        assert!(src.join("done.txt").is_file() && !src.join("work.txt").exists());
        assert!(run("echo changed>README.md"));
        assert_eq!(
            fs::read_to_string(project.join("README.md")).unwrap(),
            "changed\r\n"
        );
    }

    #[test]
    fn a_wrapped_agent_without_a_checkout_adds_top_level_files_and_takes_git_locks() {
        let folders = folders();
        let helper = helper();
        let project = &folders.project;
        let git = project.join(".git");
        let state = project.join(".crazyagent").join("context");
        fs::create_dir_all(git.join("hooks")).unwrap();
        fs::write(git.join("config"), "[core]\r\n").unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("brief.md"), "brief\r\n").unwrap();
        let sandbox = policy(&folders, SandboxNetwork::Only(Vec::new()));
        let profile = folders.profiles.join("agent.sb");
        let installed =
            WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
        let run = |line: &str| shell(&installed, &folders, line);
        assert!(run(r"echo lock>.git\index.lock"));
        let lock = git.join("index.lock");
        let staged = call(&installed, &folders, &[&lock, &git.join("index")]);
        assert!(staged.status.success(), "{staged:?}");
        assert!(git.join("index").is_file() && !lock.exists());
        assert!(run("echo new>notes.txt"));
        assert!(run("mkdir docs"));
        assert!(project.join("notes.txt").is_file() && project.join("docs").is_dir());
        assert!(!run(r"echo planted>.git\hooks\pre-commit"));
        assert!(!git.join("hooks").join("pre-commit").exists());
        assert!(!run(r"echo planted>.crazyagent\context\brief.md"));
        let moved = call(&installed, &folders, &[&git, &project.join(".git-moved")]);
        assert!(denied(&moved), "{moved:?}");
        let removed = call(&installed, &folders, &[&state]);
        assert!(denied(&removed), "{removed:?}");
        assert_eq!(
            fs::read_to_string(state.join("brief.md")).unwrap(),
            "brief\r\n"
        );
        assert!(
            run(r"echo note>context\brief.md"),
            "the root context is the agent's to change once .crazyagent holds the state"
        );
        assert_eq!(
            fs::read_to_string(git.join("config")).unwrap(),
            "[core]\r\n"
        );
    }

    #[test]
    fn a_job_folder_takes_top_level_entries_while_a_project_root_keeps_its_own() {
        let helper = helper();
        for job in [false, true] {
            let folders = folders();
            let project = &folders.project;
            let plain = policy(&folders, SandboxNetwork::Only(Vec::new()));
            let sandbox = if job { plain.as_job_folder() } else { plain };
            assert_eq!(sandbox.holds_project_guards(), !job);
            let guarded = boundary_profile(&sandbox, &folders.grants).unwrap().guarded;
            assert_eq!(
                guarded.iter().any(|item| item.guard == Guard::ReadOnly),
                !job,
                "only a project holds project guards"
            );
            assert!(guarded.iter().any(|item| item.guard == Guard::Hidden));
            let src = project.join("src");
            fs::create_dir_all(&src).unwrap();
            fs::write(src.join("work.txt"), "work\r\n").unwrap();
            let profile = folders.profiles.join("agent.sb");
            let installed =
                WindowsSandbox::install(sandbox, &helper, &profile, &folders.grants).unwrap();
            let run = |line: &str| shell(&installed, &folders, line);
            assert!(
                run(r"echo changed>src\work.txt"),
                "the agent started: job {job}"
            );
            let top = project.join("top.txt");
            let moved = call(&installed, &folders, &[&src.join("work.txt"), &top]);
            if job {
                assert!(moved.status.success() && top.is_file(), "{moved:?}");
            } else {
                assert!(denied(&moved) && !top.exists(), "{moved:?}");
            }
            assert_eq!(run("echo new>notes.txt"), job, "job {job}");
            assert_eq!(run("mkdir docs"), job, "job {job}");
            assert_eq!(project.join("notes.txt").is_file(), job);
            assert_eq!(project.join("docs").is_dir(), job);
            assert_eq!(run(r"echo note>context\brief.md"), job, "job {job}");
            assert!(!project.join(".git").exists() && !project.join(".crazyagent").exists());
        }
    }
}

mod owner_git_rows {
    use crate::error::SandboxError;
    use crate::net::SandboxNetwork;
    use crate::paths::canonical;
    use crate::profile::ProjectSandbox;
    use crate::windows::GRANTS_FOLDER;
    use crate::windows::owner_git::*;
    use crate::windows::profile::container_name;
    use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard, check_container};
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
            &crate::test_hooks::STAND_IN,
            &made.root,
            &made.folder,
            project,
            &made.git,
            &helper,
            &made.grants,
        )
        .unwrap()
    }

    #[test]
    fn git_for_windows_install_folders_are_found_from_its_programs_only() {
        let root = Path::new(r"C:\Programs\Git");
        for program in [
            r"C:\Programs\Git\cmd\git.exe",
            r"C:\Programs\Git\bin\git.exe",
            r"C:\Programs\Git\CMD\git.exe",
            r"C:\Programs\Git\mingw64\bin\git.exe",
            r"C:\Programs\Git\clangarm64\bin\git.exe",
            r"C:\Programs\Git\ucrt64\bin\git.exe",
            r"C:\Programs\Git\clang64\bin\git.exe",
        ] {
            assert_eq!(install_root(Path::new(program)), Some(root), "{program}");
        }
        for program in [
            r"C:\tools\git.exe",
            r"C:\scoop\shims\git.exe",
            r"C:\Programs\Git\mingw64\libexec\git-core\git.exe",
            r"cmd\git.exe",
            r"C:\cmd\git.exe",
        ] {
            assert_eq!(install_root(Path::new(program)), None, "{program}");
        }
    }

    #[test]
    fn owner_git_has_no_capability_writes_only_the_project_and_its_folders_and_is_no_agents_container()
     {
        let made = made();
        let owner = installed(&made, &made.project);
        let profile = BoundaryProfile::load(owner.profile()).unwrap();
        let folder = canonical(&made.folder).unwrap();
        let places = places(&folder, &made.project);
        assert_eq!(owner.profile(), places.profile);
        assert!(
            profile.capabilities.is_empty(),
            "{:?}",
            profile.capabilities
        );
        assert_eq!(
            profile.write_roots,
            [&made.project, &places.home, &places.tmp]
                .map(|path| path.to_str().unwrap().to_owned())
        );
        assert_eq!(
            profile.read_roots,
            [made.root.join("Git").to_str().unwrap().to_owned()]
        );
        assert!(
            profile
                .guarded
                .iter()
                .all(|item| item.guard == Guard::Hidden),
            "{:?}",
            profile.guarded
        );
        assert_eq!(check_container(&profile.container), Ok(()));
        let (agent_home, agent_tmp) = (made.root.join("agent-home"), made.root.join("agent-tmp"));
        fs::create_dir_all(&agent_home).unwrap();
        fs::create_dir_all(&agent_tmp).unwrap();
        let agent = ProjectSandbox::new(
            &crate::test_hooks::STAND_IN,
            &made.project,
            &agent_home,
            &agent_tmp,
            SandboxNetwork::Open,
        )
        .unwrap();
        assert_ne!(container_name(&agent), profile.container);
        let again = installed(&made, &made.project);
        assert_eq!(again.profile(), owner.profile());
        assert_eq!(
            BoundaryProfile::load(again.profile()).unwrap().container,
            profile.container
        );
        let other = made.root.join("other");
        fs::create_dir_all(&other).unwrap();
        let elsewhere = installed(&made, &other);
        assert_ne!(elsewhere.profile(), owner.profile());
        assert_ne!(
            BoundaryProfile::load(elsewhere.profile())
                .unwrap()
                .container,
            profile.container
        );
    }

    #[test]
    fn owner_git_installed_from_a_verbatim_program_wraps_its_plain_form() {
        let made = made();
        let verbatim = PathBuf::from(format!(r"\\?\{}", made.git.display()));
        let helper = std::env::current_exe().unwrap();
        let owner = OwnerGit::install(
            &crate::test_hooks::STAND_IN,
            &made.root,
            &made.folder,
            &made.project,
            &verbatim,
            &helper,
            &made.grants,
        )
        .unwrap();
        assert_eq!(owner.git(), made.git);
        let mut git = Command::new(owner.git());
        git.current_dir(&made.project).arg("--version");
        let wrapped = owner.wrap(&git).unwrap();
        assert_eq!(wrapped.get_args().nth(3), Some(made.git.as_os_str()));
    }

    #[test]
    fn owner_git_refuses_a_verbatim_program_whose_plain_path_opens_another_file() {
        let made = made();
        for tail in [".", " "] {
            let verbatim = PathBuf::from(format!(r"\\?\{}{tail}", made.git.display()));
            fs::write(&verbatim, "another git").unwrap();
            refused_install(
                &made,
                &made.folder,
                &made.project,
                &verbatim,
                "another file",
            );
        }
    }

    #[test]
    fn owner_git_refuses_a_git_outside_a_git_for_windows_install_before_any_profile() {
        let made = made();
        let loose = made.root.join("tools").join("git.exe");
        fs::create_dir_all(loose.parent().unwrap()).unwrap();
        fs::write(&loose, "").unwrap();
        let helper = std::env::current_exe().unwrap();
        match OwnerGit::install(
            &crate::test_hooks::STAND_IN,
            &made.root,
            &made.folder,
            &made.project,
            &loose,
            &helper,
            &made.grants,
        ) {
            Err(SandboxError::Invalid(reason)) => {
                assert!(reason.contains("Git for Windows"), "{reason}")
            }
            other => panic!("expected the install folder refusal, got {other:?}"),
        }
        let folder = canonical(&made.folder).unwrap();
        assert!(!places(&folder, &made.project).profile.exists());
    }

    /// Owner git for a project folder that is the engine root itself, as after the owner moves
    /// the engine's data folder onto a project, is refused before any profile, since the
    /// boundary would grant git write over the engine's files.
    #[test]
    fn owner_git_refuses_a_project_that_is_the_engine_root_before_any_profile() {
        let made = made();
        fs::write(made.root.join("engine.db"), "engine secret").unwrap();
        let helper = std::env::current_exe().unwrap();
        match OwnerGit::install(
            &crate::test_hooks::STAND_IN,
            &made.root,
            &made.folder,
            &made.root,
            &made.git,
            &helper,
            &made.grants,
        ) {
            Err(SandboxError::Invalid(reason)) => {
                assert_eq!(reason, crate::paths::SECRET_ROOT_REFUSED)
            }
            other => panic!("expected the engine root refusal, got {other:?}"),
        }
        let folder = canonical(&made.folder).unwrap();
        assert!(!places(&folder, &made.root).profile.exists());
    }

    /// Lays out a stand-in Git for Windows under `root`: each relative file is written empty
    /// and each relative folder made. Inputs: the root, the files and the folders. Output:
    /// the root's `cmd\git.exe` or, with no such file, the first file.
    fn laid_out(root: &Path, files: &[&str], folders: &[&str]) -> PathBuf {
        for folder in folders {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "").unwrap();
        }
        let cmd = root.join("cmd").join("git.exe");
        if cmd.is_file() {
            cmd
        } else {
            root.join(files[0])
        }
    }

    /// Asserts owner git refuses to install for `git` with `words` in its reason and writes no
    /// profile. Inputs: the folders, the owner-git folder, the project, git and the words.
    fn refused_install(made: &Made, folder: &Path, project: &Path, git: &Path, words: &str) {
        let helper = std::env::current_exe().unwrap();
        match OwnerGit::install(
            &crate::test_hooks::STAND_IN,
            &made.root,
            folder,
            project,
            git,
            &helper,
            &made.grants,
        ) {
            Err(SandboxError::Invalid(reason)) => {
                assert!(reason.contains(words), "{}: {reason}", git.display())
            }
            other => panic!(
                "{}: expected a refusal naming {words}, got {other:?}",
                git.display()
            ),
        }
        let folder = canonical(folder).unwrap();
        assert!(
            !places(&folder, &canonical(project).unwrap())
                .profile
                .exists()
        );
    }

    #[test]
    fn owner_git_reads_only_a_folder_that_holds_git_for_windows_markers() {
        let made = made();
        for (name, platform) in [
            ("Ucrt", "ucrt64"),
            ("Clang", "clang64"),
            ("Full", "mingw64"),
            ("Arm", "clangarm64"),
            ("Old", "mingw32"),
        ] {
            let platform_git = format!("{platform}/bin/git.exe");
            let git = laid_out(
                &made.root.join(name),
                &["cmd/git.exe", "bin/git.exe", platform_git.as_str()],
                &[],
            );
            let project = made.root.join(format!("project-{name}"));
            fs::create_dir_all(&project).unwrap();
            let helper = std::env::current_exe().unwrap();
            let owner = OwnerGit::install(
                &crate::test_hooks::STAND_IN,
                &made.root,
                &made.folder,
                &project,
                &git,
                &helper,
                &made.grants,
            )
            .unwrap_or_else(|problem| panic!("{name}: {problem:?}"));
            assert_eq!(
                BoundaryProfile::load(owner.profile()).unwrap().read_roots,
                [made.root.join(name).to_str().unwrap().to_owned()],
                "{name}"
            );
        }
        for (name, files, folders) in [
            ("UserBin", &["bin/git.exe"][..], &[][..]),
            ("UserCmd", &["cmd/git.exe"][..], &[][..]),
            (
                "NoCmd",
                &["bin/git.exe", "mingw64/bin/git.exe"][..],
                &[][..],
            ),
            (
                "NoPlatform",
                &["cmd/git.exe", "bin/git.exe"][..],
                &["usr/bin"][..],
            ),
            (
                "BareMingw",
                &["cmd/git.exe", "bin/git.exe"][..],
                &["mingw64/bin"][..],
            ),
            (
                "BareUcrt",
                &["cmd/git.exe", "bin/git.exe"][..],
                &["ucrt64/bin"][..],
            ),
        ] {
            let git = laid_out(&made.root.join(name), files, folders);
            refused_install(&made, &made.folder, &made.project, &git, "Git for Windows");
        }
    }

    #[test]
    fn owner_git_refuses_an_install_folder_that_holds_the_project_or_its_own_folder() {
        let made = made();
        let work = made.root.join("work");
        let project = work.join("project");
        fs::create_dir_all(&project).unwrap();
        let git = laid_out(&work, &["cmd/git.exe", "mingw64/bin/git.exe"], &[]);
        refused_install(&made, &made.folder, &project, &git, "holds");
        let engine = made.root.join("engine");
        let folder = engine.join(OWNER_GIT_FOLDER);
        let git = laid_out(&engine, &["cmd/git.exe", "mingw64/bin/git.exe"], &[]);
        refused_install(&made, &folder, &made.project, &git, "holds");
    }

    #[test]
    fn a_wrapped_git_refuses_another_program_a_global_config_or_a_folder_outside_the_project() {
        let made = made();
        let owner = installed(&made, &made.project);
        let refused = |git: &Command, why: &str| {
            assert!(
                matches!(owner.wrap(git), Err(SandboxError::Invalid(_))),
                "{why}"
            );
        };
        let mut other = Command::new(made.root.join("Git").join("bin").join("git.exe"));
        other.current_dir(&made.project);
        refused(&other, "another program");
        let mut config = Command::new(&made.git);
        config
            .current_dir(&made.project)
            .env(GLOBAL_CONFIG, made.root.join("gitconfig"));
        refused(&config, "a global config file");
        let mut outside = Command::new(&made.git);
        outside.current_dir(&made.root);
        refused(&outside, "a folder outside the project");
        let mut plain = Command::new(&made.git);
        plain.current_dir(&made.project);
        assert!(owner.wrap(&plain).is_ok());
    }
}
