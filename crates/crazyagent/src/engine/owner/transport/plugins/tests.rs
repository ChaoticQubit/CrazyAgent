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
    let bin = base.join("bin");
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
    let bin = base.join("bin");
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
    let bin = base.join("bin");
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
