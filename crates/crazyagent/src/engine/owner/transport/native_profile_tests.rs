use super::*;
use crate::config::{
    EffectiveConfig, ModelAccessEdit, ModelAuthentication, ModelConnection, resolve_config,
};
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::PreparedNativeProfile;
use crate::model_gateway::{Health, Quota};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use tokio::sync::Mutex as AsyncMutex;

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
members = ["connection-a", "connection-z"]
projects = ["project-fixture"]
roles = ["leader"]
fallback_allowed = false
"#;

const PROFILE_Z: &str = "e49579422bdba0c5b6ac3ed38b6f46b8d1289369e224a31071a2008e84dd4330";
const PROFILE_A: &str = "104c4491498717b382d124bbe81ae4e87648fbab08ecba7e24135ead5fc97554";

struct Basis {
    config_id: ConfigId,
    source_hash: String,
    revision: Cursor,
    connection: ModelConnection,
    database_instance: String,
    engine_epoch: String,
    writable: bool,
}

struct ProfileCase {
    state: PathBuf,
    global: PathBuf,
    project: PathBuf,
    explicit_sources: bool,
    services: AsyncMutex<Vec<Option<EngineService>>>,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

impl ProfileCase {
    async fn start(root: &Path, explicit_sources: bool) -> Self {
        let state = root.join("s");
        let settings = root.join("settings");
        private_directory(&state);
        private_directory(&settings);
        let global = settings.join("global.toml");
        private_file(&global, CATALOG.as_bytes());
        let context = Self {
            state,
            global,
            project: settings.join("project.toml"),
            explicit_sources,
            services: AsyncMutex::new(Vec::new()),
        };
        let service = context.start_service().await;
        context.services.lock().await.push(Some(service));
        context
    }

    fn config(&self) -> EffectiveConfig {
        resolve_config(&self.global, &self.project).unwrap()
    }

    async fn start_service(&self) -> EngineService {
        let (_stop, shutdown) = tokio::sync::watch::channel(false);
        if self.explicit_sources {
            EngineService::start_with_settings_sources(
                &self.state,
                "e.sock",
                &self.global,
                &self.project,
                &shutdown,
            )
            .await
            .unwrap()
        } else {
            EngineService::start(&self.state, "e.sock", &self.config(), &shutdown)
                .await
                .unwrap()
        }
    }

    async fn host(&self) -> OwnerRuntimeHandle {
        let services = self.services.lock().await;
        let host = services[0].as_ref().unwrap().owner_runtime().unwrap();
        OwnerRuntimeHandle {
            engine: host.engine.clone(),
            authority: host.authority.clone(),
        }
    }

    async fn other_host(&self) -> OwnerRuntimeHandle {
        let state = self.state.parent().unwrap().join("second");
        private_directory(&state);
        let (_stop, shutdown) = tokio::sync::watch::channel(false);
        let service = EngineService::start(&state, "e.sock", &self.config(), &shutdown)
            .await
            .unwrap();
        let host = service.owner_runtime().unwrap();
        let retained = OwnerRuntimeHandle {
            engine: host.engine.clone(),
            authority: host.authority.clone(),
        };
        let mut services = self.services.lock().await;
        assert_eq!(services.len(), 1);
        services.push(Some(service));
        retained
    }

    async fn stop_main(&self) {
        let mut service = self.services.lock().await[0].take().unwrap();
        service.shutdown().await.unwrap();
    }

    async fn reopen_main(&self) {
        let service = self.start_service().await;
        let previous = self.services.lock().await[0].replace(service);
        assert!(previous.is_none());
    }

    async fn restart(&self) {
        self.stop_main().await;
        self.reopen_main().await;
    }

    async fn close(&self) -> Result<(), EngineError> {
        let mut services = self.services.lock().await;
        let mut errors = Vec::new();
        for mut service in services.iter_mut().filter_map(Option::take) {
            if let Err(error) = service.shutdown().await {
                errors.push(error.0);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(EngineError(errors.join("; ")))
        }
    }

    async fn read(&self, query: OwnerRead) -> (OwnerReadReply, String, String) {
        let (client, identity) =
            OwnerClient::discover(&self.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let reply = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("native-profile-read").unwrap(),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read { query },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual authenticated owner read required");
        };
        (
            state,
            identity.database_instance().to_owned(),
            identity.engine_epoch().to_owned(),
        )
    }

    async fn basis(&self, id: &str) -> Basis {
        let (state, database_instance, engine_epoch) = self
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: reference(id),
            })
            .await;
        let OwnerData::ModelAccessRecord {
            source_hash,
            writable,
            record: ModelAccessRecord::Connection { connection },
        } = state.data
        else {
            panic!("actual configured connection required");
        };
        assert_eq!(connection.id.as_str(), id);
        Basis {
            config_id: state.config_id,
            source_hash,
            revision: state.revision,
            connection,
            database_instance,
            engine_epoch,
            writable,
        }
    }

    async fn prepare_at(&self, basis: &Basis) -> Result<PreparedNativeProfile, EngineError> {
        self.host()
            .await
            .prepare_native_profile(&basis.connection.id, &basis.config_id, &basis.source_hash)
            .await
    }

    async fn prepare(&self, id: &str) -> PreparedNativeProfile {
        let basis = self.basis(id).await;
        self.prepare_at(&basis).await.unwrap()
    }

    async fn replace(&self, connection: ModelConnection) {
        let basis = self.basis(connection.id.as_str()).await;
        let (client, _) =
            OwnerClient::discover(&self.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let reply = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(format!("replace-{}", basis.revision.value())).unwrap(),
                expected_revision: basis.revision.clone(),
                operation: OwnerOperation::Apply {
                    mutation: OwnerMutation::ModelAccess {
                        expected_config_id: basis.config_id,
                        expected_source_hash: basis.source_hash,
                        edit: ModelAccessEdit::ReplaceConnection { connection },
                    },
                },
            })
            .await
            .unwrap();
        let OwnerResponse::Applied { operation } = reply.response else {
            panic!("actual owner metadata edit required");
        };
        assert_eq!(operation.result, OwnerResult::Applied);
        assert_eq!(
            operation.current_revision.value(),
            basis.revision.value() + 1
        );
    }

    fn custody(&self) -> PathBuf {
        self.state.join("native-profiles")
    }

    fn profile_z(&self) -> PathBuf {
        self.custody().join(PROFILE_Z)
    }

    fn home_z(&self) -> PathBuf {
        self.profile_z().join("home")
    }

    fn manifest_z(&self) -> PathBuf {
        self.profile_z().join("identity.json")
    }
}

async fn run_case<F, T>(explicit_sources: bool, body: F)
where
    F: FnOnce(Arc<ProfileCase>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let root = tempfile::Builder::new()
        .prefix("ca-np-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let context = Arc::new(ProfileCase::start(root.path(), explicit_sources).await);
    let mut task = tokio::spawn(body(context.clone()));
    let (outcome, timed_out) = match tokio::time::timeout(Duration::from_secs(45), &mut task).await
    {
        Ok(outcome) => (outcome, false),
        Err(_) => {
            task.abort();
            (task.await, true)
        }
    };
    let cleanup = tokio::time::timeout(Duration::from_secs(5), context.close()).await;
    let closed = matches!(&cleanup, Ok(Ok(())));
    if timed_out || outcome.is_err() || !closed {
        eprintln!(
            "native profile fixture retained at {}",
            root.keep().display()
        );
    }
    assert!(closed, "actual EngineService cleanup failed: {cleanup:?}");
    assert!(!timed_out, "native profile case exceeded 45 seconds");
    if let Err(error) = outcome {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        panic!("native profile case cancelled: {error}");
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FsStamp {
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn stamp(path: &Path) -> FsStamp {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    FsStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    }
}

fn assert_private_directory(path: &Path) {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.uid(), rustix::process::getuid().as_raw());
    assert_eq!(metadata.mode() & 0o7777, 0o700);
    assert_ne!(metadata.ino(), 0);
}

fn assert_private_identity(path: &Path) {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.uid(), rustix::process::getuid().as_raw());
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert!((1..=4096).contains(&metadata.len()));
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(value.is_object());
}

fn assert_receipt(prepared: &PreparedNativeProfile, basis: &Basis) {
    let binding = prepared.binding();
    assert_eq!(binding.connection, basis.connection);
    assert_eq!(binding.config_id, basis.config_id);
    assert_eq!(binding.source_hash, basis.source_hash);
    assert_eq!(binding.at_revision, basis.revision);
    assert_eq!(binding.database_instance, basis.database_instance);
    assert_eq!(binding.engine_epoch, basis.engine_epoch);
    assert_eq!(binding.custody_id.len(), 64);
    assert!(
        binding
            .custody_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

fn assert_rejected(result: Result<PreparedNativeProfile, EngineError>, state: &Path) {
    let Err(error) = result else {
        panic!("unsafe or unbound profile preparation must fail");
    };
    assert!(!error.0.is_empty());
    assert!(!error.0.contains(state.to_string_lossy().as_ref()));
    assert!(!error.0.contains("native-owned-unread"));
}

fn slot_count(context: &ProfileCase) -> usize {
    std::fs::read_dir(context.custody())
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_name().as_os_str() != std::ffi::OsStr::new("identity.json"))
        .take(66)
        .count()
}

fn durable_snapshot(path: &Path) -> BTreeMap<String, Vec<Vec<String>>> {
    let mut database = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    database.busy_timeout(Duration::from_millis(250)).unwrap();
    let transaction = database.transaction().unwrap();
    let names = transaction
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(names.len() <= 128);
    let mut snapshot = BTreeMap::new();
    for name in names {
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        );
        let mut statement = transaction
            .prepare(&format!("SELECT * FROM \"{name}\""))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            assert!(values.len() < 4096);
            let cells = (0..columns)
                .map(|column| format!("{:?}", row.get_ref(column).unwrap()))
                .collect::<Vec<_>>();
            values.push(cells);
        }
        values.sort();
        snapshot.insert(name, values);
    }
    transaction.commit().unwrap();
    snapshot
}

#[tokio::test]
async fn native_profile_preparation_creates_exact_private_empty_custody() {
    run_case(true, |context| async move {
        let basis = context.basis("connection-z").await;
        let prepared = context.prepare_at(&basis).await.unwrap();
        assert_receipt(&prepared, &basis);
        assert_eq!(prepared.binding().connection.account_ref.as_str(), "account-z");
        assert_eq!(prepared.binding().connection.provider.as_str(), "provider-z");
        assert_eq!(prepared.binding().connection.harness.as_str(), "claude");
        assert_eq!(prepared.binding().connection.protocol.as_str(), "native");
        assert!(matches!(
            &prepared.binding().connection.auth,
            ModelAuthentication::NativeManaged { profile_ref } if profile_ref.as_str() == "profile-z"
        ));
        assert_private_directory(&context.custody());
        assert_private_directory(&context.profile_z());
        assert_private_directory(&context.home_z());
        assert_private_identity(&context.custody().join("identity.json"));
        assert_private_identity(&context.manifest_z());
        assert!(std::fs::read_dir(context.home_z()).unwrap().next().is_none());
        assert_eq!(slot_count(&context), 1);
        assert!(!context.custody().join(PROFILE_A).exists());
        assert!(!context.global.parent().unwrap().join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn repeated_preparation_preserves_physical_custody_and_manifests() {
    run_case(true, |context| async move {
        let first = context.prepare("connection-z").await;
        let home = stamp(&context.home_z());
        let profile = stamp(&context.profile_z());
        let identity = stamp(&context.manifest_z());
        let bytes = std::fs::read(context.manifest_z()).unwrap();
        let root_bytes = std::fs::read(context.custody().join("identity.json")).unwrap();
        let repeated = context.prepare("connection-z").await;
        assert_eq!(first.binding().custody_id, repeated.binding().custody_id);
        assert_eq!(stamp(&context.home_z()), home);
        assert_eq!(stamp(&context.profile_z()), profile);
        assert_eq!(stamp(&context.manifest_z()), identity);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), bytes);
        assert_eq!(
            std::fs::read(context.custody().join("identity.json")).unwrap(),
            root_bytes
        );
        assert_eq!(slot_count(&context), 1);
    })
    .await;
}

#[tokio::test]
async fn populated_engine_home_reuses_unread_native_state_after_repeat_and_restart() {
    run_case(true, |context| async move {
        let first = context.prepare("connection-z").await;
        let sentinel = context.home_z().join("native-owned-unread");
        private_file(
            &sentinel,
            b"synthetic native state that preparation must never read\n",
        );
        std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unread = stamp(&sentinel);
        let home = stamp(&context.home_z());
        let profile_identity = std::fs::read(context.manifest_z()).unwrap();
        let root_identity = std::fs::read(context.custody().join("identity.json")).unwrap();
        let accessed = std::fs::symlink_metadata(&sentinel).unwrap();
        let accessed = (accessed.atime(), accessed.atime_nsec());
        let repeated = context.prepare("connection-z").await;
        assert_eq!(repeated.binding().custody_id, first.binding().custody_id);
        assert_eq!(stamp(&sentinel), unread);
        assert_eq!(stamp(&context.home_z()), home);
        context.restart().await;
        let basis = context.basis("connection-z").await;
        let reopened = context.prepare_at(&basis).await.unwrap();
        assert_receipt(&reopened, &basis);
        assert_eq!(reopened.binding().custody_id, first.binding().custody_id);
        assert_eq!(
            reopened.binding().database_instance,
            first.binding().database_instance
        );
        assert_ne!(
            reopened.binding().engine_epoch,
            first.binding().engine_epoch
        );
        assert_eq!(stamp(&sentinel), unread);
        assert_eq!(stamp(&context.home_z()), home);
        let after = std::fs::symlink_metadata(&sentinel).unwrap();
        assert_eq!((after.atime(), after.atime_nsec()), accessed);
        assert_eq!(
            std::fs::read(context.manifest_z()).unwrap(),
            profile_identity
        );
        assert_eq!(
            std::fs::read(context.custody().join("identity.json")).unwrap(),
            root_identity
        );
        assert_eq!(slot_count(&context), 1);
    })
    .await;
}

#[tokio::test]
async fn compatible_model_connections_share_the_exact_managed_profile() {
    run_case(true, |context| async move {
        let first = context.prepare("connection-z").await;
        let mut connection = context.basis("connection-z").await.connection;
        connection.id = reference("connection-a");
        connection.model.id = "another-model".into();
        context.replace(connection.clone()).await;
        let shared = context.prepare("connection-a").await;
        assert_eq!(shared.binding().connection, connection);
        assert_eq!(shared.binding().custody_id, first.binding().custody_id);
        assert_eq!(slot_count(&context), 1);
    })
    .await;
}

#[tokio::test]
async fn distinct_accounts_keep_distinct_physical_profiles() {
    run_case(true, |context| async move {
        let a = context.prepare("connection-a").await;
        let z = context.prepare("connection-z").await;
        assert_ne!(a.binding().custody_id, z.binding().custody_id);
        assert_eq!(a.binding().connection.account_ref.as_str(), "account-a");
        assert_eq!(z.binding().connection.account_ref.as_str(), "account-z");
        let a_home = context.custody().join(PROFILE_A).join("home");
        assert_ne!(stamp(&a_home).inode, stamp(&context.home_z()).inode);
        assert_eq!(slot_count(&context), 2);
    })
    .await;
}

#[tokio::test]
async fn no_settings_write_grant_is_needed_for_real_profile_preparation() {
    run_case(false, |context| async move {
        let basis = context.basis("connection-z").await;
        assert!(!basis.writable);
        assert_eq!(basis.source_hash, basis.config_id.as_str());
        let prepared = context.prepare_at(&basis).await.unwrap();
        assert_receipt(&prepared, &basis);
        assert_private_directory(&context.home_z());
    })
    .await;
}

#[tokio::test]
async fn unavailable_custody_does_not_broaden_ordinary_engine_startup_rejection() {
    let root = tempfile::Builder::new()
        .prefix("ca-np-open-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = root.path().join("s");
    let owners = root.path().join("o");
    private_directory(&state);
    private_directory(&owners);
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o1700)).unwrap();
    let global = root.path().join("global.toml");
    private_file(&global, CATALOG.as_bytes());
    let config = resolve_config(&global, &root.path().join("project.toml")).unwrap();
    let engine = Arc::new(
        Engine::open(&state.join("engine.db"), config.clone())
            .await
            .unwrap(),
    );
    let (server, host) = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.run(stopped));
    let mut prepare = tokio::spawn(async move {
        host.prepare_native_profile(&reference("connection-z"), &config.id, config.id.as_str())
            .await
    });
    let (result, timed_out) =
        match tokio::time::timeout(Duration::from_secs(45), &mut prepare).await {
            Ok(result) => (result, false),
            Err(_) => {
                prepare.abort();
                (prepare.await, true)
            }
        };
    let _ = stop.send(());
    let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        let served = serving.await;
        let closed = engine.close().await;
        assert!(matches!(served, Ok(Ok(()))));
        closed
    })
    .await;
    let closed = matches!(&cleanup, Ok(Ok(())));
    if timed_out || result.is_err() || !closed {
        eprintln!(
            "ordinary engine profile fixture retained at {}",
            root.keep().display()
        );
    }
    assert!(closed, "ordinary Engine cleanup failed: {cleanup:?}");
    assert!(
        !timed_out,
        "ordinary Engine preparation exceeded 45 seconds"
    );
    let result = match result {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("ordinary Engine preparation cancelled: {error}"),
    };
    assert_rejected(result, &state);
    assert!(!state.join("native-profiles").exists());
    assert_eq!(stamp(&state).mode & 0o7777, 0o1700);
}

#[tokio::test]
async fn wrong_config_creates_no_custody() {
    run_case(true, |context| async move {
        let basis = context.basis("connection-z").await;
        let wrong = ConfigId::new("0".repeat(64)).unwrap();
        assert_ne!(wrong, basis.config_id);
        let result = context
            .host()
            .await
            .prepare_native_profile(&basis.connection.id, &wrong, &basis.source_hash)
            .await;
        assert_rejected(result, &context.state);
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn wrong_source_creates_no_custody() {
    run_case(true, |context| async move {
        let basis = context.basis("connection-z").await;
        let wrong = "0".repeat(64);
        assert_ne!(wrong, basis.source_hash);
        let result = context
            .host()
            .await
            .prepare_native_profile(&basis.connection.id, &basis.config_id, &wrong)
            .await;
        assert_rejected(result, &context.state);
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn missing_connection_creates_no_custody() {
    run_case(true, |context| async move {
        let basis = context.basis("connection-z").await;
        let result = context
            .host()
            .await
            .prepare_native_profile(&reference("missing"), &basis.config_id, &basis.source_hash)
            .await;
        assert_rejected(result, &context.state);
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn a_non_managed_connection_creates_no_custody() {
    run_case(true, |context| async move {
        let mut connection = context.basis("connection-z").await.connection;
        connection.auth = ModelAuthentication::None {};
        context.replace(connection).await;
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn an_http_connection_cannot_gain_a_native_profile() {
    run_case(true, |context| async move {
        let mut connection = context.basis("connection-z").await.connection;
        connection.protocol = reference("http-api");
        connection.endpoint = crate::config::ModelEndpoint::Http {
            url: "http://127.0.0.1:1".into(),
        };
        connection.auth = ModelAuthentication::None {};
        context.replace(connection).await;
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn source_inode_replacement_invalidates_old_basis_without_changing_config() {
    run_case(true, |context| async move {
        let old = context.basis("connection-z").await;
        let bytes = std::fs::read(&context.global).unwrap();
        std::fs::rename(&context.global, context.global.with_extension("old")).unwrap();
        private_file(&context.global, &bytes);
        let fresh = context.basis("connection-z").await;
        assert_eq!(old.config_id, fresh.config_id);
        assert_ne!(old.source_hash, fresh.source_hash);
        assert_rejected(context.prepare_at(&old).await, &context.state);
        assert!(!context.custody().exists());
        assert_receipt(&context.prepare_at(&fresh).await.unwrap(), &fresh);
    })
    .await;
}

#[tokio::test]
async fn source_change_after_real_preparation_retains_owned_artifacts_without_rebinding() {
    run_case(true, |context| async move {
        let old = context.basis("connection-z").await;
        let prepared = context.prepare_at(&old).await.unwrap();
        let identity = std::fs::read(context.manifest_z()).unwrap();
        let home = stamp(&context.home_z());
        let bytes = std::fs::read(&context.global).unwrap();
        std::fs::rename(&context.global, context.global.with_extension("old")).unwrap();
        private_file(&context.global, &bytes);
        assert_rejected(context.prepare_at(&old).await, &context.state);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), identity);
        assert_eq!(stamp(&context.home_z()), home);
        assert_eq!(slot_count(&context), 1);
        let fresh = context.basis("connection-z").await;
        let reused = context.prepare_at(&fresh).await.unwrap();
        assert_receipt(&reused, &fresh);
        assert_eq!(reused.binding().custody_id, prepared.binding().custody_id);
    })
    .await;
}

async fn reject_alias(field: &str) {
    run_case(true, {
        let field = field.to_owned();
        move |context| async move {
            let mut alias = context.basis("connection-z").await.connection;
            alias.id = reference("connection-a");
            match field.as_str() {
                "account" => alias.account_ref = reference("account-a"),
                "provider" => alias.provider = reference("different-provider"),
                "harness" => alias.harness = reference("different-harness"),
                _ => panic!("unknown test alias field"),
            }
            context.replace(alias).await;
            let basis = context.basis("connection-z").await;
            assert_rejected(context.prepare_at(&basis).await, &context.state);
            assert!(!context.custody().exists());
        }
    })
    .await;
}

#[tokio::test]
async fn conflicting_account_alias_cannot_create_shared_custody() {
    reject_alias("account").await;
}

#[tokio::test]
async fn conflicting_provider_alias_cannot_create_shared_custody() {
    reject_alias("provider").await;
}

#[tokio::test]
async fn conflicting_harness_alias_cannot_create_shared_custody() {
    reject_alias("harness").await;
}

#[tokio::test]
async fn persisted_profile_cannot_be_rebound_to_another_account_after_restart() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let identity = std::fs::read(context.manifest_z()).unwrap();
        let home = stamp(&context.home_z());
        let mut rebound = context.basis("connection-z").await.connection;
        rebound.account_ref = reference("account-a");
        context.replace(rebound).await;
        context.restart().await;
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), identity);
        assert_eq!(stamp(&context.home_z()), home);
        assert_eq!(slot_count(&context), 1);
    })
    .await;
}

#[tokio::test]
async fn an_unmarked_external_home_is_never_adopted() {
    run_case(true, |context| async move {
        context.prepare("connection-a").await;
        private_directory(&context.profile_z());
        private_directory(&context.home_z());
        let external = stamp(&context.home_z());
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(stamp(&context.home_z()), external);
        assert!(!context.manifest_z().exists());
    })
    .await;
}

#[tokio::test]
async fn a_replaced_private_home_is_rejected_without_adoption() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let identity = std::fs::read(context.manifest_z()).unwrap();
        let original = context.state.parent().unwrap().join("original-home");
        std::fs::rename(context.home_z(), &original).unwrap();
        private_directory(&context.home_z());
        let replacement = stamp(&context.home_z());
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(stamp(&context.home_z()), replacement);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), identity);
        assert_ne!(stamp(&original).inode, replacement.inode);
    })
    .await;
}

#[tokio::test]
async fn a_profile_directory_swap_with_copied_manifest_is_rejected() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let bytes = std::fs::read(context.manifest_z()).unwrap();
        let original = context.state.parent().unwrap().join("original-profile");
        std::fs::rename(context.profile_z(), &original).unwrap();
        private_directory(&context.profile_z());
        private_directory(&context.home_z());
        private_file(&context.manifest_z(), &bytes);
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), bytes);
    })
    .await;
}

#[tokio::test]
async fn symlinked_home_cannot_escape_to_an_outside_directory() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let outside = context.state.parent().unwrap().join("outside");
        private_directory(&outside);
        private_file(
            &outside.join("sentinel"),
            b"outside fixture remains unchanged",
        );
        std::fs::rename(context.home_z(), context.profile_z().join("original-home")).unwrap();
        symlink(&outside, context.home_z()).unwrap();
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"outside fixture remains unchanged"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    })
    .await;
}

#[tokio::test]
async fn symlinked_profile_root_cannot_create_outside_files() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let outside = context.state.parent().unwrap().join("outside");
        private_directory(&outside);
        std::fs::rename(context.custody(), context.state.join("original-custody")).unwrap();
        symlink(&outside, context.custody()).unwrap();
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert!(std::fs::read_dir(outside).unwrap().next().is_none());
    })
    .await;
}

#[tokio::test]
async fn a_state_directory_swap_cannot_redirect_preparation() {
    run_case(true, |context| async move {
        let basis = context.basis("connection-z").await;
        let parent = context.state.parent().unwrap();
        let original = parent.join("original-state");
        let refused = parent.join("replacement-evidence");
        std::fs::rename(&context.state, &original).unwrap();
        private_directory(&context.state);
        let result = context.prepare_at(&basis).await;
        std::fs::rename(&context.state, &refused).unwrap();
        std::fs::rename(&original, &context.state).unwrap();
        assert_rejected(result, &context.state);
        assert!(std::fs::read_dir(refused).unwrap().next().is_none());
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn a_new_database_inode_cannot_reuse_old_custody() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let identity = std::fs::read(context.custody().join("identity.json")).unwrap();
        context.stop_main().await;
        let database = context.state.join("engine.db");
        let original = context.state.join("original.db");
        std::fs::rename(&database, &original).unwrap();
        std::fs::copy(&original, &database).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_ne!(stamp(&original).inode, stamp(&database).inode);
        context.reopen_main().await;
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(
            std::fs::read(context.custody().join("identity.json")).unwrap(),
            identity
        );
    })
    .await;
}

#[tokio::test]
async fn a_hardlinked_profile_identity_is_rejected_without_unlinking_it() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let alias = context.state.parent().unwrap().join("identity-alias");
        std::fs::hard_link(context.manifest_z(), &alias).unwrap();
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(stamp(&context.manifest_z()).links, 2);
        assert_eq!(stamp(&alias).inode, stamp(&context.manifest_z()).inode);
    })
    .await;
}

#[tokio::test]
async fn a_symlinked_identity_is_rejected_without_changing_its_target() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let target = context.state.parent().unwrap().join("identity-target");
        std::fs::rename(context.manifest_z(), &target).unwrap();
        let identity = std::fs::read(&target).unwrap();
        symlink(&target, context.manifest_z()).unwrap();
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(std::fs::read(target).unwrap(), identity);
    })
    .await;
}

async fn reject_manifest(bytes: Vec<u8>) {
    run_case(true, move |context| async move {
        context.prepare("connection-z").await;
        private_file(&context.manifest_z(), &bytes);
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), bytes);
    })
    .await;
}

#[tokio::test]
async fn a_truncated_identity_is_not_repaired_or_adopted() {
    reject_manifest(b"{".to_vec()).await;
}

#[tokio::test]
async fn an_oversized_identity_is_not_read_as_valid_metadata() {
    reject_manifest(vec![b' '; 4097]).await;
}

#[tokio::test]
async fn unknown_identity_fields_are_rejected() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(context.manifest_z()).unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unrecognized".into(), true.into());
        let bytes = serde_json::to_vec(&value).unwrap();
        private_file(&context.manifest_z(), &bytes);
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), bytes);
    })
    .await;
}

#[tokio::test]
async fn duplicate_declared_identity_fields_are_rejected() {
    run_case(true, |context| async move {
        context.prepare("connection-z").await;
        let original = std::fs::read(context.manifest_z()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let (key, first) = value.as_object().unwrap().iter().next().unwrap();
        let mut bytes =
            format!("{{{}:{},", serde_json::to_string(key).unwrap(), first).into_bytes();
        let normalized = serde_json::to_vec(&value).unwrap();
        bytes.extend_from_slice(normalized.strip_prefix(b"{").unwrap());
        private_file(&context.manifest_z(), &bytes);
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(std::fs::read(context.manifest_z()).unwrap(), bytes);
    })
    .await;
}

async fn reject_mode(artifact: &str, mode: u32) {
    run_case(true, {
        let artifact = artifact.to_owned();
        move |context| async move {
            context.prepare("connection-z").await;
            let path = match artifact.as_str() {
                "root" => context.custody(),
                "profile" => context.profile_z(),
                "home" => context.home_z(),
                "identity" => context.manifest_z(),
                _ => panic!("unknown test artifact"),
            };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let basis = context.basis("connection-z").await;
            assert_rejected(context.prepare_at(&basis).await, &context.state);
            assert_eq!(stamp(&path).mode & 0o7777, mode);
        }
    })
    .await;
}

#[tokio::test]
async fn public_profile_root_permissions_are_not_silently_repaired() {
    reject_mode("root", 0o755).await;
}

#[tokio::test]
async fn public_profile_permissions_are_not_silently_repaired() {
    reject_mode("profile", 0o755).await;
}

#[tokio::test]
async fn public_home_permissions_are_not_silently_repaired() {
    reject_mode("home", 0o755).await;
}

#[tokio::test]
async fn public_identity_permissions_are_not_silently_repaired() {
    reject_mode("identity", 0o644).await;
}

#[tokio::test]
async fn special_directory_mode_bits_are_not_private_profile_custody() {
    reject_mode("home", 0o1700).await;
}

#[tokio::test]
async fn a_retained_old_host_incarnation_cannot_prepare_with_the_current_actor() {
    run_case(true, |context| async move {
        let old = context.host().await.identity();
        context.restart().await;
        let basis = context.basis("connection-z").await;
        let current = context.host().await;
        let result = current
            .engine
            .owner_job(|reply| {
                Job::PrepareNativeProfile(
                    old,
                    basis.connection.id.clone(),
                    basis.config_id.clone(),
                    basis.source_hash.clone(),
                    reply,
                )
            })
            .await;
        let Err(error) = result else {
            panic!("old actual host incarnation must be rejected by current actor");
        };
        assert_eq!(error.0, "owner_runtime_stale");
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn another_real_database_host_cannot_prepare_in_this_actor() {
    run_case(true, |context| async move {
        let foreign = context.other_host().await.identity();
        let basis = context.basis("connection-z").await;
        assert_ne!(foreign.db_instance, basis.database_instance);
        let current = context.host().await;
        let result = current
            .engine
            .owner_job(|reply| {
                Job::PrepareNativeProfile(
                    foreign,
                    basis.connection.id.clone(),
                    basis.config_id.clone(),
                    basis.source_hash.clone(),
                    reply,
                )
            })
            .await;
        let Err(error) = result else {
            panic!("foreign actual host stamp must be rejected by current actor");
        };
        assert_eq!(error.0, "owner_runtime_stale");
        assert!(!context.custody().exists());
    })
    .await;
}

#[tokio::test]
async fn retained_profile_capacity_refuses_the_sixty_fifth_but_reuses_the_first() {
    run_case(true, |context| async move {
        let mut first = None;
        for index in 0..64 {
            let mut connection = context.basis("connection-z").await.connection;
            connection.auth = ModelAuthentication::NativeManaged {
                profile_ref: reference(&format!("retained-{index:02}")),
            };
            context.replace(connection).await;
            let prepared = context.prepare("connection-z").await;
            if index == 0 {
                first = Some(prepared.binding().custody_id.clone());
            }
        }
        assert_eq!(slot_count(&context), 64);
        let mut overflow = context.basis("connection-z").await.connection;
        overflow.auth = ModelAuthentication::NativeManaged {
            profile_ref: reference("retained-overflow"),
        };
        context.replace(overflow).await;
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(slot_count(&context), 64);
        let mut original = context.basis("connection-z").await.connection;
        original.auth = ModelAuthentication::NativeManaged {
            profile_ref: reference("retained-00"),
        };
        context.replace(original).await;
        let reused = context.prepare("connection-z").await;
        assert_eq!(Some(reused.binding().custody_id.clone()), first);
        assert_eq!(slot_count(&context), 64);
    })
    .await;
}

#[tokio::test]
async fn incomplete_retained_slots_are_not_evicted_to_admit_another_profile() {
    run_case(true, |context| async move {
        context.prepare("connection-a").await;
        for index in 0..63 {
            let key = format!(
                "{:x}",
                Sha256::digest(format!("incomplete-{index}").as_bytes())
            );
            private_directory(&context.custody().join(key));
        }
        assert_eq!(slot_count(&context), 64);
        let basis = context.basis("connection-z").await;
        assert_rejected(context.prepare_at(&basis).await, &context.state);
        assert_eq!(slot_count(&context), 64);
        assert!(!context.profile_z().exists());
    })
    .await;
}

#[tokio::test]
async fn profile_preparation_changes_no_durable_engine_or_admission_state() {
    run_case(true, |context| async move {
        let basis = context.basis("connection-z").await;
        let before = durable_snapshot(&context.state.join("engine.db"));
        let source = std::fs::read(&context.global).unwrap();
        let host = context.host().await;
        let state = host
            .model_connection_state(&basis.connection.id)
            .await
            .unwrap();
        assert_eq!(state.health, Health::Unknown);
        assert_eq!(state.quota, Quota::Unknown);
        let prepared = context.prepare_at(&basis).await.unwrap();
        assert_receipt(&prepared, &basis);
        let current = context.basis("connection-z").await;
        assert_eq!(current.revision, basis.revision);
        assert_eq!(current.config_id, basis.config_id);
        assert_eq!(current.source_hash, basis.source_hash);
        assert_eq!(std::fs::read(&context.global).unwrap(), source);
        let state = host
            .model_connection_state(&basis.connection.id)
            .await
            .unwrap();
        assert_eq!(state.health, Health::Unknown);
        assert_eq!(state.quota, Quota::Unknown);
        assert!(state.capabilities.is_empty());
        assert_eq!(state.observed_at_ms, None);
        let (capabilities, _, _) = context.read(OwnerRead::Capabilities).await;
        assert!(matches!(
            capabilities.data,
            OwnerData::Capabilities {
                authenticated_owner_transport: true,
                native_worker_admission: false,
                human_consent_proven: false,
                native_input: false,
                ..
            }
        ));
        assert_eq!(durable_snapshot(&context.state.join("engine.db")), before);
    })
    .await;
}

#[path = "account_profile_transport_tests.rs"]
mod account_profile_transport_tests;
