use super::*;
use crate::config::ModelEndpoint;
use crate::engine::owner::{
    AccountProfileOutcome, AccountProfileRequest, AccountSecret, NativeAuthScope,
};

fn request(basis: &Basis, id: &str) -> AccountProfileRequest {
    AccountProfileRequest {
        request_id: RequestId::new(id).unwrap(),
        scope: NativeAuthScope {
            expected_database_instance: basis.database_instance.clone(),
            connection: basis.connection.id.clone(),
            expected_config_id: basis.config_id.clone(),
            expected_source_hash: basis.source_hash.clone(),
        },
    }
}

async fn configured_basis(context: &ProfileCase, api_key: bool) -> Basis {
    let mut connection = context.basis("connection-z").await.connection;
    connection.harness = reference("rcli");
    connection.protocol = reference("openai-chat");
    connection.endpoint = ModelEndpoint::Http {
        url: "http://127.0.0.1:11434/v1".into(),
    };
    connection.auth = if api_key {
        ModelAuthentication::ApiKey {
            credential_ref: reference("synthetic-key-reference"),
        }
    } else {
        ModelAuthentication::None {}
    };
    context.replace(connection).await;
    context.basis("connection-z").await
}

fn assert_secret_absent(root: &Path, secret: &[u8]) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            assert_secret_absent(&entry.path(), secret);
        } else if kind.is_file() {
            assert!(entry.metadata().unwrap().len() <= 8 * 1024 * 1024);
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(!bytes.windows(secret.len()).any(|window| window == secret));
        }
    }
}

#[tokio::test]
async fn imp1035_private_capability_read_uses_actual_actor_api_key_and_none_connections() {
    run_case(true, |context| async move {
        let (client, _) =
            OwnerClient::discover(&context.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        for api_key in [false, true] {
            let basis = configured_basis(&context, api_key).await;
            let reply = client
                .read_account_auth_capabilities(request(
                    &basis,
                    if api_key {
                        "imp1035-api-capability"
                    } else {
                        "imp1035-none-capability"
                    },
                ))
                .await
                .unwrap();
            assert_eq!(reply.database_instance, basis.database_instance);
            assert_eq!(reply.engine_epoch, basis.engine_epoch);
            assert_eq!(reply.config_id, basis.config_id);
            assert_eq!(reply.source_hash, basis.source_hash);
            assert_eq!(reply.connection, basis.connection);
        }
    })
    .await;
}

#[tokio::test]
async fn imp1035_private_secret_request_rejects_stale_actor_pins_without_persistence() {
    run_case(true, |context| async move {
        let basis = configured_basis(&context, true).await;
        let (client, _) =
            OwnerClient::discover(&context.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let marker = b"IMP1035_SYNTHETIC_UNIT_SECRET_NEVER_PERSIST";
        for field in 0..4 {
            let mut submitted = request(&basis, &format!("imp1035-stale-{field}"));
            match field {
                0 => submitted.scope.expected_database_instance = "a".repeat(64),
                1 => submitted.scope.expected_config_id = ConfigId::new("b".repeat(64)).unwrap(),
                2 => submitted.scope.expected_source_hash = "c".repeat(64),
                3 => submitted.scope.connection = reference("unknown-connection"),
                _ => unreachable!(),
            }
            let error = client
                .configure_account_profile(
                    submitted,
                    Some(AccountSecret::new(marker.to_vec()).unwrap()),
                )
                .await
                .err()
                .unwrap();
            if field == 0 {
                assert_eq!(error.0, "native_auth_database_mismatch");
            }
            assert_ne!(error.0, "account_profile_auth_unsupported");
            assert_ne!(error.0, "rcli_private_key_writer_unavailable");
            assert_secret_absent(&context.state, marker);
            let current = client
                .read_account_auth_capabilities(request(
                    &basis,
                    &format!("imp1035-after-stale-{field}"),
                ))
                .await
                .unwrap();
            assert_eq!(current.connection, basis.connection);
        }
    })
    .await;
}

#[tokio::test]
async fn imp1035_real_none_configuration_reconciles_receipt_without_replaying_write() {
    run_case(true, |context| async move {
        let basis = configured_basis(&context, false).await;
        let (client, _) =
            OwnerClient::discover(&context.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let first = client
            .configure_account_profile(request(&basis, "imp1035-none-configure"), None)
            .await
            .unwrap();
        let AccountProfileOutcome::ConfiguredOnly {
            receipt: first,
            historical_replay,
        } = first.outcome
        else {
            panic!("actual configured-only receipt required")
        };
        assert!(!historical_replay);
        assert_eq!(first.connection, basis.connection);
        assert!(matches!(
            first.connection.auth,
            ModelAuthentication::None {}
        ));
        assert!(!first.profile_ref.as_str().is_empty());
        assert_eq!(first.profile_config_sha256.len(), 64);
        assert!(first.profile_generation > 0);
        let reconciled = client
            .read_account_profile(request(&basis, "imp1035-none-configure"))
            .await
            .unwrap();
        let AccountProfileOutcome::ConfiguredOnly {
            receipt,
            historical_replay,
        } = reconciled.outcome
        else {
            panic!("recorded configured-only receipt required")
        };
        assert!(historical_replay);
        assert_eq!(receipt.connection, first.connection);
        assert_eq!(receipt.profile_ref, first.profile_ref);
        assert_eq!(receipt.custody_id, first.custody_id);
        assert_eq!(receipt.profile_config_sha256, first.profile_config_sha256);
        assert_eq!(receipt.profile_generation, first.profile_generation);
    })
    .await;
}
