#![cfg(unix)]

use crazyagent::config::ModelConnection;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, NativeAuthAction, NativeAuthFact, NativeAuthMethod,
    NativeAuthPhase, NativeAuthQuery, NativeAuthReply, NativeAuthScope, NativeAuthStart,
    OwnerClient, OwnerClientKind, OwnerData, OwnerOperation, OwnerRead, OwnerRequest,
    OwnerResponse,
};
use crazyagent::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, RequestId};
use rusqlite::{Connection, OpenFlags, params};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, kill_process, kill_process_group};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const CASE_BOUND: Duration = Duration::from_secs(30);
const REQUEST_BOUND: Duration = Duration::from_secs(2);
const OUTPUT_BOUND: usize = 4096;
const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-857"]
[[model_access.connections]]
id = "connection-857"
provider = "provider-857"
harness = "codex"
protocol = "native"
account_ref = "account-857"
endpoint = { kind = "native" }
model = { id = "model-857", author = "author-857" }
auth = { method = "native_managed", profile_ref = "profile-857" }
"#;

struct ProductEngine {
    child: Child,
    root: tempfile::TempDir,
    state: PathBuf,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    cleanup_attempted: bool,
    forced: bool,
}

impl ProductEngine {
    fn spawn() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-dispatch-857-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("s");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&global)
            .unwrap()
            .write_all(CATALOG.as_bytes())
            .unwrap();
        let child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(&state)
                .arg(global)
                .arg(root.path().join("p.toml"))
                .arg("e.sock")
                .arg("--writable-settings")
                .env_clear()
                .env("HOME", root.path())
                .env("TMPDIR", root.path())
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C")
                .current_dir(root.path())
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let engine = Self {
            child,
            root,
            state,
            stdout: Vec::new(),
            stderr: Vec::new(),
            cleanup_attempted: false,
            forced: false,
        };
        for pipe in [
            engine.child.stdout.as_ref().unwrap() as &dyn std::os::fd::AsFd,
            engine.child.stderr.as_ref().unwrap() as &dyn std::os::fd::AsFd,
        ] {
            let flags = fcntl_getfl(pipe).unwrap();
            fcntl_setfl(pipe, flags | OFlags::NONBLOCK).unwrap();
        }
        engine
    }

    fn drain(&mut self) {
        read_available(self.child.stdout.as_mut().unwrap(), &mut self.stdout);
        read_available(self.child.stderr.as_mut().unwrap(), &mut self.stderr);
    }

    async fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.drain();
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "engine exited before ready"
            );
            if self.stdout == b"ready\n" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "engine startup exceeded five seconds"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn stop(&mut self) -> Option<ExitStatus> {
        if self.cleanup_attempted {
            return self.child.try_wait().ok().flatten();
        }
        self.cleanup_attempted = true;
        if let Some(status) = self.child.try_wait().ok().flatten() {
            return Some(status);
        }
        let pid = Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        let _ = kill_process(pid, Signal::TERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().ok().flatten() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.forced = true;
        let _ = kill_process_group(pid, Signal::KILL);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().ok().flatten() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    fn observer(&self) -> Connection {
        let observer = Connection::open_with_flags(
            self.state.join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        observer.busy_timeout(Duration::from_millis(250)).unwrap();
        observer
    }
}

impl Drop for ProductEngine {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn read_available(pipe: &mut impl Read, bytes: &mut Vec<u8>) {
    let mut buffer = [0; 512];
    loop {
        let remaining = (OUTPUT_BOUND + 1 - bytes.len()).min(buffer.len());
        match pipe.read(&mut buffer[..remaining]) {
            Ok(0) => return,
            Ok(size) => {
                bytes.extend_from_slice(&buffer[..size]);
                assert!(
                    bytes.len() <= OUTPUT_BOUND,
                    "engine output exceeded private bound"
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => panic!("engine output read failed"),
        }
    }
}

fn request_id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

async fn selected_connection(
    client: &OwnerClient,
    database: &str,
) -> (NativeAuthScope, ModelConnection) {
    let reply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("dispatch-selected-connection"),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessRecord {
                    collection: ModelAccessCollection::Connections,
                    id: CredentialRef::new("connection-857").unwrap(),
                },
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("actual owner connection read required");
    };
    let OwnerData::ModelAccessRecord {
        source_hash,
        record: ModelAccessRecord::Connection { connection },
        ..
    } = state.data
    else {
        panic!("actual selected native connection required");
    };
    assert_eq!(connection.id.as_str(), "connection-857");
    let scope = NativeAuthScope {
        expected_database_instance: database.to_owned(),
        connection: connection.id.clone(),
        expected_config_id: state.config_id,
        expected_source_hash: source_hash,
    };
    (scope, connection)
}

fn assert_identity(reply: &NativeAuthReply, start: &NativeAuthStart, epoch: &str) {
    assert_eq!(
        reply.database_instance,
        start.scope.expected_database_instance
    );
    assert_eq!(reply.engine_epoch, epoch);
    assert_eq!(reply.fact.operation_id, start.request_id);
    assert_eq!(reply.fact.generation, 1);
    assert_eq!(
        reply.fact.binding.database_instance,
        reply.database_instance
    );
    assert_eq!(reply.fact.binding.engine_epoch, epoch);
    assert_eq!(reply.fact.binding.config_id, start.scope.expected_config_id);
    assert_eq!(
        reply.fact.binding.source_hash,
        start.scope.expected_source_hash
    );
    assert_eq!(reply.fact.binding.connection.id, start.scope.connection);
    assert_eq!(
        reply.fact.action,
        NativeAuthAction::SignIn {
            method: start.method
        }
    );
    assert!(reply.challenge.is_none());
    assert!(reply.account.is_none());
}

fn stored_fact(observer: &Connection, expected: &NativeAuthFact) -> Vec<u8> {
    let (key, generation, phase, request, bytes): (String, i64, String, Vec<u8>, Vec<u8>) =
        observer
            .query_row(
                "SELECT profile_key,generation,phase,request,fact FROM native_auth_operations
             WHERE id=?1",
                [expected.operation_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
    assert_eq!(key, format!("{:x}", Sha256::digest(b"profile-857")));
    assert_eq!(generation, i64::try_from(expected.generation).unwrap());
    let fact: NativeAuthFact = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(fact.operation_id, expected.operation_id);
    assert_eq!(fact.binding, expected.binding);
    assert_eq!(fact.generation, expected.generation);
    assert_eq!(fact.action, expected.action);
    assert!(matches!(
        fact.phase,
        NativeAuthPhase::Starting | NativeAuthPhase::Unavailable
    ));
    assert_eq!(
        phase,
        serde_json::to_value(fact.phase).unwrap().as_str().unwrap()
    );
    assert!(!request.is_empty());
    assert!(request.len() <= 8192 && bytes.len() <= 16384);
    request
}

fn assert_retired(observer: &Connection, fact: &NativeAuthFact) {
    let stored: Vec<u8> = observer
        .query_row(
            "SELECT fact FROM native_auth_operations WHERE id=?1 AND generation=?2",
            params![
                fact.operation_id.as_str(),
                i64::try_from(fact.generation).unwrap()
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<NativeAuthFact>(&stored).unwrap(),
        *fact
    );
    let fences: i64 = observer
        .query_row(
            "SELECT count(*) FROM native_auth_fences WHERE operation_id=?1 AND generation=?2
             AND db_instance=?3 AND engine_epoch=?4 AND custody_id=?5",
            params![
                fact.operation_id.as_str(),
                i64::try_from(fact.generation).unwrap(),
                fact.binding.database_instance,
                fact.binding.engine_epoch,
                fact.binding.custody_id
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fences, 0);
    let counts: (i64, i64) = observer
        .query_row(
            "SELECT (SELECT count(*) FROM native_auth_operations),
                    (SELECT count(*) FROM native_auth_fences)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 0));
}

fn physical_profile(engine: &ProductEngine, fact: &NativeAuthFact) -> (Vec<u8>, u64, u64) {
    let profile = engine
        .state
        .join("native-profiles")
        .join(format!("{:x}", Sha256::digest(b"profile-857")));
    for path in [&profile, &profile.join("home")] {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.mode() & 0o7777, 0o700);
    }
    let path = profile.join("identity.json");
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert!(metadata.len() <= 16384);
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        fact.binding.custody_id
    );
    (bytes, metadata.dev(), metadata.ino())
}

async fn exercise(engine: &mut ProductEngine) {
    engine.ready().await;
    let (client, identity) =
        OwnerClient::discover(&engine.state, OwnerClientKind::Tui, REQUEST_BOUND)
            .await
            .unwrap();
    let (scope, connection) = selected_connection(&client, identity.database_instance()).await;
    let start = NativeAuthStart {
        request_id: request_id("product-self-custodian-857"),
        scope,
        method: NativeAuthMethod::CodexBrowser,
    };
    let begun = client.start_native_auth(start.clone()).await.unwrap();
    assert_eq!(begun.request_id, start.request_id);
    assert_identity(&begun, &start, identity.engine_epoch());
    assert_eq!(begun.fact.binding.connection, connection);
    assert_eq!(begun.fact.phase, NativeAuthPhase::Starting);
    assert!(begun.fact.code.is_none());
    let observer = engine.observer();
    let original_request = stored_fact(&observer, &begun.fact);
    let deadline = Instant::now() + Duration::from_secs(10);
    let retired = loop {
        engine.drain();
        assert!(
            engine.child.try_wait().unwrap().is_none(),
            "product engine exited"
        );
        let reply = client
            .read_native_auth(NativeAuthQuery {
                request_id: request_id("observe-product-retirement"),
                scope: start.scope.clone(),
                operation_id: start.request_id.clone(),
            })
            .await
            .unwrap();
        assert_identity(&reply, &start, identity.engine_epoch());
        if reply.fact.phase == NativeAuthPhase::Unavailable {
            break reply;
        }
        assert_eq!(reply.fact.phase, NativeAuthPhase::Starting);
        assert!(
            Instant::now() < deadline,
            "product custody retirement exceeded ten seconds"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(retired.request_id, request_id("observe-product-retirement"));
    assert_eq!(
        retired.fact.code.as_deref(),
        Some("native_auth_host_unavailable")
    );
    assert_eq!(retired.fact.binding, begun.fact.binding);
    assert_retired(&observer, &retired.fact);
    let physical = physical_profile(engine, &retired.fact);
    let replay = client.start_native_auth(start.clone()).await.unwrap();
    assert_eq!(replay.request_id, start.request_id);
    assert_identity(&replay, &start, identity.engine_epoch());
    assert_eq!(replay.fact, retired.fact);
    assert_eq!(stored_fact(&observer, &replay.fact), original_request);
    assert_retired(&observer, &replay.fact);
    assert_eq!(physical_profile(engine, &replay.fact), physical);
    assert_eq!(
        std::fs::symlink_metadata(engine.root.path())
            .unwrap()
            .mode()
            & 0o7777,
        0o700
    );
}

#[tokio::test]
async fn product_start_commits_then_retires_exact_generation_and_replays_once() {
    let mut engine = ProductEngine::spawn();
    let result = tokio::time::timeout(CASE_BOUND, exercise(&mut engine)).await;
    let status = engine
        .stop()
        .expect("owned product engine did not exit within cleanup bounds");
    engine.drain();
    assert!(!engine.forced, "product engine required forced cleanup");
    assert!(status.success(), "product engine shutdown failed");
    result.expect("product dispatch case exceeded thirty seconds");
}
