#![cfg(unix)]

use crazyagent::engine::owner::{
    OwnerClient, OwnerClientKind, OwnerMutation, OwnerOperation, OwnerRead, OwnerRequest,
    OwnerResponse,
};
use crazyagent::{engine::Client, protocol::*};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

struct Process {
    child: Child,
    first_line: mpsc::Receiver<String>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    readers: Vec<JoinHandle<()>>,
}

impl Process {
    fn launch(root: &Path, socket: &str) -> Self {
        Self::launch_output(root, socket, Stdio::piped())
    }

    fn launch_output(root: &Path, socket: &str, output: Stdio) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .arg("engine")
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg(socket)
            .stdout(output)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let (send, first_line) = mpsc::sync_channel(1);
        let out = child.stdout.take();
        let captured = stdout.clone();
        let out_reader = std::thread::spawn(move || {
            let Some(out) = out else {
                send.send(String::new()).unwrap();
                return;
            };
            let mut reader = BufReader::new(out);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                captured.lock().unwrap().extend_from_slice(line.as_bytes());
                let _ = send.send(line);
            }
            let mut rest = Vec::new();
            reader.take(65536).read_to_end(&mut rest).unwrap();
            captured.lock().unwrap().extend(rest);
        });
        let err = child.stderr.take().unwrap();
        let captured = stderr.clone();
        let err_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            err.take(65536).read_to_end(&mut bytes).unwrap();
            captured.lock().unwrap().extend(bytes);
        });
        Self {
            child,
            first_line,
            stdout,
            stderr,
            readers: vec![out_reader, err_reader],
        }
    }

    fn ready(&self) {
        let line = self
            .first_line
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(line, "ready\n");
    }

    fn refused(&mut self, code: &str) {
        let line = self
            .first_line
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(line, "", "unsafe bootstrap state must prevent ready");
        let status = self.wait_exit();
        assert!(!status.success());
        assert_eq!(
            String::from_utf8_lossy(&self.stderr.lock().unwrap()).trim(),
            code
        );
    }

    fn wait_exit(&mut self) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "engine exit deadline");
            std::thread::sleep(Duration::from_millis(5));
        };
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        status
    }

    fn stop(&mut self, signal: &str) {
        assert!(
            Command::new("/bin/kill")
                .args(["-s", signal, &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            self.wait_exit().success(),
            "{}",
            String::from_utf8_lossy(&self.stderr.lock().unwrap())
        );
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        self.child.wait().unwrap();
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        if std::thread::panicking() {
            eprintln!(
                "engine stdout: {}\nengine stderr: {}",
                String::from_utf8_lossy(&self.stdout.lock().unwrap()),
                String::from_utf8_lossy(&self.stderr.lock().unwrap()),
            );
        }
    }
}

fn snapshot(id: &str) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("bootstrap-integration").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        command: crazyagent::protocol::Command::Snapshot,
    }
}

#[tokio::test]
async fn cli_ready_requires_authenticated_owner_bootstrap() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let process = Process::launch(root.path(), "engine.sock");
    process.ready();
    let client = Client::new(root.path().join("engine.sock"), Duration::from_secs(2)).unwrap();
    let reply = client.request(snapshot("after-ready")).await.unwrap();
    assert!(matches!(reply.outcome, Outcome::Snapshot { .. }));
    let locator = std::fs::symlink_metadata(root.path().join("owner-current"));
    assert!(
        locator.is_ok(),
        "owner bootstrap locator missing after ready: {locator:?}"
    );
    for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
        let (client, identity) = OwnerClient::discover(root.path(), kind, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(identity.database_instance().len(), 64);
        assert_eq!(identity.engine_epoch().len(), 64);
        assert!(matches!(
            client
                .request(owner_request(
                    "capabilities",
                    0,
                    OwnerOperation::Read {
                        query: OwnerRead::Capabilities
                    }
                ))
                .await
                .unwrap()
                .response,
            OwnerResponse::Read { .. }
        ));
    }
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let receipts: i64 = db
        .query_row("SELECT COUNT(*) FROM operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        receipts, 1,
        "only this test's Snapshot creates a legacy receipt"
    );
}

fn owner_request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn locator(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(root.join("owner-current")).unwrap()).unwrap()
}

fn baseline() -> crazyagent::verification::BaselineDraft {
    use crazyagent::verification::*;
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("project").unwrap(),
        origin: BaselineOrigin::Imported,
        requirements: "Actual bootstrap owner proposal".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"real code".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"real integrity".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    }
}

#[tokio::test]
async fn cli_kill_restart_same_database_rediscovery() {
    let root = private_root();
    let first = Process::launch(root.path(), "first.sock");
    first.ready();
    let ordinary = Client::new(root.path().join("first.sock"), Duration::from_secs(2)).unwrap();
    let mut create = snapshot("create");
    create.command = crazyagent::protocol::Command::CreateProject {
        project_id: ProjectId::new("project").unwrap(),
        spec: ProjectSpec {
            name: "Real owner bootstrap".into(),
            repositories: vec![],
        },
    };
    assert_eq!(
        ordinary.request(create).await.unwrap().outcome,
        Outcome::Applied
    );
    let (client, identity) =
        OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
    let operation = owner_request(
        "pending-proposal",
        1,
        OwnerOperation::Apply {
            mutation: OwnerMutation::ProposeBaseline { draft: baseline() },
        },
    );
    let original = client.request(operation.clone()).await.unwrap();
    let OwnerResponse::Applied {
        operation: original,
    } = original.response
    else {
        panic!("actual owner application required")
    };
    assert_eq!(original.current_revision.value(), 2);
    drop(first);
    let mut second = Process::launch(root.path(), "second.sock");
    second.ready();
    let old_error = client
        .request(owner_request(
            "stale",
            0,
            OwnerOperation::Read {
                query: OwnerRead::Capabilities,
            },
        ))
        .await
        .unwrap_err();
    assert!(old_error.0.contains("Connection refused"), "{old_error}");
    let (current, next) =
        OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
    assert_eq!(identity.database_instance(), next.database_instance());
    assert_ne!(identity.engine_epoch(), next.engine_epoch());
    let reconciled = current
        .request(owner_request(
            "reconcile",
            0,
            OwnerOperation::Reconcile {
                operation_id: operation.request_id.clone(),
                expected_hash: Some(operation.mutation_hash().unwrap()),
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Committed {
        operation: reconciled,
    } = reconciled.response
    else {
        panic!("recorded mutation required")
    };
    assert_eq!(original.provenance, reconciled.provenance);
    assert_eq!(original.result, reconciled.result);
    assert!(reconciled.historical_replay);
    let OwnerResponse::Applied { operation: replay } =
        current.request(operation).await.unwrap().response
    else {
        panic!("actual replay required")
    };
    assert_eq!(original.provenance, replay.provenance);
    assert_eq!(original.result, replay.result);
    assert!(replay.historical_replay);
    assert_eq!(replay.current_revision.value(), 2);
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    for (query, expected) in [
        ("SELECT COUNT(*) FROM owner_operations", 1),
        (
            "SELECT COUNT(*) FROM owner_operations WHERE event_cursor IS NOT NULL",
            1,
        ),
        ("SELECT COUNT(*) FROM verification_baselines", 1),
        ("SELECT revision FROM state", 2),
    ] {
        let actual: i64 = db.query_row(query, [], |r| r.get(0)).unwrap();
        assert_eq!(actual, expected, "{query}");
    }
    drop(db);
    second.stop("TERM");
}

#[tokio::test]
async fn cli_different_database_at_same_anchor_has_distinct_identity() {
    let root = private_root();
    let mut first = Process::launch(root.path(), "engine.sock");
    first.ready();
    let (_, old) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    first.stop("INT");
    assert!(!root.path().join("owner-current").exists());
    std::fs::rename(
        root.path().join("engine.db"),
        root.path().join("preserved.db"),
    )
    .unwrap();
    let mut second = Process::launch(root.path(), "engine.sock");
    second.ready();
    let (_, current) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_ne!(old.database_instance(), current.database_instance());
    assert_ne!(old.engine_epoch(), current.engine_epoch());
    second.stop("TERM");
}

#[test]
fn cli_preserves_unsafe_locator_objects() {
    for kind in ["symlink", "hardlink", "fifo", "oversize", "permissions"] {
        let root = private_root();
        let path = root.path().join("owner-current");
        let code = match kind {
            "symlink" => {
                std::fs::write(root.path().join("foreign"), b"preserve").unwrap();
                std::os::unix::fs::symlink("foreign", &path).unwrap();
                "owner_locator_unsafe"
            }
            "fifo" => {
                assert!(
                    Command::new("/usr/bin/mkfifo")
                        .arg("-m")
                        .arg("600")
                        .arg(&path)
                        .status()
                        .unwrap()
                        .success()
                );
                "owner_locator_unsafe"
            }
            _ => {
                std::fs::write(
                    &path,
                    if kind == "oversize" {
                        vec![b' '; 2049]
                    } else {
                        b"preserve".to_vec()
                    },
                )
                .unwrap();
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode(if kind == "permissions" {
                        0o644
                    } else {
                        0o600
                    }),
                )
                .unwrap();
                if kind == "hardlink" {
                    std::fs::hard_link(&path, root.path().join("extra")).unwrap();
                }
                if kind == "oversize" {
                    "owner_locator_bounds"
                } else {
                    "owner_locator_unsafe"
                }
            }
        };
        use std::os::unix::fs::MetadataExt;
        let before = std::fs::symlink_metadata(&path).unwrap();
        let mut process = Process::launch(root.path(), "engine.sock");
        process.refused(code);
        let after = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.len(),
                before.nlink()
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.len(),
                after.nlink()
            ),
            "{kind}"
        );
        assert!(!root.path().join("engine.db").exists());
    }
}

#[test]
fn cli_rejects_too_long_socket_path_precisely() {
    let parent = private_root();
    let root = parent.path().join("x".repeat(80));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = Process::launch(&root, "engine.sock");
    process.refused("owner_socket_path_too_long");
    assert!(!root.join("engine.db").exists());
}

#[tokio::test]
async fn cli_temp_alias_uses_trusted_canonical_anchor() {
    let parent = private_root();
    let root = parent.path().join("r");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let alias_parent = parent.path().join("alias");
    std::os::unix::fs::symlink(parent.path(), &alias_parent).unwrap();
    let aliased = alias_parent.join("r");
    let mut process = Process::launch(&aliased, "e.sock");
    process.ready();
    OwnerClient::discover(&aliased, OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .unwrap();
    process.stop("TERM");
    let leaf = parent.path().join("leaf");
    std::os::unix::fs::symlink(&root, &leaf).unwrap();
    let mut denied = Process::launch(&leaf, "e.sock");
    denied.refused("owner_anchor_requires_private_directory");
    assert!(leaf.is_symlink());
}

#[tokio::test]
async fn cli_shutdown_stops_both_before_database_unlock() {
    use tokio::io::AsyncWriteExt;
    for signal in ["INT", "TERM"] {
        let root = private_root();
        let mut process = Process::launch(root.path(), "e.sock");
        process.ready();
        let state = locator(root.path());
        let generation = root.path().join(state["generation"].as_str().unwrap());
        let mut stalled = tokio::net::UnixStream::connect(generation.join("owner.sock"))
            .await
            .unwrap();
        stalled.write_all(b"CA").await.unwrap();
        let mut ordinary = tokio::net::UnixStream::connect(root.path().join("e.sock"))
            .await
            .unwrap();
        ordinary.write_all(&[0]).await.unwrap();
        let config = crazyagent::config::resolve_config(
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
        )
        .unwrap();
        assert!(
            crazyagent::engine::Engine::open(&root.path().join("engine.db"), config.clone())
                .await
                .is_err()
        );
        process.stop(signal);
        assert!(!root.path().join("owner-current").exists());
        assert!(!root.path().join("e.sock").exists());
        assert!(!generation.exists());
        assert!(
            tokio::net::UnixStream::connect(root.path().join("e.sock"))
                .await
                .is_err()
        );
        assert!(
            tokio::net::UnixStream::connect(generation.join("owner.sock"))
                .await
                .is_err()
        );
        let engine = crazyagent::engine::Engine::open(&root.path().join("engine.db"), config)
            .await
            .unwrap();
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn discovery_rejects_foreign_database_and_replaced_generation() {
    let root = private_root();
    let process = Process::launch(root.path(), "e.sock");
    process.ready();
    let state = locator(root.path());
    let mut changed = state.clone();
    changed["database"]["inode"] = serde_json::json!(1);
    std::fs::write(
        root.path().join("owner-current"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    let error = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .err()
        .unwrap();
    assert_eq!(error.0, "owner_locator_database_mismatch");
    std::fs::write(
        root.path().join("owner-current"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let generation = root.path().join(state["generation"].as_str().unwrap());
    std::fs::rename(&generation, root.path().join("preserved-generation")).unwrap();
    std::fs::create_dir(&generation).unwrap();
    std::fs::set_permissions(&generation, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .err()
        .unwrap();
    assert_eq!(error.0, "owner_generation_replaced_or_unsafe");
    assert!(generation.exists());
    assert!(root.path().join("preserved-generation").exists());
}

#[test]
fn cli_rejects_unknown_locator_without_ready() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("owner-current");
    std::fs::write(&path, b"preserve foreign locator").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut process = Process::launch(root.path(), "engine.sock");
    process.refused("owner_locator_invalid");
    assert_eq!(std::fs::read(path).unwrap(), b"preserve foreign locator");
}

#[test]
fn cli_preserves_old_seventeen_byte_synthetic_locator() {
    use std::os::unix::fs::MetadataExt;
    let root = private_root();
    let generation = root.path().join("o0123456789abcdef");
    std::fs::create_dir(&generation).unwrap();
    std::fs::set_permissions(&generation, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(generation.join("unknown"), b"preserve old synthetic state").unwrap();
    let identity = serde_json::json!({ "device": 1, "inode": 2 });
    let bytes = serde_json::to_vec(&serde_json::json!({ "version": 1, "database": identity, "db_instance": "a".repeat(64), "engine_epoch": "b".repeat(64), "generation": "o0123456789abcdef", "generation_identity": identity, "owner_socket": identity, "ordinary_name": "e.sock", "ordinary_socket": identity })).unwrap();
    let path = root.path().join("owner-current");
    std::fs::write(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let inode = std::fs::symlink_metadata(&path).unwrap().ino();
    let mut process = Process::launch(root.path(), "e.sock");
    process.refused("owner_locator_invalid");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::symlink_metadata(path).unwrap().ino(), inode);
    assert_eq!(
        std::fs::read(generation.join("unknown")).unwrap(),
        b"preserve old synthetic state"
    );
    assert!(!root.path().join("engine.db").exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn cli_retires_only_recorded_stale_ordinary_socket() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let first = Process::launch(root.path(), "engine.sock");
    first.ready();
    drop(first);
    let second = Process::launch(root.path(), "engine.sock");
    second.ready();
}

#[test]
fn cli_preserves_unrecorded_socket() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("engine.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut process = Process::launch(root.path(), "engine.sock");
    process.refused("socket_path_already_exists");
    assert!(path.exists());
    drop(listener);
}

#[tokio::test]
async fn pending_cancellation_creates_no_runtime_state() {
    let root = private_root();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(true);
    let error = crazyagent::engine::bootstrap::EngineService::start(
        root.path(),
        "e.sock",
        &config,
        &stopped,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.0, "engine_startup_cancelled");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn cli_output_failure_cleans_runtime_before_unlock() {
    use std::os::fd::OwnedFd;
    let root = private_root();
    let (output, peer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(peer);
    let fd: OwnedFd = output.into();
    let mut process = Process::launch_output(root.path(), "e.sock", Stdio::from(fd));
    let status = process.wait_exit();
    assert!(!status.success());
    assert_eq!(
        String::from_utf8_lossy(&process.stderr.lock().unwrap()).trim(),
        "Broken pipe (os error 32)"
    );
    assert!(!root.path().join("owner-current").exists());
    assert!(!root.path().join("e.sock").exists());
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('o')
    }));
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = crazyagent::engine::Engine::open(&root.path().join("engine.db"), config)
        .await
        .unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn cli_shutdown_preserves_observed_replacements() {
    use std::os::unix::fs::MetadataExt;
    for kind in ["locator", "ordinary", "owner"] {
        let root = private_root();
        let mut process = Process::launch(root.path(), "e.sock");
        process.ready();
        let state = locator(root.path());
        let generation = root.path().join(state["generation"].as_str().unwrap());
        let path = match kind {
            "locator" => root.path().join("owner-current"),
            "ordinary" => root.path().join("e.sock"),
            _ => generation.join("owner.sock"),
        };
        std::fs::rename(&path, path.with_extension("preserved")).unwrap();
        std::fs::write(&path, b"preserve replacement").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let identity = std::fs::symlink_metadata(&path).unwrap().ino();
        assert!(
            Command::new("/bin/kill")
                .args(["-s", "TERM", &process.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(!process.wait_exit().success());
        let error = String::from_utf8(process.stderr.lock().unwrap().clone()).unwrap();
        let expected = match kind {
            "locator" => "owner_locator_replaced_or_unsafe",
            "ordinary" => "socket_replaced_during_shutdown",
            _ => "owner_cleanup_replaced_artifact_preserved",
        };
        assert!(
            error.trim().split("; ").any(|part| part == expected),
            "{kind}: {error}"
        );
        assert_eq!(std::fs::symlink_metadata(&path).unwrap().ino(), identity);
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve replacement");
        let config =
            crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p"))
                .unwrap();
        let engine = crazyagent::engine::Engine::open(&root.path().join("engine.db"), config)
            .await
            .unwrap();
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn cli_owner_transport_rejects_wrong_authentication() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = private_root();
    let mut process = Process::launch(root.path(), "e.sock");
    process.ready();
    let state = locator(root.path());
    let path = root
        .path()
        .join(state["generation"].as_str().unwrap())
        .join("owner.sock");
    let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(b"CAO1");
    stream.write_all(&packet).await.unwrap();
    let mut byte = [0; 1];
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
        .await
        .unwrap();
    assert!(
        matches!(result, Ok(0))
            || matches!(result, Err(ref error) if error.kind() == std::io::ErrorKind::ConnectionReset),
        "unauthenticated transport must close before any response: {result:?}"
    );
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
    drop(db);
    OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(2))
        .await
        .unwrap();
    process.stop("TERM");
}
