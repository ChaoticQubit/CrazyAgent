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
