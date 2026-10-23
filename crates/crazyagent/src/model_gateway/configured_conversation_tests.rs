use super::*;
use crate::config::{ModelAuthentication, ModelEndpoint};
use crate::protocol::Cursor;
use crate::storage::{
    ConfiguredInitialConversationFixture as Fixture, configured_initial_conversation_fixture,
};
use std::os::unix::fs::PermissionsExt;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn begin(case: &mut Fixture, now_ms: u64) -> Result<AttemptStart, EngineError> {
    let current = case.config.clone();
    begin_with_current(case, &current, now_ms)
}

fn begin_with_current(
    case: &mut Fixture,
    current: &EffectiveConfig,
    now_ms: u64,
) -> Result<AttemptStart, EngineError> {
    let tx = case.db.transaction().unwrap();
    let result = begin_configured_initial_conversation_attempt(
        &tx,
        &case.config,
        current,
        &case.request,
        ConfiguredInitialConversationAuth {
            proof: &case.proof,
            binding: &case.binding,
            generation: case.generation,
        },
        now_ms,
    )?;
    tx.commit().unwrap();
    Ok(result)
}

fn counts(case: &Fixture) -> (i64, i64) {
    case.db
        .query_row(
            "SELECT (SELECT count(*) FROM model_gateway_attempts), (SELECT count(*) FROM model_gateway_observations)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn reject(case: &mut Fixture, now_ms: u64, expected: Option<&str>) {
    let before = counts(case);
    let failure = begin(case, now_ms).unwrap_err();
    if let Some(expected) = expected {
        assert_eq!(failure.0, expected);
    }
    assert_eq!(counts(case), before);
}

fn record(case: &mut Fixture, kind: ObservationKind, observed_at_ms: u64, valid_until_ms: u64) {
    let tx = case.db.transaction().unwrap();
    observe(
        &tx,
        &case.config,
        &Observation {
            id: RequestId::new("configured-readiness-observation").unwrap(),
            connection: case.binding.connection.clone(),
            sequence: 1,
            observed_at_ms,
            valid_until_ms,
            kind,
        },
    )
    .unwrap();
    tx.commit().unwrap();
}

#[tokio::test]
async fn configured_receipt_admits_one_exact_initial_attempt_without_ready() {
    let mut case = configured_initial_conversation_fixture().await;
    assert_eq!(counts(&case), (0, 0));
    let before = connection_state(&case.db, &case.proof.connection, 100).unwrap();
    assert_eq!(before.health, Health::Unknown);
    assert_eq!(before.quota, Quota::Unknown);
    let first = begin(&mut case, 100).unwrap();
    assert!(!first.replayed);
    assert_eq!(first.attempt.request, case.request);
    assert_eq!(first.attempt.connection, case.proof.connection);
    assert_eq!(first.attempt.config_id, case.config.id);
    assert!(
        first
            .attempt
            .reasons
            .iter()
            .any(|reason| reason == "initial_conversation_readiness_unknown")
    );
    let replay = begin(&mut case, 101).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt, first.attempt);
    assert_eq!(counts(&case), (1, 0));
    assert_eq!(
        connection_state(&case.db, &case.proof.connection, 101).unwrap(),
        before
    );
    assert_eq!(case.request.task_id, case.session.bootstrap_task_id);
    case.close().await;
}

#[tokio::test]
async fn configured_receipt_does_not_relax_ordinary_admission() {
    let mut case = configured_initial_conversation_fixture().await;
    let tx = case.db.transaction().unwrap();
    assert!(begin_attempt(&tx, &case.config, &case.config, &case.request, 100).is_err());
    drop(tx);
    assert_eq!(counts(&case), (0, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_receipt_fields_are_reloaded_instead_of_trusting_constructed_proof() {
    let mut case = configured_initial_conversation_fixture().await;
    let genuine = case.proof.clone();
    for field in 0..13 {
        case.proof = genuine.clone();
        match field {
            0 => case.proof.registration_id = RequestId::new("unregistered-receipt").unwrap(),
            1 => case.proof.generation = genuine.generation.checked_add(1).unwrap(),
            2 => case.proof.database_instance = "0".repeat(64),
            3 => case.proof.engine_epoch = "0".repeat(64),
            4 => case.proof.config_id = ConfigId::new("0".repeat(64)).unwrap(),
            5 => case.proof.source_hash = "0".repeat(64),
            6 => case.proof.at_revision = Cursor::new(u64::MAX),
            7 => case.proof.profile_ref = reference("unregistered-profile"),
            8 => case.proof.custody_id = "unregistered-custody".into(),
            9 => case.proof.profile_config_sha256 = "0".repeat(64),
            10 => case.proof.connection.model.id = "different-model".into(),
            11 => case.proof.connection.account_ref = reference("different-account"),
            12 => case.proof.connection.protocol = reference("different-protocol"),
            _ => unreachable!(),
        }
        reject(&mut case, 100, None);
    }
    assert_eq!(counts(&case), (0, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_binding_requires_full_connection_current_scope_and_generation() {
    let mut case = configured_initial_conversation_fixture().await;
    let binding = case.binding.clone();
    for field in 0..16 {
        case.binding = binding.clone();
        match field {
            0 => case.binding.database_instance = "0".repeat(64),
            1 => case.binding.engine_epoch = "0".repeat(64),
            2 => case.binding.config_id = ConfigId::new("0".repeat(64)).unwrap(),
            3 => case.binding.source_hash = "0".repeat(64),
            4 => case.binding.at_revision = Cursor::new(u64::MAX),
            5 => case.binding.custody_id = "different-custody".into(),
            6 => case.binding.connection.id = reference("different-connection"),
            7 => case.binding.connection.provider = reference("different-provider"),
            8 => case.binding.connection.harness = reference("codex"),
            9 => case.binding.connection.protocol = reference("native"),
            10 => case.binding.connection.account_ref = reference("different-account"),
            11 => case.binding.connection.model.id = "different-model".into(),
            12 => case.binding.connection.model.author = reference("different-author"),
            13 => {
                case.binding.connection.auth = ModelAuthentication::NativeManaged {
                    profile_ref: reference("native-profile"),
                };
            }
            14 => {
                case.binding.connection.endpoint = ModelEndpoint::Http {
                    url: "http://127.0.0.1:11435/v1".into(),
                };
            }
            15 => {
                case.binding.connection.auth = ModelAuthentication::ApiKey {
                    credential_ref: reference("unregistered-key-reference"),
                };
            }
            _ => unreachable!(),
        }
        reject(&mut case, 100, None);
    }
    case.binding = binding;
    for generation in [0, case.proof.generation.checked_add(1).unwrap()] {
        case.generation = generation;
        reject(&mut case, 100, None);
    }
    case.generation = case.proof.generation;
    assert_eq!(counts(&case), (0, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_initial_scope_excludes_workers_tools_retry_and_foreign_tasks() {
    let mut case = configured_initial_conversation_fixture().await;
    let request = case.request.clone();
    for field in 0..6 {
        case.request = request.clone();
        match field {
            0 => case.request.role = Role::Worker,
            1 => case.request.route.required_capabilities.push("coding".into()),
            2 => case.request.previous_attempt = Some(RequestId::new("previous-attempt").unwrap()),
            3 => case.request.task_id = TaskId::new("foreign-first-task").unwrap(),
            4 => case.request.target_profile_ref = reference("foreign-profile"),
            5 => case.request.route.account_ref = reference("foreign-account"),
            _ => unreachable!(),
        }
        reject(&mut case, 100, None);
    }
    assert_eq!(counts(&case), (0, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_initial_shared_eligibility_preserves_strict_quota_for_real_unknown_state() {
    let case = configured_initial_conversation_fixture().await;
    let state = connection_state(&case.db, &case.proof.connection, 100).unwrap();
    assert_eq!(state.health, Health::Unknown);
    assert_eq!(state.quota, Quota::Unknown);
    assert_eq!(
        initial_conversation_eligible(
            &state,
            &case.request.route,
            ProviderQuotaPolicy::Strict,
            100,
        )
        .unwrap_err()
        .0,
        "gateway_quota_unknown"
    );
    assert_eq!(counts(&case), (0, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_initial_preserves_explicit_negative_and_stale_observations() {
    for (kind, observed, until, now, expected) in [
        (
            ObservationKind::InvalidAuthentication,
            100,
            1000,
            100,
            "gateway_invalid_authentication",
        ),
        (
            ObservationKind::RateLimited {
                retry_at_ms: Some(500),
            },
            100,
            1000,
            100,
            "gateway_rate_limited",
        ),
        (
            ObservationKind::TransientFailure {
                retry_at_ms: Some(500),
            },
            100,
            1000,
            100,
            "gateway_transient_failure",
        ),
        (
            ObservationKind::QuotaExhausted { retry_at_ms: None },
            100,
            1000,
            100,
            "gateway_quota_exhausted",
        ),
        (
            ObservationKind::Unknown,
            100,
            200,
            200,
            "gateway_initial_readiness_stale",
        ),
        (
            ObservationKind::Unknown,
            200,
            300,
            100,
            "gateway_initial_readiness_stale",
        ),
    ] {
        let mut case = configured_initial_conversation_fixture().await;
        record(&mut case, kind, observed, until);
        reject(&mut case, now, Some(expected));
        assert_eq!(counts(&case), (0, 1));
        case.close().await;
    }
}

#[tokio::test]
async fn configured_initial_cannot_start_a_second_attempt_or_retry_after_local_completion() {
    let mut case = configured_initial_conversation_fixture().await;
    let first = begin(&mut case, 100).unwrap();
    let tx = case.db.transaction().unwrap();
    finish_attempt_without_observation(
        &tx,
        &first.attempt.request.id,
        &AttemptCompletion {
            outcome: AttemptOutcome::Failed(FailureKind::Unknown),
            effects: Effects::None,
            finished_at_ms: 101,
            retry_at_ms: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    case.request.id = RequestId::new("configured-second-request").unwrap();
    reject(&mut case, 102, None);
    case.request.previous_attempt = Some(first.attempt.request.id);
    reject(&mut case, 102, None);
    assert_eq!(counts(&case), (1, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_receipt_is_revalidated_before_same_id_replay() {
    let mut case = configured_initial_conversation_fixture().await;
    begin(&mut case, 100).unwrap();
    case.proof.registration_id = RequestId::new("unregistered-replay-receipt").unwrap();
    reject(&mut case, 101, None);
    assert_eq!(counts(&case), (1, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_initial_requires_exact_current_config_and_full_connection() {
    let mut case = configured_initial_conversation_fixture().await;
    let mut current = case.config.clone();
    current.id = ConfigId::new("0".repeat(64)).unwrap();
    assert!(begin_with_current(&mut case, &current, 100).is_err());
    current = case.config.clone();
    current.settings.model_access.as_mut().unwrap().connections[0]
        .model
        .id = "changed-live-model".into();
    assert!(begin_with_current(&mut case, &current, 100).is_err());
    assert_eq!(counts(&case), (0, 0));
    case.close().await;
}

#[tokio::test]
async fn configured_gateway_revalidates_actual_source_and_physical_profile_identity() {
    for replace_profile in [false, true] {
        let mut case = configured_initial_conversation_fixture().await;
        if replace_profile {
            let config = case.profile_home.join(".backboard/config.json");
            let bytes = std::fs::read(&config).unwrap();
            std::fs::rename(&config, config.with_extension("retained")).unwrap();
            std::fs::write(&config, bytes).unwrap();
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            let mut bytes = std::fs::read(&case.global_path).unwrap();
            bytes.push(b'\n');
            std::fs::write(&case.global_path, bytes).unwrap();
        }
        reject(&mut case, 100, None);
        assert_eq!(counts(&case), (0, 0));
        case.close().await;
    }
}
