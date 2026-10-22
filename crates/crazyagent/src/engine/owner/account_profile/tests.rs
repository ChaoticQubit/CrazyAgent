use super::*;
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn configured_connection(api_key: bool) -> ModelConnection {
    ModelConnection {
        id: reference("imp1035-connection"),
        provider: reference("imp1035-provider"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference("imp1035-account"),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        },
        model: ModelReference {
            id: "imp1035-model".into(),
            author: reference("imp1035-author"),
        },
        auth: if api_key {
            ModelAuthentication::ApiKey {
                credential_ref: reference("imp1035-key-reference"),
            }
        } else {
            ModelAuthentication::None {}
        },
    }
}

fn scope(connection: &ModelConnection) -> NativeAuthScope {
    NativeAuthScope {
        expected_database_instance: "a".repeat(64),
        connection: connection.id.clone(),
        expected_config_id: ConfigId::new("b".repeat(64)).unwrap(),
        expected_source_hash: "c".repeat(64),
    }
}

#[test]
fn imp1035_secret_bounds_and_debug_are_explicit() {
    for length in [1, 8192] {
        let secret = AccountSecret::new(vec![b'x'; length]).unwrap();
        assert_eq!(format!("{secret:?}"), "AccountSecret([REDACTED])");
    }
    for bytes in [Vec::new(), vec![b'x'; 8193], b"a\0b".to_vec(),
        b"a\nb".to_vec(), b"a\rb".to_vec()]
    {
        assert_eq!(AccountSecret::new(bytes).err().unwrap().0, "account_secret_invalid");
    }
}

#[test]
fn imp1035_scope_admits_actual_api_key_and_none_connections() {
    for api_key in [false, true] {
        let connection = configured_connection(api_key);
        let expected = scope(&connection);
        authorize_account_profile_scope(
            &expected,
            &expected.expected_database_instance,
            &expected.expected_config_id,
            &expected.expected_source_hash,
            &connection,
        ).unwrap();
    }
}

#[test]
fn imp1035_scope_rejects_each_stale_actor_pin() {
    let connection = configured_connection(true);
    let current = scope(&connection);
    for field in 0..4 {
        let mut submitted = scope(&connection);
        match field {
            0 => submitted.expected_database_instance = "d".repeat(64),
            1 => submitted.expected_config_id = ConfigId::new("d".repeat(64)).unwrap(),
            2 => submitted.expected_source_hash = "d".repeat(64),
            3 => submitted.connection = reference("different-connection"),
            _ => unreachable!(),
        }
        assert!(authorize_account_profile_scope(
            &submitted,
            &current.expected_database_instance,
            &current.expected_config_id,
            &current.expected_source_hash,
            &connection,
        ).is_err());
    }
}

#[test]
fn imp1035_configuration_does_not_use_native_managed_admission() {
    let mut connection = configured_connection(true);
    connection.auth = ModelAuthentication::NativeManaged {
        profile_ref: reference("native-profile"),
    };
    let expected = scope(&connection);
    assert_eq!(authorize_account_profile_scope(
        &expected,
        &expected.expected_database_instance,
        &expected.expected_config_id,
        &expected.expected_source_hash,
        &connection,
    ).err().unwrap().0, "account_profile_auth_unsupported");
}

#[test]
fn imp1035_none_configuration_rejects_hosted_endpoint() {
    let mut connection = configured_connection(false);
    connection.endpoint = ModelEndpoint::Http {
        url: "https://example.invalid/v1".into(),
    };
    let expected = scope(&connection);
    assert!(authorize_account_profile_scope(
        &expected,
        &expected.expected_database_instance,
        &expected.expected_config_id,
        &expected.expected_source_hash,
        &connection,
    ).is_err());
}

#[test]
fn imp1035_receipt_is_configured_only_and_contains_none_profile_join() {
    let connection = configured_connection(false);
    let request = AccountProfileRequest {
        request_id: RequestId::new("imp1035-configure").unwrap(),
        scope: scope(&connection),
    };
    let reply = AccountProfileReply {
        request_id: request.request_id,
        database_instance: "a".repeat(64),
        engine_epoch: "d".repeat(64),
        outcome: AccountProfileOutcome::ConfiguredOnly {
            receipt: AccountProfileReceipt {
                config_id: request.scope.expected_config_id,
                source_hash: request.scope.expected_source_hash,
                connection,
                profile_ref: reference("owner-generated-profile"),
                custody_id: "e".repeat(64),
                profile_config_sha256: "f".repeat(64),
                profile_generation: 1,
            },
            historical_replay: true,
        },
    };
    let AccountProfileOutcome::ConfiguredOnly { receipt, historical_replay } = reply.outcome
    else { panic!("configured-only receipt required") };
    assert!(historical_replay);
    assert_eq!(receipt.profile_ref, reference("owner-generated-profile"));
    assert!(matches!(receipt.connection.auth, ModelAuthentication::None {}));
    assert_eq!(receipt.profile_generation, 1);
}
