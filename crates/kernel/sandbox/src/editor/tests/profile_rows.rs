use super::*;

#[test]
fn an_ide_profile_refuses_a_project_inside_the_engine_data_root() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let data = fs::canonicalize(scratch.path()).unwrap();
    let home = data.join("ide").join("p1").join("server");
    let build = data.join("ide").join("builds").join("v1");
    let root = data.join("projects").join("inside");
    for path in [&home, &build, &root] {
        folder(path);
    }
    let tmp = crate::toolchains::make_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    assert_eq!(
        profile(
            &crate::test_hooks::STAND_IN,
            &grants,
            PORT,
            closed(),
            &[],
            AgentFilesRule::default()
        )
        .unwrap_err(),
        "ide_sandbox_invalid"
    );
    let elsewhere = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let project = fs::canonicalize(elsewhere.path()).unwrap();
    let outside = Grants {
        root: &project,
        ..grants
    };
    assert!(
        profile(
            &crate::test_hooks::STAND_IN,
            &outside,
            PORT,
            closed(),
            &[],
            AgentFilesRule::default()
        )
        .is_ok()
    );
    assert_eq!(
        profile(
            &crate::test_hooks::STAND_IN,
            &outside,
            0,
            closed(),
            &[],
            AgentFilesRule::default()
        )
        .unwrap_err(),
        "ide_sandbox_invalid"
    );
    crate::toolchains::remove_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
}

/// A project folder named with long runs of `s` and `f` still gets an IDE profile holding its agent
/// folders rule under the folder's spelled name, rather than a panic while that name is spelled.
#[test]
fn an_ide_profile_builds_for_a_project_folder_named_with_long_fold_runs() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let data = fs::canonicalize(scratch.path()).unwrap();
    let home = data.join("ide").join("p1").join("server");
    let build = data.join("ide").join("builds").join("v1");
    let elsewhere = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = fs::canonicalize(elsewhere.path())
        .unwrap()
        .join("Glasssssss-ffffffffff");
    for path in [&home, &build, &root] {
        folder(path);
    }
    let tmp = crate::toolchains::make_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    let rule = AgentFilesRule {
        choice: IdeAgentFiles::ReadOnly,
        older_agents: true,
    };
    let text = profile(
        &crate::test_hooks::STAND_IN,
        &grants,
        PORT,
        closed(),
        &[],
        rule,
    )
    .unwrap();
    let spelled = crate::paths::any_case(root.to_str().unwrap());
    assert!(text.contains(&format!("(regex #\"^{spelled}")), "{text}");
    crate::toolchains::remove_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
}

/// The Remote settings folder the engine writes before each launch is locked inside the
/// server's writable data folder: the server reads the engine's settings, but cannot
/// overwrite, remove or add a file there, not through a case variant of the path, and
/// cannot move the folder or the folder above it aside to plant its own. The rest of its
/// data folder stays writable, and the engine's content survives every attempt.
#[test]
fn the_remote_settings_folder_is_read_only_to_the_ide_server() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(FOLDER).join("p1");
    let build = data.join(FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    let user_data = home.join("data");
    let machine = user_data.join("Machine");
    for path in [&machine, &build, &root] {
        folder(path);
    }
    let settings = machine.join("settings.json");
    let engine = "{\"chat.disableAIFeatures\": true}\n";
    fs::write(&settings, engine).unwrap();
    let tmp = crate::toolchains::make_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[machine.as_path()],
    };
    let profile = install(
        &crate::test_hooks::STAND_IN,
        &mine,
        &grants,
        PORT,
        closed(),
        &[],
        AgentFilesRule::default(),
    )
    .unwrap();
    let (read, printed) = run(&profile, "/bin/cat", &[settings.as_os_str()]);
    assert!(read);
    assert_eq!(printed, engine);
    let overwrite = r#"printf '{"chat.disableAIFeatures": false}' > "$1""#;
    let variant = user_data.join("MACHINE").join("Settings.JSON");
    assert!(!sh(&profile, overwrite, &settings));
    assert!(!sh(&profile, overwrite, &variant));
    assert!(!sh(
        &profile,
        overwrite,
        &home.join("DATA/machine/settings.json")
    ));
    assert!(!sh(&profile, r#"/bin/rm -f "$1""#, &settings));
    assert!(!sh(&profile, r#"/bin/chmod 666 "$1""#, &settings));
    assert!(!sh(&profile, r#": > "$1""#, &machine.join("planted.json")));
    assert!(!sh(&profile, r#"/bin/mkdir "$1""#, &machine.join("User")));
    assert!(!sh(&profile, r#"/bin/mv "$1" "$1-aside""#, &machine));
    assert!(!sh(&profile, r#"/bin/mv "$1" "$1-aside""#, &user_data));
    assert!(!sh(&profile, r#"/bin/rm -rf "$1""#, &user_data));
    assert!(!sh(
        &profile,
        r#"/bin/ln -s /private/tmp "$1""#,
        &user_data.join("machine")
    ));
    let hard =
        r#"linked="${1%/*/*}/linked.json"; /bin/ln "$1" "$linked" && printf '{}' > "$linked""#;
    assert!(!sh(&profile, hard, &settings));
    assert!(!user_data.join("linked.json").exists());
    assert!(sh(&profile, r#": > "$1""#, &user_data.join("logs.txt")));
    assert!(sh(
        &profile,
        r#"/bin/mkdir -p "$1""#,
        &user_data.join("User/globalStorage")
    ));
    assert!(machine.is_dir() && !machine.is_symlink());
    assert!(user_data.is_dir() && !user_data.is_symlink());
    assert_eq!(fs::read_to_string(&settings).unwrap(), engine);
    assert_eq!(
        fs::read_dir(&machine).unwrap().count(),
        1,
        "nothing was added next to the engine's settings"
    );
    crate::toolchains::remove_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
}

/// A lock outside every writable folder, a read-only folder inside a writable one, and a
/// read-only folder holding a writable one are refused, so the profile never states a
/// lock or a read-only grant that a writable grant overrides.
#[test]
fn an_ide_profile_refuses_locks_and_read_only_folders_a_writable_grant_would_override() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(FOLDER).join("p1");
    let build = data.join(FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    let extensions = mine.join("extensions");
    for path in [&home, &extensions, &build, &root] {
        folder(path);
    }
    let tmp = crate::toolchains::make_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
    let machine = home.join("data").join("Machine");
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[extensions.as_path()],
        readable: &[],
        locked: &[machine.as_path()],
    };
    assert!(
        profile(
            &crate::test_hooks::STAND_IN,
            &grants,
            PORT,
            closed(),
            &[],
            AgentFilesRule::default()
        )
        .is_ok()
    );
    let invalid = |grants: &Grants| {
        profile(
            &crate::test_hooks::STAND_IN,
            grants,
            PORT,
            closed(),
            &[],
            AgentFilesRule::default(),
        )
        .unwrap_err()
    };
    for locked in [extensions.as_path(), home.as_path(), mine.as_path()] {
        let locks = [locked];
        assert_eq!(
            invalid(&Grants {
                locked: &locks,
                ..grants
            }),
            "ide_sandbox_invalid",
            "{}",
            locked.display()
        );
    }
    let quoted = home.join("da\"ta").join("Machine");
    let locks = [quoted.as_path()];
    assert_eq!(
        invalid(&Grants {
            locked: &locks,
            ..grants
        }),
        "ide_sandbox_invalid",
        "a path the profile cannot quote"
    );
    let inside = home.join("data");
    for read_only in [inside.as_path(), mine.as_path(), home.as_path()] {
        let folders = [read_only];
        assert_eq!(
            invalid(&Grants {
                read_only: &folders,
                locked: &[],
                ..grants
            }),
            "ide_sandbox_invalid",
            "{}",
            read_only.display()
        );
    }
    crate::toolchains::remove_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
}

/// The server cannot read an agent's IDE config, any `.vscode` folder in an agent worktree and any
/// `.code-workspace` file in one, under either agents root and in any letter case, so an agent
/// cannot set the owner's IDE settings. A link an agent commits under those names is refused as
/// well, though the file it leads to stays readable by its own name. The project's own files, the
/// agent's other files and the engine's workspace file stay readable.
#[test]
fn the_ide_server_cannot_read_agent_ide_config() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(FOLDER).join("p1");
    let build = data.join(FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    let inner = root.join(".crazyagent").join("worktrees").join("agent-1");
    let outer = root.join("worktrees").join("agent-2");
    let linked = root.join("worktrees").join("agent-3");
    for path in [
        &home,
        &build,
        &inner.join(".vscode"),
        &inner.join("src").join(".vscode"),
        &outer.join(".VSCode"),
        &root.join(".vscode"),
        &linked.join("docs"),
    ] {
        folder(path);
    }
    let planted = r#"{"php.validate.executablePath": "./run.sh"}"#;
    let refused = [
        inner.join(".vscode").join("settings.json"),
        inner.join(".vscode").join("tasks.json"),
        inner.join("src").join(".vscode").join("settings.json"),
        outer.join(".VSCode").join("settings.json"),
        inner.join("team.code-workspace"),
        inner.join("src").join("Other.Code-Workspace"),
        outer.join("agents.code-workspace"),
    ];
    let workspace = mine.join("crazyagent.code-workspace");
    let allowed = [
        root.join(".vscode").join("settings.json"),
        inner.join("src").join("main.rs"),
        outer.join("README.md"),
        linked.join("docs").join("settings.json"),
        workspace.clone(),
    ];
    for path in refused.iter().chain(allowed.iter()) {
        fs::write(path, planted).unwrap();
    }
    std::os::unix::fs::symlink("docs", linked.join(".vscode")).unwrap();
    std::os::unix::fs::symlink("docs/settings.json", linked.join("team.code-workspace")).unwrap();
    std::os::unix::fs::symlink("../src/main.rs", inner.join(".vscode").join("launch.json"))
        .unwrap();
    std::os::unix::fs::symlink("docs", linked.join("notes")).unwrap();
    let through_links = [
        linked.join(".vscode").join("settings.json"),
        linked.join("team.code-workspace"),
        inner.join(".vscode").join("launch.json"),
    ];
    let tmp = crate::toolchains::make_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[workspace.as_path()],
        locked: &[],
    };
    let profile = install(
        &crate::test_hooks::STAND_IN,
        &mine,
        &grants,
        PORT,
        closed(),
        &[],
        AgentFilesRule::default(),
    )
    .unwrap();
    let read = |path: &Path| run(&profile, "/bin/cat", &[path.as_os_str()]);
    for path in &refused {
        let (ran, printed) = read(path);
        assert!(!ran && printed.is_empty(), "{}", path.display());
    }
    for path in &through_links {
        let (ran, printed) = read(path);
        assert!(!ran && printed.is_empty(), "{}", path.display());
    }
    let variant = inner.join(".VSCODE").join("Settings.json");
    assert!(!read(&variant).0, "{}", variant.display());
    assert!(!run(&profile, "/bin/ls", &[inner.join(".vscode").as_os_str()]).0);
    assert!(!run(&profile, "/bin/ls", &[linked.join(".vscode").as_os_str()]).0);
    for path in &allowed {
        assert_eq!(read(path), (true, planted.to_owned()), "{}", path.display());
    }
    let named_plainly = linked.join("notes").join("settings.json");
    assert_eq!(
        read(&named_plainly),
        (true, planted.to_owned()),
        "a link is refused for its name, not for being a link"
    );
    crate::toolchains::remove_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
}

/// A listener on a free loopback port, with nothing behind it. Output: the listener, held
/// while the row runs, and its port.
fn local_listener() -> (std::net::TcpListener, u16) {
    let listener = kernel_processes::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// Installs a real IDE profile for `network` in a scratch layout. Input: the network. Output:
/// the scratch folder, held while the row runs, and the installed profile.
fn network_profile(network: SandboxNetwork) -> (tempfile::TempDir, PathBuf) {
    kept_profile(network, &[])
}

/// Installs a real IDE profile for `network` that keeps `kept_ports`, in a scratch layout.
/// Inputs: the network and the ports. Output: the scratch folder, held while the row runs, and
/// the installed profile.
fn kept_profile(network: SandboxNetwork, kept_ports: &[u16]) -> (tempfile::TempDir, PathBuf) {
    let scratch = tempfile::Builder::new()
        .prefix("ca-iden-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(FOLDER).join("p1");
    let build = data.join(FOLDER).join("builds").join("v1");
    let (home, root) = (mine.join("server"), base.join("project"));
    for path in [&home, &build, &root] {
        folder(path);
    }
    let tmp = crate::toolchains::make_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    let profile = install(
        &crate::test_hooks::STAND_IN,
        &mine,
        &grants,
        PORT,
        network,
        kept_ports,
        AgentFilesRule::default(),
    )
    .unwrap();
    (scratch, profile)
}

/// Whether a process under the profile can listen on a loopback port. Inputs: the installed
/// profile and the port. Output: the answer, from a real listen through `sandbox-exec`.
fn binds(profile: &Path, port: u16) -> bool {
    let script = format!(
        "/usr/bin/nc -l 127.0.0.1 {port} & p=$!; /bin/sleep 1; \
         if kill -0 $p 2>/dev/null; then kill $p; echo bound; fi"
    );
    run(profile, "/bin/sh", &[OsStr::new("-c"), OsStr::new(&script)])
        .1
        .contains("bound")
}

/// Whether a process under the profile connects to a loopback port. Inputs: the installed
/// profile and the port. Output: the answer, from a real connect through `sandbox-exec`.
fn reaches(profile: &Path, port: u16) -> bool {
    let port = port.to_string();
    run(
        profile,
        "/usr/bin/nc",
        &[
            OsStr::new("-z"),
            OsStr::new("-G"),
            OsStr::new("5"),
            OsStr::new("127.0.0.1"),
            OsStr::new(&port),
        ],
    )
    .0
}

/// An Own branch server on the Open choice cannot listen on a port the owner's local model
/// uses, and still can listen on any other.
#[test]
fn an_own_branch_server_on_open_cannot_bind_a_configured_local_model_port() {
    let (first, kept) = local_listener();
    let (second, free) = local_listener();
    drop((first, second));
    let network = ide_network(AgentAccess::OwnBranch, AgentNetwork::Open);
    let (_scratch, profile) = kept_profile(network, &[kept]);
    assert!(!binds(&profile, kept), "the local model's port is refused");
    assert!(binds(&profile, free), "any other port still binds");
}

#[test]
fn an_own_branch_server_on_open_reaches_a_local_listener() {
    let (_held, port) = local_listener();
    let network = ide_network(AgentAccess::OwnBranch, AgentNetwork::Open);
    let (_scratch, profile) = network_profile(network);
    assert!(reaches(&profile, port));
}

#[test]
fn an_own_branch_server_on_port_443_refuses_another_port_and_renders_the_443_rule() {
    let (_held, port) = local_listener();
    let network = ide_network(AgentAccess::OwnBranch, AgentNetwork::Https);
    let (_scratch, profile) = network_profile(network);
    assert!(!reaches(&profile, port));
    assert!(
        fs::read_to_string(&profile)
            .unwrap()
            .contains(r#"(remote ip "*:443")"#)
    );
}

#[test]
fn an_own_branch_server_on_none_is_the_same_as_sandboxed() {
    let (_held, port) = local_listener();
    let none = ide_network(AgentAccess::OwnBranch, AgentNetwork::None);
    let (_scratch, profile) = network_profile(none.clone());
    assert!(!reaches(&profile, port));
    assert_eq!(
        none,
        ide_network(AgentAccess::Sandboxed, AgentNetwork::Open)
    );
}

#[test]
fn a_sandboxed_server_refuses_a_local_listener_even_on_open() {
    let (_held, port) = local_listener();
    let network = ide_network(AgentAccess::Sandboxed, AgentNetwork::Open);
    let (_scratch, profile) = network_profile(network);
    assert!(!reaches(&profile, port));
}

/// For a locked folder, the Remote settings lock holds against a path spelled with the long s, the
/// only other letter APFS folds to `s`, in the folders above the lock: the engine's settings keep
/// their bytes.
#[test]
fn the_remote_settings_lock_holds_against_a_long_s_spelling() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(FOLDER).join("p1");
    let build = data.join(FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    let machine = home.join("data").join("Machine");
    for path in [&machine, &build, &root] {
        folder(path);
    }
    let settings = machine.join("settings.json");
    let engine = "{\"chat.disableAIFeatures\": true}\n";
    fs::write(&settings, engine).unwrap();
    let tmp = crate::toolchains::make_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[machine.as_path()],
    };
    let profile = install(
        &crate::test_hooks::STAND_IN,
        &mine,
        &grants,
        PORT,
        closed(),
        &[],
        AgentFilesRule::default(),
    )
    .unwrap();
    let overwrite = r#"printf '{"chat.disableAIFeatures": false}' > "$1""#;
    for variant in [
        mine.join("\u{17f}erver/data/Machine/settings.json"),
        mine.join("\u{17f}erver/data/Machine/\u{17f}ettings.json"),
        mine.join("server/data/Machine/\u{17f}ettings.json"),
    ] {
        assert!(!sh(&profile, overwrite, &variant), "{}", variant.display());
    }
    assert_eq!(fs::read_to_string(&settings).unwrap(), engine);
    assert_eq!(fs::read_dir(&machine).unwrap().count(), 1);
    crate::toolchains::remove_tmp(crate::test_hooks::STAND_IN.toolchain, &home).unwrap();
}
