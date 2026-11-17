use super::*;
use crate::plugins::TOOL_NAME_BYTES_MAX;
use crate::protocol::ProjectId;

fn refused(action: Action) -> String {
    action.validate().unwrap_err().0
}

#[test]
fn tool_actions_check_the_server_id_and_tool_name_before_the_engine() {
    let project = Some(ProjectId::new("app").unwrap());
    let set = |server: Option<&str>, tool: &str| Action::SetTool {
        project: project.clone(),
        server: server.map(str::to_owned),
        tool: tool.into(),
        switch: Switch::Off,
    };
    for action in [
        Action::Tools {
            project: project.clone(),
            server: Some("Bad_Id".into()),
        },
        Action::RestoreTools {
            project: None,
            server: Some("Bad_Id".into()),
        },
        set(Some("Bad_Id"), "echo"),
        set(Some(""), "echo"),
        set(Some(&"a".repeat(65)), "echo"),
    ] {
        assert_eq!(refused(action), "server_not_found");
    }
    for tool in [
        String::new(),
        "t".repeat(TOOL_NAME_BYTES_MAX + 1),
        "a\nb".into(),
    ] {
        assert_eq!(refused(set(None, &tool)), "tool_unknown", "{tool:?}");
    }
    set(None, &"t".repeat(TOOL_NAME_BYTES_MAX))
        .validate()
        .unwrap();
    set(Some("server-1"), "echo").validate().unwrap();
    Action::Tools {
        project: None,
        server: None,
    }
    .validate()
    .unwrap();
}

#[test]
fn reading_tools_is_not_a_change_and_setting_or_restoring_them_is() {
    let project = Some(ProjectId::new("app").unwrap());
    assert!(
        !Action::Tools {
            project: project.clone(),
            server: None,
        }
        .changes()
    );
    assert!(
        Action::SetTool {
            project: project.clone(),
            server: None,
            tool: "start_work".into(),
            switch: Switch::Off,
        }
        .changes()
    );
    assert!(
        Action::RestoreTools {
            project,
            server: None,
        }
        .changes()
    );
}

/// The Settings check of the default pack's pinned context-mode server never starts the
/// approved npx launch when no shared install can run: inside the backoff after a failed
/// install it records the minutes until the retry, for a version the pack does not pin and
/// without npm beside npx it records its own code, and npx never starts.
#[cfg(unix)]
#[test]
fn the_context_mode_check_names_why_no_shared_install_can_run_and_never_starts_npx() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("engine");
    let bin = root.join("setup").join("bin");
    let npm = root.join("setup").join("npm");
    for folder in [&npm, &bin] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)
            .unwrap();
    }
    let ran = base.join("npx-ran");
    let npx = bin.join("npx");
    std::fs::write(
        &npx,
        format!("#!/bin/sh\n: > '{}'\nexit 3\n", ran.display()),
    )
    .unwrap();
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(bin.join("node"), "#!/bin/sh\necho v24.0.0\n").unwrap();
    std::fs::set_permissions(bin.join("node"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("/usr/bin/false", bin.join("npm")).unwrap();
    let check = |version: &str| {
        boundary_check(
            &root,
            "mcp-0123456789abcdef01234567",
            npx.to_str().unwrap(),
            &["-y".to_owned(), format!("context-mode@{version}")],
            &[],
            &[],
            crate::sandbox::SandboxNetwork::Only(Vec::new()),
        )
        .unwrap_err()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let failed = npm.join("context-mode@1.0.169.failed");
    std::fs::write(&failed, now.to_string()).unwrap();
    let held = check("1.0.169");
    assert_eq!(held, crate::plugins::install_failed(10));
    assert_eq!(
        crate::plugins::check_problem(&held),
        "Setting up context-mode failed. Check again in 10 minutes."
    );
    assert_eq!(check("9.9.9"), "server_version_unpinned");
    std::fs::remove_file(&failed).unwrap();
    std::fs::remove_file(bin.join("npm")).unwrap();
    assert_eq!(check("1.0.169"), "server_npm_missing");
    assert!(!ran.exists(), "npx never started");
    assert!(
        !root.join(CHECK_FOLDER).exists(),
        "no check home was made for a check that did not run"
    );
}

/// The Settings check of the default pack's pinned context-mode server on a Node.js older
/// than 22.5 records its own code, so the row says why, and neither npx nor npm starts and no
/// failure is recorded.
#[cfg(unix)]
#[test]
fn the_context_mode_check_on_a_node_older_than_22_5_says_so_and_never_starts_npx() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("engine");
    let bin = root.join("setup").join("bin");
    let npm = root.join("setup").join("npm");
    for folder in [&npm, &bin] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)
            .unwrap();
    }
    let ran = base.join("npx-ran");
    let npx = bin.join("npx");
    std::fs::write(
        &npx,
        format!("#!/bin/sh\n: > '{}'\nexit 3\n", ran.display()),
    )
    .unwrap();
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(bin.join("node"), "#!/bin/sh\necho v22.4.1\n").unwrap();
    std::fs::set_permissions(bin.join("node"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("/usr/bin/false", bin.join("npm")).unwrap();
    let refused = boundary_check(
        &root,
        "mcp-0123456789abcdef01234567",
        npx.to_str().unwrap(),
        &["-y".to_owned(), "context-mode@1.0.169".to_owned()],
        &[],
        &[],
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap_err();
    assert_eq!(refused, "server_node_too_old");
    assert_eq!(
        crate::plugins::check_problem(&refused),
        "context-mode needs Node.js 22.5 or newer. Update Node.js, then check again."
    );
    assert!(!ran.exists(), "npx never started");
    assert!(!npm.join("context-mode@1.0.169.failed").exists());
    assert!(
        !npm.join("context-mode@1.0.169.sb").exists(),
        "npm never started"
    );
    assert!(!root.join(CHECK_FOLDER).exists());
}

/// RV-DS241-150 r2: the Settings check of a spec shaped like the default pack's pinned
/// context-mode launch whose npx the tool resolver does not find, as a repository's own
/// tools folder shipping `node` and `npm`, records its own code once the background folder
/// probes ended, so the row says why, and no file of that folder starts and no check home is
/// made.
#[cfg(unix)]
#[test]
fn the_context_mode_check_of_an_npx_the_resolver_does_not_find_says_so_and_never_starts_it() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("engine");
    let tools = base.join("repo").join("tools");
    for folder in [&root, &tools] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)
            .unwrap();
    }
    let ran = base.join("ran");
    for name in ["npx", "node", "npm"] {
        std::fs::write(
            tools.join(name),
            format!(
                "#!/bin/sh\n/usr/bin/touch '{}'\necho v24.0.0\n",
                ran.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(tools.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let refused = boundary_check(
        &root,
        "mcp-0123456789abcdef01234567",
        tools.join("npx").to_str().unwrap(),
        &["-y".to_owned(), "context-mode@1.0.169".to_owned()],
        &[],
        &[],
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap_err();
    assert_eq!(refused, "server_npx_not_found");
    assert_eq!(
        crate::plugins::check_problem(&refused),
        "CrazyAgent did not find the npx this context-mode server runs on this computer, so it did not start it."
    );
    assert!(!ran.exists(), "a file of that folder started");
    assert!(!root.join(CHECK_FOLDER).exists());
}

/// The Settings check of the default pack's pinned context-mode server runs what an agent
/// launch runs, the engine's shared install with the `node` beside npx, and never npx, so
/// it lists the context-mode tools with no network even where npx cannot start the server
/// at all. Needs `CRAZYAGENT_CONTEXT_MODE_INSTALL`, an npm prefix holding context-mode
/// 1.0.169, and `node` on PATH.
#[cfg(target_os = "macos")]
#[test]
fn the_context_mode_check_runs_the_shared_install_agents_launch_and_never_npx() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    const VERSION: &str = "1.0.169";
    let Some(source) = std::env::var_os("CRAZYAGENT_CONTEXT_MODE_INSTALL") else {
        panic!(
            "UNAVAILABLE: CRAZYAGENT_CONTEXT_MODE_INSTALL, an npm prefix holding node_modules/context-mode@{VERSION}"
        );
    };
    let Some(node) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|folder| folder.join("node"))
            .find(|node| node.is_file())
    }) else {
        panic!("UNAVAILABLE: node on PATH");
    };
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let private = |path: &Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    };
    let root = base.join("engine");
    let bin = root.join("setup").join("bin");
    let npm = root.join("setup").join("npm");
    private(&npm);
    private(&bin);
    std::os::unix::fs::symlink(&node, bin.join("node")).unwrap();
    let npx = bin.join("npx");
    std::fs::write(&npx, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).unwrap();
    let copied = crate::spawn_gate::status(
        std::process::Command::new("/bin/cp")
            .arg("-cR")
            .arg(&source)
            .arg(npm.join(format!("context-mode@{VERSION}"))),
    )
    .unwrap();
    assert!(copied.success());
    let args = vec!["-y".to_owned(), format!("context-mode@{VERSION}")];
    let approved = ServerSpec::Stdio {
        command: npx.to_str().unwrap().to_owned(),
        args: args.clone(),
    };
    assert!(
        crate::context_mode::for_launch(&root, approved)
            .ok()
            .as_ref()
            .and_then(crate::context_mode::shared_install)
            .is_some(),
        "the fixture's shared install is complete, so an agent launch runs it"
    );

    let checked = boundary_check(
        &root,
        "mcp-0123456789abcdef01234567",
        npx.to_str().unwrap(),
        &args,
        &[],
        &[],
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    );
    let tools = checked.unwrap_or_else(|problem| {
        panic!("the check runs the shared install agents launch, not npx: {problem}")
    });
    assert!(
        tools.iter().any(|tool| tool == "ctx_execute"),
        "the check lists the context-mode tools: {tools:?}"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn a_windows_command_server_check_runs_in_its_boundary_or_says_why_not() {
    const ID: &str = "mcp-0123456789abcdef01234567";
    let directory = tempfile::tempdir().unwrap();
    let root = crate::sandbox::canonical(directory.path())
        .unwrap()
        .join("engine");
    std::fs::create_dir_all(&root).unwrap();
    let check = |command: &Path| {
        boundary_check(
            &root,
            ID,
            command.to_str().unwrap(),
            &["--list".to_owned()],
            &[],
            &[],
            crate::sandbox::SandboxNetwork::Only(Vec::new()),
        )
        .unwrap_err()
    };
    assert_eq!(
        check(&root.join("missing-server.exe")),
        "server_command_not_found"
    );
    let server = std::env::current_exe().unwrap();
    let helper = server.with_file_name(crate::sandbox::windows::HELPER_NAME);
    let checked = check(&server);
    if helper.is_file() {
        assert_eq!(checked, "server_exited");
    } else {
        assert_eq!(
            checked,
            crate::plugins::check_refused(crate::sandbox::windows::APPCONTAINER_MISSING)
        );
    }
    for name in ["project", "home", "tmp"] {
        assert!(root.join(CHECK_FOLDER).join(ID).join(name).is_dir());
    }
}

/// The read entry a server's check was granted on its program's folder is taken back when the
/// server is removed, and its container is deleted with the folder Windows gave it, so nothing
/// of the dead container stays; without the boundary helper beside the engine nothing is
/// granted and the check says why.
#[cfg(target_os = "windows")]
#[tokio::test]
async fn a_removed_servers_check_grants_and_container_are_taken_back() {
    const ID: &str = "mcp-0123456789abcdef01234568";
    let directory = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(directory.path()).unwrap();
    let root = base.join("engine");
    let folder = base.join("server");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    let server = folder.join("server.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &server).unwrap();
    let entries = || {
        let listed =
            crate::spawn_gate::output(std::process::Command::new("icacls").arg(&folder)).unwrap();
        assert!(listed.status.success(), "{listed:?}");
        String::from_utf8_lossy(&listed.stdout).into_owned()
    };
    let before = entries();
    let checked = boundary_check(
        &root,
        ID,
        server.to_str().unwrap(),
        &["--list".to_owned()],
        &[],
        &[],
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap_err();
    let helper = std::env::current_exe()
        .unwrap()
        .with_file_name(crate::sandbox::windows::HELPER_NAME);
    if !helper.is_file() {
        assert_eq!(
            checked,
            crate::plugins::check_refused(crate::sandbox::windows::APPCONTAINER_MISSING)
        );
        return;
    }
    assert_eq!(checked, "server_exited");
    assert_ne!(entries(), before);
    let checks = root.join(CHECK_FOLDER);
    let profile = check_profile(&checks, ID);
    let name = crazyagent_win_sandbox::profile::BoundaryProfile::load(&profile)
        .unwrap()
        .container;
    let container = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
        .join("Packages")
        .join(name);
    assert!(container.is_dir());
    let cleared = check_folders::remove_check_folder(&checks, ID);
    assert!(!checks.join(ID).exists());
    cleared.await.unwrap();
    assert_eq!(entries(), before);
    assert!(!profile.exists());
    assert!(!container.exists());
}

/// A check container a crash left behind is released when the engine starts if no server of
/// its id is stored: the read entry on its program's folder is taken back and its container,
/// profile and check folder are deleted, while a stored server's check container stays as it
/// was until that server is gone too; without the boundary helper nothing is granted.
#[cfg(target_os = "windows")]
#[test]
fn a_check_container_with_no_stored_server_is_released_at_engine_start() {
    const DEAD: &str = "mcp-0123456789abcdef0123456b";
    const LIVE: &str = "mcp-0123456789abcdef0123456c";
    let directory = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(directory.path()).unwrap();
    let root = base.join("engine");
    std::fs::create_dir_all(&root).unwrap();
    let entries = |folder: &std::path::Path| {
        let listed =
            crate::spawn_gate::output(std::process::Command::new("icacls").arg(folder)).unwrap();
        assert!(listed.status.success(), "{listed:?}");
        String::from_utf8_lossy(&listed.stdout).into_owned()
    };
    let helper = std::env::current_exe()
        .unwrap()
        .with_file_name(crate::sandbox::windows::HELPER_NAME);
    let mut programs = Vec::with_capacity(2);
    for id in [DEAD, LIVE] {
        let folder = base.join(format!("server-{id}"));
        std::fs::create_dir_all(&folder).unwrap();
        let server = folder.join("server.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &server).unwrap();
        let before = entries(&folder);
        let checked = boundary_check(
            &root,
            id,
            server.to_str().unwrap(),
            &["--list".to_owned()],
            &[],
            &[],
            crate::sandbox::SandboxNetwork::Only(Vec::new()),
        )
        .unwrap_err();
        if !helper.is_file() {
            assert_eq!(
                checked,
                crate::plugins::check_refused(crate::sandbox::windows::APPCONTAINER_MISSING)
            );
            return;
        }
        assert_eq!(checked, "server_exited");
        let granted = entries(&folder);
        assert_ne!(granted, before);
        programs.push((folder, before, granted));
    }
    let checks = root.join(CHECK_FOLDER);
    let container = |id: &str| {
        let name =
            crazyagent_win_sandbox::profile::BoundaryProfile::load(&check_profile(&checks, id))
                .unwrap()
                .container;
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
            .join("Packages")
            .join(name)
    };
    let (dead, live) = (container(DEAD), container(LIVE));
    assert_ne!(dead, live);
    assert!(dead.is_dir());
    assert!(live.is_dir());
    let connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::storage::plugins::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO plugin_items(id,kind,scope,name,body) VALUES (?1,'server','global','live','{}')",
            [LIVE],
        )
        .unwrap();

    crate::storage::trash::sweep_at_start(&connection, None).unwrap();

    let (dead_folder, dead_before, _) = &programs[0];
    let (live_folder, live_before, live_granted) = &programs[1];
    assert_eq!(&entries(dead_folder), dead_before);
    assert!(!check_profile(&checks, DEAD).exists());
    assert!(!dead.exists());
    assert!(!checks.join(DEAD).exists());
    assert_eq!(&entries(live_folder), live_granted);
    assert!(check_profile(&checks, LIVE).is_file());
    assert!(live.is_dir());
    assert!(checks.join(LIVE).is_dir());

    connection
        .execute("DELETE FROM plugin_items WHERE id=?1", [LIVE])
        .unwrap();
    crate::storage::trash::sweep_at_start(&connection, None).unwrap();
    assert_eq!(&entries(live_folder), live_before);
    assert!(!check_profile(&checks, LIVE).exists());
    assert!(!live.exists());
    assert!(!checks.join(LIVE).exists());
}

/// A server removed while one of its checks runs keeps its check folder until that check
/// ends, so the check cannot put back what the removal cleared; a server with no check
/// running is cleared at once.
#[tokio::test]
async fn a_removed_servers_check_folder_is_cleared_when_its_running_check_ends() {
    const ID: &str = "mcp-0123456789abcdef01234569";
    const IDLE: &str = "mcp-0123456789abcdef0123456a";
    let directory = tempfile::tempdir().unwrap();
    let checks = directory.path().join(CHECK_FOLDER);
    for id in [ID, IDLE] {
        std::fs::create_dir_all(checks.join(id).join("home")).unwrap();
    }
    let slot = CheckSlot::take().unwrap();
    let counted = CheckOf::take(ID, checks.clone(), &slot);
    removed_server_checks(&checks, IDLE);
    assert!(!checks.join(IDLE).exists());
    removed_server_checks(&checks, ID);
    assert!(checks.join(ID).is_dir());
    assert!(check_folders::checking().removed.contains(&ID.to_owned()));
    drop(counted);
    assert!(!checks.join(ID).exists());
    assert!(!check_folders::checking().removed.contains(&ID.to_owned()));
    assert!(!check_folders::checking().running.contains(&ID.to_owned()));
    drop(slot);
}
