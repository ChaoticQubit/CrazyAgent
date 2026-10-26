use super::*;
use crate::config::{ModelEndpoint, ModelReference};
use crate::protocol::{ConfigId, Cursor, RequestId};
use rustix::fs::{FlockOperation, flock};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixDatagram;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const EXEC_PROBE: &str = r#"
import errno, fcntl, json, os, select, signal, socket, sys
signal.alarm(15)
address, manifest = sys.argv[1:3]
protected = {tuple(item) for item in json.loads(sys.argv[3])}
control_fd, control_dev, control_ino = map(int, sys.argv[4:7])
control = os.fstat(control_fd)
assert (control.st_dev, control.st_ino) == (control_dev, control_ino)
entries = os.listdir('/dev/fd')
assert len(entries) <= 512
for entry in entries:
    try:
        observed = os.fstat(int(entry))
    except OSError as error:
        assert error.errno == errno.EBADF
        continue
    assert (observed.st_dev, observed.st_ino) not in protected
channel = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
channel.sendto(b'E', address)
for unused in range(4):
    assert select.select([0], [], [], 10)[0]
    command = os.read(0, 1)
    if command == b'Q':
        sys.exit(0)
    assert command == b'P'
    fd = os.open(manifest, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    observed = os.fstat(fd)
    assert (observed.st_dev, observed.st_ino) in protected
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        channel.sendto(b'B', address)
    else:
        channel.sendto(b'F', address)
    finally:
        os.close(fd)
raise RuntimeError('exec probe command bound')
"#;

static SOCKET_ID: AtomicUsize = AtomicUsize::new(0);

struct UseCase {
    root: tempfile::TempDir,
    state: PathBuf,
    database: PathBuf,
    host: HostStamp,
    binding: NativeProfileBinding,
    operation_id: RequestId,
    custody: Option<NativeProfileCustody>,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn request_id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(path.is_dir());
    assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o7777, 0o700);
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), bytes.len() as u64);
    assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o7777, 0o600);
}

fn connection_for(harness: &str) -> ModelConnection {
    ModelConnection {
        id: reference("unit-connection"),
        provider: reference("unit-provider"),
        harness: reference(harness),
        protocol: reference("native"),
        account_ref: reference("unit-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "unit-model".into(),
            author: reference("unit-author"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("unit-profile"),
        },
    }
}

fn purpose(index: usize) -> NativeProfileUsePurpose {
    match index {
        0 => NativeProfileUsePurpose::Authentication,
        1 => NativeProfileUsePurpose::Refresh,
        2 => NativeProfileUsePurpose::Logout,
        3 => NativeProfileUsePurpose::Execution,
        4 => NativeProfileUsePurpose::Check,
        _ => panic!("closed purpose index"),
    }
}

fn purpose_name(value: &NativeProfileUsePurpose) -> &'static str {
    match value {
        NativeProfileUsePurpose::Authentication => "authentication",
        NativeProfileUsePurpose::Refresh => "refresh",
        NativeProfileUsePurpose::Logout => "logout",
        NativeProfileUsePurpose::Execution => "execution",
        NativeProfileUsePurpose::Check => "check",
    }
}

impl UseCase {
    fn new() -> Self {
        Self::with_harness("codex")
    }

    fn with_harness(harness: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-pu-unit-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("s");
        private_directory(&state);
        let database = state.join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut custody =
            NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap())
                .unwrap();
        let host = HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        };
        let connection = connection_for(harness);
        let (custody_id, key) = custody.prepare(&host, &connection).unwrap();
        assert_eq!(key, digest(b"unit-profile"));
        assert_eq!(custody_id.len(), 64);
        let binding = NativeProfileBinding {
            database_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(7),
            connection,
            custody_id,
        };
        Self {
            root,
            state,
            database,
            host,
            binding,
            operation_id: request_id("unit-use"),
            custody: Some(custody),
        }
    }

    fn acquire(&mut self) -> NativeProfileUse {
        self.custody
            .as_mut()
            .unwrap()
            .acquire_use(&self.host, &self.binding, &self.operation_id, 1, purpose(0))
            .unwrap()
    }

    fn revalidate(&self, guard: &NativeProfileUse) -> Result<(), EngineError> {
        guard.revalidate(&self.host, &self.binding, &self.operation_id, 1, purpose(0))
    }

    fn recapture(&self) -> NativeProfileCustody {
        NativeProfileCustody::capture(&self.database, &std::fs::metadata(&self.database).unwrap())
            .unwrap()
    }

    fn profile(&self) -> PathBuf {
        self.state.join(ROOT).join(digest(b"unit-profile"))
    }

    fn manifest(&self) -> PathBuf {
        self.profile().join(MANIFEST)
    }

    fn home(&self) -> PathBuf {
        self.profile().join("home")
    }

    fn protected_paths(&self) -> Vec<PathBuf> {
        vec![
            self.state.clone(),
            self.database.clone(),
            self.state.join(ROOT),
            self.state.join(ROOT).join(MANIFEST),
            self.profile(),
            self.home(),
            self.manifest(),
        ]
    }

    fn other_binding(&mut self, mut connection: ModelConnection) -> NativeProfileBinding {
        connection.id = reference("unit-other-connection");
        let (custody_id, _) = self
            .custody
            .as_mut()
            .unwrap()
            .prepare(&self.host, &connection)
            .unwrap();
        let mut binding = self.binding.clone();
        binding.connection = connection;
        binding.custody_id = custody_id;
        assert_eq!(binding.database_instance, self.host.db_instance);
        assert_eq!(binding.engine_epoch, self.host.engine_epoch);
        binding
    }
}

fn assert_busy(path: &Path) {
    let file = File::open(path).unwrap();
    let result = flock(&file, FlockOperation::NonBlockingLockExclusive);
    assert!(result.is_err());
    assert_eq!(result.err().unwrap(), rustix::io::Errno::WOULDBLOCK);
}

fn assert_free(path: &Path) {
    let file = File::open(path).unwrap();
    assert!(flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok());
    assert_eq!(file.metadata().unwrap().nlink(), 1);
}

#[test]
fn retained_guard_revalidates_exact_scope_and_is_non_clone() {
    trait AmbiguousIfClone<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfClone<()> for T {}
    struct IfClone;
    impl<T: Clone> AmbiguousIfClone<IfClone> for T {}
    let _ = <NativeProfileUse as AmbiguousIfClone<_>>::check;
    let mut case = UseCase::new();
    let guard = case.acquire();
    assert!(case.revalidate(&guard).is_ok());
    assert_busy(&case.manifest());
    drop(guard);
    assert_free(&case.manifest());
}

#[test]
fn purpose_enum_has_exactly_the_five_closed_cases() {
    let names = (0..5)
        .map(|index| purpose_name(&purpose(index)))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["authentication", "refresh", "logout", "execution", "check"]
    );
    assert_eq!(names.len(), 5);
}

#[test]
fn every_pair_with_an_account_change_contends_for_the_same_live_physical_lock() {
    let shares = |index| {
        matches!(
            purpose(index),
            NativeProfileUsePurpose::Execution | NativeProfileUsePurpose::Check
        )
    };
    for holder in 0..5 {
        for contender in 0..5 {
            if shares(holder) && shares(contender) && (holder, contender) != (4, 4) {
                continue;
            }
            let mut case = UseCase::new();
            let guard = case
                .custody
                .as_mut()
                .unwrap()
                .acquire_use(
                    &case.host,
                    &case.binding,
                    &case.operation_id,
                    1,
                    purpose(holder),
                )
                .unwrap();
            let mut other = case.recapture();
            let result = other.acquire_use(
                &case.host,
                &case.binding,
                &request_id("other-use"),
                2,
                purpose(contender),
            );
            assert_eq!(
                result.err().expect("exclusive use required").0,
                "native_profile_in_use"
            );
            assert!(
                guard
                    .revalidate(
                        &case.host,
                        &case.binding,
                        &case.operation_id,
                        1,
                        purpose(holder),
                    )
                    .is_ok()
            );
            drop(guard);
            assert_free(&case.manifest());
        }
    }
}

#[test]
fn one_profile_serves_concurrent_executions_while_account_changes_stay_exclusive() {
    let mut case = UseCase::new();
    let execution = NativeProfileUsePurpose::Execution;
    let custody = case.custody.as_mut().unwrap();
    let first = custody
        .acquire_use(
            &case.host,
            &case.binding,
            &request_id("agent-1"),
            1,
            execution,
        )
        .unwrap();
    let second = custody
        .acquire_use(
            &case.host,
            &case.binding,
            &request_id("agent-2"),
            1,
            execution,
        )
        .unwrap();
    let third = case
        .recapture()
        .acquire_use(
            &case.host,
            &case.binding,
            &request_id("agent-3"),
            1,
            execution,
        )
        .unwrap();
    for (guard, name) in [
        (&first, "agent-1"),
        (&second, "agent-2"),
        (&third, "agent-3"),
    ] {
        assert!(
            guard
                .revalidate(&case.host, &case.binding, &request_id(name), 1, execution)
                .is_ok()
        );
    }
    for change in 0..3 {
        let result = case.recapture().acquire_use(
            &case.host,
            &case.binding,
            &request_id("account-change"),
            1,
            purpose(change),
        );
        assert_eq!(
            result.err().expect("account change must wait for agents").0,
            "native_profile_in_use"
        );
    }
    assert_busy(&case.manifest());
    drop(first);
    drop(second);
    assert_busy(&case.manifest());
    drop(third);
    assert_free(&case.manifest());
}

#[test]
fn dropping_a_lease_frees_the_profile_while_another_process_shares_its_descriptor() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    let shared = guard.manifest.file.try_clone().unwrap();
    fcntl_setfd(&shared, FdFlags::empty()).unwrap();
    assert!(!fcntl_getfd(&shared).unwrap().contains(FdFlags::CLOEXEC));
    let mut holder = crate::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .env_clear()
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .expect("actual descriptor holder required");
    drop(shared);
    assert_busy(&case.manifest());
    drop(guard);
    let file = File::open(case.manifest()).unwrap();
    let freed = flock(&file, FlockOperation::NonBlockingLockExclusive);
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(freed.is_ok(), "{freed:?}");
    assert!(holder.try_wait().unwrap().is_some());
}

#[test]
fn dropping_coordinator_custody_preserves_owned_guard_descriptors_and_lock() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    drop(case.custody.take().unwrap());
    assert!(case.revalidate(&guard).is_ok());
    assert_busy(&case.manifest());
    let mut other = case.recapture();
    let result = other.acquire_use(
        &case.host,
        &case.binding,
        &request_id("new-coordinator"),
        2,
        purpose(2),
    );
    assert_eq!(
        result.err().expect("retained lock required").0,
        "native_profile_in_use"
    );
    drop(guard);
    assert_free(&case.manifest());
}

#[test]
fn compatible_model_alias_cannot_bypass_the_retained_guard() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    let mut connection = case.binding.connection.clone();
    connection.model.id = "another-model".into();
    let alias = case.other_binding(connection);
    assert_eq!(alias.custody_id, case.binding.custody_id);
    assert_ne!(alias.connection.id, case.binding.connection.id);
    let result = case.custody.as_mut().unwrap().acquire_use(
        &case.host,
        &alias,
        &request_id("alias-use"),
        2,
        purpose(2),
    );
    assert_eq!(
        result.err().expect("shared physical lock required").0,
        "native_profile_in_use"
    );
    assert!(case.revalidate(&guard).is_ok());
}

#[test]
fn distinct_safe_profiles_can_retain_independent_guards() {
    let mut case = UseCase::new();
    let first = case.acquire();
    let mut connection = case.binding.connection.clone();
    connection.account_ref = reference("another-account");
    connection.auth = ModelAuthentication::NativeManaged {
        profile_ref: reference("another-profile"),
    };
    let binding = case.other_binding(connection);
    let operation = request_id("independent-use");
    let second = case
        .custody
        .as_mut()
        .unwrap()
        .acquire_use(&case.host, &binding, &operation, 1, purpose(2))
        .unwrap();
    assert_ne!(binding.custody_id, case.binding.custody_id);
    assert!(case.revalidate(&first).is_ok());
    assert!(
        second
            .revalidate(&case.host, &binding, &operation, 1, purpose(2))
            .is_ok()
    );
    assert_busy(&case.manifest());
}

#[test]
fn revalidation_rejects_a_different_host_database() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    let mut host = case.host.clone();
    host.db_instance = "e".repeat(64);
    assert!(
        guard
            .revalidate(&host, &case.binding, &case.operation_id, 1, purpose(0))
            .is_err()
    );
    assert!(case.revalidate(&guard).is_ok());
}

#[test]
fn revalidation_rejects_a_different_host_epoch() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    let mut host = case.host.clone();
    host.engine_epoch = "e".repeat(64);
    assert!(
        guard
            .revalidate(&host, &case.binding, &case.operation_id, 1, purpose(0))
            .is_err()
    );
    assert!(case.revalidate(&guard).is_ok());
}

fn changed_binding(binding: &NativeProfileBinding, index: usize) -> NativeProfileBinding {
    let mut changed = binding.clone();
    match index {
        0 => changed.database_instance = "e".repeat(64),
        1 => changed.engine_epoch = "e".repeat(64),
        2 => changed.config_id = ConfigId::new("e".repeat(64)).unwrap(),
        3 => changed.source_hash = "e".repeat(64),
        4 => changed.at_revision = Cursor::new(8),
        5 => changed.connection.id = reference("another-connection"),
        6 => changed.connection.model.id = "another-model".into(),
        7 => changed.connection.provider = reference("another-provider"),
        8 => changed.connection.harness = reference("claude"),
        9 => changed.connection.protocol = reference("another-protocol"),
        10 => changed.connection.account_ref = reference("another-account"),
        11 => {
            changed.connection.auth = ModelAuthentication::NativeManaged {
                profile_ref: reference("another-profile"),
            }
        }
        12 => changed.custody_id = "e".repeat(64),
        _ => panic!("binding mutation index"),
    }
    changed
}

#[test]
fn revalidation_rejects_each_resolved_binding_field_change() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    for index in 0..13 {
        let changed = changed_binding(&case.binding, index);
        assert_ne!(changed, case.binding);
        assert!(
            guard
                .revalidate(&case.host, &changed, &case.operation_id, 1, purpose(0),)
                .is_err(),
            "binding mutation {index}"
        );
        assert!(case.revalidate(&guard).is_ok());
    }
}

#[test]
fn revalidation_rejects_a_different_operation_request_id() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    assert!(
        guard
            .revalidate(
                &case.host,
                &case.binding,
                &request_id("another-use"),
                1,
                purpose(0),
            )
            .is_err()
    );
    assert!(case.revalidate(&guard).is_ok());
}

#[test]
fn revalidation_rejects_zero_and_different_generations() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    for generation in [0, 2, u64::MAX] {
        assert!(
            guard
                .revalidate(
                    &case.host,
                    &case.binding,
                    &case.operation_id,
                    generation,
                    purpose(0),
                )
                .is_err()
        );
        assert!(case.revalidate(&guard).is_ok());
    }
}

#[test]
fn revalidation_rejects_a_different_closed_purpose() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    for index in 1..5 {
        assert!(
            guard
                .revalidate(
                    &case.host,
                    &case.binding,
                    &case.operation_id,
                    1,
                    purpose(index),
                )
                .is_err()
        );
        assert!(case.revalidate(&guard).is_ok());
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SentinelStamp {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    accessed: (i64, i64),
    modified: (i64, i64),
}

fn sentinel_stamp(path: &Path) -> SentinelStamp {
    let meta = std::fs::symlink_metadata(path).unwrap();
    SentinelStamp {
        device: meta.dev(),
        inode: meta.ino(),
        owner: meta.uid(),
        mode: meta.mode(),
        links: meta.nlink(),
        length: meta.len(),
        accessed: (meta.atime(), meta.atime_nsec()),
        modified: (meta.mtime(), meta.mtime_nsec()),
    }
}

fn unread_sentinel(home: &Path) -> (PathBuf, SentinelStamp) {
    let path = home.join("native-unread");
    private_file(&path, b"synthetic native state must stay unread\n");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let stamp = sentinel_stamp(&path);
    assert_eq!(stamp.mode & 0o7777, 0);
    assert!(stamp.length > 0);
    (path, stamp)
}

#[test]
fn revalidation_rejects_each_protected_path_inode_replacement() {
    for index in 0..7 {
        let mut case = UseCase::new();
        let guard = case.acquire();
        let (sentinel, before) = unread_sentinel(&case.home());
        let path = case.protected_paths().remove(index);
        let saved = case.root.path().join("saved-artifact");
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        std::fs::rename(&path, &saved).unwrap();
        if metadata.is_dir() {
            private_directory(&path);
        } else {
            private_file(&path, &std::fs::read(&saved).unwrap());
        }
        assert_ne!(metadata.ino(), std::fs::metadata(&path).unwrap().ino());
        assert!(case.revalidate(&guard).is_err(), "protected path {index}");
        let retained = match sentinel.strip_prefix(&path) {
            Ok(relative) => saved.join(relative),
            Err(_) => sentinel,
        };
        assert_eq!(sentinel_stamp(&retained), before);
    }
}

#[test]
fn revalidation_rejects_each_protected_mode_change_without_repair() {
    for index in 0..7 {
        let mut case = UseCase::new();
        let guard = case.acquire();
        let (sentinel, before) = unread_sentinel(&case.home());
        let path = case.protected_paths().remove(index);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(case.revalidate(&guard).is_err(), "protected mode {index}");
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o777);
        assert_eq!(sentinel_stamp(&sentinel), before);
    }
}

#[test]
fn revalidation_rejects_both_manifest_byte_changes_on_the_same_inode() {
    for root_manifest in [false, true] {
        let mut case = UseCase::new();
        let guard = case.acquire();
        let path = if root_manifest {
            case.state.join(ROOT).join(MANIFEST)
        } else {
            case.manifest()
        };
        let inode = std::fs::metadata(&path).unwrap().ino();
        let mut bytes = std::fs::read(&path).unwrap();
        let original = bytes.clone();
        let position = bytes
            .windows(11)
            .position(|part| part == b"\"version\":1")
            .unwrap();
        bytes[position + 10] = b'2';
        std::fs::write(&path, &bytes).unwrap();
        let (sentinel, before) = unread_sentinel(&case.home());
        assert_eq!(bytes.len(), original.len());
        assert_ne!(bytes, original);
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        assert!(case.revalidate(&guard).is_err());
        assert_eq!(sentinel_stamp(&sentinel), before);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn revalidation_rejects_extra_root_and_profile_layout_entries() {
    for root_entry in [false, true] {
        let mut case = UseCase::new();
        let guard = case.acquire();
        let parent = if root_entry {
            case.state.join(ROOT)
        } else {
            case.profile()
        };
        let added = parent.join("unexpected-entry");
        private_file(&added, b"synthetic extra layout entry");
        assert!(case.revalidate(&guard).is_err());
        assert_eq!(
            std::fs::read(added).unwrap(),
            b"synthetic extra layout entry"
        );
    }
}

#[test]
fn revalidation_rejects_missing_home_and_identity_without_recreation() {
    for home in [false, true] {
        let mut case = UseCase::new();
        let guard = case.acquire();
        let path = if home { case.home() } else { case.manifest() };
        let saved = case.root.path().join("removed-artifact");
        std::fs::rename(&path, &saved).unwrap();
        assert!(case.revalidate(&guard).is_err());
        assert!(!path.exists());
        assert!(saved.exists());
    }
}

#[test]
fn revalidation_rejects_a_home_symlink_without_reading_the_target() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    std::fs::rename(case.home(), case.root.path().join("original-home")).unwrap();
    let outside = case.root.path().join("outside");
    private_directory(&outside);
    let (sentinel, before) = unread_sentinel(&outside);
    symlink(&outside, case.home()).unwrap();
    assert!(case.revalidate(&guard).is_err());
    assert_eq!(sentinel_stamp(&sentinel), before);
    assert!(
        std::fs::symlink_metadata(case.home())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn revalidation_rejects_new_manifest_hardlinks_without_unlinking_them() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    let alias = case.root.path().join("identity-hardlink");
    std::fs::hard_link(case.manifest(), &alias).unwrap();
    assert!(case.revalidate(&guard).is_err());
    assert_eq!(std::fs::metadata(case.manifest()).unwrap().nlink(), 2);
    assert_eq!(
        std::fs::metadata(alias).unwrap().ino(),
        std::fs::metadata(case.manifest()).unwrap().ino()
    );
}

#[test]
fn retained_revalidation_preserves_populated_unread_native_home_and_exact_layout() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    let (sentinel, before) = unread_sentinel(&case.home());
    let profile_bytes = std::fs::read(case.manifest()).unwrap();
    let root_manifest = case.state.join(ROOT).join(MANIFEST);
    let root_bytes = std::fs::read(&root_manifest).unwrap();
    for _ in 0..3 {
        assert!(case.revalidate(&guard).is_ok());
        assert_eq!(sentinel_stamp(&sentinel), before);
    }
    let mut entries = std::fs::read_dir(case.profile())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries, [OsString::from("home"), OsString::from(MANIFEST)]);
    assert_eq!(std::fs::read(case.manifest()).unwrap(), profile_bytes);
    assert_eq!(std::fs::read(root_manifest).unwrap(), root_bytes);
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        std::fs::read(sentinel).unwrap(),
        b"synthetic native state must stay unread\n"
    );
}

#[test]
fn substituted_retained_manifest_descriptor_is_rejected() {
    let mut case = UseCase::new();
    let mut guard = case.acquire();
    let replacement = case.root.path().join("unrelated-identity");
    private_file(&replacement, &std::fs::read(case.manifest()).unwrap());
    let retained = std::mem::replace(&mut guard.manifest.file, File::open(replacement).unwrap());
    assert_ne!(
        retained.metadata().unwrap().ino(),
        guard.manifest.file.metadata().unwrap().ino()
    );
    assert!(case.revalidate(&guard).is_err());
    assert_busy(&case.manifest());
    drop(guard);
    assert_busy(&case.manifest());
    drop(retained);
    assert_free(&case.manifest());
}

struct ExecProbe {
    child: Child,
    readiness: UnixDatagram,
    commands: u8,
    retired: bool,
}

impl ExecProbe {
    fn spawn(case: &UseCase) -> Self {
        let path = case.root.path().join(format!(
            "e{}.sock",
            SOCKET_ID.fetch_add(1, Ordering::Relaxed),
        ));
        let readiness = crate::spawn_gate::bind_unix_datagram_std(&path).unwrap();
        readiness
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let canary_path = case.root.path().join("exec-inheritance-control");
        private_file(&canary_path, b"synthetic inherited descriptor control");
        let canary = File::open(canary_path).unwrap();
        let metadata = canary.metadata().unwrap();
        fcntl_setfd(&canary, FdFlags::empty()).unwrap();
        assert!(!fcntl_getfd(&canary).unwrap().contains(FdFlags::CLOEXEC));
        let protected = case
            .protected_paths()
            .iter()
            .map(|path| {
                let metadata = std::fs::metadata(path).unwrap();
                [metadata.dev(), metadata.ino()]
            })
            .collect::<Vec<_>>();
        assert_eq!(protected.len(), 7);
        let child = crate::spawn_gate::spawn(
            Command::new("/usr/bin/python3")
                .env_clear()
                .args(["-I", "-S", "-c", EXEC_PROBE])
                .arg(path)
                .arg(case.manifest())
                .arg(serde_json::to_string(&protected).unwrap())
                .args([
                    canary.as_raw_fd().to_string(),
                    metadata.dev().to_string(),
                    metadata.ino().to_string(),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .expect("actual exec probe required");
        let mut probe = Self {
            child,
            readiness,
            commands: 0,
            retired: false,
        };
        fcntl_setfd(&canary, FdFlags::CLOEXEC).unwrap();
        assert_eq!(probe.event(), b'E');
        probe
    }

    fn event(&mut self) -> u8 {
        let mut bytes = [0; 2];
        let result = self.readiness.recv(&mut bytes);
        assert!(
            result.is_ok(),
            "exec readiness failed: {result:?}; child {:?}",
            self.child.try_wait()
        );
        assert_eq!(result.unwrap(), 1);
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

    fn probe(&mut self, expected: u8) {
        self.command(b'P');
        assert_eq!(self.event(), expected);
        assert!(self.child.try_wait().unwrap().is_none());
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

    fn finish(mut self) {
        self.command(b'Q');
        let status = self.wait_exit().expect("exec child retirement status");
        assert!(
            status.is_some(),
            "exec retirement deadline before fallback cleanup"
        );
        assert!(status.unwrap().success());
        assert!(self.retired);
    }
}

impl Drop for ExecProbe {
    fn drop(&mut self) {
        if self.retired {
            return;
        }
        eprintln!(
            "exec fixture {} entered fallback after an unfinished path",
            self.child.id()
        );
        let killed = self.child.kill();
        let status = self.wait_exit();
        if std::thread::panicking() {
            eprintln!("exec fixture fallback kill={killed:?}, retirement={status:?}");
        } else {
            assert!(
                matches!(status, Ok(Some(_))),
                "exec fixture did not retire: {status:?}"
            );
            panic!("exec fixture required fallback cleanup: {killed:?}");
        }
    }
}

#[test]
fn dropping_a_lease_frees_the_profile_even_while_a_duplicate_description_is_open() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    let duplicate = guard.manifest.file.try_clone().unwrap();
    assert_eq!(
        duplicate.metadata().unwrap().ino(),
        guard.manifest.file.metadata().unwrap().ino()
    );
    assert!(fcntl_getfd(&duplicate).unwrap().contains(FdFlags::CLOEXEC));
    let mut child = ExecProbe::spawn(&case);
    child.probe(b'B');
    drop(guard);
    child.probe(b'F');
    drop(duplicate);
    child.probe(b'F');
    child.finish();
    assert_free(&case.manifest());
}

#[test]
fn protected_guard_and_database_descriptors_do_not_survive_real_exec() {
    let mut case = UseCase::new();
    let guard = case.acquire();
    assert!(
        fcntl_getfd(&guard.manifest.file)
            .unwrap()
            .contains(FdFlags::CLOEXEC)
    );
    let mut child = ExecProbe::spawn(&case);
    child.probe(b'B');
    assert!(case.revalidate(&guard).is_ok());
    drop(guard);
    child.probe(b'F');
    child.finish();
    assert_free(&case.manifest());
}

fn assert_internal_assertion<F>(body: F)
where
    F: FnOnce(),
{
    let result = catch_unwind(AssertUnwindSafe(body));
    assert!(result.is_err(), "actual internal invariant must panic");
    let panic = result.err().unwrap();
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.starts_with("assertion"),
        "expected a Rust assertion, got {message:?}"
    );
}

#[test]
fn zero_generation_guard_assembly_trips_a_real_internal_assertion() {
    let mut case = UseCase::new();
    assert_internal_assertion(|| {
        let _ = case.custody.as_mut().unwrap().acquire_use(
            &case.host,
            &case.binding,
            &case.operation_id,
            0,
            purpose(0),
        );
    });
    assert_free(&case.manifest());
    let guard = case.acquire();
    assert!(
        guard
            .revalidate(&case.host, &case.binding, &case.operation_id, 1, purpose(0),)
            .is_ok()
    );
}

#[test]
fn contradictory_database_guard_assembly_trips_a_real_internal_assertion() {
    let mut case = UseCase::new();
    let mut host = case.host.clone();
    host.db_instance = "e".repeat(64);
    assert_internal_assertion(|| {
        let _ = case.custody.as_mut().unwrap().acquire_use(
            &host,
            &case.binding,
            &case.operation_id,
            1,
            purpose(0),
        );
    });
    assert_free(&case.manifest());
    let guard = case.acquire();
    assert!(case.revalidate(&guard).is_ok());
}

#[test]
fn contradictory_epoch_guard_assembly_trips_a_real_internal_assertion() {
    let mut case = UseCase::new();
    let mut host = case.host.clone();
    host.engine_epoch = "e".repeat(64);
    assert_internal_assertion(|| {
        let _ = case.custody.as_mut().unwrap().acquire_use(
            &host,
            &case.binding,
            &case.operation_id,
            1,
            purpose(0),
        );
    });
    assert_free(&case.manifest());
    let guard = case.acquire();
    assert!(case.revalidate(&guard).is_ok());
}

mod agent_copy_tests;
