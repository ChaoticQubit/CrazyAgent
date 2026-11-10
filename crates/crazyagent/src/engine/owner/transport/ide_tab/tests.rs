use super::*;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use std::os::unix::fs::PermissionsExt;

const WAIT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ide_tab_reads_the_build_and_never_opens_a_server_before_it_is_ready() {
    let root = tempfile::Builder::new()
        .prefix("ca-idetab-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config_bytes(&format!("timeout_ms = {}\n", WAIT.as_millis()), "")
            .unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let controller = OwnerController::attach(root.path(), OwnerClientKind::Desktop, WAIT)
        .await
        .unwrap();
    let client = controller.shared_client().unwrap();

    let macos = cfg!(target_os = "macos");
    let state = client.ide_tab_state(false).await.unwrap();
    assert_eq!(
        state.build,
        BuildState::Missing,
        "a fresh engine root has no build, and reading the state starts no download"
    );
    assert_eq!(
        state.unqualified,
        (!macos).then(|| std::env::consts::OS.to_owned()),
        "only macOS runs the IDE server; elsewhere the tab names the platform"
    );
    assert!(
        !root.path().join("ide").join("build-state.json").exists(),
        "reading the state wrote nothing"
    );

    let project = ProjectId::new("ide-tab-1").unwrap();
    assert!(
        !client.ide_tab_touch(project.clone()).await.unwrap(),
        "no IDE server runs for a project never opened"
    );
    let refused = if macos {
        "ide_build_not_ready"
    } else {
        "ide_platform_unqualified"
    };
    assert_eq!(
        client.ide_tab_open(project.clone()).await,
        Err(EngineError(refused.into())),
        "no server starts before the build is verified, nor on an unqualified platform"
    );
    assert!(
        !root.path().join("ide").join(project.as_str()).exists(),
        "a refused open leaves no server state"
    );
    assert!(
        !client.ide_tab_stop(project.clone()).await.unwrap(),
        "stopping a project whose IDE server never ran stops nothing"
    );
    assert!(IDE_TAB_CODES.contains(&"ide_build_not_ready"));
    for code in [
        "ide_extension_install_failed",
        "ide_capability_capacity",
        "ide_capability_unavailable",
        "ide_entropy_unavailable",
        "ide_folder_unavailable",
    ] {
        assert_eq!(
            public_code(EngineError(code.into())),
            code,
            "an open refused while installing the built-in extension or issuing its capability names why"
        );
    }
    for code in ["git_exclude_invalid", "ide_project_folder_invalid"] {
        assert_eq!(
            public_code(EngineError(code.into())),
            code,
            "an open refused for the project's git exclude file or its folder names why (FN-R2b)"
        );
    }
    assert_eq!(
        public_code(EngineError("rusqlite says no".into())),
        "ide_tab_outcome_unknown",
        "an internal failure reaches the client as one plain code"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tab_request_that_cannot_take_the_owner_barrier_in_time_is_refused_busy() {
    let root = tempfile::Builder::new()
        .prefix("ca-idetab-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let busy = Duration::from_millis(1000);
    let config =
        crate::config::resolve_config_bytes(&format!("timeout_ms = {}\n", busy.as_millis()), "")
            .unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let controller = OwnerController::attach(root.path(), OwnerClientKind::Desktop, WAIT)
        .await
        .unwrap();
    let client = controller.shared_client().unwrap();

    let held = service
        .owner_runtime()
        .unwrap()
        .engine
        .owner_egress
        .write()
        .await;
    let asked = std::time::Instant::now();
    assert_eq!(
        client.ide_tab_state(false).await,
        Err(EngineError("owner_busy".into())),
        "a request that cannot take the barrier is refused, not left to eat the open budget"
    );
    assert!(asked.elapsed() >= busy);
    assert!(
        asked.elapsed() < WAIT,
        "the barrier wait is bounded by the engine timeout"
    );
    drop(held);
    assert_eq!(
        client.ide_tab_state(false).await.unwrap().build,
        BuildState::Missing,
        "once the barrier is free the same request is served"
    );
    service.shutdown().await.unwrap();
}

/// Codes in the open path's sources that no IDE tab request answers with: the IDE bridge's
/// replies to the workbench and its extension, the extension approvals, and a line only
/// logged.
const NOT_OPEN: &[&str] = &[
    "ide_agent_message_outcome_unknown",
    "ide_capability_cleanup_failed",
    "ide_capability_refused",
    "ide_capability_replaced_preserved",
    "ide_capability_revoked",
    "ide_comment_outcome_unknown",
    "ide_connection_closed",
    "ide_events_busy",
    "ide_events_lagged",
    "ide_extension_ai_locked",
    "ide_extension_ask_missing",
    "ide_extension_decide_outcome_unknown",
    "ide_extension_request_outcome_unknown",
    "ide_extensions_full",
    "ide_git_outcome_unknown",
    "ide_memory_decide_outcome_unknown",
    "ide_owner_edit_outcome_unknown",
    "ide_project_refused",
    "ide_reader_slow",
    "ide_reply_bounds",
    "ide_request_invalid",
    "ide_request_timeout",
    "ide_request_unavailable",
    "ide_server_group_unrecorded",
    "ide_session_required",
    "ide_session_taken",
    "ide_show_all_agents_outcome_unknown",
    "ide_terminal_unavailable",
];

/// Every plain code the steps of an open can refuse with is listed, so the client never
/// reads a real refusal as an unknown outcome; a code added later must be listed or named
/// here as one no open returns.
#[test]
fn ide_tab_codes_cover_every_refusal_of_the_open_path() {
    let sources = [
        include_str!("../../../../ide/server.rs"),
        include_str!("../../../../ide/bridge.rs"),
        include_str!("../../../../ide/extensions.rs"),
        include_str!("../../../../ide/settings.rs"),
        include_str!("../../../../ide/sandbox.rs"),
        include_str!("../../../../ide/workspace.rs"),
    ];
    let mut found = std::collections::BTreeSet::new();
    for source in sources {
        let code = source.split("mod tests").next().unwrap();
        for (at, _) in code.match_indices("\"ide_") {
            let rest = &code[at + 1..];
            let end = rest
                .find(|character: char| !(character.is_ascii_lowercase() || character == '_'))
                .unwrap();
            if rest[end..].starts_with('"') {
                found.insert(&rest[..end]);
            }
        }
    }
    for name in &found {
        assert!(
            IDE_TAB_CODES.contains(name) || NOT_OPEN.contains(name),
            "{name} is in neither IDE_TAB_CODES nor NOT_OPEN"
        );
    }
    for name in NOT_OPEN {
        assert!(found.contains(name), "{name} is no longer in the sources");
        assert!(!IDE_TAB_CODES.contains(name), "{name} is listed twice");
    }
    assert!(found.len() >= 61, "{}", found.len());
    let mut sorted = IDE_TAB_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, IDE_TAB_CODES);
}

/// A test engine with a projects home and one set-up project, IDE stop, whose real IDE
/// server an open through the owner connection started.
#[cfg(target_os = "macos")]
struct OpenIde {
    _root: tempfile::TempDir,
    home: tempfile::TempDir,
    _stop: tokio::sync::watch::Sender<bool>,
    service: EngineService,
    controller: OwnerController,
    project: ProjectId,
    group: i32,
}

/// Starts a test engine, sets up the IDE stop project in its projects home and opens the
/// project's IDE. Input: the prefix of the engine's folders. Output: the running setup with
/// the server's process group.
#[cfg(target_os = "macos")]
async fn open_ide(prefix: &str) -> OpenIde {
    use crate::engine::owner::{OwnerResponse, OwnerResult};
    use crate::ide::server::tests::{cached_build, recorded_launch};

    let private = |prefix: &str| {
        let folder = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        folder
    };
    let root = private(prefix);
    let home = private(&format!("{prefix}home-"));
    let config =
        crate::config::resolve_config_bytes(&format!("timeout_ms = {}\n", WAIT.as_millis()), "")
            .unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut controller = OwnerController::attach(root.path(), OwnerClientKind::Desktop, WAIT)
        .await
        .unwrap();
    let applied = |response: OwnerResponse| {
        assert!(
            matches!(
                &response,
                OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
            ),
            "{response:?}"
        );
    };
    let home_path = std::fs::canonicalize(home.path()).unwrap();
    applied(
        controller
            .set_projects_home(home_path.to_str().unwrap().into(), 1)
            .await
            .unwrap(),
    );
    let project = ProjectId::new("ide-stop").unwrap();
    applied(
        controller
            .setup_project(
                project.clone(),
                crate::protocol::ProjectSpec {
                    name: "IDE stop".into(),
                    repositories: vec![],
                },
                crate::project_setup::ProjectSetup {
                    about: "Opens and stops its IDE.".into(),
                    tech_stack: vec![],
                    platforms: vec![crate::project_setup::Platform::Website],
                    research: vec![],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                2,
            )
            .await
            .unwrap(),
    );
    let ide = root.path().join("ide");
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(&ide)
        .unwrap();
    std::fs::write(
        ide.join("build-state.json"),
        serde_json::to_vec(&BuildState::Ready {
            version: "1.143.17183".into(),
            root: cached_build(),
        })
        .unwrap(),
    )
    .unwrap();
    let client = controller.shared_client().unwrap();
    let server = client.ide_tab_open(project.clone()).await.unwrap();
    assert!(
        server.url.starts_with("http://127.0.0.1:"),
        "{}",
        server.url
    );
    let group = recorded_launch(&ide.join(project.as_str()));
    assert!(client.ide_tab_touch(project.clone()).await.unwrap());
    OpenIde {
        _root: root,
        home,
        _stop: stop,
        service,
        controller,
        project,
        group,
    }
}

/// A closed project page's stop ends the real IDE server an open started through the owner
/// connection: the stop answers true, the server's process group is gone, and the project
/// no longer counts as running.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_ends_the_ide_server_an_open_started() {
    use crate::ide::server::tests::gone;

    let mut open = open_ide("ca-idestop-").await;
    let client = open.controller.shared_client().unwrap();
    let project = open.project.clone();
    assert!(
        client.ide_tab_stop(project.clone()).await.unwrap(),
        "the stop finds the running server"
    );
    gone(open.group).await;
    assert!(
        !client.ide_tab_touch(project.clone()).await.unwrap(),
        "no server runs once stopped"
    );
    assert!(
        !client.ide_tab_stop(project.clone()).await.unwrap(),
        "a second stop finds nothing"
    );
    open.service.shutdown().await.unwrap();
}

/// Delete everything stops the project's IDE server before any folder moves, so no open IDE
/// page writes into the project folder or recreates it after the move. The read-only home
/// keeps the folder out of the Trash, and the server is gone all the same.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_everything_stops_the_projects_ide_server_before_any_folder_moves() {
    use crate::engine::owner::{OwnerResponse, OwnerResult};
    use crate::ide::server::tests::gone;

    let mut open = open_ide("ca-idedel-").await;
    let home = std::fs::canonicalize(open.home.path()).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let response = open
        .controller
        .delete_project(open.project.clone(), "IDE stop".into(), 3)
        .await;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(
            &response,
            Ok(OwnerResponse::Applied { operation })
                if operation.result == OwnerResult::Rejected { code: "home_trash_failed".into() }
        ),
        "{response:?}"
    );
    gone(open.group).await;
    let client = open.controller.shared_client().unwrap();
    assert!(
        !client.ide_tab_touch(open.project.clone()).await.unwrap(),
        "no IDE server runs for a project being deleted"
    );
    open.service.shutdown().await.unwrap();
}

/// An IDE opened while Delete everything waits for the project's agents is refused, so no
/// server runs inside folders on their way to the Trash. The project opens again once the
/// delete has ended, here refused by a read-only home.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ide_opened_while_delete_everything_waits_is_refused_until_the_delete_ends() {
    use crate::engine::owner::{OwnerResponse, OwnerResult};
    use crate::ide::server::tests::gone;

    let mut open = open_ide("ca-idewait-").await;
    let home = std::fs::canonicalize(open.home.path()).unwrap();
    let database = open._root.path().join("engine.db");
    let write = |sql: &str| {
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    };
    write(
        "INSERT INTO team_paused_projects(project,since_ms) VALUES ('ide-stop',1);
         INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (7,'ide-stop','sub','codex',1,'working',0,0);",
    );
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let client = open.controller.shared_client().unwrap();
    let (project, group) = (open.project.clone(), open.group);
    let folder = open
        .controller
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap()
        .project_folder();
    let exclude = folder.join(".git/info/exclude");
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&exclude);
    let deleting = open
        .controller
        .delete_project(project.clone(), "IDE stop".into(), 3);
    let opening = async {
        gone(group).await;
        let reopened = client.ide_tab_open(project.clone()).await;
        let wrote = exclude.exists();
        write("UPDATE team_agents SET status='stopped' WHERE id=7");
        (reopened, wrote)
    };
    let (deleted, (reopened, wrote)) = tokio::join!(deleting, opening);
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let after = client.ide_tab_open(project.clone()).await;
    open.service.shutdown().await.unwrap();
    assert_eq!(
        reopened.map(|server| server.url).map_err(|e| e.0),
        Err("ide_project_deleting".to_owned())
    );
    assert!(
        !wrote,
        "a refused open writes nothing to the project folder"
    );
    assert!(
        matches!(
            &deleted,
            Ok(OwnerResponse::Applied { operation })
                if operation.result == OwnerResult::Rejected { code: "home_trash_failed".into() }
        ),
        "{deleted:?}"
    );
    assert!(after.is_ok(), "{after:?}");
}

/// An open's reply carries what the project's last Full access server left running, and a
/// reply without it, as an older engine sends, still reads.
#[test]
fn an_open_reply_carries_what_the_last_server_left_and_reads_without_it() {
    let server = IdeTabServer {
        url: "http://127.0.0.1:47963/".to_owned(),
        webview_port: 47_963,
        token_path: PathBuf::from("/tmp/ca-ide/token"),
        workspace: PathBuf::from("/tmp/ca-ide/workspace.code-workspace"),
        left: Some("4242 /opt/homebrew/bin/node".to_owned()),
    };
    let text = serde_json::to_string(&server).unwrap();
    assert_eq!(serde_json::from_str::<IdeTabServer>(&text).unwrap(), server);
    let none = IdeTabServer {
        left: None,
        ..server
    };
    let older = serde_json::to_string(&none).unwrap();
    assert!(!older.contains("left"), "{older}");
    assert_eq!(serde_json::from_str::<IdeTabServer>(&older).unwrap(), none);
}
