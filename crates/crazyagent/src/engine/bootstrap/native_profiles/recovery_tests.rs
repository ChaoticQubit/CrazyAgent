use super::*;
use std::time::{Duration, SystemTime};

const PROFILE: &str = "recovery-profile";

struct NativeCase {
    _root: tempfile::TempDir,
    state: PathBuf,
    database: PathBuf,
    host: HostStamp,
    connection: ModelConnection,
}

impl NativeCase {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-pr-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("state");
        private_dir(&state);
        let database = state.join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _root: root,
            state,
            database,
            host: HostStamp {
                db_instance: "a".repeat(64),
                engine_epoch: "b".repeat(64),
            },
            connection: native_connection(PROFILE),
        }
    }

    fn custody(&self) -> NativeProfileCustody {
        NativeProfileCustody::capture(&self.database, &std::fs::metadata(&self.database).unwrap())
            .unwrap()
    }

    fn root(&self) -> PathBuf {
        self.state.join(ROOT)
    }

    fn slot(&self) -> PathBuf {
        self.root().join(digest(PROFILE.as_bytes()))
    }

    fn binding(&self, connection: &ModelConnection, custody_id: String) -> NativeProfileBinding {
        NativeProfileBinding {
            database_instance: self.host.db_instance.clone(),
            engine_epoch: self.host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(7),
            connection: connection.clone(),
            custody_id,
        }
    }

    fn usable(&self, custody: &mut NativeProfileCustody, connection: &ModelConnection) {
        let (custody_id, key) = custody.prepare(&self.host, connection).unwrap();
        let guard = custody
            .acquire_use(
                &self.host,
                &self.binding(connection, custody_id),
                &RequestId::new("recovery-use").unwrap(),
                1,
                NativeProfileUsePurpose::Authentication,
            )
            .unwrap();
        assert_eq!(
            guard.profile_path().unwrap(),
            self.root().join(key).join("home")
        );
    }
}

fn native_connection(profile: &str) -> ModelConnection {
    ModelConnection {
        id: reference(&format!("{profile}-connection")),
        provider: reference("recovery-provider"),
        harness: reference("codex"),
        protocol: reference("native"),
        account_ref: reference("recovery-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "recovery-model".into(),
            author: reference("recovery-author"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference(profile),
        },
    }
}

fn interrupt(point: usize) {
    staging::INTERRUPT.set(point);
}

fn staged_entries(directory: &Path) -> Vec<String> {
    if !directory.exists() {
        return Vec::new();
    }
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(".staging-") || name.starts_with(".native-profiles-"))
        .collect()
}

fn assert_whole(case: &NativeCase) {
    if case.root().exists() {
        let bytes = std::fs::read(case.root().join(MANIFEST)).unwrap();
        serde_json::from_slice::<RootIdentity>(&bytes).unwrap();
    }
    if case.slot().exists() {
        let bytes = std::fs::read(case.slot().join(MANIFEST)).unwrap();
        serde_json::from_slice::<ProfileIdentity>(&bytes).unwrap();
        assert!(case.slot().join("home").is_dir());
    }
}

fn touch(path: &Path) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn configured_binding(
    case: &ConfiguredCase,
    connection: &ModelConnection,
    metadata: &ConfiguredRcliProfileMetadata,
) -> NativeProfileBinding {
    let mut binding = case.binding(metadata);
    binding.connection = connection.clone();
    binding
}

fn acquire(
    case: &mut ConfiguredCase,
    connection: &ModelConnection,
    metadata: &ConfiguredRcliProfileMetadata,
) -> Result<NativeProfileUse, EngineError> {
    let binding = configured_binding(case, connection, metadata);
    case.custody.acquire_configured_rcli_use(
        &case.host,
        &binding,
        &case.profile,
        &RequestId::new("recovery-configured-turn").unwrap(),
        metadata.profile_generation,
    )
}

fn recapture(case: &ConfiguredCase) -> NativeProfileCustody {
    let database = case.root.path().join("state/engine.db");
    NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap()).unwrap()
}

#[test]
fn native_profile_creation_interrupted_at_every_step_recovers_on_the_next_call() {
    let mut interrupted = 0;
    for point in 1..64 {
        let case = NativeCase::new();
        let mut custody = case.custody();
        interrupt(point);
        let first = custody.prepare(&case.host, &case.connection);
        interrupt(0);
        let Err(error) = first else {
            assert!(staged_entries(&case.state).is_empty());
            assert!(staged_entries(&case.root()).is_empty());
            break;
        };
        assert_eq!(error.0, "native_profile_interrupted", "step {point}");
        interrupted += 1;
        assert_whole(&case);
        case.usable(&mut custody, &case.connection);
        assert!(staged_entries(&case.state).is_empty(), "step {point}");
        assert!(staged_entries(&case.root()).is_empty(), "step {point}");
        case.usable(&mut case.custody(), &case.connection);
    }
    assert_eq!(interrupted, 11);
}

#[test]
fn native_profile_creation_interrupted_then_restarted_recovers() {
    for point in 1..12 {
        let case = NativeCase::new();
        interrupt(point);
        let first = case.custody().prepare(&case.host, &case.connection);
        interrupt(0);
        assert_eq!(first.err().unwrap().0, "native_profile_interrupted");
        case.usable(&mut case.custody(), &case.connection);
        assert!(staged_entries(&case.state).is_empty(), "step {point}");
        assert!(staged_entries(&case.root()).is_empty(), "step {point}");
    }
}

#[test]
fn slot_holding_home_data_without_a_manifest_is_never_removed() {
    let case = NativeCase::new();
    case.custody()
        .prepare(&case.host, &case.connection)
        .unwrap();
    let saved = case.state.join("saved-manifest");
    std::fs::rename(case.slot().join(MANIFEST), &saved).unwrap();
    private_file(
        &case.slot().join("home/credentials"),
        b"synthetic signed-in state",
    );
    assert_eq!(
        case.custody()
            .prepare(&case.host, &case.connection)
            .err()
            .unwrap()
            .0,
        "native_profile_incomplete"
    );
    assert_eq!(
        std::fs::read(case.slot().join("home/credentials")).unwrap(),
        b"synthetic signed-in state"
    );
    assert!(!case.slot().join(MANIFEST).exists());
}

#[test]
fn slot_this_custody_pinned_is_not_rebuilt_when_it_turns_incomplete() {
    let case = NativeCase::new();
    let mut custody = case.custody();
    custody.prepare(&case.host, &case.connection).unwrap();
    private_file(&case.slot().join(MANIFEST), b"");
    assert!(custody.prepare(&case.host, &case.connection).is_err());
    assert_eq!(std::fs::read(case.slot().join(MANIFEST)).unwrap(), b"");
    assert!(case.slot().join("home").is_dir());
}

#[test]
fn staging_directory_held_by_a_live_creator_is_left_alone() {
    let case = NativeCase::new();
    case.custody()
        .prepare(&case.host, &case.connection)
        .unwrap();
    let live = case.root().join(format!(".staging-{}", "1".repeat(32)));
    private_dir(&live);
    let held = File::open(&live).unwrap();
    flock(&held, FlockOperation::NonBlockingLockExclusive).unwrap();
    case.usable(&mut case.custody(), &native_connection("second-profile"));
    assert!(live.is_dir());
    case.usable(&mut case.custody(), &case.connection);
    drop(held);
    case.usable(&mut case.custody(), &native_connection("third-profile"));
    assert!(!live.exists());
}

#[test]
fn foreign_entry_with_a_staging_name_is_refused_and_kept() {
    let case = NativeCase::new();
    case.custody()
        .prepare(&case.host, &case.connection)
        .unwrap();
    let foreign = case.root().join(format!(".staging-{}", "2".repeat(32)));
    private_dir(&foreign);
    std::os::unix::fs::symlink("/tmp", foreign.join("link")).unwrap();
    assert_eq!(
        case.custody()
            .prepare(&case.host, &native_connection("second-profile"))
            .err()
            .unwrap()
            .0,
        "native_profile_custody_unsafe"
    );
    assert!(
        std::fs::symlink_metadata(foreign.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn configured_registration_interrupted_at_every_step_recovers() {
    let mut interrupted = 0;
    for point in 1..64 {
        let mut case = ConfiguredCase::new(false, true);
        interrupt(point);
        let first = case.custody.register_configured_rcli_profile(
            &case.host,
            &case.connection,
            &case.profile,
            &case.home,
        );
        interrupt(0);
        let Err(error) = first else {
            break;
        };
        assert_eq!(error.0, "native_profile_interrupted", "step {point}");
        interrupted += 1;
        let metadata = case.register();
        assert_eq!(metadata.profile_generation, 1);
        let connection = case.connection.clone();
        acquire(&mut case, &connection, &metadata).unwrap();
        let state = case.root.path().join("state");
        assert!(staged_entries(&state).is_empty(), "step {point}");
        assert!(staged_entries(&state.join(ROOT)).is_empty(), "step {point}");
    }
    assert_eq!(interrupted, 10);
}

#[test]
fn generated_home_creation_interrupted_at_every_step_recovers() {
    let mut interrupted = 0;
    for point in 1..64 {
        let mut case = ConfiguredCase::new(false, false);
        interrupt(point);
        let first = case.custody.prepare_configured_rcli_profile(
            &case.host,
            &case.connection,
            &case.profile,
            None,
        );
        interrupt(0);
        let Err(error) = first else {
            break;
        };
        assert_eq!(error.0, "native_profile_interrupted", "step {point}");
        interrupted += 1;
        let metadata = case
            .custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
            .unwrap();
        assert_eq!(metadata.profile_generation, 1);
        let connection = case.connection.clone();
        acquire(&mut case, &connection, &metadata).unwrap();
        let state = case.root.path().join("state");
        for directory in [
            state.clone(),
            state.join(ROOT),
            state.join("configured-sdk-homes"),
        ] {
            assert!(staged_entries(&directory).is_empty(), "step {point}");
        }
    }
    assert_eq!(interrupted, 16);
}

#[test]
fn generated_home_is_not_left_behind_when_the_root_lock_is_busy() {
    let mut case = ConfiguredCase::new(false, false);
    case.custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let second = reference("second-generated-profile");
    let mut connection = case.connection.clone();
    connection.id = reference("second-generated-connection");
    let root_manifest =
        File::open(case.root.path().join("state").join(ROOT).join(MANIFEST)).unwrap();
    flock(&root_manifest, FlockOperation::NonBlockingLockExclusive).unwrap();
    assert_eq!(
        case.custody
            .prepare_configured_rcli_profile(&case.host, &connection, &second, None)
            .err()
            .unwrap()
            .0,
        "native_profile_in_use"
    );
    let home = case.custody.generated_configured_home(&second);
    assert!(!home.exists());
    drop(root_manifest);
    let metadata = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &connection, &second, None)
        .unwrap();
    assert_eq!(metadata.profile_generation, 1);
}

#[test]
fn half_made_entries_outside_staging_are_refused_and_kept() {
    for layout in ["root", "root-manifest", "slot-home", "slot-manifest"] {
        let case = NativeCase::new();
        if layout.starts_with("slot") {
            case.usable(&mut case.custody(), &native_connection("kept-profile"));
            private_dir(&case.slot());
        } else {
            private_dir(&case.root());
        }
        match layout {
            "root-manifest" => private_file(&case.root().join(MANIFEST), b"{\"version\":1,\"da"),
            "slot-home" => private_dir(&case.slot().join("home")),
            "slot-manifest" => private_file(&case.slot().join(MANIFEST), b""),
            _ => {}
        }
        let before = std::fs::symlink_metadata(case.slot())
            .or_else(|_| std::fs::symlink_metadata(case.root()))
            .unwrap();
        assert!(
            case.custody()
                .prepare(&case.host, &case.connection)
                .is_err(),
            "{layout}"
        );
        let after = std::fs::symlink_metadata(case.slot())
            .or_else(|_| std::fs::symlink_metadata(case.root()))
            .unwrap();
        assert_eq!(after.ino(), before.ino(), "{layout}");
        match layout {
            "root" => assert_eq!(std::fs::read_dir(case.root()).unwrap().count(), 0),
            "root-manifest" => assert_eq!(
                std::fs::read(case.root().join(MANIFEST)).unwrap(),
                b"{\"version\":1,\"da"
            ),
            "slot-home" => {
                assert!(case.slot().join("home").is_dir());
                assert!(!case.slot().join(MANIFEST).exists());
            }
            "slot-manifest" => {
                assert_eq!(std::fs::read(case.slot().join(MANIFEST)).unwrap(), b"");
                assert!(!case.slot().join("home").exists());
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn unregistered_generated_home_with_other_content_is_refused_and_kept() {
    let mut case = ConfiguredCase::new(false, false);
    let home = case.custody.generated_configured_home(&case.profile);
    private_dir(home.parent().unwrap());
    private_dir(&home);
    private_dir(&home.join(".backboard"));
    private_file(&home.join(".backboard/config.json"), b"{\"model\":");
    assert_eq!(
        case.custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
            .err()
            .unwrap()
            .0,
        "native_profile_custody_unsafe"
    );
    assert_eq!(
        std::fs::read(home.join(".backboard/config.json")).unwrap(),
        b"{\"model\":"
    );
}

#[test]
fn unregistered_generated_home_holding_keys_is_refused_and_kept() {
    let mut case = ConfiguredCase::new(false, false);
    let home = case.custody.generated_configured_home(&case.profile);
    private_dir(home.parent().unwrap());
    private_dir(&home);
    private_dir(&home.join(".backboard"));
    private_file(&home.join(".backboard/config.json"), b"{}");
    private_file(
        &home.join(".backboard/keys.json"),
        b"synthetic key material",
    );
    assert_eq!(
        case.custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
            .err()
            .unwrap()
            .0,
        "native_profile_custody_unsafe"
    );
    assert_eq!(
        std::fs::read(home.join(".backboard/keys.json")).unwrap(),
        b"synthetic key material"
    );
}

#[test]
fn owner_registration_repins_a_config_whose_times_changed() {
    let mut case = ConfiguredCase::new(false, true);
    let first = case.register();
    let config = case.home.join(".backboard/config.json");
    touch(&config);
    assert_eq!(
        recapture(&case)
            .configured_rcli_profile(&case.host, &case.connection, &case.profile)
            .err()
            .unwrap()
            .0,
        "rcli_profile_identity_changed"
    );
    let second = case.register();
    assert_eq!(second.profile_generation, 2);
    assert_ne!(second.custody_id, first.custody_id);
    assert_eq!(second.profile_config_sha256, first.profile_config_sha256);
    let connection = case.connection.clone();
    assert!(acquire(&mut case, &connection, &first).is_err());
    drop(acquire(&mut case, &connection, &second).unwrap());
    assert_eq!(case.register(), second);
    let reloaded = recapture(&case)
        .configured_rcli_profile(&case.host, &case.connection, &case.profile)
        .unwrap();
    assert_eq!(reloaded, second);
}

#[test]
fn owner_registration_repins_a_changed_connection() {
    let mut case = ConfiguredCase::new(true, true);
    let first = case.register();
    let original = case.connection.clone();
    case.connection.model.id = "replacement-model".into();
    let config = case.home.join(".backboard/config.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    value["model"]["model"] = "replacement-model".into();
    private_file(&config, &serde_json::to_vec(&value).unwrap());
    assert_eq!(
        recapture(&case)
            .configured_rcli_profile(&case.host, &case.connection, &case.profile)
            .err()
            .unwrap()
            .0,
        "rcli_profile_binding_conflict"
    );
    let second = case.register();
    assert_eq!(second.profile_generation, 2);
    assert_ne!(second.custody_id, first.custody_id);
    assert_eq!(
        second.profile_config_sha256,
        digest(&std::fs::read(&config).unwrap())
    );
    assert!(
        recapture(&case)
            .configured_rcli_profile(&case.host, &original, &case.profile)
            .is_err()
    );
    let connection = case.connection.clone();
    drop(acquire(&mut case, &connection, &second).unwrap());
}

#[test]
fn swap_the_owner_did_not_make_is_still_refused() {
    for swap in ["replaced-file", "edited-bytes", "replaced-home"] {
        let mut case = ConfiguredCase::new(false, true);
        let first = case.register();
        let key = digest(case.profile.as_str().as_bytes());
        let manifest = case
            .root
            .path()
            .join("state")
            .join(ROOT)
            .join(key)
            .join(MANIFEST);
        let pinned = std::fs::read(&manifest).unwrap();
        let config = case.home.join(".backboard/config.json");
        let original = std::fs::read(&config).unwrap();
        let expected = match swap {
            "replaced-file" => {
                let replacement = case.home.join(".backboard/replacement.json");
                private_file(&replacement, &original);
                std::fs::rename(&replacement, &config).unwrap();
                "rcli_profile_identity_changed"
            }
            "edited-bytes" => {
                let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                std::fs::write(&config, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
                "rcli_profile_config_changed"
            }
            "replaced-home" => {
                std::fs::rename(&case.home, case.root.path().join("moved-home")).unwrap();
                private_dir(&case.home);
                private_dir(&case.home.join(".backboard"));
                private_file(&config, &original);
                "rcli_profile_identity_changed"
            }
            _ => unreachable!(),
        };
        let mut restarted = recapture(&case);
        assert_eq!(
            restarted
                .register_configured_rcli_profile(
                    &case.host,
                    &case.connection,
                    &case.profile,
                    &case.home,
                )
                .err()
                .unwrap()
                .0,
            expected,
            "{swap}"
        );
        assert_eq!(std::fs::read(&manifest).unwrap(), pinned, "{swap}");
        let connection = case.connection.clone();
        assert!(acquire(&mut case, &connection, &first).is_err(), "{swap}");
    }
}

#[test]
fn repin_is_refused_while_the_profile_is_in_use() {
    let mut case = ConfiguredCase::new(false, true);
    let first = case.register();
    let connection = case.connection.clone();
    let guard = acquire(&mut case, &connection, &first).unwrap();
    touch(&case.home.join(".backboard/config.json"));
    let mut restarted = recapture(&case);
    assert_eq!(
        restarted
            .register_configured_rcli_profile(
                &case.host,
                &case.connection,
                &case.profile,
                &case.home,
            )
            .err()
            .unwrap()
            .0,
        "native_profile_in_use"
    );
    drop(guard);
    case.custody = restarted;
    assert_eq!(case.register().profile_generation, 2);
}

#[test]
fn repin_interrupted_at_every_step_settles_on_one_new_generation() {
    let mut interrupted = 0;
    for point in 1..64 {
        let mut case = ConfiguredCase::new(false, true);
        case.register();
        touch(&case.home.join(".backboard/config.json"));
        interrupt(point);
        let first = case.custody.register_configured_rcli_profile(
            &case.host,
            &case.connection,
            &case.profile,
            &case.home,
        );
        interrupt(0);
        let Err(error) = first else {
            break;
        };
        assert_eq!(error.0, "native_profile_interrupted", "step {point}");
        interrupted += 1;
        case.custody = recapture(&case);
        let settled = case.register();
        assert_eq!(settled.profile_generation, 2, "step {point}");
        let connection = case.connection.clone();
        drop(acquire(&mut case, &connection, &settled).unwrap());
        let state = case.root.path().join("state");
        assert!(staged_entries(&state.join(ROOT)).is_empty(), "step {point}");
    }
    assert_eq!(interrupted, 5);
}

#[test]
fn account_setup_rewrites_the_generated_config_for_a_changed_connection() {
    let mut case = ConfiguredCase::new(false, false);
    let first = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let mut changed = case.connection.clone();
    changed.model.id = "replacement-model".into();
    let second = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &changed, &case.profile, None)
        .unwrap();
    assert_eq!(second.profile_generation, 2);
    let config = case
        .custody
        .generated_configured_home(&case.profile)
        .join(".backboard/config.json");
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(written["model"]["model"], "replacement-model");
    assert_ne!(second.profile_config_sha256, first.profile_config_sha256);
    let replay = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &changed, &case.profile, None)
        .unwrap();
    assert_eq!(replay, second);
    drop(acquire(&mut case, &changed, &second).unwrap());
    let original = case.connection.clone();
    assert!(acquire(&mut case, &original, &first).is_err());
}

#[test]
fn account_setup_repins_a_generated_config_whose_times_changed() {
    let mut case = ConfiguredCase::new(false, false);
    let first = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    touch(
        &case
            .custody
            .generated_configured_home(&case.profile)
            .join(".backboard/config.json"),
    );
    let second = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    assert_eq!(second.profile_generation, 2);
    assert_eq!(second.profile_config_sha256, first.profile_config_sha256);
}
