use super::*;
use crate::config::{ModelAuthentication, resolve_config};
use crate::engine::owner::{
    NativeAuthAction, NativeAuthFact, NativeAuthMethod, NativeAuthPhase, NativeProfileBinding,
};
use crate::protocol::Cursor;

const CATALOG: &str = r#"
allowed_destinations = ["sandbox-profile"]
[routing]
approved_accounts = ["account-a"]
[model_access]
accounts = ["account-a"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "conversation-model", author = "model-author" }
auth = { method = "native_managed", profile_ref = "auth-profile-a" }
[[model_access.pools]]
id = "master-pool"
members = ["connection-a"]
projects = ["project-a"]
roles = ["project_leader", "worker"]
fallback_allowed = true
"#;

struct Fixture {
    directory: tempfile::TempDir,
    config: EffectiveConfig,
    database: Connection,
    request: AttemptRequest,
    fact: NativeAuthFact,
    binding: NativeProfileBinding,
    generation: u64,
}

impl Fixture {
    fn new(catalog: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("global.toml");
        std::fs::write(&settings, catalog).unwrap();
        let config = resolve_config(&settings, &directory.path().join("project.toml")).unwrap();
        let database = Connection::open(directory.path().join("gateway.sqlite")).unwrap();
        initialize(&database).unwrap();
        let binding = NativeProfileBinding {
            database_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
            config_id: config.id.clone(),
            source_hash: "c".repeat(64),
            at_revision: Cursor::new(4),
            connection: config.settings.model_access.as_ref().unwrap().connections[0].clone(),
            custody_id: "d".repeat(64),
        };
        let fact = NativeAuthFact {
            operation_id: RequestId::new("auth-operation").unwrap(),
            action: NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexBrowser,
            },
            binding: binding.clone(),
            generation: 1,
            phase: NativeAuthPhase::AuthenticatedObserved,
            code: None,
        };
        let request = AttemptRequest {
            id: RequestId::new("initial-conversation").unwrap(),
            task_id: TaskId::new("master-session-first-turn").unwrap(),
            project_id: ProjectId::new("project-a").unwrap(),
            role: Role::ProjectLeader,
            pool_ref: CredentialRef::new("master-pool").unwrap(),
            target_profile_ref: CredentialRef::new("sandbox-profile").unwrap(),
            route: RouteRequest {
                preferred_target: Some(CredentialRef::new("sandbox-profile").unwrap()),
                required_target: None,
                required_capabilities: vec![],
                account_ref: CredentialRef::new("account-a").unwrap(),
                budget_ref: CredentialRef::new("conversation-budget").unwrap(),
                resources: ResourceBudget::unit(),
            },
            previous_attempt: None,
        };
        Self {
            directory,
            config,
            database,
            request,
            fact,
            binding,
            generation: 1,
        }
    }

    fn begin(&mut self, now_ms: u64) -> Result<AttemptStart, EngineError> {
        let tx = self.database.transaction().unwrap();
        let result = begin_initial_conversation_attempt(
            &tx,
            &self.config,
            &self.config,
            &self.request,
            InitialConversationAuth {
                fact: &self.fact,
                binding: &self.binding,
                generation: self.generation,
            },
            now_ms,
        )?;
        tx.commit().unwrap();
        Ok(result)
    }

    fn record(&mut self, kind: ObservationKind, observed_at_ms: u64, valid_until_ms: u64) {
        let tx = self.database.transaction().unwrap();
        observe(
            &tx,
            &self.config,
            &Observation {
                id: RequestId::new("readiness-observation").unwrap(),
                connection: self.binding.connection.clone(),
                sequence: 1,
                observed_at_ms,
                valid_until_ms,
                kind,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    fn reject(&mut self, now_ms: u64, expected: &str) {
        assert_eq!(self.begin(now_ms).unwrap_err().0, expected);
        assert_eq!(
            read_attempt(&self.database, &self.request.id).unwrap(),
            None
        );
    }
}

#[test]
fn ordinary_admission_still_rejects_unknown_model_readiness() {
    let mut fixture = Fixture::new(CATALOG);
    let tx = fixture.database.transaction().unwrap();
    assert_eq!(
        begin_attempt(&tx, &fixture.config, &fixture.config, &fixture.request, 100)
            .unwrap_err()
            .0,
        "gateway_health_unknown"
    );
    assert_eq!(read_attempt(&tx, &fixture.request.id).unwrap(), None);
}

#[test]
fn initial_authenticated_conversation_persists_one_attempt_without_marking_ready() {
    let mut fixture = Fixture::new(CATALOG);
    let before = connection_state(&fixture.database, &fixture.binding.connection, 100).unwrap();
    let selected = fixture.begin(100).unwrap();
    assert!(!selected.replayed);
    assert_eq!(selected.attempt.request, fixture.request);
    assert_eq!(selected.attempt.connection, fixture.binding.connection);
    assert_eq!(selected.attempt.config_id, fixture.config.id);
    assert_eq!(selected.attempt.completion, None);
    assert!(
        selected
            .attempt
            .reasons
            .iter()
            .any(|reason| reason == "initial_conversation_readiness_unknown")
    );
    assert_eq!(before.health, Health::Unknown);
    assert_eq!(before.quota, Quota::Unknown);
    assert_eq!(
        connection_state(&fixture.database, &fixture.binding.connection, 100).unwrap(),
        before
    );
    let reopened = Connection::open(fixture.directory.path().join("gateway.sqlite")).unwrap();
    assert_eq!(
        read_attempt(&reopened, &fixture.request.id).unwrap(),
        Some(selected.attempt)
    );
    assert_eq!(
        connection_state(&reopened, &fixture.binding.connection, 100).unwrap(),
        before
    );
}

#[test]
fn every_non_authenticated_phase_rejects_without_allocating_an_attempt() {
    for phase in [
        NativeAuthPhase::Starting,
        NativeAuthPhase::AwaitingOwner,
        NativeAuthPhase::CheckingAccount,
        NativeAuthPhase::UnauthenticatedObserved,
        NativeAuthPhase::Cancelling,
        NativeAuthPhase::Unavailable,
        NativeAuthPhase::Uncertain,
    ] {
        let mut fixture = Fixture::new(CATALOG);
        fixture.fact.phase = phase;
        fixture.reject(100, "gateway_initial_authentication_required");
    }
}

#[test]
fn logout_and_mismatched_official_method_cannot_prove_sign_in() {
    for action in [
        NativeAuthAction::Logout,
        NativeAuthAction::SignIn {
            method: NativeAuthMethod::ClaudeOfficial,
        },
    ] {
        let mut fixture = Fixture::new(CATALOG);
        fixture.fact.action = action;
        fixture.reject(100, "gateway_initial_authentication_required");
    }
}

#[test]
fn zero_or_superseded_auth_generation_rejects() {
    for generation in [0, 2] {
        let mut fixture = Fixture::new(CATALOG);
        fixture.generation = generation;
        fixture.reject(100, "gateway_initial_authentication_stale");
    }
}

#[test]
fn stale_binding_identity_source_profile_model_or_account_rejects() {
    for index in 0..9 {
        let mut fixture = Fixture::new(CATALOG);
        let binding = &mut fixture.fact.binding;
        match index {
            0 => binding.database_instance = "e".repeat(64),
            1 => binding.engine_epoch = "e".repeat(64),
            2 => binding.source_hash = "e".repeat(64),
            3 => binding.at_revision = Cursor::new(3),
            4 => binding.custody_id = "e".repeat(64),
            5 => binding.connection.model.id = "different-model".into(),
            6 => binding.connection.account_ref = CredentialRef::new("other-account").unwrap(),
            7 => {
                binding.connection.auth = ModelAuthentication::NativeManaged {
                    profile_ref: CredentialRef::new("other-profile").unwrap(),
                }
            }
            8 => binding.config_id = ConfigId::new("e".repeat(64)).unwrap(),
            _ => unreachable!(),
        }
        fixture.reject(100, "gateway_initial_authentication_stale");
    }
}

#[test]
fn binding_must_also_match_the_selected_live_connection() {
    for index in 0..2 {
        let mut fixture = Fixture::new(CATALOG);
        match index {
            0 => fixture.binding.connection.model.id = "different-model".into(),
            1 => fixture.binding.connection.auth = ModelAuthentication::None {},
            _ => unreachable!(),
        }
        fixture.fact.binding = fixture.binding.clone();
        fixture.reject(100, "gateway_initial_authentication_stale");
    }
}

#[test]
fn a_sign_in_from_an_earlier_config_with_the_same_connection_is_admitted() {
    let mut fixture = Fixture::new(CATALOG);
    let earlier = fixture.config.id.clone();
    let settings = fixture.directory.path().join("global.toml");
    let edited = CATALOG.replace(
        "roles = [\"project_leader\", \"worker\"]",
        "name = \"Daily work\"\nroles = [\"project_leader\", \"worker\"]",
    );
    assert_ne!(edited, CATALOG);
    std::fs::write(&settings, edited).unwrap();
    fixture.config =
        resolve_config(&settings, &fixture.directory.path().join("project.toml")).unwrap();
    assert_ne!(fixture.config.id, earlier);
    assert_eq!(fixture.binding.config_id, earlier);
    let started = fixture.begin(100).unwrap();
    assert!(!started.replayed);
    assert_eq!(started.attempt.connection, fixture.binding.connection);
    assert_eq!(started.attempt.config_id, fixture.config.id);
}

#[test]
fn explicit_unhealthy_exhausted_and_cooldown_states_remain_denied() {
    for (kind, expected) in [
        (
            ObservationKind::InvalidAuthentication,
            "gateway_invalid_authentication",
        ),
        (
            ObservationKind::RateLimited {
                retry_at_ms: Some(500),
            },
            "gateway_rate_limited",
        ),
        (
            ObservationKind::TransientFailure {
                retry_at_ms: Some(500),
            },
            "gateway_transient_failure",
        ),
        (
            ObservationKind::QuotaExhausted { retry_at_ms: None },
            "gateway_quota_exhausted",
        ),
    ] {
        let mut fixture = Fixture::new(CATALOG);
        fixture.record(kind, 100, 1000);
        fixture.reject(100, expected);
    }
}

#[test]
fn future_readiness_metadata_cannot_be_reclassified_as_absent() {
    for kind in [
        ObservationKind::Unknown,
        ObservationKind::QuotaExhausted { retry_at_ms: None },
        ObservationKind::Ready {
            remaining: None,
            capabilities: vec![],
        },
    ] {
        let mut fixture = Fixture::new(CATALOG);
        fixture.record(kind, 200, 300);
        fixture.reject(100, "gateway_initial_readiness_stale");
    }
}

#[test]
fn fresh_unknown_metadata_allows_only_the_initial_conversation() {
    let mut fixture = Fixture::new(CATALOG);
    fixture.record(ObservationKind::Unknown, 100, 1000);
    fixture.begin(100).unwrap();
    assert_eq!(
        connection_state(&fixture.database, &fixture.binding.connection, 100)
            .unwrap()
            .health,
        Health::Unknown
    );
}

#[test]
fn strict_quota_policy_remains_strict_for_initial_conversation() {
    let mut fixture = Fixture::new(&format!("{CATALOG}\nprovider_quota_policy = \"strict\"\n"));
    fixture.reject(100, "gateway_quota_unknown");
}

#[test]
fn worker_role_and_execution_capabilities_cannot_use_the_conversation_exception() {
    let mut worker = Fixture::new(CATALOG);
    worker.request.role = Role::Worker;
    worker.reject(100, "gateway_initial_conversation_scope");
    let mut coding = Fixture::new(CATALOG);
    coding.request.route.required_capabilities = vec!["coding".into()];
    coding.reject(100, "gateway_initial_conversation_scope");
}

#[test]
fn invalid_or_excessive_resource_budget_still_rejects() {
    for invalid in [true, false] {
        let mut fixture = Fixture::new(CATALOG);
        fixture.request.route.resources.memory_mib = if invalid { 0 } else { 8193 };
        let tx = fixture.database.transaction().unwrap();
        let result = begin_initial_conversation_attempt(
            &tx,
            &fixture.config,
            &fixture.config,
            &fixture.request,
            InitialConversationAuth {
                fact: &fixture.fact,
                binding: &fixture.binding,
                generation: 1,
            },
            100,
        );
        assert_eq!(
            result.unwrap_err().0,
            if invalid {
                "invalid_resource_budget"
            } else {
                "gateway_route_denied"
            }
        );
        assert_eq!(read_attempt(&tx, &fixture.request.id).unwrap(), None);
    }
}

#[test]
fn replay_does_not_allocate_a_second_attempt_and_distinct_second_request_is_denied() {
    let mut fixture = Fixture::new(CATALOG);
    let first = fixture.begin(100).unwrap();
    let replay = fixture.begin(101).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt, first.attempt);
    let tx = fixture.database.transaction().unwrap();
    finish_attempt(
        &tx,
        &fixture.request.id,
        &AttemptCompletion {
            outcome: AttemptOutcome::Succeeded,
            effects: Effects::None,
            finished_at_ms: 102,
            retry_at_ms: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    fixture.request.id = RequestId::new("second-conversation-request").unwrap();
    fixture.reject(103, "gateway_previous_attempt_required");
    assert_eq!(
        fixture
            .database
            .query_row("SELECT count(*) FROM model_gateway_attempts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn continuation_cannot_use_a_previous_attempt_to_reenter_the_initial_path() {
    let mut fixture = Fixture::new(CATALOG);
    fixture.request.previous_attempt = Some(RequestId::new("prior-attempt").unwrap());
    fixture.reject(100, "gateway_initial_conversation_scope");
}

#[test]
fn initial_auth_binding_never_falls_back_to_a_different_ready_account() {
    let catalog = format!(
        "{}\n{}",
        CATALOG
            .replace(
                "accounts = [\"account-a\"]",
                "accounts = [\"account-a\", \"account-b\"]"
            )
            .replace(
                "members = [\"connection-a\"]",
                "members = [\"connection-a\", \"connection-b\"]"
            ),
        r#"
[[model_access.connections]]
id = "connection-b"
provider = "provider-b"
harness = "codex"
protocol = "native"
account_ref = "account-b"
endpoint = { kind = "native" }
model = { id = "conversation-model", author = "model-author" }
auth = { method = "native_managed", profile_ref = "auth-profile-b" }
"#
    );
    let mut fixture = Fixture::new(&catalog);
    fixture.record(
        ObservationKind::QuotaExhausted { retry_at_ms: None },
        100,
        1000,
    );
    let tx = fixture.database.transaction().unwrap();
    let other = fixture
        .config
        .settings
        .model_access
        .as_ref()
        .unwrap()
        .connections[1]
        .clone();
    observe(
        &tx,
        &fixture.config,
        &Observation {
            id: RequestId::new("other-account-ready").unwrap(),
            connection: other.clone(),
            sequence: 1,
            observed_at_ms: 100,
            valid_until_ms: 1000,
            kind: ObservationKind::Ready {
                remaining: None,
                capabilities: vec![],
            },
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = fixture.database.transaction().unwrap();
    let ordinary =
        begin_attempt(&tx, &fixture.config, &fixture.config, &fixture.request, 100).unwrap();
    assert_eq!(ordinary.attempt.connection, other);
    tx.rollback().unwrap();
    fixture.reject(100, "gateway_quota_exhausted");
}

#[test]
fn initial_success_makes_the_next_ordinary_conversation_attempt_admissible() {
    let mut fixture = Fixture::new(CATALOG);
    let first = fixture.begin(100).unwrap();
    let tx = fixture.database.transaction().unwrap();
    finish_attempt(
        &tx,
        &first.attempt.request.id,
        &AttemptCompletion {
            outcome: AttemptOutcome::Succeeded,
            effects: Effects::Committed,
            finished_at_ms: 102,
            retry_at_ms: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    fixture.request.id = RequestId::new("second-conversation-turn").unwrap();
    let tx = fixture.database.transaction().unwrap();
    let second =
        begin_attempt(&tx, &fixture.config, &fixture.config, &fixture.request, 103).unwrap();
    assert!(!second.replayed);
    assert_eq!(second.attempt.connection, first.attempt.connection);
    assert!(
        !second
            .attempt
            .reasons
            .iter()
            .any(|reason| reason == "initial_conversation_readiness_unknown")
    );
    assert_eq!(
        begin_attempt(
            &tx,
            &fixture.config,
            &fixture.config,
            &{
                let mut third = fixture.request.clone();
                third.id = RequestId::new("concurrent-conversation-turn").unwrap();
                third
            },
            104
        )
        .unwrap_err()
        .0,
        "gateway_attempt_active"
    );
    tx.commit().unwrap();
}

fn finish(fixture: &mut Fixture, id: &RequestId, completion: AttemptCompletion) {
    let tx = fixture.database.transaction().unwrap();
    finish_attempt(&tx, id, &completion).unwrap();
    tx.commit().unwrap();
}

fn next_turn(fixture: &mut Fixture, name: &str) {
    fixture.request.id = RequestId::new(format!("{name}-attempt")).unwrap();
    fixture.request.task_id = TaskId::new(format!("{name}-task")).unwrap();
}

fn observe_at(
    fixture: &mut Fixture,
    sequence: u64,
    observed_at_ms: u64,
    valid_until_ms: u64,
    kind: ObservationKind,
) {
    let tx = fixture.database.transaction().unwrap();
    observe(
        &tx,
        &fixture.config,
        &Observation {
            id: RequestId::new(format!("observation-{sequence}")).unwrap(),
            connection: fixture.binding.connection.clone(),
            sequence,
            observed_at_ms,
            valid_until_ms,
            kind,
        },
    )
    .unwrap();
    tx.commit().unwrap();
}

#[test]
fn expired_readiness_admits_one_probe_whose_success_restores_ready() {
    for kind in [
        ObservationKind::Unknown,
        ObservationKind::QuotaExhausted { retry_at_ms: None },
        ObservationKind::Ready {
            remaining: None,
            capabilities: vec![],
        },
    ] {
        let mut fixture = Fixture::new(CATALOG);
        fixture.record(kind, 100, 200);
        let probe = fixture.begin(200).unwrap();
        assert!(
            probe
                .attempt
                .reasons
                .iter()
                .any(|reason| reason == "initial_conversation_readiness_unknown")
        );
        finish(
            &mut fixture,
            &probe.attempt.request.id,
            AttemptCompletion {
                outcome: AttemptOutcome::Succeeded,
                effects: Effects::Committed,
                finished_at_ms: 201,
                retry_at_ms: None,
            },
        );
        let state = connection_state(&fixture.database, &fixture.binding.connection, 202).unwrap();
        assert_eq!(state.health, Health::Ready);
        assert_eq!(state.observed_at_ms, Some(201));
    }
}

#[test]
fn an_older_retry_time_still_denies_a_probe_after_newer_readiness_expired() {
    let mut fixture = Fixture::new(CATALOG);
    observe_at(
        &mut fixture,
        1,
        100,
        5_000,
        ObservationKind::RateLimited {
            retry_at_ms: Some(5_000),
        },
    );
    observe_at(
        &mut fixture,
        2,
        200,
        300,
        ObservationKind::Ready {
            remaining: None,
            capabilities: vec![],
        },
    );
    let state = connection_state(&fixture.database, &fixture.binding.connection, 300).unwrap();
    assert_eq!(state.health, Health::Unknown);
    assert_eq!(state.cooldown_until_ms, Some(5_000));
    fixture.reject(4_999, "gateway_cooldown");
    fixture.begin(5_000).unwrap();
}

#[test]
fn a_failed_turn_holds_its_connection_only_until_its_retry_time_or_the_failure_hold() {
    for (kind, retry_at_ms, held_until_ms, expected) in [
        (
            FailureKind::RateLimited,
            None,
            102 + FAILURE_HOLD_MS,
            "gateway_rate_limited",
        ),
        (
            FailureKind::RateLimited,
            Some(5_000),
            5_000,
            "gateway_rate_limited",
        ),
        (
            FailureKind::TransientFailure,
            None,
            102 + FAILURE_HOLD_MS,
            "gateway_transient_failure",
        ),
        (
            FailureKind::QuotaExhausted,
            None,
            102 + FAILURE_HOLD_MS,
            "gateway_quota_exhausted",
        ),
        (
            FailureKind::InvalidAuthentication,
            None,
            102 + FAILURE_HOLD_MS,
            "gateway_invalid_authentication",
        ),
    ] {
        let mut fixture = Fixture::new(CATALOG);
        let first = fixture.begin(100).unwrap();
        finish(
            &mut fixture,
            &first.attempt.request.id,
            AttemptCompletion {
                outcome: AttemptOutcome::Failed(kind),
                effects: Effects::Uncertain,
                finished_at_ms: 102,
                retry_at_ms,
            },
        );
        next_turn(&mut fixture, "next-turn");
        fixture.reject(held_until_ms - 1, expected);
        let probe = fixture.begin(held_until_ms).unwrap();
        assert!(probe.attempt.request.previous_attempt.is_none());
    }
}

#[test]
fn a_busy_day_of_observations_keeps_the_connection_admissible_and_its_history_bounded() {
    let mut fixture = Fixture::new(CATALOG);
    observe_at(
        &mut fixture,
        1,
        1_000,
        1_100,
        ObservationKind::RateLimited {
            retry_at_ms: Some(1_100),
        },
    );
    observe_at(
        &mut fixture,
        2,
        1_001,
        1_000_000,
        ObservationKind::TransientFailure {
            retry_at_ms: Some(900_000),
        },
    );
    for sequence in 3..=300 {
        observe_at(
            &mut fixture,
            sequence,
            1_000 + sequence,
            1_000 + sequence + 3_600_000,
            ObservationKind::Ready {
                remaining: None,
                capabilities: vec![],
            },
        );
    }
    let busy = connection_state(&fixture.database, &fixture.binding.connection, 1_400).unwrap();
    assert_eq!(busy.health, Health::Ready);
    assert_eq!(busy.cooldown_until_ms, Some(900_000));
    let rows: i64 = fixture
        .database
        .query_row(
            "SELECT count(*) FROM model_gateway_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2);
    let rested = connection_state(&fixture.database, &fixture.binding.connection, 900_000).unwrap();
    assert_eq!(rested.health, Health::Ready);
    assert_eq!(rested.cooldown_until_ms, None);
    next_turn(&mut fixture, "rested-turn");
    let tx = fixture.database.transaction().unwrap();
    begin_attempt(
        &tx,
        &fixture.config,
        &fixture.config,
        &fixture.request,
        900_000,
    )
    .unwrap();
    tx.commit().unwrap();
}
