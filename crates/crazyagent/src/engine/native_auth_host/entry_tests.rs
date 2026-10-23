use super::entry::read_bootstrap;
use crate::engine::bootstrap::{EngineService, runner_binding};
use crate::engine::owner::transport::native_auth_channel::NativeAuthCustodyChannel;
use crate::engine::owner::{
    ModelAccessCollection, NativeProfileBinding, OwnerClient, OwnerClientKind, OwnerData,
    OwnerOperation, OwnerRead, OwnerRequest, OwnerResponse,
};
use crate::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, RequestId};
use rusqlite::{Connection, OpenFlags};
use rustix::fs::OFlags;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOKEN: [u8; 32] = [
    0, 255, 10, 13, 32, 9, 128, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    20, 21, 22, 23, 24, 25,
];
const SHORT_BOUND: Duration = Duration::from_millis(100);
const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.pools]]
id = "pool-a"
members = ["connection-a"]
projects = ["project-a"]
roles = ["leader"]
fallback_allowed = false
"#;

struct BootstrapPipe {
    reader: OwnedFd,
    writer: Option<File>,
}

impl BootstrapPipe {
    fn new() -> Self {
        let (reader, writer) = std::io::pipe().unwrap();
        let reader = OwnedFd::from(reader);
        let writer = OwnedFd::from(writer);
        for fd in [&reader, &writer] {
            let flags = rustix::fs::fcntl_getfl(fd).unwrap();
            rustix::fs::fcntl_setfl(fd, flags | OFlags::NONBLOCK).unwrap();
            rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC).unwrap();
        }
        Self {
            reader,
            writer: Some(File::from(writer)),
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        assert!(bytes.len() <= 33);
        self.writer.as_mut().unwrap().write_all(bytes).unwrap();
    }

    fn eof(&mut self) {
        assert!(self.writer.take().is_some());
    }
}

#[tokio::test]
async fn native_auth_custodian_entry_exact_raw_bytes_and_eof() {
    let mut pipe = BootstrapPipe::new();
    pipe.write(&TOKEN);
    pipe.eof();
    let decoded = read_bootstrap(&pipe.reader, Instant::now() + SHORT_BOUND)
        .await
        .unwrap();
    assert_eq!(decoded, TOKEN);
    let mut trailing = [0; 1];
    assert_eq!(rustix::io::read(&pipe.reader, &mut trailing).unwrap(), 0);
}

#[tokio::test]
async fn native_auth_custodian_entry_fragmented_input_waits_for_eof() {
    let mut pipe = BootstrapPipe::new();
    pipe.write(&TOKEN[..7]);
    let reader = &pipe.reader;
    let mut writer = pipe.writer.take().unwrap();
    let started = Instant::now();
    let ((decoded, returned), closed) = tokio::join!(
        async {
            let decoded = read_bootstrap(reader, started + Duration::from_secs(1)).await;
            (decoded, Instant::now())
        },
        async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            writer.write_all(&TOKEN[7..]).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let closed = Instant::now();
            drop(writer);
            closed
        }
    );
    assert_eq!(decoded.unwrap(), TOKEN);
    assert!(returned >= closed);
    assert!(started.elapsed() >= Duration::from_millis(40));
}

#[tokio::test]
async fn native_auth_custodian_entry_empty_eof_is_truncated() {
    let mut pipe = BootstrapPipe::new();
    pipe.eof();
    let error = read_bootstrap(&pipe.reader, Instant::now() + SHORT_BOUND)
        .await
        .unwrap_err();
    assert_eq!(error.0, "native_auth_bootstrap_truncated");
}

#[tokio::test]
async fn native_auth_custodian_entry_every_partial_length_is_truncated() {
    for length in 1..32 {
        let mut pipe = BootstrapPipe::new();
        pipe.write(&TOKEN[..length]);
        pipe.eof();
        let error = read_bootstrap(&pipe.reader, Instant::now() + SHORT_BOUND)
            .await
            .unwrap_err();
        assert_eq!(
            error.0, "native_auth_bootstrap_truncated",
            "length={length}"
        );
    }
}

#[tokio::test]
async fn native_auth_custodian_entry_byte_33_with_eof_is_excess() {
    let mut pipe = BootstrapPipe::new();
    let mut bytes = TOKEN.to_vec();
    bytes.push(0);
    pipe.write(&bytes);
    pipe.eof();
    let error = read_bootstrap(&pipe.reader, Instant::now() + SHORT_BOUND)
        .await
        .unwrap_err();
    assert_eq!(error.0, "native_auth_bootstrap_excess");
}

#[tokio::test]
async fn native_auth_custodian_entry_byte_33_rejects_without_eof() {
    let mut pipe = BootstrapPipe::new();
    let mut bytes = TOKEN.to_vec();
    bytes.push(b'\n');
    pipe.write(&bytes);
    let error = read_bootstrap(&pipe.reader, Instant::now() + SHORT_BOUND)
        .await
        .unwrap_err();
    assert!(pipe.writer.is_some());
    assert_eq!(error.0, "native_auth_bootstrap_excess");
}

async fn assert_missing_eof(bytes: &[u8]) {
    let mut pipe = BootstrapPipe::new();
    pipe.write(bytes);
    let started = Instant::now();
    let (result, heartbeat) =
        tokio::join!(read_bootstrap(&pipe.reader, started + SHORT_BOUND), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Instant::now()
        });
    assert_eq!(result.unwrap_err().0, "native_auth_bootstrap_timeout");
    assert!(pipe.writer.is_some());
    assert!(heartbeat.duration_since(started) < SHORT_BOUND);
    assert!(started.elapsed() >= SHORT_BOUND);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn native_auth_custodian_entry_exact_32_without_eof_times_out() {
    assert_missing_eof(&TOKEN).await;
}

#[tokio::test]
async fn native_auth_custodian_entry_empty_live_pipe_times_out() {
    assert_missing_eof(&[]).await;
}

#[tokio::test]
async fn native_auth_custodian_entry_partial_live_pipe_times_out() {
    assert_missing_eof(&TOKEN[..31]).await;
}

#[tokio::test]
async fn native_auth_custodian_entry_expired_deadline_preserves_unread_bytes() {
    let mut pipe = BootstrapPipe::new();
    pipe.write(&TOKEN);
    pipe.eof();
    let error = read_bootstrap(&pipe.reader, Instant::now())
        .await
        .unwrap_err();
    assert_eq!(error.0, "native_auth_bootstrap_timeout");
    let mut unread = [0; 32];
    assert_eq!(rustix::io::read(&pipe.reader, &mut unread).unwrap(), 32);
    assert_eq!(unread, TOKEN);
}

async fn start_service(root: &Path) -> EngineService {
    let global = root.join("g.toml");
    std::fs::write(&global, CATALOG).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    EngineService::start_with_settings_sources(
        root,
        "e.sock",
        &global,
        &root.join("p.toml"),
        &stopped,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn native_auth_engine_service_registers_distinct_codex_before_ready() {
    let root = tempfile::Builder::new()
        .prefix("ca-ep-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut service = start_service(root.path()).await;
    let executable = root.path().join("crazyagent-pin-only");
    let codex = root.path().join("codex-pin-only");
    for path in [&executable, &codex] {
        std::fs::write(path, path.file_name().unwrap().as_encoded_bytes()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    service.register_native_auth_host(&executable).unwrap();
    service.register_codex_executable(&codex).unwrap();
    let registered = service
        .owner_runtime()
        .unwrap()
        .registered_codex_executable()
        .unwrap();
    assert_eq!(registered.verified_path().unwrap(), codex);
    assert_eq!(
        registered.verified_sha256().unwrap(),
        format!("{:x}", Sha256::digest(std::fs::read(&codex).unwrap()))
    );
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    service.ensure_ready(&stopped).unwrap();
    service.shutdown().await.unwrap();
}

async fn prepare_profile(root: &Path, service: &EngineService) -> NativeProfileBinding {
    let config = crate::config::resolve_config(&root.join("g.toml"), &root.join("p.toml")).unwrap();
    let (client, identity) =
        OwnerClient::discover(root, OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
    let reply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("entry-source").unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessRecord {
                    collection: ModelAccessCollection::Connections,
                    id: CredentialRef::new("connection-a").unwrap(),
                },
            },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("actual owner did not return source record")
    };
    let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else {
        panic!("actual owner did not return source hash")
    };
    let prepared = service
        .owner_runtime()
        .unwrap()
        .prepare_native_profile(
            &CredentialRef::new("connection-a").unwrap(),
            &config.id,
            &source_hash,
        )
        .await
        .unwrap();
    assert_eq!(
        prepared.binding().database_instance,
        identity.database_instance()
    );
    assert_eq!(prepared.binding().engine_epoch, identity.engine_epoch());
    assert_eq!(prepared.binding().source_hash, source_hash);
    assert_eq!(prepared.binding().config_id, config.id);
    prepared.binding().clone()
}

async fn engine_case<F, T>(case: F)
where
    F: FnOnce(PathBuf, NativeProfileBinding) -> T + Send + 'static,
    T: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::Builder::new()
        .prefix("ca-ee-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let service = Arc::new(tokio::sync::Mutex::new(start_service(root.path()).await));
    let task_service = service.clone();
    let path = root.path().to_path_buf();
    let mut task = tokio::spawn(async move {
        let prepared = {
            let service = task_service.lock().await;
            prepare_profile(&path, &service).await
        };
        case(path, prepared).await;
    });
    let outcome = match tokio::time::timeout(Duration::from_secs(20), &mut task).await {
        Ok(result) => result,
        Err(_) => {
            task.abort();
            let _ = task.await;
            let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
                service.lock().await.shutdown().await
            })
            .await;
            eprintln!(
                "entry timeout evidence retained at {}",
                root.keep().display()
            );
            assert!(
                matches!(cleanup, Ok(Ok(()))),
                "entry timeout cleanup: {cleanup:?}"
            );
            panic!("entry engine case exceeded 20 seconds");
        }
    };
    let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        service.lock().await.shutdown().await
    })
    .await;
    if outcome.is_err() || !matches!(&cleanup, Ok(Ok(()))) {
        eprintln!("entry evidence retained at {}", root.keep().display());
    }
    assert!(matches!(cleanup, Ok(Ok(()))), "entry cleanup: {cleanup:?}");
    if let Err(error) = outcome {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("entry task: {error}");
    }
}

fn auth_counts(root: &Path) -> (i64, i64) {
    let db = Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db.query_row(
        "SELECT (SELECT count(*) FROM native_auth_operations),
         (SELECT count(*) FROM native_auth_fences)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[tokio::test]
async fn native_auth_custodian_entry_decoded_bytes_do_not_grant_profile_authority() {
    engine_case(|root, prepared| async move {
        let before = auth_counts(&root);
        assert_eq!(before, (0, 0));
        let manifest = root
            .join("native-profiles")
            .join(format!("{:x}", Sha256::digest(b"profile-a")))
            .join("identity.json");
        let original = std::fs::read(&manifest).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&original)),
            prepared.custody_id
        );
        let mut pipe = BootstrapPipe::new();
        pipe.write(&TOKEN);
        pipe.eof();
        let token = read_bootstrap(&pipe.reader, Instant::now() + SHORT_BOUND)
            .await
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            NativeAuthCustodyChannel::connect(&root, token),
        )
        .await
        .unwrap();
        assert!(
            result.is_err(),
            "unreserved bytes authenticated a custodian"
        );
        assert_eq!(auth_counts(&root), before);
        assert_eq!(std::fs::read(&manifest).unwrap(), original);
        let (client, _) =
            OwnerClient::discover(&root, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        drop(client);
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_retained_profile_capture_detects_manifest_replacement() {
    engine_case(|root, prepared| async move {
        let (_, identity, binding) = runner_binding(&root).unwrap();
        let mut custody = binding.capture_native_profile_custody().unwrap();
        let host = crate::engine::owner::HostStamp {
            db_instance: identity.database_instance().to_owned(),
            engine_epoch: identity.engine_epoch().to_owned(),
        };
        let (custody_id, _) = custody.prepare(&host, &prepared.connection).unwrap();
        assert_eq!(custody_id, prepared.custody_id);
        let before = auth_counts(&root);
        assert_eq!(before, (0, 0));
        let manifest = root
            .join("native-profiles")
            .join(format!("{:x}", Sha256::digest(b"profile-a")))
            .join("identity.json");
        let bytes = std::fs::read(&manifest).unwrap();
        let replacement = manifest.with_file_name("replacement");
        std::fs::write(&replacement, &bytes).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&replacement, &manifest).unwrap();
        assert!(custody.prepare(&host, &prepared.connection).is_err());
        assert_eq!(auth_counts(&root), before);
        assert_eq!(std::fs::read(&manifest).unwrap(), bytes);
    })
    .await;
}
