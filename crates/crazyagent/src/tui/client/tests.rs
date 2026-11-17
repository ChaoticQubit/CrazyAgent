use super::*;
use crate::engine::{Engine, Server};

/// rv-code2 RS1-2: a StartOnboarding whose answer came late and was settled afterwards
/// reads as done when the engine answers the project it started, not as unknown.
#[test]
fn a_settled_stray_onboarding_start_reads_as_done() {
    assert_eq!(
        stray_outcome(OwnerResult::OnboardingStarted {
            project_id: ProjectId::new("my-app-2").unwrap(),
        }),
        Ok(())
    );
    assert_eq!(stray_outcome(OwnerResult::Applied), Ok(()));
    assert_eq!(
        stray_outcome(OwnerResult::Rejected {
            code: "project_exists".into()
        }),
        Err("project_exists".into())
    );
}

#[test]
fn a_tui_that_cannot_attach_to_a_stopped_engine_says_so_plainly() {
    assert_eq!(
        access_problem("owner_locator_database_mismatch"),
        format!(
            "{} (owner_locator_database_mismatch)",
            crate::engine::bootstrap::STALE_ENGINE_TEXT
        )
    );
    assert_eq!(
        access_problem("owner_locator_missing"),
        "owner_locator_missing"
    );
    assert_eq!(
        access_problem("raw text with /a/path"),
        "trusted_owner_connection_failed"
    );
}
use crate::protocol::{Outcome, ProjectId, ProjectSpec};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn response(connection: &mut Connection) -> Result<WireReply, ClientError> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(result) = connection.poll().unwrap() {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "client response exceeded three seconds"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn unavailable_endpoint_retry_preserves_exact_identity_and_commits_once() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.path().join("engine.sock");
    let mut connection =
        Connection::connect(Client::new(socket.clone(), Duration::from_secs(1)).unwrap()).unwrap();
    connection
        .start(
            Command::CreateProject {
                project_id: ProjectId::new("retry-project").unwrap(),
                spec: ProjectSpec {
                    name: "Real retry".into(),
                    repositories: Vec::new(),
                },
            },
            Cursor::new(0),
        )
        .unwrap();
    let original = connection.pending.as_ref().unwrap().request.clone();
    assert!(response(&mut connection).await.is_err());
    assert!(connection.uncertain());
    assert!(connection.start(Command::Snapshot, Cursor::new(0)).is_err());
    assert_eq!(connection.pending.as_ref().unwrap().request, original);
    let config =
        crate::config::resolve_config(&root.path().join("global"), &root.path().join("project"))
            .unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("engine.db"), config.clone())
            .await
            .unwrap(),
    );
    let server = Server::bind(&socket, engine.clone(), &config).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let service = tokio::spawn(server.run(stopped));
    connection.retry().unwrap();
    assert_eq!(connection.pending.as_ref().unwrap().request, original);
    let applied = response(&mut connection).await.unwrap();
    assert_eq!(applied.outcome, Outcome::Applied);
    assert!(!connection.busy());
    let observer = Client::new(socket, Duration::from_secs(1)).unwrap();
    assert_eq!(observer.request(original.clone()).await.unwrap(), applied);
    let reply = observer
        .request(WireRequest {
            request_id: RequestId::new("observer").unwrap(),
            command: Command::Snapshot,
            ..original
        })
        .await
        .unwrap();
    let Outcome::Snapshot { projects, .. } = reply.outcome else {
        panic!("snapshot required")
    };
    assert_eq!(projects.len(), 1);
    assert_eq!(reply.revision.value(), 1);
    drop(connection);
    stop.send(()).unwrap();
    service.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn paired_client_configuration_cannot_be_stripped_back_to_unbound() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-bound-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = crate::engine::bootstrap::EngineService::start(
        root.path(),
        "engine.sock",
        &config,
        &stopped,
    )
    .await
    .unwrap();
    let ordinary = Client::new(root.path().join("engine.sock"), Duration::from_secs(1)).unwrap();
    let (paired, owner, _) = ordinary.with_owner(OwnerClientKind::Tui).await.unwrap();
    let result = paired.into_unbound_configuration();
    drop(owner);
    service.shutdown().await.unwrap();
    assert!(
        matches!(result, Err(error) if error.0 == "tui_requires_original_unbound_configuration")
    );
}

#[tokio::test]
async fn actual_owner_completion_trips_corrupt_pending_invariant() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-pending-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = crate::engine::bootstrap::EngineService::start(
        root.path(),
        "engine.sock",
        &config,
        &stopped,
    )
    .await
    .unwrap();
    let mut connection = Connection::connect_paired(
        Client::new(root.path().join("engine.sock"), Duration::from_secs(1)).unwrap(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(access) = connection.poll_owner().unwrap() {
            assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual owner attach exceeded deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    connection.reconnect().unwrap();
    connection.owner_busy = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let tripped = loop {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| connection.poll_owner())) {
            Err(_) => break true,
            Ok(Ok(None)) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            _ => break false,
        }
    };
    drop(connection);
    service.shutdown().await.unwrap();
    assert!(tripped, "real completion must detect corrupt pending state");
}

#[tokio::test]
async fn limited_mode_retains_exact_original_ordinary_endpoint_without_owner_access() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-limited-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_watch, stopped) = tokio::sync::watch::channel(false);
    let mut owner_service = crate::engine::bootstrap::EngineService::start(
        root.path(),
        "engine.sock",
        &config,
        &stopped,
    )
    .await
    .unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("other.db"), config.clone())
            .await
            .unwrap(),
    );
    let socket = root.path().join("ordinary.sock");
    let server = Server::bind(&socket, engine.clone(), &config).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server.run(stopped));
    let mut native = NativeOwner::new(socket, Duration::from_secs(1));
    let (ordinary, access) = native.attach().await;
    let unavailable = native
        .step(ProjectId::new("ordinary-only-project").unwrap(), 1, true)
        .await;
    let created = ordinary
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("limited-endpoint").unwrap(),
            request_id: RequestId::new("create-ordinary-only").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::CreateProject {
                project_id: ProjectId::new("ordinary-only-project").unwrap(),
                spec: ProjectSpec {
                    name: "Original ordinary endpoint".into(),
                    repositories: vec![],
                },
            },
        })
        .await;
    let (_, reconnected) = native.attach().await;
    let actual_owner =
        Client::new(root.path().join("engine.sock"), Duration::from_secs(1)).unwrap();
    let observed = actual_owner
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("owner-observer").unwrap(),
            request_id: RequestId::new("owner-snapshot").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::Snapshot,
        })
        .await;
    drop(native);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    engine.close().await.unwrap();
    owner_service.shutdown().await.unwrap();
    assert!(matches!(access, OwnerEvent::Access { ready: false, .. }));
    assert!(matches!(
        reconnected,
        OwnerEvent::Access { ready: false, .. }
    ));
    assert!(matches!(unavailable, Err(error) if error == "trusted_connection_unavailable"));
    assert!(matches!(created.unwrap().outcome, Outcome::Applied));
    assert!(
        matches!(observed.unwrap().outcome, Outcome::Snapshot { projects, .. } if projects.is_empty())
    );
}

/// A login service no manager knows and whose definition is never written, so a start
/// runs `executable` detached for the layout. Inputs: the test folder, the layout and
/// the program. Output: the service.
fn idle_service(
    folder: &std::path::Path,
    layout: &crate::paths::Layout,
    executable: PathBuf,
) -> crate::service::Service {
    crate::service::Service {
        platform: crate::paths::Platform::CURRENT,
        name: format!("dev.crazyagent.test.tui-start.{}", std::process::id()),
        executable,
        root: layout.root().to_path_buf(),
        path: "/usr/bin:/bin".into(),
        definition: folder.join("not-installed.plist"),
        user: None,
    }
}

/// Makes a test folder under the real /tmp that only this user can open, as engine
/// data roots must be.
/// Input: the name prefix. Output: the folder.
fn private_folder(prefix: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

/// Waits up to 5 s for the connection's next owner event, polling as the TUI's loop
/// does, and fails the row when none comes.
/// Input: the connection. Output: the event.
async fn next_owner_event(connection: &mut Connection) -> OwnerEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = connection.poll_owner().unwrap() {
            return event;
        }
        assert!(Instant::now() < deadline, "no owner event within 5 s");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// DS-118: a start ask that finds the engine answering starts nothing and reconnects;
/// the service's program here would fail at once had it run, so a ready reconnect
/// shows the attach followed a good start.
#[tokio::test]
async fn a_start_ask_finding_the_engine_answering_starts_nothing_and_reconnects() {
    let root = private_folder("ca-tui-start-ready-");
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut engine = crate::engine::bootstrap::EngineService::start(
        root.path(),
        "engine.sock",
        &config,
        &stopped,
    )
    .await
    .unwrap();
    let layout = crate::paths::Layout::new(root.path().to_path_buf()).unwrap();
    let login = idle_service(root.path(), &layout, PathBuf::from("/usr/bin/false"));
    let client = Client::new(layout.socket(), Duration::from_secs(1)).unwrap();
    let mut connection = Connection::connect_starting(client, Some((layout, login))).unwrap();
    let first = next_owner_event(&mut connection).await;
    assert!(matches!(first, OwnerEvent::Access { ready: true, .. }));
    connection.start_engine().unwrap();
    let again = next_owner_event(&mut connection).await;
    drop(connection);
    engine.shutdown().await.unwrap();
    assert!(matches!(again, OwnerEvent::Access { ready: true, .. }));
}

/// DS-118: a start ask still waiting on its engine leaves the screen free, as the worker
/// runs it, and a quit ends it at once instead of waiting out the start's bound; the
/// program here never opens the engine socket.
#[tokio::test]
async fn a_start_ask_still_waiting_leaves_the_screen_free_and_a_quit_ends_it() {
    let root = private_folder("ca-tui-start-parked-");
    let layout = crate::paths::Layout::new(root.path().join("engine")).unwrap();
    let parked = root.path().join("parked");
    std::fs::write(&parked, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
    std::fs::set_permissions(&parked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let login = idle_service(root.path(), &layout, parked);
    let client = Client::new(layout.socket(), Duration::from_secs(1)).unwrap();
    let mut connection = Connection::connect_starting(client, Some((layout, login))).unwrap();
    let first = next_owner_event(&mut connection).await;
    assert!(matches!(first, OwnerEvent::Access { ready: false, .. }));
    let asked = Instant::now();
    connection.start_engine().unwrap();
    assert!(asked.elapsed() < Duration::from_secs(1));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        connection.poll_owner().unwrap().is_none(),
        "the start still waits"
    );
    assert!(connection.busy());
    let quit = Instant::now();
    drop(connection);
    assert!(
        quit.elapsed() < Duration::from_secs(3),
        "{:?}",
        quit.elapsed()
    );
}

/// DS-118: a TUI opened on an explicit socket has no engine of its own to start, so its
/// start ask only reconnects: the data folder is never prepared and no start failure
/// comes back.
#[tokio::test]
async fn a_start_ask_without_the_default_engine_only_reconnects() {
    let root = private_folder("ca-tui-start-none-");
    let layout = crate::paths::Layout::new(root.path().join("engine")).unwrap();
    let client = Client::new(layout.socket(), Duration::from_secs(1)).unwrap();
    let mut connection = Connection::connect_paired(client).unwrap();
    let first = next_owner_event(&mut connection).await;
    assert!(matches!(first, OwnerEvent::Access { ready: false, .. }));
    connection.start_engine().unwrap();
    let again = next_owner_event(&mut connection).await;
    drop(connection);
    let OwnerEvent::Access { ready, problem, .. } = again else {
        panic!("an access event");
    };
    assert!(!ready && problem != "engine_start_failed", "{problem}");
    assert!(!layout.root().exists(), "nothing was prepared or started");
}
