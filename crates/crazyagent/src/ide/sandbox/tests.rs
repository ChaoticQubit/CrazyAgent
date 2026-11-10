use super::*;
use crate::ide::bridge::engine_step::{FORWARD_REQUEST, FORWARDER_VARIABLES};
use crate::sandbox::policy::{AgentAccess, AgentNetwork, ide_network};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

const PORT: u16 = 45_731;

/// The network of a server that reaches nothing, as a Sandboxed one has.
fn closed() -> SandboxNetwork {
    SandboxNetwork::Only(Vec::new())
}

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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
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
    assert_eq!(
        profile(&grants, PORT, closed(), &[]).unwrap_err().0,
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
    assert!(profile(&outside, PORT, closed(), &[]).is_ok());
    assert_eq!(
        profile(&outside, 0, closed(), &[]).unwrap_err().0,
        "ide_sandbox_invalid"
    );
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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
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
    assert!(profile(&grants, PORT, closed(), &[]).is_ok());
    let invalid = |grants: &Grants| profile(grants, PORT, closed(), &[]).unwrap_err().0;
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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
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

const REAL_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// Runs real git for a test's setup, outside any sandbox and with no owner or system
/// config, plus the given variables. Inputs: the folder, the variables and the arguments.
/// Output: whether git exited 0, with what it printed on standard output.
fn setup_git(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (bool, String) {
    let output = crate::spawn_gate::output(
        Command::new(REAL_GIT)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .envs(env.iter().copied())
            .args([
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
            ])
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgSign=false",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn setup(dir: &Path, args: &[&str]) -> String {
    let (ran, printed) = setup_git(dir, &[], args);
    assert!(ran, "git {args:?} in {}", dir.display());
    printed
}

/// Runs git the way the IDE server's git does: the engine's shim under the installed
/// profile, in the server's environment ([`crate::ide::env::server_env`], with the
/// project's git ceiling and configuration), real git first on the path and the server data
/// folder as home. Inputs: the profile, the shim, the home, the project folder, the folder
/// git runs in and the arguments. Output: whether git exited 0, with what it printed on
/// standard error.
fn ide_git(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    dir: &Path,
    args: &[&str],
) -> (bool, String) {
    let output = ide_git_output(profile, shim, home, root, dir, args);
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// [`ide_git`] with everything git printed. Inputs: as [`ide_git`]. Output: git's output.
fn ide_git_output(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    dir: &Path,
    args: &[&str],
) -> std::process::Output {
    ide_git_recorded(profile, shim, home, root, false, dir, args)
}

/// [`ide_git_output`] with the ceiling the engine builds when it records an agent in the
/// project's older agents root or not ([`crate::ide::git::ceiling`]). Inputs: as
/// [`ide_git`], with that answer. Output: git's output.
fn ide_git_recorded(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    older_agents: bool,
    dir: &Path,
    args: &[&str],
) -> std::process::Output {
    let mut command = server_git(
        profile,
        shim,
        home,
        root,
        older_agents,
        "/Library/Developer/CommandLineTools/usr/bin:/usr/bin:/bin",
    );
    command.args(args).current_dir(dir);
    crate::ide::bridge::engine_step::output_with_engine(&mut command, home, root)
}

/// The IDE server's git as VS Code starts it: the shim under the installed profile, in the
/// server's environment built from this PATH and the home, with the project's ceiling for
/// an agent recorded in the older agents root or not, and an owner identity. Inputs: the
/// profile, the shim, the home, the project folder, that answer and the PATH. Output: the
/// command, ready for its arguments and working folder.
fn server_git(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    older_agents: bool,
    path: &str,
) -> Command {
    let source = [
        (
            std::ffi::OsString::from("PATH"),
            std::ffi::OsString::from(path),
        ),
        (
            std::ffi::OsString::from("HOME"),
            home.as_os_str().to_owned(),
        ),
    ];
    let env = crate::ide::env::server_env(
        source,
        &home.join("capability"),
        &home.join("owner.sock"),
        &home.join("locked-settings.json"),
        AgentAccess::Sandboxed,
        Some(crate::ide::env::ServerGit {
            ceiling: &crate::ide::git::ceiling(root, older_agents).unwrap(),
            config: &home.join("gitconfig"),
        }),
    );
    let mut command = super::command(profile, shim);
    command
        .env_clear()
        .envs(env)
        .envs([
            ("GIT_AUTHOR_NAME", "Owner"),
            ("GIT_AUTHOR_EMAIL", "owner@example.invalid"),
            ("GIT_COMMITTER_NAME", "Owner"),
            ("GIT_COMMITTER_EMAIL", "owner@example.invalid"),
        ])
        .envs(crate::ide::bridge::engine_step::forwarder_env(home))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Writes an executable script that records that it ran by creating `marker`, then passes
/// its input through. Inputs: the script path and the marker. Output: none.
fn planted(script: &Path, marker: &Path) {
    fs::write(
        script,
        format!("#!/bin/sh\n: > '{}'\nexec cat\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
}

/// INT-1, IN1-1, IN1-F1 and INT1-R2-1 under the installed IDE profile, through the engine's
/// own git shim and git configuration. Agent folders under a `.crazyagent` project's agents
/// root, as an agent's own clone, a linked worktree whose admin folder it can write, a
/// `gitdir:` file and a bare-looking folder, each with a planted clean filter that runs when
/// git works there, are refused with the hooks switch Off and On, and the IDE cannot read a
/// clone's config. The owner's own tracked `worktrees` folder keeps the project's git. In
/// the Project section the IDE stages and commits, runs the project's hooks only when the
/// switch is On, never signs, and still cannot write the project's git config or hooks.
#[test]
fn ide_git_refuses_agent_folders_and_runs_project_hooks_only_when_on() {
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
    let marks = home.join("marks");
    for path in [&mine, &home, &build, &root, &marks, &root.join("tools")] {
        folder(path);
    }
    let ran = |name: &str| marks.join(name).exists();

    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    let sub = root.join("worktrees").join("sub");
    folder(&sub);
    fs::write(sub.join("own.txt"), "owner\n").unwrap();
    setup(&root, &["add", "a.txt", "worktrees/sub/own.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
    planted(
        &root.join(".git").join("hooks").join("pre-commit"),
        &marks.join("project-hook"),
    );
    planted(
        &root.join("tools").join("sign.sh"),
        &marks.join("project-sign"),
    );
    setup(
        &root,
        &[
            "config",
            "gpg.program",
            root.join("tools/sign.sh").to_str().unwrap(),
        ],
    );
    setup(&root, &["config", "extensions.worktreeConfig", "true"]);
    for key in ["commit.gpgSign", "tag.gpgSign", "tag.forceSignAnnotated"] {
        setup(&root, &["config", key, "true"]);
    }

    let inner = root.join(".crazyagent").join("worktrees");
    folder(&inner);
    let clone = inner.join("agent-clone");
    setup(
        &base,
        &[
            "clone",
            "-q",
            root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let linked = inner.join("agent-linked");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-linked",
            linked.to_str().unwrap(),
        ],
    );
    let gitdir = inner.join("agent-gitdir");
    folder(&gitdir);
    setup(
        &gitdir,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            gitdir.join("meta").to_str().unwrap(),
        ],
    );
    let bare = inner.join("agent-bare");
    folder(&bare);
    setup(&bare, &["init", "-q", "--bare"]);
    setup(&bare, &["config", "core.bare", "false"]);
    setup(&bare, &["config", "core.worktree", bare.to_str().unwrap()]);
    let admin = root.join(".git").join("worktrees").join("agent-linked");
    let agents = [
        (&clone, clone.join(".git").join("config")),
        (&linked, admin.join("config.worktree")),
        (&gitdir, gitdir.join("meta").join("config")),
        (&bare, bare.join("config")),
    ];
    for (agent, config) in &agents {
        let name = agent.file_name().unwrap().to_str().unwrap();
        let filter = agent.join("filter.sh");
        planted(&filter, &marks.join(name));
        let mut text = fs::read_to_string(config).unwrap_or_default();
        text.push_str(&format!(
            "[filter \"evil\"]\n\tclean = {}\n\trequired = true\n",
            filter.display()
        ));
        fs::write(config, text).unwrap();
        fs::write(agent.join(".gitattributes"), "*.txt filter=evil\n").unwrap();
        fs::write(agent.join("notes.txt"), "planted\n").unwrap();
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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let refuse_agents = |mode: &str| {
        for (agent, _) in &agents {
            let name = agent.file_name().unwrap().to_str().unwrap();
            for args in [&["status", "--porcelain"][..], &["add", "notes.txt"]] {
                let (worked, said) = ide_git(&profile, &shim, &home, &root, agent, args);
                assert!(!worked, "{mode}: git {args:?} worked in {name}: {said}");
                let reason = if *agent == &bare {
                    "cannot use bare repository"
                } else {
                    "not a git repository"
                };
                assert!(said.contains(reason), "{mode}: {name}: {said}");
            }
            assert!(!ran(name), "{mode}: {name}'s filter ran");
        }
        let (read, printed) = run(
            &profile,
            "/bin/cat",
            &[clone.join(".git/config").as_os_str()],
        );
        assert!(
            !read && printed.is_empty(),
            "{mode}: the clone's config was read"
        );
        assert!(!sh(
            &profile,
            r#"printf x >> "$1""#,
            &root.join(".git/config")
        ));
        assert!(!sh(
            &profile,
            r#": > "$1""#,
            &root.join(".git/hooks/pre-commit")
        ));
    };

    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(
            None,
            crate::research::runner::sandboxed_git(),
            Some((&root, &home.join(crate::ide::git::PRIVATE_INDEX)))
        )
        .unwrap()
    );
    let (status, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["status", "--porcelain"],
    );
    assert!(
        status,
        "the Project section's git works under the profile: {said}"
    );
    let own = ide_git_output(
        &profile,
        &shim,
        &home,
        &root,
        &sub,
        &["rev-parse", "--show-toplevel"],
    );
    assert!(
        own.status.success(),
        "{}",
        String::from_utf8_lossy(&own.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&own.stdout).trim(),
        root.to_str().unwrap(),
        "the owner's own worktrees folder keeps the project's git"
    );
    fs::write(root.join("a.txt"), "off\n").unwrap();
    let (staged, said) = ide_git(&profile, &shim, &home, &root, &root, &["add", "a.txt"]);
    assert!(staged, "Off: the IDE stages in the Project section: {said}");
    let (committed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-m", "off"],
    );
    assert!(
        committed,
        "Off: the IDE commits in the Project section: {said}"
    );
    assert!(!ran("project-hook"), "Off: the project's hook never runs");
    let (tagged, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["tag", "-a", "-m", "off", "off"],
    );
    assert!(
        tagged,
        "Off: a project that signs every tag still tags: {said}"
    );
    assert!(!setup(&root, &["cat-file", "tag", "off"]).contains("BEGIN PGP"));
    let (signed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-S", "--allow-empty", "-m", "signed"],
    );
    assert!(
        !signed && said.contains("gpg failed to sign"),
        "Off: {said}"
    );
    assert!(!ran("project-sign"));
    refuse_agents("Off");

    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        Some(&root),
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    assert_ne!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(
            None,
            crate::research::runner::sandboxed_git(),
            Some((&root, &home.join(crate::ide::git::PRIVATE_INDEX)))
        )
        .unwrap()
    );
    fs::write(root.join("a.txt"), "on\n").unwrap();
    let (staged, said) = ide_git(&profile, &shim, &home, &root, &root, &["add", "a.txt"]);
    assert!(staged, "On: {said}");
    let (committed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-m", "on"],
    );
    assert!(committed, "On: {said}");
    assert!(
        ran("project-hook"),
        "On: the project's hook runs in the Project section"
    );
    let (signed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-S", "--allow-empty", "-m", "signed"],
    );
    assert!(!signed && said.contains("gpg failed to sign"), "On: {said}");
    assert!(!ran("project-sign"), "On: the IDE never signs");
    refuse_agents("On");
    assert_eq!(setup(&root, &["log", "--format=%s"]), "on\noff\nbase\n");
    for commit in ["HEAD", "HEAD~1"] {
        assert!(
            !setup(&root, &["cat-file", "commit", commit]).contains("gpgsig"),
            "IDE commits in a project that signs every commit go through unsigned"
        );
    }

    assert!(sh(
        &profile,
        r#""$1" < /dev/null"#,
        &root.join("tools/sign.sh")
    ));
    assert!(
        ran("project-sign"),
        "a program the IDE runs can record that it ran"
    );
    for (agent, _) in &agents {
        let name = agent.file_name().unwrap().to_str().unwrap();
        assert!(sh(
            &profile,
            r#""$1" < /dev/null"#,
            &agent.join("filter.sh")
        ));
        assert!(ran(name), "{name}'s filter can record under the profile");
        fs::remove_file(marks.join(name)).unwrap();
        let env: &[(&str, &str)] = if *agent == &bare {
            &[]
        } else {
            &crate::ide::git::CONFIG_ENV
        };
        let (added, _) = setup_git(agent, env, &["add", "notes.txt"]);
        assert!(
            added && ran(name),
            "{name}'s planted filter runs where git reaches it"
        );
    }
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// IN1-R1 under the installed IDE profile: in an older-layout project, whose agents sit at
/// `<project>/worktrees`, an agent folder's unreadable `.git` makes git in the Project
/// section see the agent's files as untracked, so Stage All would commit them past Accept.
/// Once the IDE start hides the agents root ([`crate::projects_home::exclude_agents`]),
/// Stage All through the IDE's git stages the owner's files only, and git in the agent's
/// folder stops at that root (INT1-R2-1).
#[test]
fn stage_all_in_an_older_layout_project_stages_no_agent_file() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("older");
    let home = mine.join("server");
    for path in [&mine, &home, &build, &root, &root.join("worktrees")] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let agent = root.join("worktrees").join("agent-3");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-3",
            agent.to_str().unwrap(),
        ],
    );
    fs::write(agent.join("work.txt"), "agent\n").unwrap();
    fs::write(root.join("notes.txt"), "owner\n").unwrap();
    assert_eq!(
        crate::projects_home::state_folder(&root),
        root,
        "an older-layout project"
    );

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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);

    let before = ide_git_output(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["add", "-A", "--dry-run"],
    );
    let listed = String::from_utf8_lossy(&before.stdout).into_owned();
    assert!(
        listed.contains("worktrees/agent-3/work.txt"),
        "without the exclude, Stage All takes the agent's file: {listed}"
    );

    assert!(crate::projects_home::exclude_agents(&root).unwrap());
    let (staged, said) = ide_git(&profile, &shim, &home, &root, &root, &["add", "-A"]);
    assert!(staged, "{said}");
    assert_eq!(
        setup(&root, &["diff", "--cached", "--name-only"]),
        "notes.txt\n"
    );
    let (committed, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &root,
        &["commit", "-q", "-m", "owner"],
    );
    assert!(committed, "{said}");
    assert_eq!(
        setup(&root, &["show", "--name-only", "--format=", "HEAD"]),
        "notes.txt\n"
    );
    assert_eq!(setup(&agent, &["status", "--porcelain"]), "?? work.txt\n");
    let (inside, said) = ide_git(
        &profile,
        &shim,
        &home,
        &root,
        &agent,
        &["status", "--porcelain"],
    );
    assert!(
        !inside && said.contains("not a git repository"),
        "the older layout's agents root stops the IDE's git: {said}"
    );
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// INT1-R3-1 under the installed IDE profile: an older-layout project keeps an agent's
/// linked worktree at `<project>/worktrees/agent-3` and then gains a `.crazyagent` folder by
/// hand. The profile still hides the agent's `.git`, so with the new layout's root alone git
/// in the agent's folder walks up and acts on the project; with the older root the engine
/// adds while it records the agent there, the IDE's git finds no repository, and the
/// Project section keeps its own.
#[test]
fn an_older_agent_keeps_its_git_stop_after_the_project_gains_a_crazyagent_folder() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("older");
    let home = mine.join("server");
    for path in [&mine, &home, &build, &root, &root.join("worktrees")] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let agent = root.join("worktrees").join("agent-3");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-3",
            agent.to_str().unwrap(),
        ],
    );
    folder(&root.join(".crazyagent"));
    assert_eq!(
        crate::projects_home::state_folder(&root),
        root.join(".crazyagent"),
        "the project now reads as the newer layout"
    );

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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let top = |older_agents: bool, dir: &Path| {
        ide_git_recorded(
            &profile,
            &shim,
            &home,
            &root,
            older_agents,
            dir,
            &["rev-parse", "--show-toplevel"],
        )
    };

    let reached = top(false, &agent);
    assert!(reached.status.success());
    assert_eq!(
        String::from_utf8_lossy(&reached.stdout).trim(),
        root.to_str().unwrap(),
        "with the new layout's root alone the IDE's git in the agent's folder acts on the project"
    );
    let stopped = top(true, &agent);
    let said = String::from_utf8_lossy(&stopped.stderr);
    assert!(
        !stopped.status.success() && said.contains("not a git repository"),
        "the older root stops the IDE's git in the agent's folder: {said}"
    );
    let own = top(true, &root);
    assert!(own.status.success());
    assert_eq!(
        String::from_utf8_lossy(&own.stdout).trim(),
        root.to_str().unwrap()
    );
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// INT1R3-L1 under the installed IDE profile: an older-layout project ran a research job,
/// whose linked worktree stays at `<project>/worktrees/research-market-<job>-1` with no team
/// record, and then gains a `.crazyagent` folder by hand. The engine's own answer for the
/// older agents root keeps it in the ceiling, so the IDE's git in that folder finds no
/// repository instead of acting on the project's, and the Project section keeps its own.
#[test]
fn a_research_worktree_keeps_its_git_stop_after_the_project_gains_a_crazyagent_folder() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("older");
    let home = mine.join("server");
    for path in [&mine, &home, &build, &root, &root.join("worktrees")] {
        folder(path);
    }
    let project = crate::protocol::ProjectId::new("older").unwrap();
    let job = crate::research::jobs::ResearchJobs::open(&data.join("engine.db"))
        .unwrap()
        .enqueue(project.as_str(), "market", "codex", false)
        .unwrap();
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let name = format!("research-market-{job}-1");
    let research = root.join("worktrees").join(&name);
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &name,
            research.to_str().unwrap(),
        ],
    );
    folder(&root.join(".crazyagent"));
    assert_eq!(
        crate::projects_home::state_folder(&root),
        root.join(".crazyagent"),
        "the project now reads as the newer layout"
    );
    let older_agents = crate::ide::workspace::older_agents(&data, &project, &root).unwrap();
    assert!(
        older_agents,
        "the engine counts the research worktree left in the older agents root"
    );

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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let top = |dir: &Path| {
        ide_git_recorded(
            &profile,
            &shim,
            &home,
            &root,
            older_agents,
            dir,
            &["rev-parse", "--show-toplevel"],
        )
    };

    let stopped = top(&research);
    let said = String::from_utf8_lossy(&stopped.stderr);
    assert!(
        !stopped.status.success() && said.contains("not a git repository"),
        "the IDE's git in the research worktree finds no repository: {said}"
    );
    let own = top(&root);
    assert!(own.status.success());
    assert_eq!(
        String::from_utf8_lossy(&own.stdout).trim(),
        root.to_str().unwrap()
    );
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// IN1-FN1 under the installed IDE profile: the IDE cannot read an agent worktree's `.git`,
/// so git in the Project section sees the worktree as missing and a prune would drop its
/// registration. Through the IDE's git, `worktree prune` and `worktree remove` are refused
/// and a gc keeps the registration, while a dry-run prune by real git under the same
/// profile lists the worktree for removal.
#[test]
fn ide_git_never_prunes_an_agent_worktree() {
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
    for path in [&mine, &home, &build, &root] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let agent = root.join(".crazyagent").join("worktrees").join("agent-4");
    setup(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-4",
            agent.to_str().unwrap(),
        ],
    );
    let admin = root.join(".git").join("worktrees").join("agent-4");
    let year = std::time::Duration::from_secs(365 * 24 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(admin.join("index"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - year)
        .unwrap();

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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let said = |output: &std::process::Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };
    let registered = || admin.join("gitdir").is_file() && agent.join(".git").is_file();

    let dry = ide_git_output(
        &profile,
        Path::new(REAL_GIT),
        &home,
        &root,
        &root,
        &["worktree", "prune", "-n", "-v", "--expire=3.months.ago"],
    );
    assert!(
        said(&dry).contains("agent-4"),
        "the IDE sees the agent worktree as missing: {}",
        said(&dry)
    );
    for args in [
        &["worktree", "prune"][..],
        &["worktree", "prune", "-v", "--expire=now"],
        &[
            "worktree",
            "remove",
            "--force",
            "--force",
            agent.to_str().unwrap(),
        ],
    ] {
        let refused = ide_git_output(&profile, &shim, &home, &root, &root, args);
        assert!(!refused.status.success(), "{args:?}");
        assert!(
            said(&refused).contains("the IDE does not run git worktree"),
            "{}",
            said(&refused)
        );
        assert!(registered(), "{args:?}");
    }
    let collected = ide_git_output(&profile, &shim, &home, &root, &root, &["gc", "-q"]);
    assert!(collected.status.success(), "{}", said(&collected));
    assert!(registered(), "a gc in the IDE keeps the agent worktree");
    assert_eq!(setup(&agent, &["status", "--porcelain"]), "");
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// FZ-7 under the installed IDE profile with the engine's own PATH, `/usr/bin` first: the
/// IDE's git never runs the `/usr/bin/git` xcrun shim, whose `xcrun_db` cache write in the
/// per-user temp folder the profile denies on every call. The status and the lookup of an
/// unset key that VS Code makes on every refresh print no such error, and the lookup still
/// exits 1 as git does, which VS Code logs as "Failed to execute git".
#[test]
fn ide_git_runs_no_xcrun_shim_under_the_profile_with_the_engines_path() {
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
    for path in [&mine, &home, &build, &root] {
        folder(path);
    }
    setup(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    let ide = |args: &[&str]| {
        let mut command = server_git(&profile, &shim, &home, &root, false, "/usr/bin:/bin");
        command.args(args).current_dir(&root);
        crate::spawn_gate::output(&mut command).unwrap()
    };

    let status = ide(&["status", "--porcelain"]);
    let said = String::from_utf8_lossy(&status.stderr);
    assert!(status.status.success(), "{said}");
    assert!(
        !said.contains("xcrun_db"),
        "the IDE's status ran the xcrun shim: {said}"
    );
    let unset = ide(&["config", "--get", "--local", "core.virtualfilesystem"]);
    let said = String::from_utf8_lossy(&unset.stderr);
    assert_eq!(unset.status.code(), Some(1), "{said}");
    assert!(
        !said.contains("xcrun_db"),
        "the IDE's config lookup ran the xcrun shim: {said}"
    );
    let git = crate::research::runner::sandboxed_git();
    if git != Path::new("/usr/bin/git") {
        let text = fs::read_to_string(&shim).unwrap();
        for run in [
            format!("  exec '{}' \"$@\"\n", git.display()),
            format!("GIT_INDEX_FILE=\"$work\" '{}' \"$@\"\n", git.display()),
        ] {
            assert!(text.contains(&run), "the shim runs the engine's git: {run}");
        }
    }
    crate::sandbox::toolchains::remove_tmp(&home).unwrap();
}

/// A listener on a free loopback port, with nothing behind it. Output: the listener, held
/// while the row runs, and its port.
fn local_listener() -> (std::net::TcpListener, u16) {
    let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
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
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let (home, root) = (mine.join("server"), base.join("project"));
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
    let profile = install(&mine, &grants, PORT, network, kept_ports).unwrap();
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

const NO_FORWARDER_LINE: &str = "carries git index changes through the IDE's Source Control";

/// One project with one commit under an installed IDE profile, and the engine's shim and
/// private index folder as a launch writes them.
struct Server {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    profile: PathBuf,
    shim: PathBuf,
}

/// Makes a [`Server`]. Output: the server.
fn server() -> Server {
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
    for path in [&mine, &home, &build, &root] {
        folder(path);
    }
    setup(&root, &["init", "-q", "-b", "main"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        crate::sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    Server {
        _scratch: scratch,
        root,
        home,
        profile,
        shim,
    }
}

impl Server {
    /// Git through the shim in the project folder, with the engine step behind it. Input:
    /// the arguments. Output: whether it exited 0 and what it printed on standard error.
    fn git(&self, args: &[&str]) -> (bool, String) {
        ide_git(
            &self.profile,
            &self.shim,
            &self.home,
            &self.root,
            &self.root,
            args,
        )
    }

    /// Git as a task or the IDE shell runs it, by name with the shim's folder first on the
    /// path and no forwarder in its environment. Input: the shell line. Output: its output.
    fn task(&self, line: &str) -> std::process::Output {
        let path = format!(
            "{}:/Library/Developer/CommandLineTools/usr/bin:/usr/bin:/bin",
            crate::ide::settings::machine_folder(&self.home).display()
        );
        let mut command = server_git(
            &self.profile,
            Path::new("/bin/sh"),
            &self.home,
            &self.root,
            false,
            &path,
        );
        for name in FORWARDER_VARIABLES {
            command.env_remove(name);
        }
        command.args(["-c", line]).current_dir(&self.root);
        crate::spawn_gate::output(&mut command).unwrap()
    }

    /// The real index's path. Output: the path.
    fn index(&self) -> PathBuf {
        self.root.join(".git").join("index")
    }

    /// Whether the private index folder is empty. Output: the answer.
    fn no_private_files(&self) -> bool {
        fs::read_dir(self.home.join(crate::ide::git::PRIVATE_INDEX))
            .unwrap()
            .next()
            .is_none()
    }

    /// The owner's own `git status --porcelain`, outside the sandbox. Output: the text.
    fn status(&self) -> String {
        setup(&self.root, &["status", "--porcelain"])
    }
}

/// SECAUDIT-IDE-SERVER-INDEX and SECAUDIT-GIT-STATE-FILES under the installed `server.sb`:
/// no door reaches the real index or its lock, by the plain name or a case variant, and
/// nothing can be planted under the folders that carry a program or another repository.
/// The private index folder stays writable and each tool used by a door works elsewhere.
#[test]
fn the_server_reaches_the_real_index_and_the_program_carrying_files_by_no_door() {
    let s = server();
    fs::write(s.root.join("CaseProbe"), "").unwrap();
    assert!(
        s.root.join("caseprobe").exists(),
        "needs a case-insensitive volume"
    );
    fs::remove_file(s.root.join("CaseProbe")).unwrap();
    let (index, lock) = (s.index(), s.root.join(".git/index.lock"));
    let spare = s.root.join("spare");
    fs::write(&spare, "raw\n").unwrap();
    let before = fs::read(&index).unwrap();
    let from = spare.display();
    let swap = format!(
        "/usr/bin/perl -e 'my ($first, $second) = @ARGV; exit(syscall(488, -2, $first, -2, $second, 2) == 0 ? 0 : 1)' '{from}' \"$1\""
    );
    let doors = [
        ("write", r#"printf x >> "$1""#.to_owned(), index.clone()),
        ("create the lock", r#": > "$1""#.to_owned(), lock.clone()),
        (
            "rename over",
            format!(r#"/bin/mv '{from}' "$1""#),
            index.clone(),
        ),
        (
            "hard link the lock",
            format!(r#"/bin/ln '{from}' "$1""#),
            lock.clone(),
        ),
        (
            "hard link over",
            format!(r#"/bin/ln -f '{from}' "$1""#),
            index.clone(),
        ),
        (
            "clone the lock",
            format!(r#"/bin/cp -c '{from}' "$1""#),
            lock.clone(),
        ),
        ("swap", swap.clone(), index.clone()),
        ("unlink", r#"/bin/rm "$1""#.to_owned(), index.clone()),
        (
            "case variant of the folder",
            r#"printf x >> "$1""#.to_owned(),
            s.root.join(".GIT/index"),
        ),
        (
            "case variant of the file",
            r#"printf x >> "$1""#.to_owned(),
            s.root.join(".git/INDEX"),
        ),
        (
            "case variant of the lock",
            r#": > "$1""#.to_owned(),
            s.root.join(".GIT/INDEX.LOCK"),
        ),
        (
            "move the git folder aside",
            r#"/bin/mv "$1" "$1.aside""#.to_owned(),
            s.root.join(".git"),
        ),
    ];
    for (door, script, target) in &doors {
        assert!(!sh(&s.profile, script, target), "{door} ran");
        assert_eq!(fs::read(&index).unwrap(), before, "{door}");
        assert!(!lock.exists(), "{door}");
    }
    for below in [
        ".git/rebase-merge/git-rebase-todo",
        ".git/rebase-apply/next",
        ".git/remotes/origin",
        ".git/branches/origin",
        ".git/modules/x/hooks/post-checkout",
        ".git/modules/x/config",
        ".GIT/REBASE-MERGE/git-rebase-todo",
        ".git/Remotes/origin",
    ] {
        let target = s.root.join(below);
        let ran = sh(
            &s.profile,
            r#"/bin/mkdir -p "$(/usr/bin/dirname "$1")" && printf x > "$1""#,
            &target,
        );
        assert!(!ran, "{below} was planted");
        assert!(!target.exists(), "{below}");
    }
    for (name, script) in [
        ("hard link", format!(r#"/bin/ln '{from}' "$1""#)),
        ("clone", format!(r#"/bin/cp -c '{from}' "$1""#)),
    ] {
        let target = s.root.join(format!("control {name}"));
        assert!(sh(&s.profile, &script, &target), "control {name}");
        assert!(target.exists(), "control {name}");
    }
    let target = s.root.join("control swap");
    fs::write(&target, "other\n").unwrap();
    assert!(sh(&s.profile, &swap, &target), "control swap");
    assert_eq!(fs::read_to_string(&target).unwrap(), "raw\n");
    let target = s.root.join("control rename");
    assert!(sh(
        &s.profile,
        &format!(r#"/bin/mv '{from}' "$1""#),
        &target
    ));
    let private = s.home.join(crate::ide::git::PRIVATE_INDEX).join("written");
    assert!(sh(&s.profile, r#"printf x > "$1""#, &private));
    fs::remove_file(private).unwrap();
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}

/// A git the shim does not run, as an extension or anything else in the server can start,
/// cannot stage: it fails on the denied lock and the real index is byte-identical.
#[test]
fn a_git_outside_the_shim_cannot_stage() {
    let s = server();
    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    let before = fs::read(s.index()).unwrap();
    let real = Path::new(crate::research::runner::TOOLS_GIT);
    let mut command = server_git(&s.profile, real, &s.home, &s.root, false, "/usr/bin:/bin");
    command.args(["add", "a.txt"]).current_dir(&s.root);
    let output = crate::spawn_gate::output(&mut command).unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("index.lock"),
        "{output:?}"
    );
    assert_eq!(fs::read(s.index()).unwrap(), before);
    assert!(!s.root.join(".git/index.lock").exists());
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}

/// Source Control keeps working through the private index, with the real shim under the
/// installed profile and the real engine step: stage, unstage, a partial stage, commit,
/// checkout and a conflicted merge with its three stages, each read back by the owner's own
/// git outside the sandbox. No private file outlives a command.
#[test]
fn source_control_works_through_the_private_index() {
    let s = server();
    let staged = |args: &[&str]| {
        let (worked, said) = s.git(args);
        assert!(worked, "git {args:?}: {said}");
        assert!(s.no_private_files(), "git {args:?} left a private file");
    };

    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    staged(&["add", "a.txt"]);
    assert_eq!(s.status(), "M  a.txt\n");
    staged(&["reset", "-q", "HEAD", "--", "a.txt"]);
    assert_eq!(s.status(), " M a.txt\n");

    fs::write(s.root.join("partial"), "part\n").unwrap();
    let oid = setup(&s.root, &["hash-object", "-w", "partial"]);
    fs::remove_file(s.root.join("partial")).unwrap();
    staged(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{},a.txt", oid.trim()),
    ]);
    assert_eq!(s.status(), "MM a.txt\n");

    staged(&["commit", "-q", "-m", "ide"]);
    assert_eq!(s.status(), " M a.txt\n");
    assert_eq!(
        setup(&s.root, &["show", "HEAD:a.txt"]),
        "part\n",
        "the commit holds the partial stage"
    );
    staged(&["checkout", "-q", "--", "a.txt"]);
    assert_eq!(s.status(), "");
    assert_eq!(fs::read_to_string(s.root.join("a.txt")).unwrap(), "part\n");

    setup(&s.root, &["switch", "-q", "-c", "other", "HEAD~1"]);
    fs::write(s.root.join("a.txt"), "theirs\n").unwrap();
    setup(&s.root, &["commit", "-q", "-am", "theirs"]);
    setup(&s.root, &["switch", "-q", "main"]);
    let (merged, said) = s.git(&["merge", "other"]);
    assert!(!merged, "the merge conflicts: {said}");
    assert!(s.no_private_files());
    assert_eq!(s.status(), "UU a.txt\n");
    assert_eq!(
        setup(&s.root, &["ls-files", "-u"]).lines().count(),
        3,
        "all three stages reached the real index"
    );

    fs::write(s.root.join("a.txt"), "resolved\n").unwrap();
    staged(&["add", "a.txt"]);
    staged(&["commit", "-q", "--no-edit"]);
    assert_eq!(s.status(), "");
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}

/// A new checkout keeps its own index: `git worktree add` runs a child `reset --hard` and
/// `git clone` its checkout, and on the private index both would lose it, and a carry would
/// then unstage the project's files. Both run directly. The server's own profile refuses a
/// checkout at its `.git`; under a profile with no such limit the checkout gets its own
/// index. In both the project's real index is untouched and no private file is made.
#[test]
fn a_worktree_and_a_clone_through_the_shim_keep_their_own_index() {
    let s = server();
    fs::write(s.root.join("staged"), "staged\n").unwrap();
    setup(&s.root, &["add", "staged"]);
    let before = fs::read(s.index()).unwrap();
    let untouched = |args: &[&str]| {
        assert!(s.no_private_files(), "git {args:?} made a private file");
        assert_eq!(fs::read(s.index()).unwrap(), before, "git {args:?}");
    };
    for args in [
        &["worktree", "add", "-q", "refused", "-b", "refused", "HEAD"][..],
        &["clone", "-q", ".", "refused-clone"][..],
    ] {
        let (worked, said) = s.git(args);
        assert!(!worked, "git {args:?}: {said}");
        assert!(said.contains("Operation not permitted"), "{said}");
        untouched(args);
    }

    let open = s.home.join("open.sb");
    fs::write(&open, "(version 1)(allow default)\n").unwrap();
    let checked = |args: &[&str], folder: &str, index: &str| {
        let (worked, said) = ide_git(&open, &s.shim, &s.home, &s.root, &s.root, args);
        assert!(worked, "git {args:?}: {said}");
        untouched(args);
        assert!(s.root.join(index).exists(), "git {args:?} left no index");
        assert_eq!(setup(&s.root.join(folder), &["status", "--porcelain"]), "");
    };
    checked(
        &["worktree", "add", "-q", "wtree", "-b", "other", "HEAD"],
        "wtree",
        ".git/worktrees/wtree/index",
    );
    checked(
        &["clone", "-q", ".", "cloned"],
        "cloned",
        "cloned/.git/index",
    );
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}

/// A read-only command runs on the real index, so Source Control sees what is staged, and
/// makes no private copy.
#[test]
fn a_read_only_git_sees_the_real_index_and_copies_nothing() {
    let s = server();
    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    setup(&s.root, &["add", "a.txt"]);
    let output = ide_git_output(
        &s.profile,
        &s.shim,
        &s.home,
        &s.root,
        &s.root,
        &["status", "--porcelain"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "M  a.txt\n");
    assert!(s.no_private_files());
    assert!(!s.home.join(FORWARD_REQUEST).exists());
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}

/// A git command that cannot reach the engine fails before git runs, with a plain line:
/// the shim with no forwarder, and a task's `git add` with the shim first on its path, as
/// the IDE shell and tasks run it. Read-only commands still work there.
#[test]
fn a_git_that_cannot_reach_the_engine_fails_before_it_runs() {
    let s = server();
    fs::write(s.root.join("a.txt"), "changed\n").unwrap();
    let before = fs::read(s.index()).unwrap();
    let held = |output: &std::process::Output| {
        assert_eq!(output.status.code(), Some(128), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(NO_FORWARDER_LINE),
            "{output:?}"
        );
        assert_eq!(fs::read(s.index()).unwrap(), before);
        assert!(s.no_private_files());
    };
    let mut direct = server_git(
        &s.profile,
        &s.shim,
        &s.home,
        &s.root,
        false,
        "/usr/bin:/bin",
    );
    for name in FORWARDER_VARIABLES {
        direct.env_remove(name);
    }
    direct.args(["add", "a.txt"]).current_dir(&s.root);
    held(&crate::spawn_gate::output(&mut direct).unwrap());
    held(&s.task("git add a.txt"));
    let read = s.task("git branch --show-current");
    assert!(read.status.success(), "{read:?}");
    assert_eq!(String::from_utf8_lossy(&read.stdout), "main\n");
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}

/// A rebase from the IDE stops with a plain line and leaves no state folder, since the
/// sandbox denies it; a merge still runs.
#[test]
fn a_rebase_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    let (ran, said) = s.git(&["rebase", "main"]);
    assert!(!ran);
    assert!(
        said.contains("does not run git rebase from the IDE"),
        "{said}"
    );
    assert!(!s.root.join(".git/rebase-merge").exists());
    assert!(s.no_private_files());
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}

/// SECAUDIT ide-git-commondir-case-variant-config-redirect: IDE-sandboxed code cannot plant a
/// case-variant `commondir` in the project's git folder.
#[test]
fn ide_sandboxed_code_cannot_plant_a_case_variant_commondir() {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    fs::write(base.join("CaseProbe"), "").unwrap();
    assert!(
        base.join("caseprobe").exists(),
        "needs a case-insensitive volume"
    );
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    for path in [&home, &build, &root] {
        folder(path);
    }
    let tmp = crate::sandbox::toolchains::make_tmp(&home).unwrap();
    setup(&root, &["init", "-q"]);
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
    let profile = install(&mine, &grants, PORT, closed(), &[]).unwrap();
    let wrote = sh(&profile, "printf x > \"$1\"", &root.join(".git/COMMONDIR"));
    let planted: Vec<String> = fs::read_dir(root.join(".git"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.eq_ignore_ascii_case("commondir"))
        .collect();
    assert!(!wrote, "the write ran: {planted:?}");
    assert!(planted.is_empty(), "{planted:?}");
}

/// An intent-to-add entry has no form the carry can bring across, so the shim stops every
/// spelling of it in the project folder with a plain line before git runs, and nothing is
/// staged (SECAUDIT-14-L1).
#[test]
fn an_intent_to_add_from_the_ide_stops_with_a_plain_line() {
    let s = server();
    fs::write(s.root.join("new.txt"), "eight by").unwrap();
    let spellings: [&[&str]; 6] = [
        &["add", "-N", "new.txt"],
        &["add", "--intent-to-add", "new.txt"],
        &["add", "-vN", "new.txt"],
        &["stage", "--inten", "new.txt"],
        &["reset", "-N"],
        &["apply", "-N", "none.patch"],
    ];
    for args in spellings {
        let (ran, said) = s.git(args);
        assert!(!ran, "{args:?}");
        assert!(
            said.contains("does not mark files as intent to add from the IDE"),
            "{args:?}: {said}"
        );
    }
    assert!(s.no_private_files());
    let (ran, said) = s.git(&["add", "--", "-N"]);
    assert!(
        !said.contains("intent to add"),
        "a path after -- is a path: {said}"
    );
    assert!(!ran, "git finds no file named -N: {said}");
    crate::sandbox::toolchains::remove_tmp(&s.home).unwrap();
}
