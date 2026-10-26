#![cfg(unix)]

use crazyagent::config::ModelConnection;
use crazyagent::engine::EngineError;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, NativeAuthAction, NativeAuthCancel, NativeAuthLogout,
    NativeAuthMethod, NativeAuthPhase, NativeAuthQuery, NativeAuthReply, NativeAuthScope,
    NativeAuthStart, NativeProfileBinding, OwnerClient, OwnerClientKind, OwnerData, OwnerOperation,
    OwnerRead, OwnerRequest, OwnerResponse,
};
use crazyagent::model_gateway::{Health, Quota};
use crazyagent::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, RequestId};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::fs::Permissions;
use std::future::Future;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a", "account-z"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.connections]]
id = "connection-alias"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-alias", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.connections]]
id = "connection-z"
provider = "provider-z"
harness = "claude"
protocol = "native"
account_ref = "account-z"
endpoint = { kind = "native" }
model = { id = "model-z", author = "author-z" }
auth = { method = "native_managed", profile_ref = "profile-z" }
[[model_access.pools]]
id = "configured-pool"
members = ["connection-a", "connection-alias", "connection-z"]
projects = ["project-fixture"]
roles = ["leader"]
fallback_allowed = false
"#;

const LOCK_PROCESS: &str = r#"
import fcntl, os, select, signal, socket, stat, sys
signal.alarm(15)
path, address, mode = sys.argv[1:4]
expected = tuple(map(int, sys.argv[4:9]))
fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
opened, named = os.fstat(fd), os.lstat(path)
actual = (opened.st_dev, opened.st_ino, opened.st_uid, opened.st_mode, opened.st_nlink)
assert actual == expected
assert stat.S_ISREG(opened.st_mode)
assert stat.S_IMODE(opened.st_mode) == 0o600
assert opened.st_nlink == 1
assert (named.st_dev, named.st_ino) == (opened.st_dev, opened.st_ino)
channel = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    channel.sendto(b'B', address)
    os.close(fd)
    sys.exit(0)
channel.sendto(b'L', address)
if mode == 'probe':
    os.close(fd)
    sys.exit(0)
assert mode == 'hold'
for unused in range(4):
    assert select.select([0], [], [], 10)[0]
    command = os.read(0, 1)
    if command == b'Q':
        sys.exit(0)
    if command == b'D':
        retained = os.dup(fd)
        os.close(fd)
        fd = retained
        channel.sendto(b'D', address)
    elif command == b'C':
        os.close(fd)
        fd = -1
        channel.sendto(b'C', address)
    else:
        raise RuntimeError('invalid fixture command')
raise RuntimeError('fixture command bound')
"#;

static SOCKET_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
struct Basis {
    scope: NativeAuthScope,
    connection: ModelConnection,
    epoch: String,
}

type StoredRow = (String, String, i64, String, Vec<u8>, Vec<u8>);

struct ProfileCase {
    root: tempfile::TempDir,
    state: PathBuf,
    global: PathBuf,
    project: PathBuf,
    service: Mutex<Option<EngineService>>,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn request_id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, Permissions::from_mode(0o700)).unwrap();
    assert!(path.is_dir());
    assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o7777, 0o700);
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), bytes.len() as u64);
    assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o7777, 0o600);
}

impl ProfileCase {
    async fn new(catalog: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-pu-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("s");
        let settings = root.path().join("c");
        private_directory(&state);
        private_directory(&settings);
        let global = settings.join("g.toml");
        let project = settings.join("p.toml");
        private_file(&global, catalog.as_bytes());
        let (_stop, shutdown) = tokio::sync::watch::channel(false);
        let service = tokio::time::timeout(
            Duration::from_secs(5),
            EngineService::start_with_settings_sources(
                &state, "e.sock", &global, &project, &shutdown,
            ),
        )
        .await
        .expect("actual owner startup deadline")
        .unwrap();
        assert!(state.join("engine.db").is_file());
        assert!(global.is_file());
        Self {
            root,
            state,
            global,
            project,
            service: Mutex::new(Some(service)),
        }
    }

    async fn client(&self, kind: OwnerClientKind) -> OwnerClient {
        OwnerClient::discover(&self.state, kind, Duration::from_secs(2))
            .await
            .unwrap()
            .0
    }

    async fn basis(&self, id: &str) -> Basis {
        let (client, identity) =
            OwnerClient::discover(&self.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let reply = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: request_id("actual-profile-use-basis"),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read {
                    query: OwnerRead::ModelAccessRecord {
                        collection: ModelAccessCollection::Connections,
                        id: reference(id),
                    },
                },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read required");
        };
        let OwnerData::ModelAccessRecord {
            source_hash,
            record: ModelAccessRecord::Connection { connection },
            ..
        } = state.data
        else {
            panic!("actual selected connection required");
        };
        assert_eq!(connection.id.as_str(), id);
        assert_eq!(source_hash.len(), 64);
        Basis {
            scope: NativeAuthScope {
                expected_database_instance: identity.database_instance().to_owned(),
                connection: connection.id.clone(),
                expected_config_id: state.config_id,
                expected_source_hash: source_hash,
            },
            connection,
            epoch: identity.engine_epoch().to_owned(),
        }
    }

    async fn prepare(&self, basis: &Basis) -> NativeProfileBinding {
        let service = self.service.lock().await;
        let prepared = service
            .as_ref()
            .unwrap()
            .owner_runtime()
            .unwrap()
            .prepare_native_profile(
                &basis.scope.connection,
                &basis.scope.expected_config_id,
                &basis.scope.expected_source_hash,
            )
            .await
            .unwrap();
        assert_eq!(prepared.binding().connection, basis.connection);
        assert_eq!(prepared.binding().engine_epoch, basis.epoch);
        prepared.binding().clone()
    }

    fn profile(&self, name: &str) -> PathBuf {
        self.state
            .join("native-profiles")
            .join(format!("{:x}", Sha256::digest(name.as_bytes())))
    }

    fn manifest(&self) -> PathBuf {
        self.profile("profile-a").join("identity.json")
    }

    fn stored(&self) -> Vec<StoredRow> {
        let observer = Connection::open_with_flags(
            self.state.join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = observer
            .prepare(
                "SELECT id,profile_key,generation,phase,request,fact
             FROM native_auth_operations ORDER BY id LIMIT 1025",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<StoredRow>, _>>()
            .unwrap();
        assert!(rows.len() <= 1024);
        assert!(
            rows.iter()
                .all(|row| row.4.len() <= 8192 && row.5.len() <= 16384)
        );
        rows
    }

    async fn assert_gateway_unknown(&self, id: &str) {
        let service = self.service.lock().await;
        let state = service
            .as_ref()
            .unwrap()
            .owner_runtime()
            .unwrap()
            .model_connection_state(&reference(id))
            .await
            .unwrap();
        assert_eq!(state.health, Health::Unknown);
        assert_eq!(state.quota, Quota::Unknown);
        assert!(state.observed_at_ms.is_none());
        assert!(state.capabilities.is_empty());
    }

    async fn close(&self) {
        let mut service = self.service.lock().await.take().unwrap();
        tokio::time::timeout(Duration::from_secs(5), service.shutdown())
            .await
            .expect("actual owner shutdown deadline")
            .unwrap();
        assert!(self.global.is_file());
        assert!(!self.project.exists());
    }
}

async fn run_case<F, T>(catalog: &str, seconds: u64, body: F)
where
    F: FnOnce(Arc<ProfileCase>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let case = Arc::new(ProfileCase::new(catalog).await);
    let mut task = tokio::spawn(body(Arc::clone(&case)));
    let result = tokio::time::timeout(Duration::from_secs(seconds), &mut task).await;
    if result.is_err() {
        task.abort();
        let cancelled = task.await;
        assert!(cancelled.is_err());
    }
    case.close().await;
    assert!(case.root.path().is_dir());
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Ok(Err(error)) => panic!("owner test task ended: {error}"),
        Err(error) => panic!("owner test body deadline: {error}"),
    }
}

struct LockProcess {
    child: Child,
    readiness: UnixDatagram,
    commands: u8,
    retired: bool,
}

impl LockProcess {
    fn spawn(case: &ProfileCase, mode: &str, expected: u8) -> Self {
        let path = case.manifest();
        let stamp = std::fs::symlink_metadata(&path).unwrap();
        assert!(stamp.is_file());
        assert_eq!(stamp.nlink(), 1);
        let socket = case.root.path().join(format!(
            "l{}.sock",
            SOCKET_ID.fetch_add(1, Ordering::Relaxed),
        ));
        let readiness = crazyagent::spawn_gate::bind_unix_datagram_std(&socket).unwrap();
        readiness
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let child = crazyagent::spawn_gate::spawn(
            Command::new("/usr/bin/python3")
                .env_clear()
                .args(["-I", "-S", "-c", LOCK_PROCESS])
                .arg(&path)
                .arg(&socket)
                .arg(mode)
                .args([
                    stamp.dev().to_string(),
                    stamp.ino().to_string(),
                    stamp.uid().to_string(),
                    stamp.mode().to_string(),
                    stamp.nlink().to_string(),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .expect("real lock fixture process required");
        let mut process = Self {
            child,
            readiness,
            commands: 0,
            retired: false,
        };
        assert_eq!(process.event(), expected);
        process
    }

    fn event(&mut self) -> u8 {
        let mut bytes = [0; 2];
        let received = self.readiness.recv(&mut bytes);
        assert!(
            received.is_ok(),
            "lock readiness failed: {received:?}, child {:?}",
            self.child.try_wait()
        );
        assert_eq!(received.unwrap(), 1);
        bytes[0]
    }

    fn command(&mut self, command: u8) {
        assert!(self.commands < 4);
        assert!(!self.retired);
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&[command])
            .unwrap();
        self.commands += 1;
    }

    fn wait_exit(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait()? {
                self.retired = true;
                return Ok(Some(status));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(None)
    }

    fn finish(mut self, holding: bool) {
        if holding {
            self.command(b'Q');
        }
        let status = self.wait_exit().expect("lock child retirement status");
        assert!(
            status.is_some(),
            "lock child retirement deadline before fallback cleanup"
        );
        assert!(status.unwrap().success());
        assert!(self.retired);
    }

    fn terminate(mut self) {
        assert!(self.child.try_wait().unwrap().is_none());
        self.child.kill().unwrap();
        let status = self
            .wait_exit()
            .expect("killed lock child retirement status");
        assert!(
            status.is_some(),
            "killed lock child retirement deadline before fallback cleanup"
        );
        assert!(!status.unwrap().success());
        assert!(self.retired);
    }
}

impl Drop for LockProcess {
    fn drop(&mut self) {
        if self.retired {
            return;
        }
        eprintln!(
            "lock fixture {} entered fallback after an unfinished path",
            self.child.id()
        );
        let killed = self.child.kill();
        let status = self.wait_exit();
        if std::thread::panicking() {
            eprintln!("lock fixture fallback kill={killed:?}, retirement={status:?}");
        } else {
            assert!(
                matches!(status, Ok(Some(_))),
                "lock fixture fallback did not retire the child: {status:?}"
            );
            panic!("lock fixture required fallback cleanup: {killed:?}");
        }
    }
}

fn start(basis: &Basis, id: &str) -> NativeAuthStart {
    NativeAuthStart {
        request_id: request_id(id),
        scope: basis.scope.clone(),
        method: if basis.connection.harness.as_str() == "codex" {
            NativeAuthMethod::CodexBrowser
        } else {
            NativeAuthMethod::ClaudeOfficial
        },
    }
}

fn logout(basis: &Basis, id: &str) -> NativeAuthLogout {
    NativeAuthLogout {
        request_id: request_id(id),
        scope: basis.scope.clone(),
    }
}

fn query(basis: &Basis, id: &str, operation: &str) -> NativeAuthQuery {
    NativeAuthQuery {
        request_id: request_id(id),
        scope: basis.scope.clone(),
        operation_id: request_id(operation),
    }
}

fn cancel(basis: &Basis, id: &str, operation: &str) -> NativeAuthCancel {
    NativeAuthCancel {
        request_id: request_id(id),
        scope: basis.scope.clone(),
        operation_id: request_id(operation),
    }
}

fn assert_error(reply: Result<NativeAuthReply, EngineError>, code: &str) {
    assert_eq!(reply.unwrap_err().0, code);
}

fn assert_unavailable(reply: &NativeAuthReply, basis: &Basis, id: &str) {
    assert_eq!(reply.request_id.as_str(), id);
    assert_eq!(
        reply.database_instance,
        basis.scope.expected_database_instance
    );
    assert_eq!(reply.engine_epoch, basis.epoch);
    assert_eq!(reply.fact.operation_id.as_str(), id);
    assert_eq!(reply.fact.binding.connection, basis.connection);
    assert_eq!(
        reply.fact.binding.database_instance,
        reply.database_instance
    );
    assert_eq!(reply.fact.binding.engine_epoch, basis.epoch);
    assert_eq!(reply.fact.binding.config_id, basis.scope.expected_config_id);
    assert_eq!(
        reply.fact.binding.source_hash,
        basis.scope.expected_source_hash
    );
    assert_eq!(reply.fact.binding.custody_id.len(), 64);
    assert!(reply.fact.generation > 0);
    assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
    assert_eq!(
        reply.fact.code.as_deref(),
        Some("native_auth_host_unavailable")
    );
    assert!(reply.challenge.is_none());
    assert!(reply.account.is_none());
}

async fn busy_case(case: Arc<ProfileCase>, connection: &str, kind: OwnerClientKind, out: bool) {
    let basis = case.basis(connection).await;
    case.prepare(&basis).await;
    let client = case.client(kind).await;
    let before = case.stored();
    let holder = LockProcess::spawn(&case, "hold", b'L');
    let reply = if out {
        client
            .logout_native_auth(logout(&basis, "busy-logout"))
            .await
    } else {
        client.start_native_auth(start(&basis, "busy-start")).await
    };
    holder.finish(true);
    assert_error(reply, "native_profile_in_use");
    assert_eq!(case.stored(), before);
    case.assert_gateway_unknown(connection).await;
}

#[tokio::test]
async fn owner_start_rejects_a_real_physical_profile_lock() {
    run_case(CATALOG, 20, |case| {
        busy_case(case, "connection-a", OwnerClientKind::Tui, false)
    })
    .await;
}

#[tokio::test]
async fn owner_logout_rejects_a_real_physical_profile_lock() {
    run_case(CATALOG, 20, |case| {
        busy_case(case, "connection-a", OwnerClientKind::Desktop, true)
    })
    .await;
}

#[tokio::test]
async fn compatible_connection_alias_start_shares_the_physical_lock() {
    run_case(CATALOG, 20, |case| async move {
        let original = case.prepare(&case.basis("connection-a").await).await;
        let alias = case.prepare(&case.basis("connection-alias").await).await;
        assert_eq!(original.custody_id, alias.custody_id);
        assert_ne!(original.connection.id, alias.connection.id);
        busy_case(case, "connection-alias", OwnerClientKind::Desktop, false).await;
    })
    .await;
}

#[tokio::test]
async fn compatible_connection_alias_logout_shares_the_physical_lock() {
    run_case(CATALOG, 20, |case| async move {
        case.prepare(&case.basis("connection-a").await).await;
        busy_case(case, "connection-alias", OwnerClientKind::Tui, true).await;
    })
    .await;
}

#[tokio::test]
async fn distinct_safe_profile_remains_available_while_another_is_locked() {
    run_case(CATALOG, 20, |case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let original = case.prepare(&a).await;
        let client = case.client(OwnerClientKind::Tui).await;
        let holder = LockProcess::spawn(&case, "hold", b'L');
        let started = client.start_native_auth(start(&z, "distinct-start")).await;
        let logged_out = client.logout_native_auth(logout(&z, "distinct-out")).await;
        LockProcess::spawn(&case, "probe", b'B').finish(false);
        holder.finish(true);
        assert_unavailable(&started.unwrap(), &z, "distinct-start");
        let logged_out = logged_out.unwrap();
        assert_unavailable(&logged_out, &z, "distinct-out");
        assert_ne!(original.custody_id, logged_out.fact.binding.custody_id);
        assert_eq!(case.stored().len(), 2);
        case.assert_gateway_unknown("connection-z").await;
    })
    .await;
}

#[tokio::test]
async fn normal_process_exit_releases_the_lock_for_start_and_logout() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let client = case.client(OwnerClientKind::Tui).await;
        let holder = LockProcess::spawn(&case, "hold", b'L');
        LockProcess::spawn(&case, "probe", b'B').finish(false);
        holder.finish(true);
        LockProcess::spawn(&case, "probe", b'L').finish(false);
        let first = client
            .start_native_auth(start(&basis, "after-exit-start"))
            .await
            .unwrap();
        let second = client
            .logout_native_auth(logout(&basis, "after-exit-out"))
            .await
            .unwrap();
        assert_unavailable(&first, &basis, "after-exit-start");
        assert_unavailable(&second, &basis, "after-exit-out");
        assert_eq!(second.fact.generation, first.fact.generation + 1);
        LockProcess::spawn(&case, "probe", b'L').finish(false);
    })
    .await;
}

#[tokio::test]
async fn killed_process_is_reaped_before_observing_actual_lock_release() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let holder = LockProcess::spawn(&case, "hold", b'L');
        LockProcess::spawn(&case, "probe", b'B').finish(false);
        holder.terminate();
        LockProcess::spawn(&case, "probe", b'L').finish(false);
        let client = case.client(OwnerClientKind::Desktop).await;
        let reply = client
            .start_native_auth(start(&basis, "after-kill"))
            .await
            .unwrap();
        assert_unavailable(&reply, &basis, "after-kill");
        assert_eq!(case.stored().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn duplicate_description_keeps_lock_until_final_close_while_child_lives() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let mut holder = LockProcess::spawn(&case, "hold", b'L');
        holder.command(b'D');
        assert_eq!(holder.event(), b'D');
        LockProcess::spawn(&case, "probe", b'B').finish(false);
        let client = case.client(OwnerClientKind::Tui).await;
        let blocked = client
            .start_native_auth(start(&basis, "duplicate-busy"))
            .await;
        holder.command(b'C');
        assert_eq!(holder.event(), b'C');
        assert!(holder.child.try_wait().unwrap().is_none());
        LockProcess::spawn(&case, "probe", b'L').finish(false);
        let released = client
            .logout_native_auth(logout(&basis, "closed-live-child"))
            .await;
        holder.finish(true);
        assert_error(blocked, "native_profile_in_use");
        assert_unavailable(&released.unwrap(), &basis, "closed-live-child");
        assert_eq!(case.stored().len(), 1);
    })
    .await;
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    accessed: (i64, i64),
}

fn stamp(path: &Path) -> Stamp {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    Stamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        accessed: (metadata.atime(), metadata.atime_nsec()),
    }
}

fn unread_sentinel(home: &Path) -> (PathBuf, Stamp) {
    let path = home.join("native-owned-unread");
    private_file(&path, b"synthetic native contents must remain unread\n");
    std::fs::set_permissions(&path, Permissions::from_mode(0o000)).unwrap();
    let snapshot = stamp(&path);
    assert_eq!(snapshot.mode & 0o7777, 0);
    assert!(snapshot.length > 0);
    (path, snapshot)
}

fn assert_layout(case: &ProfileCase) {
    let mut names = std::fs::read_dir(case.profile("profile-a"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            std::ffi::OsString::from("home"),
            std::ffi::OsString::from("identity.json")
        ]
    );
    assert_eq!(
        std::fs::metadata(case.manifest()).unwrap().mode() & 0o7777,
        0o600
    );
}

#[tokio::test]
async fn busy_and_unavailable_operations_preserve_populated_unread_home_and_layout() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        let prepared = case.prepare(&basis).await;
        let home = case.profile("profile-a").join("home");
        let (sentinel, before) = unread_sentinel(&home);
        let profile_bytes = std::fs::read(case.manifest()).unwrap();
        let root_manifest = case.state.join("native-profiles/identity.json");
        let root_bytes = std::fs::read(&root_manifest).unwrap();
        let client = case.client(OwnerClientKind::Tui).await;
        let holder = LockProcess::spawn(&case, "hold", b'L');
        let busy = client
            .start_native_auth(start(&basis, "populated-busy"))
            .await;
        holder.finish(true);
        let started = client
            .start_native_auth(start(&basis, "populated-start"))
            .await
            .unwrap();
        let out = client
            .logout_native_auth(logout(&basis, "populated-out"))
            .await
            .unwrap();
        assert_eq!(stamp(&sentinel), before);
        assert_eq!(std::fs::read(case.manifest()).unwrap(), profile_bytes);
        assert_eq!(std::fs::read(root_manifest).unwrap(), root_bytes);
        assert_eq!(started.fact.binding, prepared);
        assert_unavailable(&started, &basis, "populated-start");
        assert_unavailable(&out, &basis, "populated-out");
        assert_error(busy, "native_profile_in_use");
        assert_layout(&case);
        std::fs::set_permissions(&sentinel, Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            std::fs::read(sentinel).unwrap(),
            b"synthetic native contents must remain unread\n"
        );
    })
    .await;
}

async fn assert_tamper_rejected(case: &ProfileCase, basis: &Basis, code: &str) {
    let client = case.client(OwnerClientKind::Tui).await;
    let before = case.stored();
    assert_error(
        client
            .start_native_auth(start(basis, "tampered-start"))
            .await,
        code,
    );
    assert_error(
        client
            .logout_native_auth(logout(basis, "tampered-out"))
            .await,
        code,
    );
    assert_eq!(case.stored(), before);
    case.assert_gateway_unknown(basis.scope.connection.as_str())
        .await;
}

#[tokio::test]
async fn replaced_manifest_inode_is_rejected_while_old_inode_remains_locked() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let (sentinel, before) = unread_sentinel(&case.profile("profile-a").join("home"));
        let bytes = std::fs::read(case.manifest()).unwrap();
        let original = case.root.path().join("original-identity");
        let holder = LockProcess::spawn(&case, "hold", b'L');
        std::fs::rename(case.manifest(), &original).unwrap();
        private_file(&case.manifest(), &bytes);
        assert_ne!(stamp(&original).inode, stamp(&case.manifest()).inode);
        let client = case.client(OwnerClientKind::Tui).await;
        let started = client
            .start_native_auth(start(&basis, "replaced-identity"))
            .await;
        let out = client
            .logout_native_auth(logout(&basis, "replaced-identity-out"))
            .await;
        holder.finish(true);
        assert_error(started, "native_profile_binding_conflict");
        assert_error(out, "native_profile_binding_conflict");
        assert_eq!(stamp(&sentinel), before);
        assert!(case.stored().is_empty());
    })
    .await;
}

#[tokio::test]
async fn replaced_home_inode_is_rejected_without_reading_replacement_contents() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let home = case.profile("profile-a").join("home");
        let original = case.root.path().join("original-home");
        std::fs::rename(&home, &original).unwrap();
        private_directory(&home);
        let (sentinel, before) = unread_sentinel(&home);
        assert_ne!(stamp(&home).inode, stamp(&original).inode);
        assert_tamper_rejected(&case, &basis, "native_profile_binding_conflict").await;
        assert_eq!(stamp(&sentinel), before);
    })
    .await;
}

#[tokio::test]
async fn replaced_profile_directory_with_copied_manifest_is_rejected() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let bytes = std::fs::read(case.manifest()).unwrap();
        let original = case.root.path().join("original-profile");
        std::fs::rename(case.profile("profile-a"), &original).unwrap();
        private_directory(&case.profile("profile-a"));
        let home = case.profile("profile-a").join("home");
        private_directory(&home);
        private_file(&case.manifest(), &bytes);
        let (sentinel, before) = unread_sentinel(&home);
        assert_tamper_rejected(&case, &basis, "native_profile_binding_conflict").await;
        assert_eq!(stamp(&sentinel), before);
        assert_ne!(
            stamp(&original).inode,
            stamp(&case.profile("profile-a")).inode
        );
    })
    .await;
}

#[tokio::test]
async fn replaced_root_manifest_inode_is_rejected_without_adopting_it() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let root_manifest = case.state.join("native-profiles/identity.json");
        let bytes = std::fs::read(&root_manifest).unwrap();
        let original = case.root.path().join("original-root-identity");
        std::fs::rename(&root_manifest, &original).unwrap();
        private_file(&root_manifest, &bytes);
        let (sentinel, before) = unread_sentinel(&case.profile("profile-a").join("home"));
        assert_tamper_rejected(&case, &basis, "native_profile_binding_conflict").await;
        assert_eq!(stamp(&sentinel), before);
        assert_ne!(stamp(&root_manifest).inode, stamp(&original).inode);
    })
    .await;
}

#[tokio::test]
async fn symlinked_home_is_rejected_without_reading_its_target() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let home = case.profile("profile-a").join("home");
        std::fs::rename(&home, case.root.path().join("original-home")).unwrap();
        let outside = case.root.path().join("outside");
        private_directory(&outside);
        let (sentinel, before) = unread_sentinel(&outside);
        symlink(&outside, &home).unwrap();
        assert_tamper_rejected(&case, &basis, "native_profile_custody_unsafe").await;
        assert_eq!(stamp(&sentinel), before);
        assert!(
            std::fs::symlink_metadata(&home)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    })
    .await;
}

#[tokio::test]
async fn unsafe_profile_chain_modes_are_rejected_without_permission_repair() {
    for artifact in ["root", "profile", "home", "identity"] {
        run_case(CATALOG, 20, move |case| async move {
            let basis = case.basis("connection-a").await;
            case.prepare(&basis).await;
            let path = match artifact {
                "root" => case.state.join("native-profiles"),
                "profile" => case.profile("profile-a"),
                "home" => case.profile("profile-a").join("home"),
                "identity" => case.manifest(),
                _ => unreachable!(),
            };
            std::fs::set_permissions(&path, Permissions::from_mode(0o777)).unwrap();
            assert_tamper_rejected(&case, &basis, "native_profile_custody_unsafe").await;
            assert_eq!(stamp(&path).mode & 0o7777, 0o777);
            assert!(case.stored().is_empty());
        })
        .await;
    }
}

#[tokio::test]
async fn same_inode_manifest_content_change_is_rejected_without_home_reads() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let path = case.manifest();
        let inode = stamp(&path).inode;
        let bytes = std::fs::read(&path).unwrap();
        let changed = String::from_utf8(bytes)
            .unwrap()
            .replace("account-a", "account-x");
        std::fs::write(&path, changed.as_bytes()).unwrap();
        let (sentinel, before) = unread_sentinel(&case.profile("profile-a").join("home"));
        assert_eq!(stamp(&path).inode, inode);
        assert_tamper_rejected(&case, &basis, "native_profile_binding_conflict").await;
        assert_eq!(stamp(&sentinel), before);
        assert_eq!(std::fs::read(path).unwrap(), changed.as_bytes());
    })
    .await;
}

async fn assert_known_while_busy(
    case: &ProfileCase,
    client: &OwnerClient,
    basis: &Basis,
    started: &NativeAuthReply,
    logged_out: &NativeAuthReply,
) {
    let first = started.fact.operation_id.as_str();
    let second = logged_out.fact.operation_id.as_str();
    assert_eq!(
        client
            .start_native_auth(start(basis, first))
            .await
            .unwrap()
            .fact,
        started.fact
    );
    assert_eq!(
        client
            .logout_native_auth(logout(basis, second))
            .await
            .unwrap()
            .fact,
        logged_out.fact
    );
    let read = client
        .read_native_auth(query(basis, "busy-terminal-read", first))
        .await
        .unwrap();
    assert_eq!(read.request_id.as_str(), "busy-terminal-read");
    assert_eq!(read.fact, started.fact);
    assert!(read.challenge.is_none());
    assert!(read.account.is_none());
    assert_error(
        client
            .cancel_native_auth(cancel(basis, "busy-terminal-cancel", first))
            .await,
        "native_auth_not_active",
    );
    assert_error(
        client
            .read_native_auth(query(basis, "busy-missing-read", "missing"))
            .await,
        "native_auth_operation_missing",
    );
    assert_error(
        client
            .cancel_native_auth(cancel(basis, "busy-missing-cancel", "missing"))
            .await,
        "native_auth_operation_missing",
    );
    let mut changed = start(basis, first);
    changed.method = NativeAuthMethod::CodexDeviceCode;
    assert_error(
        client.start_native_auth(changed).await,
        "native_auth_operation_conflict",
    );
    assert_error(
        client.logout_native_auth(logout(basis, first)).await,
        "native_auth_operation_conflict",
    );
    case.assert_gateway_unknown(basis.scope.connection.as_str())
        .await;
}

#[tokio::test]
async fn known_receipts_terminal_reads_and_cancel_precede_busy_profile_effects() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let started = client
            .start_native_auth(start(&basis, "known-start"))
            .await
            .unwrap();
        let logged_out = client
            .logout_native_auth(logout(&basis, "known-out"))
            .await
            .unwrap();
        let before = case.stored();
        let holder = LockProcess::spawn(&case, "hold", b'L');
        let home = case.profile("profile-a").join("home");
        std::fs::set_permissions(&home, Permissions::from_mode(0o777)).unwrap();
        assert_known_while_busy(&case, &client, &basis, &started, &logged_out).await;
        holder.finish(true);
        assert_eq!(case.stored(), before);
        assert_eq!(stamp(&home).mode & 0o7777, 0o777);
        assert_eq!(started.fact.generation, 1);
        assert_eq!(logged_out.fact.generation, 2);
    })
    .await;
}

#[tokio::test]
async fn connection_and_action_conflicts_precede_the_shared_alias_lock() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        let alias = case.basis("connection-alias").await;
        let client = case.client(OwnerClientKind::Desktop).await;
        client
            .start_native_auth(start(&basis, "known-identity"))
            .await
            .unwrap();
        let before = case.stored();
        let holder = LockProcess::spawn(&case, "hold", b'L');
        let rebound = client
            .start_native_auth(start(&alias, "known-identity"))
            .await;
        let action = client
            .logout_native_auth(logout(&alias, "known-identity"))
            .await;
        let read = client
            .read_native_auth(query(&alias, "alias-read", "known-identity"))
            .await;
        holder.finish(true);
        assert_error(rebound, "native_auth_operation_conflict");
        assert_error(action, "native_auth_operation_conflict");
        assert_error(read, "native_auth_operation_scope_conflict");
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn full_capacity_keeps_known_ids_and_the_busy_lock_while_a_new_profile_prunes_history() {
    run_case(CATALOG, 90, |case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let first = client
            .start_native_auth(start(&a, "capacity-0"))
            .await
            .unwrap();
        let second = client
            .logout_native_auth(logout(&a, "capacity-1"))
            .await
            .unwrap();
        for index in 2..1024 {
            let id = format!("capacity-{index}");
            let reply = client.start_native_auth(start(&a, &id)).await.unwrap();
            assert_unavailable(&reply, &a, &id);
            assert_eq!(reply.fact.generation, index + 1);
        }
        let before = case.stored();
        assert_eq!(before.len(), 1024);
        assert!(!case.profile("profile-z").exists());
        let holder = LockProcess::spawn(&case, "hold", b'L');
        assert_known_while_busy(&case, &client, &a, &first, &second).await;
        let busy_start = client
            .start_native_auth(start(&a, "capacity-busy-start"))
            .await;
        let busy_out = client
            .logout_native_auth(logout(&a, "capacity-busy-out"))
            .await;
        let new_start = client
            .start_native_auth(start(&z, "capacity-new-start"))
            .await;
        let new_out = client
            .logout_native_auth(logout(&z, "capacity-new-out"))
            .await;
        LockProcess::spawn(&case, "probe", b'B').finish(false);
        holder.finish(true);
        assert_error(busy_start, "native_profile_in_use");
        assert_error(busy_out, "native_profile_in_use");
        assert_unavailable(&new_start.unwrap(), &z, "capacity-new-start");
        assert_unavailable(&new_out.unwrap(), &z, "capacity-new-out");
        let ids: Vec<String> = case.stored().into_iter().map(|row| row.0).collect();
        assert_eq!(ids.len(), 1024);
        assert!(
            !ids.iter()
                .any(|id| id == "capacity-0" || id == "capacity-1")
        );
        assert!(ids.iter().any(|id| id == "capacity-1023"));
    })
    .await;
}

#[tokio::test]
async fn incompatible_profile_alias_is_rejected_before_any_profile_or_receipt() {
    let catalog = CATALOG.replace("profile_ref = \"profile-z\"", "profile_ref = \"profile-a\"");
    run_case(&catalog, 20, |case| async move {
        let basis = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .start_native_auth(start(&basis, "incompatible-alias"))
                .await,
            "native_profile_binding_conflict",
        );
        assert_error(
            client
                .logout_native_auth(logout(&basis, "incompatible-alias-out"))
                .await,
            "native_profile_binding_conflict",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn terminal_unavailable_retains_no_live_lock_or_authentication_claim() {
    run_case(CATALOG, 20, |case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let started = client
            .start_native_auth(start(&basis, "unavailable-start"))
            .await
            .unwrap();
        LockProcess::spawn(&case, "probe", b'L').finish(false);
        let logged_out = client
            .logout_native_auth(logout(&basis, "unavailable-out"))
            .await
            .unwrap();
        LockProcess::spawn(&case, "probe", b'L').finish(false);
        assert_unavailable(&started, &basis, "unavailable-start");
        assert_unavailable(&logged_out, &basis, "unavailable-out");
        assert_eq!(
            started.fact.action,
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexBrowser
            }
        );
        assert_eq!(logged_out.fact.action, NativeAuthAction::Logout);
        assert_eq!(case.stored().len(), 2);
        assert_layout(&case);
        case.assert_gateway_unknown("connection-a").await;
    })
    .await;
}
