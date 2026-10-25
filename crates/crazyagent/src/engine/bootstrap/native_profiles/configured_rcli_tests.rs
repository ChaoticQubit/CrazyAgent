use super::*;
use crate::config::{ModelEndpoint, ModelReference};
use crate::protocol::{ConfigId, Cursor};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct ConfiguredCase {
    root: tempfile::TempDir,
    custody: NativeProfileCustody,
    host: HostStamp,
    connection: ModelConnection,
    profile: CredentialRef,
    home: PathBuf,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_dir(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

impl ConfiguredCase {
    fn new(api_key: bool, configured: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-rc-")
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
        let custody =
            NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap())
                .unwrap();
        let home = root.path().join("configured-home");
        private_dir(&home);
        if configured {
            private_dir(&home.join(".backboard"));
            let auth = if api_key { "apiKey" } else { "none" };
            let config = serde_json::json!({
                "model": { "provider": "local-provider", "model": "local-model" },
                "providers": [{
                    "id": "local-provider", "name": "Local provider",
                    "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
                    "auth": { "type": auth }
                }]
            });
            private_file(
                &home.join(".backboard/config.json"),
                &serde_json::to_vec(&config).unwrap(),
            );
        }
        let connection = ModelConnection {
            id: reference("configured-connection"),
            provider: reference("local-provider"),
            harness: reference("rcli"),
            protocol: reference("openai-chat"),
            account_ref: reference("declared-account"),
            endpoint: ModelEndpoint::Http {
                url: "http://127.0.0.1:11434/v1".into(),
            },
            model: ModelReference {
                id: "local-model".into(),
                author: reference("local-author"),
            },
            auth: if api_key {
                ModelAuthentication::ApiKey {
                    credential_ref: reference("owner-key-reference"),
                }
            } else {
                ModelAuthentication::None {}
            },
        };
        Self {
            root,
            custody,
            host: HostStamp {
                db_instance: "a".repeat(64),
                engine_epoch: "b".repeat(64),
            },
            connection,
            profile: reference("declared-profile"),
            home,
        }
    }

    fn register(&mut self) -> ConfiguredRcliProfileMetadata {
        self.custody
            .register_configured_rcli_profile(
                &self.host,
                &self.connection,
                &self.profile,
                &self.home,
            )
            .unwrap()
    }

    fn binding(&self, metadata: &ConfiguredRcliProfileMetadata) -> NativeProfileBinding {
        NativeProfileBinding {
            database_instance: self.host.db_instance.clone(),
            engine_epoch: self.host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(7),
            connection: self.connection.clone(),
            custody_id: metadata.custody_id.clone(),
        }
    }
}

#[test]
fn configured_rcli_registration_checks_real_config_without_creating_keys() {
    for api_key in [false, true] {
        let mut case = ConfiguredCase::new(api_key, true);
        let config = case.home.join(".backboard/config.json");
        let original = std::fs::read(&config).unwrap();
        let metadata = case.register();
        assert_eq!(metadata.profile_ref, case.profile);
        assert_eq!(metadata.profile_config_sha256, digest(&original));
        assert!(metadata.profile_generation > 0);
        assert_eq!(std::fs::read(config).unwrap(), original);
        assert!(!case.home.join(".backboard/keys.json").exists());
    }
}

#[test]
fn configured_rcli_missing_config_requires_owner_setup_without_materialization() {
    let mut case = ConfiguredCase::new(true, false);
    let result = case.custody.register_configured_rcli_profile(
        &case.host,
        &case.connection,
        &case.profile,
        &case.home,
    );
    assert_eq!(
        result.err().unwrap().0,
        "rcli_profile_configuration_required"
    );
    assert_eq!(std::fs::read_dir(&case.home).unwrap().count(), 0);
}

#[test]
fn configured_rcli_execution_uses_existing_guard_and_retained_home() {
    let mut case = ConfiguredCase::new(false, true);
    let metadata = case.register();
    let binding = case.binding(&metadata);
    let operation = RequestId::new("configured-turn").unwrap();
    let guard = case
        .custody
        .acquire_configured_rcli_use(&case.host, &binding, &case.profile, &operation, 1)
        .unwrap();
    assert_eq!(guard.profile_path().unwrap(), case.home);
    assert!(
        case.custody
            .acquire_configured_rcli_use(&case.host, &binding, &case.profile, &operation, 1,)
            .is_err()
    );
    guard
        .revalidate(
            &case.host,
            &binding,
            &operation,
            1,
            NativeProfileUsePurpose::Execution,
        )
        .unwrap();
    drop(guard);
    assert!(
        case.custody
            .acquire_configured_rcli_use(&case.host, &binding, &case.profile, &operation, 1,)
            .is_ok()
    );
}

#[test]
fn configured_rcli_registration_does_not_change_native_managed_admission() {
    let mut case = ConfiguredCase::new(false, true);
    assert_eq!(
        case.custody
            .prepare(&case.host, &case.connection)
            .err()
            .unwrap()
            .0,
        "native_profile_not_managed"
    );
    let metadata = case.register();
    let binding = case.binding(&metadata);
    assert!(
        case.custody
            .acquire_use(
                &case.host,
                &binding,
                &RequestId::new("native-route-rejected").unwrap(),
                1,
                NativeProfileUsePurpose::Authentication,
            )
            .is_err()
    );
    let mut native = case.connection.clone();
    native.auth = ModelAuthentication::NativeManaged {
        profile_ref: case.profile.clone(),
    };
    assert!(
        case.custody
            .register_configured_rcli_profile(&case.host, &native, &case.profile, &case.home,)
            .is_err()
    );
}

#[test]
fn configured_rcli_receipt_is_bound_to_exact_connection_account_and_profile() {
    let mut case = ConfiguredCase::new(true, true);
    let _metadata = case.register();
    for field in 0..7 {
        let mut changed = case.connection.clone();
        match field {
            0 => changed.id = reference("other-connection"),
            1 => changed.account_ref = reference("other-account"),
            2 => changed.provider = reference("other-provider"),
            3 => changed.model.id = "other-model".into(),
            4 => {
                changed.endpoint = ModelEndpoint::Http {
                    url: "http://127.0.0.1:9999/v1".into(),
                }
            }
            5 => {
                changed.auth = ModelAuthentication::ApiKey {
                    credential_ref: reference("other-key-reference"),
                }
            }
            6 => changed.auth = ModelAuthentication::None {},
            _ => unreachable!(),
        }
        assert!(
            case.custody
                .configured_rcli_profile(&case.host, &changed, &case.profile)
                .is_err()
        );
    }
    assert!(
        case.custody
            .configured_rcli_profile(&case.host, &case.connection, &reference("other-profile"),)
            .is_err()
    );
    assert!(
        case.custody
            .configured_rcli_profile(&case.host, &case.connection, &case.profile)
            .is_ok()
    );
}

#[test]
fn configured_rcli_retained_guard_detects_config_or_home_identity_changes() {
    for change in 0..3 {
        let mut case = ConfiguredCase::new(false, true);
        let metadata = case.register();
        let binding = case.binding(&metadata);
        let operation = RequestId::new("retained-configured-turn").unwrap();
        let guard = case
            .custody
            .acquire_configured_rcli_use(&case.host, &binding, &case.profile, &operation, 1)
            .unwrap();
        let config = case.home.join(".backboard/config.json");
        let original = std::fs::read(&config).unwrap();
        match change {
            0 => private_file(&config, b"{}"),
            1 => {
                let replacement = case.home.join(".backboard/replacement.json");
                private_file(&replacement, &original);
                std::fs::rename(replacement, &config).unwrap();
            }
            2 => {
                std::fs::rename(&case.home, case.root.path().join("original-home")).unwrap();
                private_dir(&case.home);
                private_dir(&case.home.join(".backboard"));
                private_file(&config, &original);
            }
            _ => unreachable!(),
        }
        assert!(guard.profile_path().is_err());
        assert!(
            case.custody
                .configured_rcli_profile(&case.host, &case.connection, &case.profile)
                .is_err()
        );
    }
}

#[test]
fn configured_rcli_same_physical_home_cannot_overlap_execution_through_an_alias() {
    let mut case = ConfiguredCase::new(false, true);
    let metadata = case.register();
    let binding = case.binding(&metadata);
    let operation = RequestId::new("configured-alias-turn").unwrap();
    let guard = case
        .custody
        .acquire_configured_rcli_use(&case.host, &binding, &case.profile, &operation, 1)
        .unwrap();
    let other_profile = reference("alias-profile");
    match case.custody.register_configured_rcli_profile(
        &case.host,
        &case.connection,
        &other_profile,
        &case.home,
    ) {
        Err(_) => {}
        Ok(alias) => {
            let alias_binding = case.binding(&alias);
            assert!(
                case.custody
                    .acquire_configured_rcli_use(
                        &case.host,
                        &alias_binding,
                        &other_profile,
                        &operation,
                        1,
                    )
                    .is_err()
            );
        }
    }
    guard.revalidate_retained().unwrap();
}

#[test]
fn configured_rcli_metadata_survives_recapture_without_reading_key_contents() {
    let mut case = ConfiguredCase::new(true, true);
    let sentinel = case.home.join(".backboard/keys.json");
    private_file(&sentinel, b"synthetic-unread-key-sentinel");
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o000)).unwrap();
    let before = std::fs::symlink_metadata(&sentinel).unwrap();
    let metadata = case.register();
    let database = case.root.path().join("state/engine.db");
    let mut recaptured =
        NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap()).unwrap();
    let actual = recaptured
        .configured_rcli_profile(&case.host, &case.connection, &case.profile)
        .unwrap();
    assert_eq!(actual.custody_id, metadata.custody_id);
    assert_eq!(actual.profile_config_sha256, metadata.profile_config_sha256);
    assert_eq!(actual.profile_generation, metadata.profile_generation);
    let after = std::fs::symlink_metadata(&sentinel).unwrap();
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.mode(), before.mode());
    assert_eq!(after.len(), before.len());
    assert_eq!(
        (after.atime(), after.atime_nsec()),
        (before.atime(), before.atime_nsec())
    );
    assert_eq!(
        (after.mtime(), after.mtime_nsec()),
        (before.mtime(), before.mtime_nsec())
    );
}

#[test]
fn configured_rcli_auth_none_requires_an_explicit_local_endpoint() {
    let mut case = ConfiguredCase::new(false, true);
    case.connection.endpoint = ModelEndpoint::Http {
        url: "https://remote.invalid/v1".into(),
    };
    let path = case.home.join(".backboard/config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["providers"][0]["baseUrl"] = "https://remote.invalid/v1".into();
    private_file(&path, &serde_json::to_vec(&config).unwrap());
    assert!(
        case.custody
            .register_configured_rcli_profile(
                &case.host,
                &case.connection,
                &case.profile,
                &case.home,
            )
            .is_err()
    );
    assert!(!case.home.join(".backboard/keys.json").exists());
}

#[path = "generated_configured_profile_tests.rs"]
mod generated_configured_profile_tests;
