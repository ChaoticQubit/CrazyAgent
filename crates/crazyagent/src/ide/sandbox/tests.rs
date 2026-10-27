use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

const PORT: u16 = 45_731;

fn folder(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Runs `program` with `args` the way the IDE server starts: under the installed profile
/// through [`command`], from a cleared environment. Output: whether it exited 0, and its
/// standard output.
fn run(profile: &Path, program: &str, args: &[&std::ffi::OsStr]) -> (bool, String) {
    let mut command = super::command(profile, Path::new(program));
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crate::spawn_gate::output(&mut command).unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn a_process_launched_the_ide_way_reads_its_capability_but_no_owner_key_or_engine_database() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let owner = data.join("o00000000");
    let ide = data.join(crate::ide::FOLDER);
    let mine = ide.join("p1");
    let other = ide.join("p2");
    let build = ide.join("builds").join("v1");
    let root = base.join("project");
    let (home, extensions) = (mine.join("server"), mine.join("extensions"));
    for path in [&owner, &home, &extensions, &other, &build, &root] {
        folder(path);
    }
    let files = [
        (owner.join("owner-desktop.cap"), "owner desktop key"),
        (owner.join("owner-tui.cap"), "owner tui key"),
        (owner.join("r00.cap"), "runner key"),
        (data.join("engine.db"), "engine database"),
        (data.join("engine.db-wal"), "engine database log"),
        (mine.join("capability"), "my capability"),
        (mine.join("token"), "my token"),
        (
            mine.join(crate::ide::server::LOG),
            "server output with the token",
        ),
        (extensions.join("extension.js"), "approved extension"),
        (other.join("capability"), "another capability"),
        (build.join("server-main.js"), "build file"),
        (root.join("README"), "project file"),
    ];
    for (path, text) in &files {
        fs::write(path, text).unwrap();
    }
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
    let (capability, token) = (mine.join("capability"), mine.join("token"));
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[extensions.as_path()],
        readable: &[token.as_path(), capability.as_path()],
        locked: &[],
    };
    let profile = install(&mine, &grants, PORT).unwrap();
    assert_eq!(profile, mine.join(PROFILE));
    let read = |path: &Path| run(&profile, "/bin/cat", &[path.as_os_str()]);
    for (path, text) in &files {
        let allowed = [
            &capability,
            &token,
            &build.join("server-main.js"),
            &root.join("README"),
            &extensions.join("extension.js"),
        ]
        .contains(&path);
        let (ran, printed) = read(path.as_path());
        assert_eq!(ran, allowed, "{}", path.display());
        assert_eq!(printed == *text, allowed, "{}", path.display());
    }
    assert!(!run(&profile, "/bin/ls", &[data.as_os_str()]).0);
    assert!(!run(&profile, "/bin/ls", &[owner.as_os_str()]).0);
    let touch = |path: &Path| run(&profile, "/usr/bin/touch", &[path.as_os_str()]).0;
    assert!(touch(home.join("state").as_path()));
    assert!(!touch(extensions.join("state").as_path()));
    assert!(!touch(extensions.join("extension.js").as_path()));
    assert_eq!(
        fs::read_to_string(extensions.join("extension.js")).unwrap(),
        "approved extension"
    );
    assert!(touch(root.join("edited").as_path()));
    assert!(!touch(mine.join("planted").as_path()));
    assert!(!touch(data.join("planted").as_path()));
    assert!(!touch(owner.join("owner-desktop.cap").as_path()));
    assert!(!touch(mine.join(PROFILE).as_path()));
    assert_eq!(
        fs::read_to_string(owner.join("owner-desktop.cap")).unwrap(),
        "owner desktop key"
    );
    let socket = owner.join("owner.sock");
    let stray = owner.join("other.sock");
    let _listeners = [
        crate::spawn_gate::bind_unix_std(&socket).unwrap(),
        crate::spawn_gate::bind_unix_std(&stray).unwrap(),
    ];
    let connect = |path: &Path| {
        run(
            &profile,
            "/usr/bin/perl",
            &[
                std::ffi::OsStr::new("-MIO::Socket::UNIX"),
                std::ffi::OsStr::new("-e"),
                std::ffi::OsStr::new("IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 3"),
                path.as_os_str(),
            ],
        )
        .0
    };
    assert!(connect(socket.as_path()));
    assert!(!connect(stray.as_path()));
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

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
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
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
    assert_eq!(profile(&grants, PORT).unwrap_err().0, "ide_sandbox_invalid");
    let elsewhere = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let project = fs::canonicalize(elsewhere.path()).unwrap();
    let outside = Grants {
        root: &project,
        ..grants
    };
    assert!(profile(&outside, PORT).is_ok());
    assert_eq!(profile(&outside, 0).unwrap_err().0, "ide_sandbox_invalid");
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// Runs one `/bin/sh` script under the profile with `path` as `$1`. Output: whether it
/// exited 0.
fn sh(profile: &Path, script: &str, path: &Path) -> bool {
    run(
        profile,
        "/bin/sh",
        &[
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new(script),
            std::ffi::OsStr::new("sh"),
            path.as_os_str(),
        ],
    )
    .0
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
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
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
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
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
    let profile = install(&mine, &grants, PORT).unwrap();
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
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
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
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    let extensions = mine.join("extensions");
    for path in [&home, &extensions, &build, &root] {
        folder(path);
    }
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
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
    assert!(profile(&grants, PORT).is_ok());
    let invalid = |grants: &Grants| profile(grants, PORT).unwrap_err().0;
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
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// DEC-098 addendum 9: the server cannot read an agent's IDE config, any `.vscode` folder
/// in an agent worktree and any `.code-workspace` file in one, under either agents root and
/// in any letter case, so an agent cannot set the owner's IDE settings. A link an agent
/// commits under those names is refused as well, though the file it leads to stays
/// readable by its own name. The project's own files, the agent's other files and the
/// engine's workspace file stay readable.
#[test]
fn the_ide_server_cannot_read_agent_ide_config() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
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
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
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
    let profile = install(&mine, &grants, PORT).unwrap();
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
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}
