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
