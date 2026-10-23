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
            remaining: None,
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
    let strict = load_config(&format!("{CATALOG}\nprovider_quota_policy = 'strict'\n"));
    assert_eq!(
        begin_attempt(&tx, &strict, &strict, &request("first", None), 100)
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
        ["preferred_account", "bounded_attempt_quota_unknown"]
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
    let strict = load_config(&format!("{CATALOG}\nprovider_quota_policy = 'strict'\n"));
    for (sequence, remaining, capabilities) in [
        (1, None, vec!["coding".into()]),
        (2, None, vec!["different".into()]),
    ] {
        let mut event = observation(&config, 0, sequence);
        event.kind = ObservationKind::Ready {
            remaining,
            capabilities,
        };
        observe(&tx, &config, &event).unwrap();
        let policy = if sequence == 1 { &strict } else { &config };
        assert_eq!(
            begin_attempt(&tx, policy, policy, &request("first", None), 100)
                .unwrap_err()
                .0,
            if sequence == 1 {
                "gateway_quota_unknown"
            } else {
                "gateway_capability_unavailable"
            }
        );
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

#[test]
fn quota_policy_default_allows_fresh_ready_with_unknown_numeric_quota() {
    let config = load_config(CATALOG);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.sqlite");
    let mut database = Connection::open(&path).unwrap();
    initialize(&database).unwrap();
    let event = Observation {
        kind: ObservationKind::Ready {
            remaining: None,
            capabilities: vec!["coding".into()],
        },
        ..observation(&config, 0, 1)
    };
    let tx = database.transaction().unwrap();
    observe(&tx, &config, &event).unwrap();
    tx.commit().unwrap();
    let state = connection_state(&database, &event.connection, 100).unwrap();
    assert_eq!(state.health, Health::Ready);
    assert_eq!(state.quota, Quota::Unknown);
    assert_eq!(state.observed_at_ms, Some(100));
    assert_eq!(state.valid_until_ms, Some(10000));
    assert_eq!(state.cooldown_until_ms, None);
    let requested = request("bounded-unknown-quota", None);
    let tx = database.transaction().unwrap();
    let selected = begin_attempt(&tx, &config, &config, &requested, 100)
        .expect("default policy must allow fresh readiness with unknown numeric provider quota");
    assert!(!selected.replayed);
    assert_eq!(selected.attempt.request, requested);
    assert_eq!(selected.attempt.connection, event.connection);
    assert_eq!(selected.attempt.config_id, config.id);
    assert_eq!(selected.attempt.selected_at_ms, 100);
    assert_eq!(selected.attempt.completion, None);
    assert_eq!(
        selected.attempt.reasons,
        ["preferred_account", "bounded_attempt_quota_unknown"]
    );
    assert_eq!(
        connection_state(&tx, &event.connection, 100).unwrap(),
        state
    );
    tx.commit().unwrap();
    drop(database);
    let database = Connection::open(&path).unwrap();
    assert_eq!(
        read_attempt(&database, &requested.id).unwrap(),
        Some(selected.attempt)
    );
    assert_eq!(
        connection_state(&database, &event.connection, 100).unwrap(),
        state
    );
}

#[test]
fn quota_policy_strict_in_either_pinned_or_current_config_rejects_unknown_quota() {
    let bounded = load_config(CATALOG);
    let strict = load_config(&format!("{CATALOG}\nprovider_quota_policy = 'strict'\n"));
    for (pinned, current) in [(&strict, &strict), (&strict, &bounded), (&bounded, &strict)] {
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        ready(&mut database, &bounded);
        let tx = database.transaction().unwrap();
        assert_eq!(
            begin_attempt(&tx, pinned, current, &request("strict", None), 100)
                .unwrap_err()
                .0,
            "gateway_quota_unknown"
        );
        let count: i64 = tx
            .query_row("SELECT count(*) FROM model_gateway_attempts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    ready(&mut database, &bounded);
    let tx = database.transaction().unwrap();
    let selected = begin_attempt(&tx, &bounded, &bounded, &request("existing", None), 100).unwrap();
    let replay = begin_attempt(&tx, &bounded, &strict, &request("existing", None), 150).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt, selected.attempt);
}

#[test]
fn quota_policy_never_bypasses_health_identity_or_authority_guards() {
    for policy in ["bounded_attempt", "strict"] {
        let catalog = CATALOG.replace("fallback_allowed = true", "fallback_allowed = false");
        let config = load_config(&format!("{catalog}\nprovider_quota_policy = '{policy}'\n"));
        for (kind, now, code) in [
            (ObservationKind::Unknown, 100, "gateway_health_unknown"),
            (
                ObservationKind::InvalidAuthentication,
                100,
                "gateway_invalid_authentication",
            ),
            (
                ObservationKind::RateLimited {
                    retry_at_ms: Some(500),
                },
                100,
                "gateway_rate_limited",
            ),
            (
                ObservationKind::TransientFailure {
                    retry_at_ms: Some(500),
                },
                100,
                "gateway_transient_failure",
            ),
            (
                ObservationKind::QuotaExhausted {
                    retry_at_ms: Some(500),
                },
                100,
                "gateway_quota_exhausted",
            ),
            (
                ObservationKind::Ready {
                    remaining: None,
                    capabilities: vec!["coding".into()],
                },
                99,
                "gateway_health_unknown",
            ),
            (
                ObservationKind::Ready {
                    remaining: None,
                    capabilities: vec!["coding".into()],
                },
                10000,
                "gateway_health_unknown",
            ),
        ] {
            let mut database = Connection::open_in_memory().unwrap();
            initialize(&database).unwrap();
            let tx = database.transaction().unwrap();
            let event = Observation {
                kind,
                ..observation(&config, 0, 1)
            };
            observe(&tx, &config, &event).unwrap();
            assert_eq!(
                begin_attempt(&tx, &config, &config, &request("guarded", None), now)
                    .unwrap_err()
                    .0,
                code
            );
        }
        for changed in [
            CATALOG.replace("auth-profile-a", "changed-profile"),
            CATALOG.replace(
                "approved_accounts = [\"account-a\", \"account-b\"]",
                "approved_accounts = [\"account-b\"]",
            ),
            CATALOG.replace(
                "projects = [\"project-a\"]",
                "projects = [\"other-project\"]",
            ),
            CATALOG.replace(
                "allowed_destinations = [\"sandbox-profile\"]",
                "allowed_destinations = []",
            ),
        ] {
            let current = load_config(&format!("{changed}\nprovider_quota_policy = '{policy}'\n"));
            let mut database = Connection::open_in_memory().unwrap();
            initialize(&database).unwrap();
            ready(&mut database, &config);
            let tx = database.transaction().unwrap();
            assert!(begin_attempt(&tx, &config, &current, &request("changed", None), 100).is_err());
        }
    }
}

fn quota_config_with_capacity(capacity: &ResourceBudget) -> EffectiveConfig {
    let budget = format!(
        "slots = {}\ncpu_millis = {}\nmemory_mib = {}\nprocesses = {}\noutput_bytes = {}\nwall_time_ms = {}\nspend_microunits = {}\n",
        capacity.slots,
        capacity.cpu_millis,
        capacity.memory_mib,
        capacity.processes,
        capacity.output_bytes,
        capacity.wall_time_ms,
        capacity.spend_microunits
    );
    let routing = format!(
        "[routing]\napproved_accounts = [\"account-a\", \"account-b\"]\n[routing.global_capacity]\n{budget}\n[routing.project_capacity]\n{budget}\n[routing.task_capacity]\n{budget}"
    );
    load_config(&CATALOG.replacen(
        "[routing]\napproved_accounts = [\"account-a\", \"account-b\"]",
        &routing,
        1,
    ))
}

#[test]
fn quota_policy_does_not_substitute_for_engine_resource_limits() {
    let tight = quota_config_with_capacity(&ResourceBudget::unit());
    let wide = quota_config_with_capacity(&ResourceBudget {
        slots: 2,
        cpu_millis: 2000,
        memory_mib: 1024,
        processes: 8,
        output_bytes: 2097152,
        wall_time_ms: 120000,
        spend_microunits: 1,
    });
    for dimension in 0..7 {
        let mut requested = request("bounded-resources", None);
        let value = match dimension {
            0 => &mut requested.route.resources.slots,
            1 => &mut requested.route.resources.cpu_millis,
            2 => &mut requested.route.resources.memory_mib,
            3 => &mut requested.route.resources.processes,
            4 => &mut requested.route.resources.output_bytes,
            5 => &mut requested.route.resources.wall_time_ms,
            6 => &mut requested.route.resources.spend_microunits,
            _ => unreachable!(),
        };
        *value += 1;
        for (pinned, current) in [(&tight, &wide), (&wide, &tight)] {
            let mut database = Connection::open_in_memory().unwrap();
            initialize(&database).unwrap();
            ready(&mut database, &wide);
            let tx = database.transaction().unwrap();
            assert_eq!(
                begin_attempt(&tx, pinned, current, &requested, 100)
                    .unwrap_err()
                    .0,
                "gateway_route_denied"
            );
            let count: i64 = tx
                .query_row("SELECT count(*) FROM model_gateway_attempts", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0);
        }
    }
}

fn legacy_ready_body(event: &Observation, remaining: Option<ResourceBudget>) -> String {
    let mut body = serde_json::to_value(event).unwrap();
    body["kind"]["Ready"]["remaining"] = serde_json::to_value(remaining).unwrap();
    serde_json::to_string(&body).unwrap()
}

fn insert_legacy_observation(database: &Connection, event: &Observation, body: &str) {
    use sha2::Digest;
    let revision = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(&event.connection).unwrap())
    );
    database.execute("INSERT INTO model_gateway_observations(id, connection_revision, sequence, body) VALUES(?1, ?2, ?3, ?4)", rusqlite::params![event.id.as_str(), revision, i64::try_from(event.sequence).unwrap(), body]).unwrap();
}

#[test]
fn quota_policy_legacy_sqlite_ready_preserves_unknown_quota_and_original_bytes() {
    let config = load_config(CATALOG);
    let strict = load_config(&format!("{CATALOG}\nprovider_quota_policy = 'strict'\n"));
    for remaining in [
        None,
        Some(ResourceBudget::zero()),
        Some(ResourceBudget::unit()),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.sqlite");
        let database = Connection::open(&path).unwrap();
        initialize(&database).unwrap();
        let event = observation(&config, 0, 1);
        let body = legacy_ready_body(&event, remaining.clone());
        if remaining.is_some() {
            assert!(serde_json::from_str::<Observation>(&body).is_err());
        }
        insert_legacy_observation(&database, &event, &body);
        drop(database);
        let mut database = Connection::open(&path).unwrap();
        let state = connection_state(&database, &event.connection, 100).unwrap();
        assert_eq!(state.health, Health::Ready);
        assert_eq!(state.quota, Quota::Unknown);
        assert_eq!(state.capabilities, ["coding"]);
        let tx = database.transaction().unwrap();
        assert_eq!(
            begin_attempt(&tx, &strict, &strict, &request("strict-legacy", None), 100)
                .unwrap_err()
                .0,
            "gateway_quota_unknown"
        );
        assert_eq!(
            observe(&tx, &config, &event).unwrap_err().0,
            "gateway_observation_conflict"
        );
        let selected =
            begin_attempt(&tx, &config, &config, &request("bounded-legacy", None), 100).unwrap();
        assert_eq!(selected.attempt.connection, event.connection);
        assert_eq!(
            begin_attempt(&tx, &config, &config, &request("again", None), 100)
                .unwrap_err()
                .0,
            "gateway_attempt_active"
        );
        observe(&tx, &config, &observation(&config, 0, 2)).unwrap();
        tx.commit().unwrap();
        let raw: String = database
            .query_row(
                "SELECT body FROM model_gateway_observations WHERE id = ?1",
                [event.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw, body);
        assert_eq!(
            connection_state(&database, &event.connection, 10000)
                .unwrap()
                .health,
            Health::Unknown
        );
        let mut historical = selected.attempt;
        historical.reasons[1] = "observed_capacity_checked".into();
        database
            .execute(
                "UPDATE model_gateway_attempts SET body = ?1 WHERE id = ?2",
                rusqlite::params![
                    serde_json::to_string(&historical).unwrap(),
                    historical.request.id.as_str()
                ],
            )
            .unwrap();
        drop(database);
        let mut database = Connection::open(&path).unwrap();
        assert_eq!(
            read_attempt(&database, &historical.request.id).unwrap(),
            Some(historical.clone())
        );
        let tx = database.transaction().unwrap();
        let replay = begin_attempt(&tx, &config, &strict, &historical.request, 100).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.attempt, historical);
    }
}

#[test]
fn quota_policy_legacy_ids_order_bounds_and_transactions_remain_exact() {
    let config = load_config(CATALOG);
    let event = observation(&config, 0, 1);
    let valid = serde_json::from_str::<serde_json::Value>(&legacy_ready_body(
        &event,
        Some(ResourceBudget::unit()),
    ))
    .unwrap();
    let mut malformed = Vec::new();
    for (field, value) in [
        ("extra", serde_json::json!(true)),
        ("id", serde_json::json!("changed-observation-id")),
        ("sequence", serde_json::json!(u64::MAX)),
        ("valid_until_ms", serde_json::json!(100)),
    ] {
        let mut body = valid.clone();
        body[field] = value;
        malformed.push(serde_json::to_string(&body).unwrap());
    }
    let mut changed_connection = valid.clone();
    changed_connection["connection"]["auth"]["profile_ref"] = serde_json::json!("changed-profile");
    malformed.push(serde_json::to_string(&changed_connection).unwrap());
    for remaining in [
        serde_json::json!({"slots": 1}),
        serde_json::json!({"slots": 0, "cpu_millis": 0, "memory_mib": 0, "processes": 0, "output_bytes": 0, "wall_time_ms": 0, "spend_microunits": 0, "extra": true}),
        serde_json::json!(-1),
    ] {
        let mut body = valid.clone();
        body["kind"]["Ready"]["remaining"] = remaining;
        malformed.push(serde_json::to_string(&body).unwrap());
    }
    malformed.push(
        serde_json::to_string(&valid)
            .unwrap()
            .replace("\"sequence\":1", "\"sequence\":1,\"sequence\":1"),
    );
    for body in malformed {
        let database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        insert_legacy_observation(&database, &event, &body);
        assert!(connection_state(&database, &event.connection, 100).is_err());
    }
    let mut oversized = valid.clone();
    oversized["kind"]["Ready"]["capabilities"] = serde_json::json!(["é".repeat(8192)]);
    let oversized = serde_json::to_string(&oversized).unwrap();
    assert!(oversized.len() > 16384);
    assert!(oversized.chars().count() <= 16384);
    let database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    insert_legacy_observation(&database, &event, &oversized);
    assert_eq!(
        connection_state(&database, &event.connection, 100)
            .unwrap_err()
            .0,
        "gateway_record_bounds"
    );
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let mut latest = observation(&config, 0, 3);
    latest.observed_at_ms = 300;
    let body = legacy_ready_body(&latest, Some(ResourceBudget::unit()));
    insert_legacy_observation(&database, &latest, &body);
    let tx = database.transaction().unwrap();
    assert_eq!(
        observe(&tx, &config, &observation(&config, 0, 2))
            .unwrap_err()
            .0,
        "gateway_observation_stale"
    );
    assert_eq!(
        observe(&tx, &config, &observation(&config, 0, 4))
            .unwrap_err()
            .0,
        "gateway_observation_stale"
    );
    tx.commit().unwrap();
    database.execute_batch("CREATE TRIGGER reject_quota_attempt BEFORE INSERT ON model_gateway_attempts BEGIN SELECT RAISE(ABORT, 'quota attempt rollback'); END;").unwrap();
    let tx = database.transaction().unwrap();
    assert!(begin_attempt(&tx, &config, &config, &request("aborted", None), 300).is_err());
    tx.rollback().unwrap();
    assert!(
        read_attempt(&database, &RequestId::new("aborted").unwrap())
            .unwrap()
            .is_none()
    );
    let raw: String = database
        .query_row(
            "SELECT body FROM model_gateway_observations WHERE id = ?1",
            [latest.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, body);
}

#[test]
fn quota_policy_unknown_does_not_grant_failover_or_replay_uncertain_effects() {
    let config = load_config(CATALOG);
    let strict = load_config(&format!("{CATALOG}\nprovider_quota_policy = 'strict'\n"));
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    ready(&mut database, &config);
    let tx = database.transaction().unwrap();
    assert_eq!(
        begin_attempt(&tx, &strict, &strict, &request("strict", None), 100)
            .unwrap_err()
            .0,
        "gateway_quota_unknown"
    );
    let selected = begin_attempt(&tx, &config, &config, &request("bounded", None), 100).unwrap();
    assert_eq!(
        selected.attempt.connection.account_ref.as_str(),
        "account-a"
    );
    assert_eq!(
        selected.attempt.reasons,
        ["preferred_account", "bounded_attempt_quota_unknown"]
    );
    finish_attempt(
        &tx,
        &selected.attempt.request.id,
        &completion(FailureKind::QuotaExhausted, Effects::Uncertain),
    )
    .unwrap();
    assert_eq!(
        begin_attempt(
            &tx,
            &config,
            &config,
            &request("uncertain", Some("bounded")),
            200
        )
        .unwrap_err()
        .0,
        "gateway_failover_unsafe"
    );
}

#[test]
fn quota_policy_fresh_ready_unknown_preserves_active_cooldown() {
    let config = load_config(CATALOG);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.sqlite");
    let mut database = Connection::open(&path).unwrap();
    initialize(&database).unwrap();
    let initial = observation(&config, 0, 1);
    let control = request("without-prior-cooldown", None);
    let tx = database.transaction().unwrap();
    observe(&tx, &config, &initial).unwrap();
    let selected = begin_attempt(&tx, &config, &config, &control, 100).unwrap();
    assert_eq!(selected.attempt.connection, initial.connection);
    assert_eq!(
        connection_state(&tx, &initial.connection, 100)
            .unwrap()
            .cooldown_until_ms,
        None
    );
    tx.rollback().unwrap();
    assert_eq!(read_attempt(&database, &control.id).unwrap(), None);

    let limited = Observation {
        kind: ObservationKind::RateLimited {
            retry_at_ms: Some(500),
        },
        ..initial
    };
    let tx = database.transaction().unwrap();
    observe(&tx, &config, &limited).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        connection_state(&database, &limited.connection, 100)
            .unwrap()
            .cooldown_until_ms,
        Some(500)
    );

    let fresh = Observation {
        observed_at_ms: 200,
        ..observation(&config, 0, 2)
    };
    assert_eq!(fresh.connection, limited.connection);
    let tx = database.transaction().unwrap();
    observe(&tx, &config, &fresh).unwrap();
    tx.commit().unwrap();
    drop(database);

    let mut database = Connection::open(&path).unwrap();
    let state = connection_state(&database, &fresh.connection, 200).unwrap();
    assert_eq!(state.health, Health::Ready);
    assert_eq!(state.quota, Quota::Unknown);
    assert_eq!(state.observed_at_ms, Some(200));
    assert_eq!(state.valid_until_ms, Some(10000));
    assert_eq!(state.capabilities, ["coding"]);
    assert_eq!(state.cooldown_until_ms, Some(500));
    let blocked = request("during-prior-cooldown", None);
    let tx = database.transaction().unwrap();
    assert_eq!(
        begin_attempt(&tx, &config, &config, &blocked, 200)
            .unwrap_err()
            .0,
        "gateway_cooldown"
    );
    assert_eq!(read_attempt(&tx, &blocked.id).unwrap(), None);
    tx.commit().unwrap();
}

#[test]
fn cooldown_history_preserves_maximum_exact_expiry_and_negative_health() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let first = Observation {
        kind: ObservationKind::RateLimited {
            retry_at_ms: Some(700),
        },
        ..observation(&config, 0, 1)
    };
    let second = Observation {
        observed_at_ms: 200,
        kind: ObservationKind::RateLimited {
            retry_at_ms: Some(400),
        },
        ..observation(&config, 0, 2)
    };
    let tx = database.transaction().unwrap();
    observe(&tx, &config, &first).unwrap();
    observe(&tx, &config, &second).unwrap();
    tx.commit().unwrap();
    let negative = connection_state(&database, &second.connection, 700).unwrap();
    assert_eq!(negative.cooldown_until_ms, None);
    assert_eq!(negative.health, Health::RateLimited);
    let tx = database.transaction().unwrap();
    assert_eq!(
        begin_attempt(
            &tx,
            &config,
            &config,
            &request("negative-expiry", None),
            700
        )
        .unwrap_err()
        .0,
        "gateway_rate_limited"
    );
    let fresh = Observation {
        observed_at_ms: 300,
        ..observation(&config, 0, 3)
    };
    observe(&tx, &config, &fresh).unwrap();
    tx.commit().unwrap();
    let active = connection_state(&database, &fresh.connection, 699).unwrap();
    assert_eq!(active.health, Health::Ready);
    assert_eq!(active.quota, Quota::Unknown);
    assert_eq!(active.cooldown_until_ms, Some(700));
    let tx = database.transaction().unwrap();
    assert_eq!(
        begin_attempt(&tx, &config, &config, &request("before-expiry", None), 699)
            .unwrap_err()
            .0,
        "gateway_cooldown"
    );
    assert_eq!(
        connection_state(&tx, &fresh.connection, 700)
            .unwrap()
            .cooldown_until_ms,
        None
    );
    assert!(begin_attempt(&tx, &config, &config, &request("exact-expiry", None), 700).is_ok());
    assert_eq!(
        connection_state(&tx, &fresh.connection, 10000)
            .unwrap()
            .health,
        Health::Unknown
    );
}

#[test]
fn cooldown_history_enforces_exact_256_and_257_row_boundary() {
    let config = load_config(CATALOG);
    for historical in [256_u64, 257] {
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        let tx = database.transaction().unwrap();
        for sequence in 1..=historical + 1 {
            let mut event = observation(&config, 0, sequence);
            if sequence == 1 {
                event.kind = ObservationKind::RateLimited {
                    retry_at_ms: Some(500),
                };
            }
            observe(&tx, &config, &event).unwrap();
        }
        observe(&tx, &config, &observation(&config, 1, 1)).unwrap();
        tx.commit().unwrap();
        let model = &config.settings.model_access.as_ref().unwrap().connections[0];
        let expected = if historical == 256 {
            assert_eq!(
                connection_state(&database, model, 200)
                    .unwrap()
                    .cooldown_until_ms,
                Some(500)
            );
            "gateway_cooldown"
        } else {
            assert_eq!(
                connection_state(&database, model, 200).unwrap_err().0,
                "gateway_cooldown_history_limit"
            );
            "gateway_cooldown_history_limit"
        };
        let requested = request("bounded-history", None);
        let tx = database.transaction().unwrap();
        assert_eq!(
            begin_attempt(&tx, &config, &config, &requested, 200)
                .unwrap_err()
                .0,
            expected
        );
        assert_eq!(read_attempt(&tx, &requested.id).unwrap(), None);
    }
}

#[test]
fn cooldown_history_old_cutoff_proves_expiry_before_extra_history() {
    let config = load_config(CATALOG);
    let now = 86_400_100;
    for recent in [0_u64, 255] {
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        let tx = database.transaction().unwrap();
        for sequence in 1..=301 + recent {
            let mut event = observation(&config, 0, sequence);
            if sequence > 300 {
                event.observed_at_ms = now;
                event.valid_until_ms = now + 10000;
            }
            if sequence == 1 {
                event.kind = ObservationKind::RateLimited {
                    retry_at_ms: Some(500),
                };
            }
            if sequence == 300 {
                insert_legacy_observation(
                    &tx,
                    &event,
                    &legacy_ready_body(&event, Some(ResourceBudget::zero())),
                );
            } else {
                observe(&tx, &config, &event).unwrap();
            }
        }
        tx.commit().unwrap();
        let model = &config.settings.model_access.as_ref().unwrap().connections[0];
        let state = connection_state(&database, model, now).unwrap();
        assert_eq!(state.health, Health::Ready);
        assert_eq!(state.quota, Quota::Unknown);
        assert_eq!(state.cooldown_until_ms, None);
        let tx = database.transaction().unwrap();
        assert!(begin_attempt(&tx, &config, &config, &request("old-history", None), now).is_ok());
    }
}

#[test]
fn cooldown_history_expired_middle_does_not_hide_older_active_deadline() {
    let config = load_config(CATALOG);
    let mut database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let tx = database.transaction().unwrap();
    let first = Observation {
        valid_until_ms: 1000,
        kind: ObservationKind::RateLimited {
            retry_at_ms: Some(900),
        },
        ..observation(&config, 0, 1)
    };
    let middle = Observation {
        observed_at_ms: 200,
        valid_until_ms: 250,
        ..observation(&config, 0, 2)
    };
    let latest = Observation {
        observed_at_ms: 300,
        ..observation(&config, 0, 3)
    };
    for event in [&first, &middle, &latest] {
        observe(&tx, &config, event).unwrap();
    }
    tx.commit().unwrap();
    let state = connection_state(&database, &latest.connection, 500).unwrap();
    assert_eq!(state.health, Health::Ready);
    assert_eq!(state.quota, Quota::Unknown);
    assert_eq!(state.cooldown_until_ms, Some(900));
    let tx = database.transaction().unwrap();
    assert_eq!(
        begin_attempt(&tx, &config, &config, &request("expired-middle", None), 500)
            .unwrap_err()
            .0,
        "gateway_cooldown"
    );
}

#[test]
fn cooldown_history_rejects_historical_identity_bounds_and_time_corruption() {
    let config = load_config(CATALOG);
    for (case, expected) in [
        (0, "gateway_observation_identity"),
        (1, "gateway_observation_identity"),
        (2, "gateway_observation_identity"),
        (3, "gateway_observation_bounds"),
        (4, "gateway_observation_stale"),
    ] {
        let mut database = Connection::open_in_memory().unwrap();
        initialize(&database).unwrap();
        let historical = Observation {
            kind: ObservationKind::RateLimited {
                retry_at_ms: Some(500),
            },
            ..observation(&config, 0, 1)
        };
        let latest = Observation {
            observed_at_ms: 200,
            ..observation(&config, 0, 2)
        };
        let tx = database.transaction().unwrap();
        observe(&tx, &config, &historical).unwrap();
        observe(&tx, &config, &latest).unwrap();
        tx.commit().unwrap();
        let mut body = serde_json::to_value(&historical).unwrap();
        match case {
            0 => body["id"] = serde_json::json!("different-observation"),
            1 => body["sequence"] = serde_json::json!(2),
            2 => body["connection"]["provider"] = serde_json::json!("changed-provider"),
            3 => body["valid_until_ms"] = serde_json::json!(86_400_101_u64),
            4 => body["observed_at_ms"] = serde_json::json!(300),
            _ => unreachable!(),
        }
        database
            .execute(
                "UPDATE model_gateway_observations SET body = ?1 WHERE id = ?2",
                rusqlite::params![
                    serde_json::to_string(&body).unwrap(),
                    historical.id.as_str()
                ],
            )
            .unwrap();
        assert_eq!(
            connection_state(&database, &latest.connection, 400)
                .unwrap_err()
                .0,
            expected
        );
        let requested = request("corrupt-history", None);
        let tx = database.transaction().unwrap();
        assert_eq!(
            begin_attempt(&tx, &config, &config, &requested, 400)
                .unwrap_err()
                .0,
            expected
        );
        assert_eq!(read_attempt(&tx, &requested.id).unwrap(), None);
    }
}
