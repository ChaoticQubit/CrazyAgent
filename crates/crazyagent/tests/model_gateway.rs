use crazyagent::config::{EffectiveConfig, ModelAuthentication, resolve_config};
use crazyagent::coordination::Role;
use crazyagent::model_gateway::{
    AttemptCompletion, AttemptOutcome, AttemptRequest, Effects, FailureKind, Health, Observation,
    ObservationKind, Quota, begin_attempt, connection_state, finish_attempt, initialize, observe,
    read_attempt,
};
use crazyagent::protocol::{CredentialRef, ProjectId, RequestId, TaskId};
use crazyagent::routing::{ResourceBudget, RouteRequest};
use rusqlite::Connection;

const CATALOG: &str = r#"
allowed_destinations = ["sandbox-profile"]
[routing]
approved_accounts = ["account-a", "account-b"]
[model_access]
accounts = ["account-a", "account-b"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "coding-model", author = "model-author" }
auth = { method = "native_managed", profile_ref = "auth-profile-a" }
[[model_access.connections]]
id = "connection-b"
provider = "provider-b"
harness = "codex"
protocol = "native"
account_ref = "account-b"
endpoint = { kind = "native" }
model = { id = "coding-model", author = "model-author" }
auth = { method = "native_managed", profile_ref = "auth-profile-b" }
[[model_access.pools]]
id = "worker-pool"
members = ["connection-b", "connection-a"]
projects = ["project-a"]
roles = ["worker"]
fallback_allowed = true
"#;

fn load_config(text: &str) -> EffectiveConfig {
    let directory = tempfile::tempdir().unwrap();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, text).unwrap();
    resolve_config(&global, &directory.path().join("project.toml")).unwrap()
}

fn request(id: &str, previous: Option<&str>) -> AttemptRequest {
    AttemptRequest {
        id: RequestId::new(id).unwrap(),
        task_id: TaskId::new("task-a").unwrap(),
        project_id: ProjectId::new("project-a").unwrap(),
        role: Role::Worker,
        pool_ref: CredentialRef::new("worker-pool").unwrap(),
        target_profile_ref: CredentialRef::new("sandbox-profile").unwrap(),
        route: RouteRequest {
            preferred_target: Some(CredentialRef::new("sandbox-profile").unwrap()),
            required_target: None,
            required_capabilities: vec!["coding".into()],
            account_ref: CredentialRef::new("account-a").unwrap(),
            budget_ref: CredentialRef::new("budget-a").unwrap(),
            resources: ResourceBudget::unit(),
        },
        previous_attempt: previous.map(|id| RequestId::new(id).unwrap()),
    }
}

fn observation(config: &EffectiveConfig, index: usize, sequence: u64) -> Observation {
    Observation {
        id: RequestId::new(format!("observation-{index}-{sequence}")).unwrap(),
        connection: config.settings.model_access.as_ref().unwrap().connections[index].clone(),
        sequence,
        observed_at_ms: 100,
        valid_until_ms: 10000,
        kind: ObservationKind::Ready {
            remaining: Some(config.settings.routing.task_capacity.clone()),
            capabilities: vec!["coding".into()],
        },
    }
}

fn ready(database: &mut Connection, config: &EffectiveConfig) {
    let tx = database.transaction().unwrap();
    for index in 0..2 {
        observe(&tx, config, &observation(config, index, 1)).unwrap();
    }
    tx.commit().unwrap();
}

fn completion(failure: FailureKind, effects: Effects) -> AttemptCompletion {
    AttemptCompletion {
        outcome: AttemptOutcome::Failed(failure),
        effects,
        finished_at_ms: 200,
        retry_at_ms: Some(500),
    }
}

#[test]
fn unknown_and_expired_observations_never_invent_capacity_or_authentication() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let connection = &config.settings.model_access.as_ref().unwrap().connections[0];
    let state = connection_state(&database, connection, 100).unwrap();
    assert_eq!(state.health, Health::Unknown);
    assert_eq!(state.quota, Quota::Unknown);
    let tx = database.transaction().unwrap();
    assert_eq!(
        begin_attempt(&tx, &config, &config, &request("first", None), 100)
            .unwrap_err()
            .0,
        "gateway_health_unknown"
    );
    let mut unknown = observation(&config, 0, 1);
    unknown.kind = ObservationKind::Ready {
        remaining: None,
        capabilities: vec!["coding".into()],
    };
    observe(&tx, &config, &unknown).unwrap();
    assert_eq!(
        begin_attempt(&tx, &config, &config, &request("first", None), 100)
            .unwrap_err()
            .0,
        "gateway_quota_unknown"
    );
    observe(&tx, &config, &observation(&config, 0, 2)).unwrap();
    assert!(begin_attempt(&tx, &config, &config, &request("first", None), 9999).is_ok());
    assert_eq!(
        connection_state(&tx, connection, 10000).unwrap().health,
        Health::Unknown
    );
    assert_eq!(
        connection_state(&tx, connection, 99).unwrap().health,
        Health::Unknown
    );
}

#[test]
fn observed_failure_classes_and_cooldowns_remain_distinct() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    for (sequence, kind, health, quota) in [
        (
            1,
            ObservationKind::QuotaExhausted {
                retry_at_ms: Some(500),
            },
            Health::Ready,
            Quota::Exhausted,
        ),
        (
            2,
            ObservationKind::RateLimited {
                retry_at_ms: Some(500),
            },
            Health::RateLimited,
            Quota::Unknown,
        ),
        (
            3,
            ObservationKind::InvalidAuthentication,
            Health::InvalidAuthentication,
            Quota::Unknown,
        ),
        (
            4,
            ObservationKind::TransientFailure { retry_at_ms: None },
            Health::TransientFailure,
            Quota::Unknown,
        ),
        (5, ObservationKind::Unknown, Health::Unknown, Quota::Unknown),
    ] {
        let tx = database.transaction().unwrap();
        let mut event = observation(&config, 0, sequence);
        event.kind = kind;
        observe(&tx, &config, &event).unwrap();
        let state = connection_state(&tx, &event.connection, 200).unwrap();
        assert_eq!(state.health, health);
        assert_eq!(state.quota, quota);
        if sequence <= 2 {
            assert_eq!(state.cooldown_until_ms, Some(500));
            assert_eq!(
                connection_state(&tx, &event.connection, 500)
                    .unwrap()
                    .cooldown_until_ms,
                None
            );
        }
        tx.commit().unwrap();
    }
}

#[test]
fn selection_pins_distinct_profile_account_and_connection_across_restart() {
    let config = load_config(CATALOG);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.sqlite");
    let mut database = Connection::open(&path).unwrap();
    initialize(&database).unwrap();
    ready(&mut database, &config);
    let tx = database.transaction().unwrap();
    let selected = begin_attempt(&tx, &config, &config, &request("first", None), 100).unwrap();
    assert!(!selected.replayed);
    assert_eq!(selected.attempt.connection.id.as_str(), "connection-a");
    assert_eq!(
        selected.attempt.connection.account_ref.as_str(),
        "account-a"
    );
    assert_eq!(
        selected.attempt.request.target_profile_ref.as_str(),
        "sandbox-profile"
    );
    assert_eq!(selected.attempt.config_id, config.id);
    assert_eq!(
        selected.attempt.reasons,
        ["preferred_account", "observed_capacity_checked"]
    );
    assert!(
        matches!(&selected.attempt.connection.auth, ModelAuthentication::NativeManaged { profile_ref } if profile_ref.as_str() == "auth-profile-a")
    );
    let replay = begin_attempt(&tx, &config, &config, &request("first", None), 150).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt, selected.attempt);
    tx.commit().unwrap();
    drop(database);
    let database = Connection::open(&path).unwrap();
    assert_eq!(
        read_attempt(&database, &RequestId::new("first").unwrap()).unwrap(),
        Some(selected.attempt)
    );
}

#[test]
fn only_completed_effect_free_quota_failure_selects_an_approved_next_account() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    ready(&mut database, &config);
    let tx = database.transaction().unwrap();
    let first = begin_attempt(&tx, &config, &config, &request("first", None), 100).unwrap();
    assert_eq!(
        begin_attempt(
            &tx,
            &config,
            &config,
            &request("second", Some("first")),
            200
        )
        .unwrap_err()
        .0,
        "gateway_attempt_active"
    );
    finish_attempt(
        &tx,
        &RequestId::new("first").unwrap(),
        &completion(FailureKind::QuotaExhausted, Effects::None),
    )
    .unwrap();
    let second = begin_attempt(
        &tx,
        &config,
        &config,
        &request("second", Some("first")),
        200,
    )
    .unwrap();
    assert_eq!(second.attempt.connection.account_ref.as_str(), "account-b");
    assert!(
        second
            .attempt
            .reasons
            .iter()
            .any(|reason| reason == "quota_failover")
    );
    let stored_first = read_attempt(&tx, &RequestId::new("first").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored_first.connection, first.attempt.connection);
    assert_eq!(
        stored_first.connection_revision,
        first.attempt.connection_revision
    );
    assert_eq!(
        connection_state(&tx, &stored_first.connection, 200)
            .unwrap()
            .quota,
        Quota::Exhausted
    );
    assert!(begin_attempt(&tx, &config, &config, &request("fork", Some("first")), 200).is_err());
    tx.commit().unwrap();
}

#[test]
fn uncertain_effects_nonquota_errors_and_completed_work_do_not_automatically_rotate() {
    let config = load_config(CATALOG);
    for (outcome, effects) in [
        (
            AttemptOutcome::Failed(FailureKind::QuotaExhausted),
            Effects::Uncertain,
        ),
        (
            AttemptOutcome::Failed(FailureKind::QuotaExhausted),
            Effects::Committed,
        ),
        (
            AttemptOutcome::Failed(FailureKind::RateLimited),
            Effects::None,
        ),
        (
            AttemptOutcome::Failed(FailureKind::InvalidAuthentication),
            Effects::None,
        ),
        (
            AttemptOutcome::Failed(FailureKind::TransientFailure),
            Effects::None,
        ),
        (AttemptOutcome::Failed(FailureKind::Unknown), Effects::None),
        (AttemptOutcome::Succeeded, Effects::None),
    ] {
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        ready(&mut database, &config);
        let tx = database.transaction().unwrap();
        begin_attempt(&tx, &config, &config, &request("first", None), 100).unwrap();
        finish_attempt(
            &tx,
            &RequestId::new("first").unwrap(),
            &AttemptCompletion {
                outcome,
                effects,
                finished_at_ms: 200,
                retry_at_ms: None,
            },
        )
        .unwrap();
        assert_eq!(
            begin_attempt(
                &tx,
                &config,
                &config,
                &request("second", Some("first")),
                200
            )
            .unwrap_err()
            .0,
            "gateway_failover_unsafe"
        );
        assert_eq!(
            begin_attempt(&tx, &config, &config, &request("bypass", None), 200)
                .unwrap_err()
                .0,
            "gateway_previous_attempt_required"
        );
    }
}

#[test]
fn pinned_and_current_authority_both_restrict_fallback_and_billing_membership() {
    let pinned = load_config(CATALOG);
    for text in [
        CATALOG.replace("fallback_allowed = true", "fallback_allowed = false"),
        CATALOG.replace("approved_accounts = [\"account-a\", \"account-b\"]", "approved_accounts = [\"account-a\"]"),
        CATALOG.replace("members = [\"connection-b\", \"connection-a\"]", "members = [\"connection-a\"]"),
        CATALOG.replace("projects = [\"project-a\"]", "projects = [\"other-project\"]"),
        CATALOG.replace("roles = [\"worker\"]", "roles = [\"project_leader\"]"),
        CATALOG.replace("allowed_destinations = [\"sandbox-profile\"]", "allowed_destinations = [\"sandbox-profile\"]\nrevoked_destinations = [\"sandbox-profile\"]"),
        CATALOG.replace("auth-profile-b", "changed-auth-profile"),
    ] {
        let current = load_config(&text);
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        ready(&mut database, &pinned);
        let tx = database.transaction().unwrap();
        begin_attempt(&tx, &pinned, &pinned, &request("first", None), 100).unwrap();
        finish_attempt(&tx, &RequestId::new("first").unwrap(), &completion(FailureKind::QuotaExhausted, Effects::None)).unwrap();
        assert!(begin_attempt(&tx, &pinned, &current, &request("second", Some("first")), 200).is_err());
    }
}

#[test]
fn observations_reject_stale_sequence_conflicting_replay_and_changed_connections() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let tx = database.transaction().unwrap();
    let first = observation(&config, 0, 2);
    observe(&tx, &config, &first).unwrap();
    observe(&tx, &config, &first).unwrap();
    let mut conflict = first.clone();
    conflict.kind = ObservationKind::Unknown;
    assert!(observe(&tx, &config, &conflict).is_err());
    assert!(observe(&tx, &config, &observation(&config, 0, 1)).is_err());
    let changed = load_config(&CATALOG.replace("auth-profile-a", "changed-profile"));
    assert!(observe(&tx, &changed, &first).is_err());
    let new_connection = &changed.settings.model_access.as_ref().unwrap().connections[0];
    assert_eq!(
        connection_state(&tx, new_connection, 100).unwrap().quota,
        Quota::Unknown
    );
    let mut backward = observation(&config, 0, 3);
    backward.observed_at_ms = 99;
    assert!(observe(&tx, &config, &backward).is_err());
}

#[test]
fn sqlite_rollback_removes_selection_and_preserves_observation_history() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    ready(&mut database, &config);
    {
        let tx = database.transaction().unwrap();
        begin_attempt(&tx, &config, &config, &request("first", None), 100).unwrap();
    }
    assert!(
        read_attempt(&database, &RequestId::new("first").unwrap())
            .unwrap()
            .is_none()
    );
    let count: i64 = database
        .query_row(
            "SELECT count(*) FROM model_gateway_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let tx = database.transaction().unwrap();
    assert!(begin_attempt(&tx, &config, &config, &request("first", None), 100).is_ok());
}

#[test]
fn initial_selection_falls_back_only_for_explicit_quota_exhaustion() {
    let config = load_config(CATALOG);
    for kind in [
        ObservationKind::Unknown,
        ObservationKind::InvalidAuthentication,
        ObservationKind::RateLimited {
            retry_at_ms: Some(500),
        },
        ObservationKind::TransientFailure { retry_at_ms: None },
        ObservationKind::QuotaExhausted {
            retry_at_ms: Some(500),
        },
    ] {
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        ready(&mut database, &config);
        let tx = database.transaction().unwrap();
        let is_quota = matches!(kind, ObservationKind::QuotaExhausted { .. });
        let mut event = observation(&config, 0, 2);
        event.kind = kind;
        observe(&tx, &config, &event).unwrap();
        let selected = begin_attempt(&tx, &config, &config, &request("first", None), 200);
        if is_quota {
            let selected = selected.unwrap();
            assert_eq!(
                selected.attempt.connection.account_ref.as_str(),
                "account-b"
            );
            assert!(
                selected
                    .attempt
                    .reasons
                    .iter()
                    .any(|reason| reason == "quota_failover")
            );
        } else {
            assert!(selected.is_err());
        }
    }
}

#[test]
fn fallback_never_changes_model_harness_protocol_or_approved_scope() {
    for text in [
        CATALOG.replace("model = { id = \"coding-model\", author = \"model-author\" }\nauth = { method = \"native_managed\", profile_ref = \"auth-profile-b\" }", "model = { id = \"other-model\", author = \"model-author\" }\nauth = { method = \"native_managed\", profile_ref = \"auth-profile-b\" }"),
        CATALOG.replace("provider = \"provider-b\"\nharness = \"codex\"", "provider = \"provider-b\"\nharness = \"other-harness\""),
        CATALOG.replace("provider = \"provider-b\"\nharness = \"codex\"\nprotocol = \"native\"", "provider = \"provider-b\"\nharness = \"codex\"\nprotocol = \"openai-compatible\"")
            .replace("account_ref = \"account-b\"\nendpoint = { kind = \"native\" }", "account_ref = \"account-b\"\nendpoint = { kind = \"http\", url = \"https://example.invalid/v1\" }")
            .replace("auth = { method = \"native_managed\", profile_ref = \"auth-profile-b\" }", "auth = { method = \"api_key\", credential_ref = \"key-ref-b\" }"),
    ] {
        let config = load_config(&text);
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        ready(&mut database, &config);
        let tx = database.transaction().unwrap();
        begin_attempt(&tx, &config, &config, &request("first", None), 100).unwrap();
        finish_attempt(&tx, &RequestId::new("first").unwrap(), &completion(FailureKind::QuotaExhausted, Effects::None)).unwrap();
        assert_eq!(begin_attempt(&tx, &config, &config, &request("second", Some("first")), 200).unwrap_err().0, "gateway_incompatible_connection");
    }
}

#[test]
fn attempts_are_idempotent_but_request_and_completion_conflicts_are_rejected() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    ready(&mut database, &config);
    let tx = database.transaction().unwrap();
    begin_attempt(&tx, &config, &config, &request("first", None), 100).unwrap();
    let mut conflict = request("first", None);
    conflict.route.budget_ref = CredentialRef::new("different-budget").unwrap();
    assert_eq!(
        begin_attempt(&tx, &config, &config, &conflict, 100)
            .unwrap_err()
            .0,
        "gateway_attempt_conflict"
    );
    let finished = completion(FailureKind::QuotaExhausted, Effects::None);
    finish_attempt(&tx, &RequestId::new("first").unwrap(), &finished).unwrap();
    finish_attempt(&tx, &RequestId::new("first").unwrap(), &finished).unwrap();
    assert!(
        finish_attempt(
            &tx,
            &RequestId::new("first").unwrap(),
            &completion(FailureKind::QuotaExhausted, Effects::Uncertain)
        )
        .is_err()
    );
    let mut continuation = request("second", Some("first"));
    continuation.route.resources.spend_microunits = 1;
    assert!(begin_attempt(&tx, &config, &config, &continuation, 200).is_err());
    let count: i64 = tx
        .query_row("SELECT count(*) FROM model_gateway_attempts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn unknown_capacity_capability_mismatch_and_observation_bounds_are_rejected() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let tx = database.transaction().unwrap();
    for (sequence, remaining, capabilities) in [
        (1, Some(ResourceBudget::zero()), vec!["coding".into()]),
        (2, Some(ResourceBudget::unit()), vec!["different".into()]),
    ] {
        let mut event = observation(&config, 0, sequence);
        event.kind = ObservationKind::Ready {
            remaining,
            capabilities,
        };
        observe(&tx, &config, &event).unwrap();
        assert!(begin_attempt(&tx, &config, &config, &request("first", None), 100).is_err());
    }
    for (sequence, observed_at_ms, valid_until_ms) in
        [(0, 100, 10000), (3, 100, 100), (3, 100, 86_400_101)]
    {
        let mut event = observation(&config, 0, sequence);
        event.observed_at_ms = observed_at_ms;
        event.valid_until_ms = valid_until_ms;
        assert!(observe(&tx, &config, &event).is_err());
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM model_gateway_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn sqlite_sequence_maximum_is_exact_and_completion_overflow_preserves_active_attempt() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let tx = database.transaction().unwrap();
    let maximum = u64::try_from(i64::MAX).unwrap();
    let event = observation(&config, 0, maximum);
    observe(&tx, &config, &event).unwrap();
    let (sequence, storage_type, body): (i64, String, String) = tx
        .query_row(
            "SELECT sequence, typeof(sequence), body FROM model_gateway_observations WHERE id = ?1",
            [event.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(sequence, i64::MAX);
    assert_eq!(storage_type, "integer");
    let persisted: Observation = serde_json::from_str(&body).unwrap();
    assert_eq!(persisted, event);
    let selected = begin_attempt(&tx, &config, &config, &request("first", None), 100).unwrap();
    assert_eq!(selected.attempt.connection, event.connection);
    assert_eq!(
        finish_attempt(
            &tx,
            &RequestId::new("first").unwrap(),
            &completion(FailureKind::QuotaExhausted, Effects::None),
        )
        .unwrap_err()
        .0,
        "gateway_observation_bounds"
    );
    assert_eq!(
        read_attempt(&tx, &RequestId::new("first").unwrap())
            .unwrap()
            .unwrap(),
        selected.attempt
    );
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM model_gateway_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    tx.commit().unwrap();
}

#[test]
fn sqlite_sequence_overflow_is_rejected_before_insertion_without_corrupting_history() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let tx = database.transaction().unwrap();
    let first = observation(&config, 0, 1);
    observe(&tx, &config, &first).unwrap();
    let maximum = u64::try_from(i64::MAX).unwrap();
    for sequence in [maximum.checked_add(1).unwrap(), u64::MAX] {
        let overflow = observation(&config, 0, sequence);
        assert_eq!(
            observe(&tx, &config, &overflow).unwrap_err().0,
            "gateway_observation_bounds"
        );
    }
    let (count, sequence, body): (i64, i64, String) = tx
        .query_row(
            "SELECT count(*), sequence, body FROM model_gateway_observations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(sequence, 1);
    assert_eq!(serde_json::from_str::<Observation>(&body).unwrap(), first);
    tx.commit().unwrap();
}
