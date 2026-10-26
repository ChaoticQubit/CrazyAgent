#![cfg(unix)]

use crazyagent::config::{EffectiveConfig, resolve_config};
use crazyagent::engine::Engine;
use crazyagent::protocol::*;
use crazyagent::protocol::{Command, WireRequest};
use crazyagent::verification::*;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

fn id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn draft() -> BaselineDraft {
    BaselineDraft {
        version: 1,
        project_id: ProjectId::new("project").unwrap(),
        origin: BaselineOrigin::Imported,
        requirements: "Return a correct sum and detect weakened tests".into(),
        checks: vec![
            CheckSpec {
                id: id("code"),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"assert_eq!(sum(2, 3), 5)".to_vec(),
            },
            CheckSpec {
                id: id("integrity"),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"reject selected broken implementations".to_vec(),
            },
            CheckSpec {
                id: id("holdout"),
                kind: CheckKind::Holdout,
                minimum_risk: Risk::High,
                content: b"approved additional cases".to_vec(),
            },
        ],
        rules: vec![RiskRule {
            path_prefix: "src".into(),
            minimum: Risk::Medium,
        }],
        retry_limit: 0,
    }
}

fn snapshot() -> CandidateSnapshot {
    CandidateSnapshot {
        files: BTreeMap::from([(
            "src/main.rs".into(),
            b"fn sum(a:i32,b:i32)->i32 { a+b }".to_vec(),
        )]),
        head_digest: "a".repeat(64),
        target_digest: "b".repeat(64),
    }
}

struct Harness {
    root: tempfile::TempDir,
    engine: Engine,
    config: EffectiveConfig,
    revision: u64,
    sequence: u64,
}

impl Harness {
    async fn open() -> Self {
        let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
        let engine = Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap();
        Self {
            root,
            engine,
            config,
            revision: 0,
            sequence: 0,
        }
    }

    fn request(&mut self, command: Command) -> WireRequest {
        self.sequence += 1;
        WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("host").unwrap(),
            request_id: id(&format!("op-{}", self.sequence)),
            expected_revision: Cursor::new(self.revision),
            command,
        }
    }

    async fn call(&mut self, command: Command) -> Outcome {
        let request = self.request(command);
        let reply = self.engine.apply(request).await.unwrap();
        self.revision = reply.revision.value();
        reply.outcome
    }

    async fn owner(&mut self, action: OwnerVerification) -> Outcome {
        self.call(Command::OwnerVerification { action }).await
    }

    async fn setup(&mut self) -> BaselineId {
        assert_eq!(
            self.call(Command::CreateProject {
                project_id: ProjectId::new("project").unwrap(),
                spec: ProjectSpec {
                    name: "project".into(),
                    repositories: vec![]
                }
            })
            .await,
            Outcome::Applied
        );
        let baseline_id = draft().identity().unwrap();
        assert_eq!(
            self.call(Command::ProposeBaseline { draft: draft() }).await,
            Outcome::Applied
        );
        assert_eq!(
            self.owner(OwnerVerification::Approve {
                baseline_id: baseline_id.clone()
            })
            .await,
            Outcome::Applied
        );
        assert_eq!(
            self.call(Command::PinTask {
                context: ExecutionContext {
                    project_id: ProjectId::new("project").unwrap(),
                    task_id: TaskId::new("task").unwrap(),
                    config_id: self.config.id.clone(),
                    snapshot_id: SnapshotId::new("initial").unwrap(),
                    baseline_id: baseline_id.clone(),
                    allowed_destinations: self.config.settings.allowed_destinations.clone(),
                    budget_ref: CredentialRef::new("local").unwrap(),
                }
            })
            .await,
            Outcome::Applied
        );
        assert_eq!(
            self.owner(OwnerVerification::Register {
                id: id("candidate"),
                task_id: TaskId::new("task").unwrap(),
                snapshot: snapshot(),
                environment_id: EnvironmentId::new("controlled-fixture").unwrap()
            })
            .await,
            Outcome::Applied
        );
        baseline_id
    }

    async fn state(&mut self) -> VerificationState {
        match self
            .owner(OwnerVerification::Inspect {
                candidate_id: id("candidate"),
            })
            .await
        {
            Outcome::Verification { state } => *state,
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn human_admission_is_exact_and_current_baseline_does_not_rewrite_pin() {
    let mut h = Harness::open().await;
    let original = h.setup().await;
    let state = h.state().await;
    assert_eq!(state.current.baseline_id, original);
    let mut updated = draft();
    updated
        .requirements
        .push_str(" and preserve overflow behavior");
    let next = updated.identity().unwrap();
    assert_ne!(next, original);
    assert_eq!(
        h.owner(OwnerVerification::Approve {
            baseline_id: next.clone()
        })
        .await,
        Outcome::Rejected {
            code: "baseline_missing".into()
        }
    );
    assert_eq!(
        h.call(Command::ProposeBaseline { draft: updated }).await,
        Outcome::Applied
    );
    assert_eq!(h.state().await.current.baseline_id, original);
    assert_eq!(
        h.owner(OwnerVerification::Approve {
            baseline_id: next.clone()
        })
        .await,
        Outcome::Applied
    );
    let current = h.state().await;
    assert_eq!(current.current.baseline_id, next);
    assert_eq!(current.candidate.pinned_baseline_id, original);
    match h
        .call(Command::TaskContext {
            task_id: TaskId::new("task").unwrap(),
        })
        .await
    {
        Outcome::Task { context } => assert_eq!(context.baseline_id, original),
        other => panic!("{other:?}"),
    }
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn unavailable_attempts_survive_restart_replay_and_revocation() {
    let mut h = Harness::open().await;
    let baseline_id = h.setup().await;
    let request = h.request(Command::OwnerVerification {
        action: OwnerVerification::Verify {
            candidate_id: id("candidate"),
            attempt_id: id("attempt"),
        },
    });
    let first = h.engine.apply(request.clone()).await.unwrap();
    assert_eq!(first.outcome, Outcome::Applied);
    h.revision = first.revision.value();
    assert_eq!(h.engine.apply(request.clone()).await.unwrap(), first);
    let state = h.state().await;
    assert_eq!(state.attempts.len(), 1);
    assert!(
        state.attempts[0]
            .1
            .outcomes
            .values()
            .all(|v| *v == CheckOutcome::Unavailable)
    );
    h.engine.close().await.unwrap();
    h.engine = Engine::open(&h.root.path().join("db"), h.config.clone())
        .await
        .unwrap();
    assert_eq!(h.state().await, state);
    assert_eq!(h.engine.apply(request.clone()).await.unwrap(), first);
    assert_eq!(
        h.owner(OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt")
        })
        .await,
        Outcome::Rejected {
            code: "required_evidence_not_passed".into()
        }
    );
    assert_eq!(
        h.owner(OwnerVerification::Revoke { baseline_id }).await,
        Outcome::Applied
    );
    assert_eq!(
        h.owner(OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt")
        })
        .await,
        Outcome::Rejected {
            code: "candidate_authority_revoked".into()
        }
    );
    assert_eq!(h.engine.apply(request).await.unwrap(), first);
    h.engine.close().await.unwrap();
    let db = rusqlite::Connection::open(h.root.path().join("db")).unwrap();
    let body: String = db
        .query_row(
            "SELECT body FROM verification_attempts WHERE candidate='candidate' AND id='attempt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let kept: VerificationReceipt = serde_json::from_str(&body).unwrap();
    assert_eq!(kept.outcomes, state.attempts[0].1.outcomes);
}

#[tokio::test]
async fn independent_escalation_and_full_suite_only_add_checks() {
    let mut h = Harness::open().await;
    h.setup().await;
    assert_eq!(h.state().await.risk.deterministic_floor, Risk::Medium);
    assert_eq!(h.state().await.current.checks.len(), 2);
    assert_eq!(
        h.owner(OwnerVerification::Escalate {
            candidate_id: id("candidate"),
            risk: Risk::High
        })
        .await,
        Outcome::Applied
    );
    let raised = h.state().await;
    assert_eq!(raised.current.checks.len(), 3);
    assert_eq!(raised.risk.raised_risk, Risk::High);
    assert_eq!(
        h.owner(OwnerVerification::Escalate {
            candidate_id: id("candidate"),
            risk: Risk::Low
        })
        .await,
        Outcome::Applied
    );
    assert_eq!(h.state().await.risk.raised_risk, Risk::High);
    assert_eq!(
        h.owner(OwnerVerification::FullSuite {
            candidate_id: id("candidate")
        })
        .await,
        Outcome::Applied
    );
    assert!(h.state().await.candidate.full_suite);
    assert_eq!(
        h.owner(OwnerVerification::Escalate {
            candidate_id: id("candidate"),
            risk: Risk::Unknown
        })
        .await,
        Outcome::Applied
    );
    assert_eq!(h.state().await.risk.raised_risk, Risk::Unknown);
    assert_eq!(h.state().await.current.checks.len(), 3);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn stale_receipt_never_accepts_and_json_success_is_unproven() {
    let mut h = Harness::open().await;
    h.setup().await;
    h.owner(OwnerVerification::Verify {
        candidate_id: id("candidate"),
        attempt_id: id("attempt"),
    })
    .await;
    let state = h.state().await;
    let receipt = &state.attempts[0].1;
    let mut changed = vec![];
    let mut request = state.current.clone();
    request.baseline_id = BaselineId::new("c".repeat(64)).unwrap();
    changed.push(request);
    let mut request = state.current.clone();
    request.config_id = ConfigId::new("c".repeat(64)).unwrap();
    changed.push(request);
    let mut request = state.current.clone();
    request.environment_id = EnvironmentId::new("changed").unwrap();
    changed.push(request);
    let mut request = state.current.clone();
    request.head_digest = "c".repeat(64);
    changed.push(request);
    let mut request = state.current.clone();
    request.target_digest = "c".repeat(64);
    changed.push(request);
    let mut request = state.current.clone();
    request.source_digest = "c".repeat(64);
    changed.push(request);
    let mut request = state.current.clone();
    request.snapshot_id = SnapshotId::new("c".repeat(64)).unwrap();
    changed.push(request);
    let mut request = state.current.clone();
    request.checks.insert(id("extra"), "c".repeat(64));
    changed.push(request);
    for request in changed {
        assert_eq!(
            receipt.accepts(&request).unwrap_err().0,
            "stale_verification_inputs"
        );
    }
    let mut forged = serde_json::to_value(receipt).unwrap();
    forged["outcomes"] = serde_json::to_value(
        state
            .current
            .checks
            .keys()
            .map(|id| (id.clone(), CheckOutcome::Passed))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    let forged: VerificationReceipt = serde_json::from_value(forged.clone()).unwrap();
    assert_eq!(
        forged.accepts(&state.current).unwrap_err().0,
        "unproven_execution"
    );
    let mut injected = serde_json::to_value(&forged).unwrap();
    injected["execution_proven"] = true.into();
    assert!(serde_json::from_value::<VerificationReceipt>(injected).is_err());
    for outcome in [
        CheckOutcome::Failed,
        CheckOutcome::Interrupted,
        CheckOutcome::TimedOut,
        CheckOutcome::Unknown,
        CheckOutcome::Conflicting,
        CheckOutcome::Unavailable,
    ] {
        let mut receipt = forged.clone();
        receipt.outcomes.insert(id("code"), outcome);
        assert_eq!(
            receipt.accepts(&state.current).unwrap_err().0,
            "required_evidence_not_passed"
        );
    }
    let mut missing = forged.clone();
    missing.outcomes.remove(&id("code"));
    assert_eq!(
        missing.accepts(&state.current).unwrap_err().0,
        "required_evidence_not_passed"
    );
    let mut tampered = forged.clone();
    tampered.artifacts.values_mut().next().unwrap().push(0);
    assert_eq!(
        tampered.accepts(&state.current).unwrap_err().0,
        "missing_or_tampered_artifact"
    );
    let mut missing = forged.clone();
    missing.artifacts.clear();
    assert_eq!(
        missing.accepts(&state.current).unwrap_err().0,
        "missing_or_tampered_artifact"
    );
    let mut changed = state.current.clone();
    changed
        .checks
        .values_mut()
        .next()
        .unwrap()
        .replace_range(..1, "d");
    assert_eq!(
        forged.accepts(&changed).unwrap_err().0,
        "stale_verification_inputs"
    );
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn retained_attempt_history_remains_bounded_and_retrievable() {
    let mut h = Harness::open().await;
    h.setup().await;
    for n in 0..100 {
        assert_eq!(
            h.owner(OwnerVerification::Verify {
                candidate_id: id("candidate"),
                attempt_id: id(&format!("attempt-{n:03}"))
            })
            .await,
            Outcome::Applied
        );
    }
    let state = h.state().await;
    assert!(!state.attempts.is_empty());
    assert!(state.attempts.len() <= 2);
    assert_eq!(serde_json::to_value(&state).unwrap()["attempts_total"], 100);
    let mut seen = std::collections::BTreeSet::new();
    let mut page = state;
    loop {
        for (attempt, _) in page.attempts {
            assert!(seen.insert(attempt));
        }
        let Some(after) = page.attempts_next else {
            break;
        };
        page = match h
            .owner(OwnerVerification::History {
                candidate_id: id("candidate"),
                after,
            })
            .await
        {
            Outcome::Verification { state } => *state,
            other => panic!("{other:?}"),
        };
        assert_eq!(page.attempts_total, 100);
        assert!(page.attempts.len() <= 2);
        assert!(seen.len() <= 100);
    }
    assert_eq!(seen.len(), 100);
    assert_eq!(
        h.owner(OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt-099")
        })
        .await,
        Outcome::Rejected {
            code: "required_evidence_not_passed".into()
        }
    );
    for n in 100..128 {
        assert_eq!(
            h.owner(OwnerVerification::Verify {
                candidate_id: id("candidate"),
                attempt_id: id(&format!("attempt-{n:03}"))
            })
            .await,
            Outcome::Applied
        );
    }
    let overflow = h.request(Command::OwnerVerification {
        action: OwnerVerification::Verify {
            candidate_id: id("candidate"),
            attempt_id: id("overflow"),
        },
    });
    assert_eq!(
        h.engine.apply(overflow).await.unwrap_err().0,
        "verification_capacity"
    );
    assert_eq!(h.state().await.attempts_total, 128);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn storage_interruption_rolls_back_admission_and_operation_acknowledgement() {
    let mut h = Harness::open().await;
    assert_eq!(
        h.call(Command::CreateProject {
            project_id: ProjectId::new("project").unwrap(),
            spec: ProjectSpec {
                name: "project".into(),
                repositories: vec![]
            }
        })
        .await,
        Outcome::Applied
    );
    assert_eq!(
        h.call(Command::ProposeBaseline { draft: draft() }).await,
        Outcome::Applied
    );
    h.engine.close().await.unwrap();
    let database = rusqlite::Connection::open(h.root.path().join("db")).unwrap();
    database
        .execute_batch(
            "CREATE TRIGGER interrupt_admission BEFORE INSERT ON verification_current
        BEGIN SELECT RAISE(ABORT,'storage interruption'); END;",
        )
        .unwrap();
    h.engine = Engine::open(&h.root.path().join("db"), h.config.clone())
        .await
        .unwrap();
    let request = h.request(Command::OwnerVerification {
        action: OwnerVerification::Approve {
            baseline_id: draft().identity().unwrap(),
        },
    });
    assert!(
        h.engine
            .apply(request.clone())
            .await
            .unwrap_err()
            .0
            .contains("storage interruption")
    );
    let approved: bool = database
        .query_row("SELECT approved FROM verification_baselines", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!approved);
    let acknowledged: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE id=?1",
            [request.request_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(acknowledged, 0);
    let revision: i64 = database
        .query_row("SELECT revision FROM state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(revision as u64, h.revision);
    h.engine.close().await.unwrap();
    database
        .execute_batch("DROP TRIGGER interrupt_admission;")
        .unwrap();
    drop(database);
    h.engine = Engine::open(&h.root.path().join("db"), h.config.clone())
        .await
        .unwrap();
    assert_eq!(
        h.engine.apply(request).await.unwrap().outcome,
        Outcome::Applied
    );
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn persistent_source_baseline_and_receipt_tampering_cannot_pass() {
    let mut h = Harness::open().await;
    let baseline_id = h.setup().await;
    h.owner(OwnerVerification::Verify {
        candidate_id: id("candidate"),
        attempt_id: id("attempt"),
    })
    .await;
    let original = h.state().await;
    h.engine.close().await.unwrap();
    let database = rusqlite::Connection::open(h.root.path().join("db")).unwrap();
    let mut candidate = original.candidate.clone();
    candidate
        .snapshot
        .files
        .values_mut()
        .next()
        .unwrap()
        .push(0);
    database
        .execute(
            "UPDATE verification_candidates SET body=?1",
            [serde_json::to_string(&candidate).unwrap()],
        )
        .unwrap();
    h.engine = Engine::open(&h.root.path().join("db"), h.config.clone())
        .await
        .unwrap();
    assert_eq!(
        h.owner(OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt")
        })
        .await,
        Outcome::Rejected {
            code: "candidate_tampered".into()
        }
    );
    h.engine.close().await.unwrap();
    database
        .execute(
            "UPDATE verification_candidates SET body=?1",
            [serde_json::to_string(&original.candidate).unwrap()],
        )
        .unwrap();
    let mut changed = draft();
    changed.checks[0].content = b"weakened check".to_vec();
    database
        .execute(
            "UPDATE verification_baselines SET body=?1 WHERE id=?2",
            rusqlite::params![
                serde_json::to_string(&changed).unwrap(),
                baseline_id.as_str()
            ],
        )
        .unwrap();
    h.engine = Engine::open(&h.root.path().join("db"), h.config.clone())
        .await
        .unwrap();
    assert_eq!(
        h.owner(OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt")
        })
        .await,
        Outcome::Rejected {
            code: "baseline_tampered".into()
        }
    );
    h.engine.close().await.unwrap();
    database
        .execute(
            "UPDATE verification_baselines SET body=?1",
            [serde_json::to_string(&draft()).unwrap()],
        )
        .unwrap();
    let mut receipt = original.attempts[0].1.clone();
    receipt.artifacts.clear();
    database
        .execute(
            "UPDATE verification_attempts SET body=?1",
            [serde_json::to_string(&receipt).unwrap()],
        )
        .unwrap();
    h.engine = Engine::open(&h.root.path().join("db"), h.config.clone())
        .await
        .unwrap();
    assert_eq!(
        h.owner(OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt")
        })
        .await,
        Outcome::Rejected {
            code: "missing_or_tampered_artifact".into()
        }
    );
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn full_suite_adds_optional_checks_and_candidate_identity_cannot_be_replaced() {
    let mut h = Harness::open().await;
    h.setup().await;
    assert_eq!(h.state().await.current.checks.len(), 2);
    assert_eq!(
        h.owner(OwnerVerification::FullSuite {
            candidate_id: id("candidate")
        })
        .await,
        Outcome::Applied
    );
    assert_eq!(h.state().await.current.checks.len(), 3);
    let original = h.state().await.candidate;
    let mut changed = snapshot();
    changed.files.values_mut().next().unwrap().push(0);
    assert_eq!(
        h.owner(OwnerVerification::Register {
            id: id("candidate"),
            task_id: TaskId::new("task").unwrap(),
            snapshot: changed,
            environment_id: EnvironmentId::new("different").unwrap()
        })
        .await,
        Outcome::Rejected {
            code: "candidate_exists".into()
        }
    );
    assert_eq!(h.state().await.candidate, original);
    h.engine.close().await.unwrap();
}

#[tokio::test]
async fn acceptance_replay_rechecks_immediate_destination_revocation() {
    let mut h = Harness::open().await;
    std::fs::write(h.root.path().join("g"), "allowed_destinations=['local']").unwrap();
    h.config = resolve_config(&h.root.path().join("g"), &h.root.path().join("p")).unwrap();
    h.engine.update_config(h.config.clone()).await.unwrap();
    h.setup().await;
    h.owner(OwnerVerification::Verify {
        candidate_id: id("candidate"),
        attempt_id: id("attempt"),
    })
    .await;
    let request = h.request(Command::OwnerVerification {
        action: OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt"),
        },
    });
    assert_eq!(
        h.engine.apply(request.clone()).await.unwrap().outcome,
        Outcome::Rejected {
            code: "required_evidence_not_passed".into()
        }
    );
    std::fs::write(
        h.root.path().join("g"),
        "allowed_destinations=['local']\nrevoked_destinations=['local']",
    )
    .unwrap();
    h.config = resolve_config(&h.root.path().join("g"), &h.root.path().join("p")).unwrap();
    h.engine.update_config(h.config.clone()).await.unwrap();
    assert_eq!(
        h.engine.apply(request.clone()).await.unwrap().outcome,
        Outcome::Rejected {
            code: "candidate_authority_revoked".into()
        }
    );
    h.engine.close().await.unwrap();
    h.engine = Engine::open(&h.root.path().join("db"), h.config.clone())
        .await
        .unwrap();
    assert_eq!(
        h.engine.apply(request).await.unwrap().outcome,
        Outcome::Rejected {
            code: "candidate_authority_revoked".into()
        }
    );
    h.engine.close().await.unwrap();
}

#[test]
fn bounded_imports_and_path_safe_snapshot_identity() {
    let original = draft();
    assert_eq!(
        BaselineDraft::import(&serde_json::to_vec(&original).unwrap()).unwrap(),
        original
    );
    for origin in [
        BaselineOrigin::Generated,
        BaselineOrigin::HumanAuthored,
        BaselineOrigin::Imported,
    ] {
        let mut draft = original.clone();
        draft.origin = origin;
        assert!(draft.identity().is_ok());
    }
    let mut padded = serde_json::to_vec(&original).unwrap();
    padded.resize(32768, b' ');
    assert_eq!(BaselineDraft::import(&padded).unwrap(), original);
    padded.push(b' ');
    assert_eq!(
        BaselineDraft::import(&padded).unwrap_err().0,
        "baseline_bounds"
    );
    let mut invalid = original.clone();
    invalid.retry_limit = 1;
    assert!(invalid.validate().is_err());
    let mut invalid = original.clone();
    invalid.checks.remove(1);
    assert!(invalid.validate().is_err());
    let mut invalid = original.clone();
    invalid.checks[1].minimum_risk = Risk::High;
    assert!(invalid.validate().is_err());
    let mut invalid = original.clone();
    invalid.checks.push(invalid.checks[0].clone());
    assert!(invalid.validate().is_err());
    let original = snapshot();
    let identity = original.identity().unwrap();
    for path in [
        "../receipt",
        "/baseline",
        "x/../policy",
        "x\\baseline",
        "x//check",
        "x/./check",
        "x:\u{1b}",
        "",
    ] {
        let mut snapshot = original.clone();
        snapshot.files = BTreeMap::from([(path.into(), vec![0])]);
        assert!(snapshot.identity().is_err(), "{path}");
    }
    let mut changed = original.clone();
    changed.files.values_mut().next().unwrap().push(0);
    assert_ne!(changed.identity().unwrap(), identity);
    let mut changed = original.clone();
    changed.target_digest = "c".repeat(64);
    assert_ne!(changed.identity().unwrap(), identity);
    let mut invalid = original.clone();
    invalid.files.insert("src".into(), vec![0]);
    assert!(invalid.identity().is_err());
    let mut invalid = original.clone();
    invalid.files.values_mut().next().unwrap().resize(16385, 0);
    assert!(invalid.identity().is_err());
}

#[tokio::test]
async fn current_configuration_and_operation_identity_are_enforced() {
    let mut h = Harness::open().await;
    h.setup().await;
    let request = h.request(Command::OwnerVerification {
        action: OwnerVerification::Verify {
            candidate_id: id("candidate"),
            attempt_id: id("attempt"),
        },
    });
    let first = h.engine.apply(request.clone()).await.unwrap();
    h.revision = first.revision.value();
    let mut conflict = request.clone();
    conflict.command = Command::OwnerVerification {
        action: OwnerVerification::Verify {
            candidate_id: id("candidate"),
            attempt_id: id("replacement"),
        },
    };
    assert_eq!(
        h.engine.apply(conflict).await.unwrap_err().0,
        "operation_identity_conflict"
    );
    let old = h.state().await.current;
    std::fs::write(h.root.path().join("p"), "timeout_ms=1200").unwrap();
    h.config = resolve_config(&h.root.path().join("g"), &h.root.path().join("p")).unwrap();
    h.engine.update_config(h.config.clone()).await.unwrap();
    let current = h.state().await.current;
    assert_ne!(old.config_id, current.config_id);
    assert_eq!(
        h.owner(OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt")
        })
        .await,
        Outcome::Rejected {
            code: "stale_verification_inputs".into()
        }
    );
    let mut stale = h.request(Command::OwnerVerification {
        action: OwnerVerification::FullSuite {
            candidate_id: id("candidate"),
        },
    });
    stale.expected_revision = Cursor::new(0);
    assert_eq!(
        h.engine.apply(stale).await.unwrap().outcome,
        Outcome::Rejected {
            code: "stale_revision".into()
        }
    );
    assert!(!h.state().await.candidate.full_suite);
    h.engine.close().await.unwrap();
}

#[test]
fn worker_can_draft_content_but_cannot_allocate_or_admit_a_baseline() {
    let draft = serde_json::json!({
        "version": 1, "project_id": "project", "origin": "Imported",
        "requirements": "Approved behavior must be checked",
        "checks": [
            {"id":"code", "kind":"Code", "minimum_risk":"Low", "content":[97]},
            {"id":"integrity", "kind":"TestIntegrity", "minimum_risk":"Low", "content":[98]}
        ], "rules": [], "retry_limit": 0
    });
    let command = serde_json::json!({"kind":"ProposeBaseline", "draft": draft});
    assert!(
        crazyagent::verification::BaselineDraft::import(&serde_json::to_vec(&draft).unwrap())
            .is_ok()
    );
    refused_variant(command, "ProposeBaseline");
    let baseline_id = || BaselineId::new("b".repeat(64)).unwrap();
    for action in [
        OwnerVerification::Approve {
            baseline_id: baseline_id(),
        },
        OwnerVerification::Revoke {
            baseline_id: baseline_id(),
        },
        OwnerVerification::Register {
            id: id("candidate"),
            task_id: TaskId::new("task").unwrap(),
            snapshot: snapshot(),
            environment_id: EnvironmentId::new("controlled-fixture").unwrap(),
        },
        OwnerVerification::Escalate {
            candidate_id: id("candidate"),
            risk: Risk::Low,
        },
        OwnerVerification::FullSuite {
            candidate_id: id("candidate"),
        },
        OwnerVerification::Verify {
            candidate_id: id("candidate"),
            attempt_id: id("attempt"),
        },
        OwnerVerification::Accept {
            candidate_id: id("candidate"),
            attempt_id: id("attempt"),
        },
    ] {
        let command = serde_json::to_value(Command::OwnerVerification { action }).unwrap();
        refused_variant(command, "OwnerVerification");
    }
    let request = serde_json::json!({
        "protocol_version":1,"client_id":"worker","request_id":"operation",
        "expected_revision":"0","command":{"kind":"ProposeBaseline","draft":draft}
    });
    let error = serde_json::from_value::<WireRequest>(request)
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("unknown variant `ProposeBaseline`"),
        "{error}"
    );
}

/// Checks that a well-formed command is refused only because its kind is hidden.
/// Takes the command JSON and its kind. Panics unless decoding fails with
/// serde's unknown-variant error for exactly that kind.
fn refused_variant(command: serde_json::Value, kind: &str) {
    let error = serde_json::from_value::<Command>(command)
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with(&format!("unknown variant `{kind}`")),
        "{kind}: {error}"
    );
}
