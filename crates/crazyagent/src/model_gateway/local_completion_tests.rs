use super::*;
use crate::config::resolve_config;

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
model = { id = "coding-model", author = "model-author" }
auth = { method = "native_managed", profile_ref = "auth-profile-a" }
[[model_access.pools]]
id = "worker-pool"
members = ["connection-a"]
projects = ["project-a"]
roles = ["worker"]
fallback_allowed = false
"#;

type Finish = fn(&Transaction<'_>, &RequestId, &AttemptCompletion) -> Result<(), EngineError>;
type ObservationRow = (String, String, i64, String);
type AttemptRow = (String, String, Option<String>, String, Option<String>);

struct Fixture {
    database: Connection,
    directory: tempfile::TempDir,
    config: EffectiveConfig,
    request: AttemptRequest,
    original: Observation,
    begun: Attempt,
}

fn finishers() -> [(&'static str, Finish); 2] {
    [
        ("local", finish_attempt_without_observation),
        ("ordinary", finish_attempt),
    ]
}

fn completion() -> AttemptCompletion {
    AttemptCompletion {
        outcome: AttemptOutcome::Failed(FailureKind::Unknown),
        effects: Effects::None,
        finished_at_ms: 200,
        retry_at_ms: None,
    }
}

fn observation_rows(database: &Connection) -> Vec<ObservationRow> {
    database
        .prepare(
            "SELECT id,connection_revision,sequence,body
             FROM model_gateway_observations ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn attempt_rows(database: &Connection) -> Vec<AttemptRow> {
    database
        .prepare(
            "SELECT id,task_id,previous_id,body,completion
             FROM model_gateway_attempts ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, CATALOG).unwrap();
        let config = resolve_config(&global, &directory.path().join("project.toml")).unwrap();
        let mut database = Connection::open(directory.path().join("gateway.sqlite3")).unwrap();
        initialize(&database).unwrap();
        let request = AttemptRequest {
            id: RequestId::new("local-completion-attempt").unwrap(),
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
            previous_attempt: None,
        };
        let original = Observation {
            id: RequestId::new("original-provider-observation").unwrap(),
            connection: config.settings.model_access.as_ref().unwrap().connections[0].clone(),
            sequence: 7,
            observed_at_ms: 100,
            valid_until_ms: 10_000,
            kind: ObservationKind::Ready {
                remaining: None,
                capabilities: vec!["coding".into()],
            },
        };
        let tx = database.transaction().unwrap();
        observe(&tx, &config, &original).unwrap();
        let selected = begin_attempt(&tx, &config, &config, &request, 100).unwrap();
        assert!(!selected.replayed);
        assert!(selected.attempt.completion.is_none());
        assert_eq!(selected.attempt.connection, original.connection);
        tx.commit().unwrap();
        assert_eq!(
            read_attempt(&database, &request.id).unwrap(),
            Some(selected.attempt.clone())
        );
        Self {
            database,
            directory,
            config,
            request,
            original,
            begun: selected.attempt,
        }
    }

    fn persisted(&self) -> Attempt {
        read_attempt(&self.database, &self.request.id)
            .unwrap()
            .unwrap()
    }
}

#[test]
fn local_completion_unknown_persists_attempt_without_changing_provider_observation() {
    let mut fixture = Fixture::new();
    let before = observation_rows(&fixture.database);
    assert_eq!(before.len(), 1);
    let state = connection_state(&fixture.database, &fixture.original.connection, 200).unwrap();
    let finished = completion();
    let tx = fixture.database.transaction().unwrap();
    finish_attempt_without_observation(&tx, &fixture.request.id, &finished).unwrap();
    tx.commit().unwrap();
    let reopened = Connection::open(fixture.directory.path().join("gateway.sqlite3")).unwrap();
    let mut expected = fixture.begun.clone();
    expected.completion = Some(finished);
    assert_eq!(
        read_attempt(&reopened, &fixture.request.id).unwrap(),
        Some(expected.clone())
    );
    assert_eq!(observation_rows(&reopened), before);
    assert_eq!(
        connection_state(&reopened, &fixture.original.connection, 200).unwrap(),
        state
    );
    assert_eq!(state.health, Health::Ready);
    assert_eq!(state.quota, Quota::Unknown);
    let tx = fixture.database.transaction().unwrap();
    let replay =
        begin_attempt(&tx, &fixture.config, &fixture.config, &fixture.request, 201).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.attempt, expected);
    assert_eq!(attempt_rows(&tx).len(), 1);
    assert_eq!(observation_rows(&tx), before);
    tx.commit().unwrap();
}

#[test]
fn local_completion_ordinary_finish_still_records_provider_failure_observation() {
    let mut fixture = Fixture::new();
    let before = observation_rows(&fixture.database);
    let finished = completion();
    let tx = fixture.database.transaction().unwrap();
    finish_attempt(&tx, &fixture.request.id, &finished).unwrap();
    tx.commit().unwrap();
    let after = observation_rows(&fixture.database);
    assert_eq!(after.len(), before.len() + 1);
    assert!(after.contains(&before[0]));
    let added = after.iter().find(|row| row.0 != before[0].0).unwrap();
    let observed: Observation = serde_json::from_str(&added.3).unwrap();
    assert_eq!(
        observed.id.as_str(),
        format!(
            "failure:{}",
            crate::memory::hash(fixture.request.id.as_str().as_bytes())
        )
    );
    assert_eq!(observed.connection, fixture.original.connection);
    assert_eq!(observed.sequence, fixture.original.sequence + 1);
    assert_eq!(observed.observed_at_ms, finished.finished_at_ms);
    assert_eq!(
        observed.valid_until_ms,
        finished.finished_at_ms + OBSERVATION_LIFETIME_MAX_MS
    );
    assert_eq!(observed.kind, ObservationKind::Unknown);
    assert_eq!(fixture.persisted().completion, Some(finished));
    assert_eq!(
        connection_state(&fixture.database, &fixture.original.connection, 200)
            .unwrap()
            .health,
        Health::Unknown
    );
}

#[test]
fn local_completion_replays_and_conflicts_match_ordinary_finish_across_both_apis() {
    for (initial_name, initial_finish) in finishers() {
        let mut fixture = Fixture::new();
        let finished = completion();
        let tx = fixture.database.transaction().unwrap();
        initial_finish(&tx, &fixture.request.id, &finished).unwrap();
        tx.commit().unwrap();
        let observations = observation_rows(&fixture.database);
        let attempts = attempt_rows(&fixture.database);
        let mut conflicts = Vec::new();
        let mut changed = finished.clone();
        changed.effects = Effects::Uncertain;
        conflicts.push(changed);
        let mut changed = finished.clone();
        changed.outcome = AttemptOutcome::Succeeded;
        conflicts.push(changed);
        let mut changed = finished.clone();
        changed.finished_at_ms = 99;
        conflicts.push(changed);
        let mut changed = finished.clone();
        changed.retry_at_ms = Some(201);
        conflicts.push(changed);
        for (replay_name, replay_finish) in finishers() {
            let tx = fixture.database.transaction().unwrap();
            replay_finish(&tx, &fixture.request.id, &finished).unwrap();
            for conflict in &conflicts {
                assert_eq!(
                    replay_finish(&tx, &fixture.request.id, conflict)
                        .unwrap_err()
                        .0,
                    "gateway_completion_conflict",
                    "initial {initial_name}, replay {replay_name}"
                );
            }
            tx.commit().unwrap();
            assert_eq!(observation_rows(&fixture.database), observations);
            assert_eq!(attempt_rows(&fixture.database), attempts);
        }
    }
}

#[test]
fn local_completion_bounds_match_ordinary_finish_and_preserve_active_attempt() {
    let mut invalid = Vec::new();
    let mut value = completion();
    value.finished_at_ms = 99;
    invalid.push(value);
    let mut value = completion();
    value.finished_at_ms = i64::MAX as u64 - OBSERVATION_LIFETIME_MAX_MS + 1;
    invalid.push(value);
    for retry_at_ms in [200, 199, 200 + OBSERVATION_LIFETIME_MAX_MS + 1, u64::MAX] {
        let mut value = completion();
        value.outcome = AttemptOutcome::Failed(FailureKind::RateLimited);
        value.retry_at_ms = Some(retry_at_ms);
        invalid.push(value);
    }
    for outcome in [
        AttemptOutcome::Failed(FailureKind::Unknown),
        AttemptOutcome::Failed(FailureKind::InvalidAuthentication),
        AttemptOutcome::Succeeded,
    ] {
        let mut value = completion();
        value.outcome = outcome;
        value.retry_at_ms = Some(201);
        invalid.push(value);
    }
    let valid = [
        AttemptCompletion {
            finished_at_ms: 100,
            ..completion()
        },
        AttemptCompletion {
            finished_at_ms: i64::MAX as u64 - OBSERVATION_LIFETIME_MAX_MS,
            ..completion()
        },
        AttemptCompletion {
            outcome: AttemptOutcome::Failed(FailureKind::RateLimited),
            retry_at_ms: Some(200 + OBSERVATION_LIFETIME_MAX_MS),
            ..completion()
        },
    ];
    for (name, finish) in finishers() {
        let mut fixture = Fixture::new();
        let observations = observation_rows(&fixture.database);
        let attempts = attempt_rows(&fixture.database);
        for value in &invalid {
            let tx = fixture.database.transaction().unwrap();
            assert_eq!(
                finish(&tx, &fixture.request.id, value).unwrap_err().0,
                "gateway_completion_bounds",
                "finisher {name}, completion {value:?}"
            );
            tx.commit().unwrap();
            assert_eq!(observation_rows(&fixture.database), observations);
            assert_eq!(attempt_rows(&fixture.database), attempts);
            assert_eq!(fixture.persisted(), fixture.begun);
        }
        for value in &valid {
            let mut boundary = Fixture::new();
            let observations = observation_rows(&boundary.database);
            let tx = boundary.database.transaction().unwrap();
            finish(&tx, &boundary.request.id, value).unwrap();
            tx.commit().unwrap();
            assert_eq!(boundary.persisted().completion.as_ref(), Some(value));
            if name == "local" {
                assert_eq!(observation_rows(&boundary.database), observations);
            } else {
                assert_eq!(
                    observation_rows(&boundary.database).len(),
                    observations.len() + 1
                );
            }
        }
    }
}

#[test]
fn local_completion_missing_attempt_matches_ordinary_finish_without_writes() {
    let missing = RequestId::new("missing-local-completion-attempt").unwrap();
    for (name, finish) in finishers() {
        let mut fixture = Fixture::new();
        let observations = observation_rows(&fixture.database);
        let attempts = attempt_rows(&fixture.database);
        let tx = fixture.database.transaction().unwrap();
        assert_eq!(
            finish(&tx, &missing, &completion()).unwrap_err().0,
            "gateway_attempt_missing",
            "finisher {name}"
        );
        tx.commit().unwrap();
        assert!(read_attempt(&fixture.database, &missing).unwrap().is_none());
        assert_eq!(observation_rows(&fixture.database), observations);
        assert_eq!(attempt_rows(&fixture.database), attempts);
    }
}

#[test]
fn local_completion_caller_rollback_preserves_attempt_and_observation_rows() {
    for (name, finish) in finishers() {
        let mut fixture = Fixture::new();
        let observations = observation_rows(&fixture.database);
        let attempts = attempt_rows(&fixture.database);
        let finished = completion();
        let tx = fixture.database.transaction().unwrap();
        finish(&tx, &fixture.request.id, &finished).unwrap();
        assert_eq!(
            read_attempt(&tx, &fixture.request.id)
                .unwrap()
                .unwrap()
                .completion,
            Some(finished)
        );
        tx.rollback().unwrap();
        assert_eq!(
            observation_rows(&fixture.database),
            observations,
            "finisher {name}"
        );
        assert_eq!(attempt_rows(&fixture.database), attempts, "finisher {name}");
        assert_eq!(fixture.persisted(), fixture.begun);
    }
}

#[test]
fn ordinary_success_records_ready_observation_that_admits_the_next_attempt() {
    let mut fixture = Fixture::new();
    let finished = AttemptCompletion {
        outcome: AttemptOutcome::Succeeded,
        effects: Effects::Committed,
        finished_at_ms: 20_000,
        retry_at_ms: None,
    };
    assert_eq!(
        connection_state(&fixture.database, &fixture.original.connection, 20_001)
            .unwrap()
            .health,
        Health::Unknown
    );
    let tx = fixture.database.transaction().unwrap();
    finish_attempt(&tx, &fixture.request.id, &finished).unwrap();
    tx.commit().unwrap();
    let state = connection_state(&fixture.database, &fixture.original.connection, 20_001).unwrap();
    assert_eq!(state.health, Health::Ready);
    assert_eq!(state.quota, Quota::Unknown);
    assert_eq!(state.capabilities, vec!["coding".to_owned()]);
    let mut next = fixture.request.clone();
    next.id = RequestId::new("local-completion-next-attempt").unwrap();
    let tx = fixture.database.transaction().unwrap();
    let selected = begin_attempt(&tx, &fixture.config, &fixture.config, &next, 20_001).unwrap();
    assert!(!selected.replayed);
    assert_eq!(selected.attempt.connection, fixture.original.connection);
    tx.commit().unwrap();
}
