use super::*;

#[test]
fn imp1035_generated_none_home_is_private_and_checked_by_existing_custody() {
    let mut case = ConfiguredCase::new(false, false);
    let metadata = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let guard = case
        .custody
        .acquire_configured_rcli_use(
            &case.host,
            &case.binding(&metadata),
            &case.profile,
            &RequestId::new("imp1035-turn").unwrap(),
            1,
        )
        .unwrap();
    let home = guard.profile_path().unwrap();
    assert_ne!(home, case.home);
    assert!(home.starts_with(case.root.path().join("state")));
    assert!(!home.starts_with(case.root.path().join("state/native-profiles")));
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let config = home.join(".backboard/config.json");
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        digest(&std::fs::read(config).unwrap()),
        metadata.profile_config_sha256
    );
    assert!(!home.join(".backboard/keys.json").exists());
    assert_eq!(metadata.profile_ref, case.profile);
}

#[test]
fn imp1035_generated_profile_replay_does_not_overwrite_or_advance_generation() {
    let mut case = ConfiguredCase::new(false, false);
    let first = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let second = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    assert_eq!(first.custody_id, second.custody_id);
    assert_eq!(first.profile_config_sha256, second.profile_config_sha256);
    assert_eq!(first.profile_generation, second.profile_generation);
}

#[test]
fn imp1035_generated_profile_configuration_obeys_execution_exclusion() {
    let mut case = ConfiguredCase::new(false, false);
    let metadata = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let guard = case
        .custody
        .acquire_configured_rcli_use(
            &case.host,
            &case.binding(&metadata),
            &case.profile,
            &RequestId::new("imp1035-exclusive-turn").unwrap(),
            1,
        )
        .unwrap();
    assert!(
        case.custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None,)
            .is_err()
    );
    assert!(guard.profile_path().is_ok());
}

#[test]
fn imp1035_generated_profile_detects_replaced_held_home() {
    let mut case = ConfiguredCase::new(false, false);
    let metadata = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let guard = case
        .custody
        .acquire_configured_rcli_use(
            &case.host,
            &case.binding(&metadata),
            &case.profile,
            &RequestId::new("imp1035-retained-turn").unwrap(),
            1,
        )
        .unwrap();
    let home = guard.profile_path().unwrap();
    std::fs::rename(&home, case.root.path().join("replaced-home")).unwrap();
    private_dir(&home);
    assert!(guard.profile_path().is_err());
}

#[test]
fn imp1035_two_accounts_get_distinct_physical_homes() {
    let mut case = ConfiguredCase::new(false, false);
    let first = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let mut second_connection = case.connection.clone();
    second_connection.id = reference("second-connection");
    second_connection.account_ref = reference("second-account");
    let second_profile = reference("second-profile");
    let second = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &second_connection, &second_profile, None)
        .unwrap();
    let first_guard = case
        .custody
        .acquire_configured_rcli_use(
            &case.host,
            &case.binding(&first),
            &case.profile,
            &RequestId::new("imp1035-first-turn").unwrap(),
            1,
        )
        .unwrap();
    let mut second_binding = case.binding(&second);
    second_binding.connection = second_connection;
    let second_guard = case
        .custody
        .acquire_configured_rcli_use(
            &case.host,
            &second_binding,
            &second_profile,
            &RequestId::new("imp1035-second-turn").unwrap(),
            1,
        )
        .unwrap();
    assert_ne!(
        first_guard.profile_path().unwrap(),
        second_guard.profile_path().unwrap()
    );
    assert_ne!(first.custody_id, second.custody_id);
}

#[test]
fn imp1035_api_key_configuration_requires_private_secret_before_creating_home() {
    let mut case = ConfiguredCase::new(true, false);
    assert!(
        case.custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None,)
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(case.root.path().join("state"))
            .unwrap()
            .count(),
        1
    );
}

/// The case's connection as a keyed OpenAI account saved before DEC-150 addendum 9a, under
/// the builtin provider id rcli keeps. The key reference is synthetic. Input: the case.
/// Output: the connection.
fn keyed_under_builtin(case: &ConfiguredCase) -> ModelConnection {
    ModelConnection {
        provider: reference("openai"),
        auth: ModelAuthentication::ApiKey {
            credential_ref: reference("synthetic-key-ref-legacy"),
        },
        ..case.connection.clone()
    }
}

/// Writes the SDK home account setup generated for a keyed connection before DEC-150
/// addendum 9a: the engine's config naming the builtin provider id, and no profile slot,
/// as a save refused at its profile step left it. Inputs: the case, the connection and
/// the model id the config names. Output: the config's path.
fn legacy_home(case: &ConfiguredCase, connection: &ModelConnection, model: &str) -> PathBuf {
    let homes = case.root.path().join("state/configured-sdk-homes");
    private_dir(&homes);
    let home = homes.join(digest(case.profile.as_str().as_bytes()));
    private_dir(&home);
    private_dir(&home.join(".backboard"));
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        panic!("a keyed RCLI connection has an HTTP endpoint");
    };
    let config = home.join(".backboard/config.json");
    let bytes = serde_json::to_vec(&serde_json::json!({
        "model": { "provider": connection.provider.as_str(), "model": model },
        "providers": [{
            "id": connection.provider.as_str(), "name": "Configured provider",
            "protocol": connection.protocol.as_str(), "baseUrl": url,
            "auth": { "type": "apiKey" }
        }]
    }))
    .unwrap();
    private_file(&config, &bytes);
    config
}

fn synthetic_secret() -> crate::engine::owner::AccountSecret {
    crate::engine::owner::AccountSecret::new(b"synthetic-key-not-a-real-key".to_vec()).unwrap()
}

#[test]
fn a_keyed_account_saved_before_the_builtin_id_fix_sets_up_again_on_its_hashed_provider_id() {
    let mut case = ConfiguredCase::new(true, false);
    let keyed = keyed_under_builtin(&case);
    let config = legacy_home(&case, &keyed, &keyed.model.id);
    let metadata = case
        .custody
        .prepare_configured_rcli_profile(
            &case.host,
            &keyed,
            &case.profile,
            Some(&synthetic_secret()),
        )
        .unwrap();
    let bytes = std::fs::read(&config).unwrap();
    assert_eq!(digest(&bytes), metadata.profile_config_sha256);
    let hashed = crate::rcli_host::configured_sdk_connection(&keyed)
        .unwrap()
        .provider;
    assert_ne!(hashed, keyed.provider);
    let written: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(written["model"]["provider"], hashed.as_str());
    assert_eq!(written["providers"][0]["id"], hashed.as_str());
    assert!(!config.with_file_name("keys.json").exists());
}

#[test]
fn a_slotless_home_that_is_not_the_engine_s_old_config_for_the_account_stays_refused() {
    let mut case = ConfiguredCase::new(true, false);
    let keyed = keyed_under_builtin(&case);
    let config = legacy_home(&case, &keyed, "another-model");
    let before = std::fs::read(&config).unwrap();
    let refused = case
        .custody
        .prepare_configured_rcli_profile(
            &case.host,
            &keyed,
            &case.profile,
            Some(&synthetic_secret()),
        )
        .unwrap_err();
    assert_eq!(refused.0, "native_profile_custody_unsafe");
    assert_eq!(std::fs::read(&config).unwrap(), before);
}

/// KY-M1 option A order: master start repairs a pre-fix keyed home and registers it under
/// the session's target profile first; the engine's key setup then sets the account up
/// under its own reference on the same home, the key file is written, and the target
/// profile the master turn holds still opens for execution.
#[test]
fn a_repaired_keyed_home_held_by_the_master_sets_up_again_and_the_master_keeps_its_profile() {
    let mut case = ConfiguredCase::new(true, false);
    let keyed = keyed_under_builtin(&case);
    let config = legacy_home(&case, &keyed, &keyed.model.id);
    case.custody
        .repair_legacy_configured_home(&case.host, &keyed, &case.profile)
        .unwrap();
    let home = config.parent().unwrap().parent().unwrap().to_path_buf();
    let target = reference("synthetic-master-target");
    let master = case
        .custody
        .register_configured_rcli_profile(&case.host, &keyed, &target, &home)
        .unwrap();
    let account = case
        .custody
        .prepare_configured_rcli_profile(
            &case.host,
            &keyed,
            &case.profile,
            Some(&synthetic_secret()),
        )
        .unwrap();
    assert_eq!(account.profile_config_sha256, master.profile_config_sha256);
    private_file(&config.with_file_name("keys.json"), b"{}");
    let binding = NativeProfileBinding {
        connection: keyed.clone(),
        ..case.binding(&master)
    };
    case.custody
        .acquire_configured_rcli_use(
            &case.host,
            &binding,
            &target,
            &RequestId::new("ky-m1-master-turn").unwrap(),
            master.profile_generation,
        )
        .unwrap();
}

/// RV-DS241-51: a master turn holds the generated home through its own target profile
/// while the owner saves a changed connection for the account. The save is refused before
/// it writes anything, so the config the master runs on stays the one it checked.
#[test]
fn a_changed_account_save_never_rewrites_the_config_a_running_master_uses() {
    let mut case = ConfiguredCase::new(false, false);
    case.custody
        .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
        .unwrap();
    let home = case.custody.generated_configured_home(&case.profile);
    let config = home.join(".backboard/config.json");
    let before = std::fs::read(&config).unwrap();
    let target = reference("synthetic-master-target");
    let master = case
        .custody
        .register_configured_rcli_profile(&case.host, &case.connection, &target, &home)
        .unwrap();
    let guard = case
        .custody
        .acquire_configured_rcli_use(
            &case.host,
            &case.binding(&master),
            &target,
            &RequestId::new("rv-ds241-51-master-turn").unwrap(),
            master.profile_generation,
        )
        .unwrap();
    let mut changed = case.connection.clone();
    changed.model.id = "another-local-model".into();
    let refused = case
        .custody
        .prepare_configured_rcli_profile(&case.host, &changed, &case.profile, None)
        .unwrap_err();
    assert_eq!(refused.0, "native_profile_in_use");
    assert_eq!(std::fs::read(&config).unwrap(), before);
    guard.revalidate_retained().unwrap();
    drop(guard);
    case.custody
        .prepare_configured_rcli_profile(&case.host, &changed, &case.profile, None)
        .unwrap();
}

/// RV-DS241-50: a save of a connection the custody refuses (a keyless endpoint off this
/// machine, an endpoint with a query) leaves the account's config as it was, so saving
/// the old connection again keeps the same config and its turns still start.
#[test]
fn a_save_the_custody_refuses_leaves_the_account_on_its_old_connection() {
    for url in [
        "http://192.0.2.10:11434/v1",
        "http://127.0.0.1:11434/v1?probe=1",
    ] {
        let mut case = ConfiguredCase::new(false, false);
        let saved = case
            .custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
            .unwrap();
        let config = case
            .custody
            .generated_configured_home(&case.profile)
            .join(".backboard/config.json");
        let before = std::fs::read(&config).unwrap();
        let mut refused = case.connection.clone();
        refused.endpoint = ModelEndpoint::Http { url: url.into() };
        assert!(
            case.custody
                .prepare_configured_rcli_profile(&case.host, &refused, &case.profile, None)
                .is_err()
        );
        assert_eq!(std::fs::read(&config).unwrap(), before, "{url}");
        let again = case
            .custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
            .unwrap_or_else(|error| panic!("{url}: the old connection is stranded: {}", error.0));
        assert_eq!(again.profile_config_sha256, saved.profile_config_sha256);
        case.custody
            .acquire_configured_rcli_use(
                &case.host,
                &case.binding(&again),
                &case.profile,
                &RequestId::new("rv-ds241-50-turn").unwrap(),
                again.profile_generation,
            )
            .unwrap();
    }
}

/// RV-DS241-50: a save of a changed connection stopped at any one of its steps, the way a
/// full disk or a crash stops it, leaves an account the old connection still saves on and
/// opens for a turn. Output: the row also shows some steps stop it and a later one ends it.
#[test]
fn a_save_stopped_at_any_step_never_strands_the_account_on_a_config_it_does_not_pin() {
    let mut stopped = 0;
    let mut finished = false;
    for point in 1..=64 {
        let mut case = ConfiguredCase::new(false, false);
        case.custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
            .unwrap();
        let mut changed = case.connection.clone();
        changed.model.id = "another-local-model".into();
        staging::INTERRUPT.set(point);
        let saved =
            case.custody
                .prepare_configured_rcli_profile(&case.host, &changed, &case.profile, None);
        staging::INTERRUPT.set(0);
        if saved.is_ok() {
            finished = true;
            break;
        }
        stopped += 1;
        let back = case
            .custody
            .prepare_configured_rcli_profile(&case.host, &case.connection, &case.profile, None)
            .unwrap_or_else(|error| {
                panic!(
                    "a save stopped at step {point} strands the account: {}",
                    error.0
                )
            });
        case.custody
            .acquire_configured_rcli_use(
                &case.host,
                &case.binding(&back),
                &case.profile,
                &RequestId::new("rv-ds241-50-stopped-turn").unwrap(),
                back.profile_generation,
            )
            .unwrap_or_else(|error| panic!("step {point}: no turn opens: {}", error.0));
    }
    assert!(finished);
    assert!(stopped > 0);
}
