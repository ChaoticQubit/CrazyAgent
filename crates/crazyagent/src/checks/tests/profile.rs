use crate::checks::profile::{
    ARG_BYTES_MAX, ARGV_MAX, CHECKS_MAX, HOLDOUT_FILE_BYTES_MAX, HOLDOUT_FILES_MAX, HOLDOUTS_MAX,
    ID_BYTES_MAX, PROTECTED_PATHS_MAX, TIMEOUT_S_MAX, TIMEOUT_S_MIN,
};
use crate::checks::settings::{self, GateMode, ProtectedChange};
use crate::checks::*;
use crate::engine::EngineError;
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::ProjectId;
use crate::trust::{Origin, Trust};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

const AGENT: Actor = Actor::Agent(7);
const MASTER: Actor = Actor::Master(3);
const PENDING_CHECK_QUESTIONS: &str = "SELECT COUNT(*) FROM owner_questions WHERE project='alpha' AND asker='engine:checks' AND status='pending'";

/// One engine root with the real engine schema and two set-up projects, `alpha` and `beta`,
/// whose folders hold `.crazyagent/` for the questions page.
struct Case {
    _tmp: tempfile::TempDir,
    db: Connection,
    alpha: ProjectId,
    beta: ProjectId,
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ca-checks-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("engine");
        std::fs::create_dir(&root).unwrap();
        let db = crate::questions::open(&root).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::team::initialize(&db).unwrap();
        for name in ["alpha", "beta"] {
            let folder = base.join(name);
            std::fs::create_dir_all(folder.join(CRAZYAGENT_DIRECTORY)).unwrap();
            crate::ide::workspace::tests::add_project(&db, name, &folder);
        }
        Self {
            _tmp: tmp,
            db,
            alpha: ProjectId::new("alpha").unwrap(),
            beta: ProjectId::new("beta").unwrap(),
        }
    }

    fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn propose(
        &self,
        project: &ProjectId,
        profile: &Value,
        actor: Actor,
    ) -> Result<Proposal, EngineError> {
        propose(
            &self.db,
            project,
            &serde_json::to_vec(profile).unwrap(),
            actor,
        )
    }

    fn state(&self, id: &ProfileId) -> ProfileState {
        show(&self.db, &self.alpha, id).unwrap().unwrap().state
    }

    fn question_status(&self, question: u64) -> String {
        self.db
            .query_row(
                "SELECT status FROM owner_questions WHERE id=?1",
                [i64::try_from(question).unwrap()],
                |row| row.get(0),
            )
            .unwrap()
    }
}

fn code(text: &str) -> EngineError {
    EngineError(text.into())
}

fn unit_check(id: &str) -> Value {
    json!({
        "id": id,
        "kind": "unit",
        "toolchain": "rust",
        "argv": ["cargo", "test", "--locked", "--offline"],
        "cwd": ".",
        "format": "libtest",
        "report": null,
        "timeout_s": 1800,
        "network": "off",
        "when": ["before_accept"],
        "required": true,
        "protected": false
    })
}

fn profile(checks: Vec<Value>) -> Value {
    json!({
        "version": 1,
        "checks": checks,
        "holdouts": [],
        "protected_paths": ["tests/acceptance/"]
    })
}

fn changed(edit: impl FnOnce(&mut Value)) -> Value {
    let mut value = profile(vec![unit_check("unit")]);
    edit(&mut value);
    value
}

/// A distinct valid profile for each number: one unit check named after it.
fn numbered(n: usize) -> Value {
    profile(vec![unit_check(&format!("c{n}"))])
}

fn holdout(id: &str, files: Value) -> Value {
    json!({ "check": unit_check(id), "files": files })
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn an_agent_cannot_approve_a_profile_and_its_proposal_raises_one_owner_question() {
    let case = Case::new();
    let proposed = profile(vec![unit_check("unit")]);
    let first = case.propose(&case.alpha, &proposed, AGENT).unwrap();
    let question = first.question.expect("an agent proposal asks the owner");
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 1);
    let again = case.propose(&case.alpha, &proposed, AGENT).unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.question, Some(question));
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 1);
    let asked: String = case
        .db
        .query_row(
            "SELECT question FROM owner_questions WHERE id=?1",
            [i64::try_from(question).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(asked.contains("agent 7"), "{asked}");
    assert!(asked.contains(&first.id.as_str()[..12]), "{asked}");
    let other_agent = case
        .propose(&case.alpha, &proposed, Actor::Agent(8))
        .unwrap();
    assert_eq!(other_agent.question, Some(question));
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 1);

    for actor in [AGENT, MASTER] {
        assert_eq!(
            approve(&case.db, &case.alpha, &first.id, actor),
            Err(code("checks_owner_only"))
        );
        assert_eq!(
            revoke(&case.db, &case.alpha, &first.id, actor),
            Err(code("checks_owner_only"))
        );
    }
    assert_eq!(current_profile(&case.db, &case.alpha).unwrap(), None);
    assert_eq!(case.state(&first.id), ProfileState::Proposed);
    let stored = show(&case.db, &case.alpha, &first.id).unwrap().unwrap();
    assert_eq!(stored.proposed_by, "agent:7");
    assert_eq!(stored.approved_ms, None);
    assert_eq!(stored.question, Some(question));

    let admitted = approve(&case.db, &case.alpha, &first.id, Actor::Owner).unwrap();
    assert_eq!(admitted.id, first.id);
    assert_eq!(
        current_profile(&case.db, &case.alpha).unwrap(),
        Some(admitted)
    );
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 0);
    assert_eq!(case.question_status(question), "dismissed");

    let own = case
        .propose(
            &case.alpha,
            &profile(vec![unit_check("other")]),
            Actor::Owner,
        )
        .unwrap();
    assert_eq!(own.question, None);
    let approved_again = case.propose(&case.alpha, &proposed, MASTER).unwrap();
    assert_eq!(approved_again.question, None);
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 0);

    revoke(&case.db, &case.alpha, &first.id, Actor::Owner).unwrap();
    assert_eq!(
        case.propose(&case.alpha, &proposed, AGENT),
        Err(code("checks_profile_revoked"))
    );
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 0);
}

#[test]
fn an_agent_profile_proposal_is_refused_in_an_untrusted_project() {
    let case = Case::new();
    crate::trust::save(&case.db, "alpha", Trust::Untrusted, Origin::Cloned).unwrap();
    let proposed = profile(vec![unit_check("unit")]);
    for actor in [AGENT, MASTER] {
        assert_eq!(
            case.propose(&case.alpha, &proposed, actor),
            Err(code("checks_owner_only")),
            "{actor:?}"
        );
    }
    assert_eq!(
        case.count("SELECT COUNT(*) FROM check_profiles WHERE project='alpha'"),
        0
    );
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 0);

    let reviewed = case.propose(&case.beta, &proposed, AGENT).unwrap();
    assert!(
        reviewed.question.is_some(),
        "a reviewed project still takes an agent proposal"
    );

    let own = case.propose(&case.alpha, &proposed, Actor::Owner).unwrap();
    assert_eq!(own.question, None);
    for actor in [AGENT, MASTER] {
        assert_eq!(
            case.propose(&case.alpha, &proposed, actor),
            Err(code("checks_owner_only")),
            "the owner's stored profile is not the agent's to propose again: {actor:?}"
        );
    }
    let stored = show(&case.db, &case.alpha, &own.id).unwrap().unwrap();
    assert_eq!(stored.proposed_by, "owner");
    assert_eq!(stored.question, None);
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 0);
    let admitted = approve(&case.db, &case.alpha, &own.id, Actor::Owner).unwrap();
    assert_eq!(
        current_profile(&case.db, &case.alpha).unwrap(),
        Some(admitted)
    );

    case.db
        .execute(
            "UPDATE project_trust SET level='trusted' WHERE project='alpha'",
            [],
        )
        .unwrap();
    assert_eq!(
        case.propose(&case.alpha, &numbered(1), AGENT),
        Err(code("checks_unavailable")),
        "an unreadable trust level never reads as reviewed"
    );

    crate::trust::save(&case.db, "alpha", Trust::Reviewed, Origin::Owner).unwrap();
    let raised = case.propose(&case.alpha, &numbered(2), AGENT).unwrap();
    assert!(
        raised.question.is_some(),
        "the level is read at each proposal"
    );
}

#[test]
fn a_proposal_past_the_question_bound_is_stored_and_a_settled_profile_frees_its_question() {
    let case = Case::new();
    let proposals: Vec<Proposal> = (1..=9)
        .map(|n| {
            case.propose(&case.alpha, &numbered(n), Actor::Agent(n as i64))
                .unwrap()
        })
        .collect();
    assert!(proposals[..8].iter().all(|p| p.question.is_some()));
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 8);
    assert_eq!(proposals[8].question, None);
    assert_eq!(case.state(&proposals[8].id), ProfileState::Proposed);

    revoke(&case.db, &case.alpha, &proposals[0].id, Actor::Owner).unwrap();
    assert_eq!(
        case.question_status(proposals[0].question.unwrap()),
        "dismissed"
    );
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 7);
    let asked_again = case
        .propose(&case.alpha, &numbered(9), Actor::Agent(9))
        .unwrap();
    let ninth = asked_again.question.expect("room for the ninth question");
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 8);
    assert_eq!(
        show(&case.db, &case.alpha, &proposals[8].id)
            .unwrap()
            .unwrap()
            .question,
        Some(ninth)
    );

    approve(&case.db, &case.alpha, &proposals[1].id, Actor::Owner).unwrap();
    assert_eq!(
        case.question_status(proposals[1].question.unwrap()),
        "dismissed"
    );
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 7);
}

#[test]
fn an_agents_proposals_evict_only_its_own_oldest_and_never_the_owners() {
    let case = Case::new();
    let owners = case
        .propose(&case.alpha, &numbered(0), Actor::Owner)
        .unwrap();
    let others = case
        .propose(&case.alpha, &numbered(1), Actor::Agent(8))
        .unwrap();
    let mine: Vec<Proposal> = (0..=PENDING_PER_PROPOSER_MAX)
        .map(|n| {
            case.propose(&case.alpha, &numbered(100 + n as usize), AGENT)
                .unwrap()
        })
        .collect();
    assert_eq!(show(&case.db, &case.alpha, &mine[0].id).unwrap(), None);
    assert_eq!(case.question_status(mine[0].question.unwrap()), "dismissed");
    assert_eq!(
        case.count("SELECT COUNT(*) FROM check_profiles WHERE proposed_by='agent:7'"),
        PENDING_PER_PROPOSER_MAX
    );
    assert_eq!(case.state(&owners.id), ProfileState::Proposed);
    assert_eq!(case.state(&others.id), ProfileState::Proposed);

    let mut n = 1000;
    while case.count("SELECT COUNT(*) FROM check_profiles") < PROFILES_KEPT_MAX {
        let agent = Actor::Agent(100 + (n as i64 - 1000) / PENDING_PER_PROPOSER_MAX);
        case.propose(&case.alpha, &numbered(n), agent).unwrap();
        n += 1;
    }
    case.propose(&case.alpha, &numbered(n), Actor::Agent(99))
        .unwrap();
    assert_eq!(
        case.count("SELECT COUNT(*) FROM check_profiles"),
        PROFILES_KEPT_MAX
    );
    assert_eq!(case.state(&owners.id), ProfileState::Proposed);
    assert_eq!(show(&case.db, &case.alpha, &others.id).unwrap(), None);
    assert_eq!(case.state(&mine[1].id), ProfileState::Proposed);
}

#[test]
fn an_owner_answer_to_a_profile_question_stays_when_the_profile_is_settled() {
    let case = Case::new();
    let first = case.propose(&case.alpha, &numbered(1), AGENT).unwrap();
    let second = case.propose(&case.alpha, &numbered(2), AGENT).unwrap();
    let answered = first.question.unwrap();
    assert_eq!(
        crate::questions::answer(
            &case.db,
            answered,
            0,
            &crate::questions::Answer {
                picks: Vec::new(),
                comment: Some("Looks fine".into()),
            }
        ),
        Ok(crate::questions::Settlement::Settled)
    );

    approve(&case.db, &case.alpha, &first.id, Actor::Owner).unwrap();
    assert_eq!(case.question_status(answered), "answered");
    revoke(&case.db, &case.alpha, &second.id, Actor::Owner).unwrap();
    assert_eq!(case.question_status(second.question.unwrap()), "dismissed");
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 0);
}

#[test]
fn a_profile_over_a_bound_or_with_an_unsafe_path_is_refused() {
    let case = Case::new();
    let digest = hex(b"assert!(true);");
    let refused: Vec<(&str, Value)> = vec![
        ("no checks", changed(|p| p["checks"] = json!([]))),
        (
            "too many checks",
            changed(|p| {
                p["checks"] = (0..=CHECKS_MAX)
                    .map(|n| unit_check(&format!("c{n}")))
                    .collect()
            }),
        ),
        (
            "duplicate ids",
            changed(|p| p["checks"] = json!([unit_check("unit"), unit_check("unit")])),
        ),
        (
            "id with a space",
            changed(|p| p["checks"][0]["id"] = json!("my unit")),
        ),
        (
            "id too long",
            changed(|p| p["checks"][0]["id"] = json!("a".repeat(ID_BYTES_MAX + 1))),
        ),
        (
            "empty command",
            changed(|p| p["checks"][0]["argv"] = json!([])),
        ),
        (
            "command too long",
            changed(|p| p["checks"][0]["argv"] = json!(vec!["x"; ARGV_MAX + 1])),
        ),
        (
            "command item too long",
            changed(|p| p["checks"][0]["argv"] = json!(["cargo", "a".repeat(ARG_BYTES_MAX + 1)])),
        ),
        (
            "program is an absolute path",
            changed(|p| p["checks"][0]["argv"] = json!(["/bin/sh", "-c", "true"])),
        ),
        (
            "program is a relative path",
            changed(|p| p["checks"][0]["argv"] = json!(["./run.sh"])),
        ),
        (
            "control character",
            changed(|p| p["checks"][0]["argv"] = json!(["cargo", "test\nrm"])),
        ),
        (
            "hidden character",
            changed(|p| p["checks"][0]["argv"] = json!(["cargo", "te\u{202e}st"])),
        ),
        (
            "cwd escapes",
            changed(|p| p["checks"][0]["cwd"] = json!("../outside")),
        ),
        (
            "cwd absolute",
            changed(|p| p["checks"][0]["cwd"] = json!("/Users")),
        ),
        ("cwd empty", changed(|p| p["checks"][0]["cwd"] = json!(""))),
        (
            "timeout too short",
            changed(|p| p["checks"][0]["timeout_s"] = json!(TIMEOUT_S_MIN - 1)),
        ),
        (
            "timeout too long",
            changed(|p| p["checks"][0]["timeout_s"] = json!(TIMEOUT_S_MAX + 1)),
        ),
        (
            "no trigger",
            changed(|p| p["checks"][0]["when"] = json!([])),
        ),
        (
            "repeated trigger",
            changed(|p| p["checks"][0]["when"] = json!(["turn_end", "turn_end"])),
        ),
        (
            "junit without a report",
            changed(|p| p["checks"][0]["format"] = json!("junit")),
        ),
        (
            "libtest with a report",
            changed(|p| p["checks"][0]["report"] = json!("target/report.xml")),
        ),
        (
            "report escapes",
            changed(|p| {
                p["checks"][0]["format"] = json!("junit");
                p["checks"][0]["report"] = json!("../report.xml");
            }),
        ),
        (
            "protected path escapes",
            changed(|p| p["protected_paths"] = json!(["tests/../../etc"])),
        ),
        (
            "protected path absolute",
            changed(|p| p["protected_paths"] = json!(["/etc/"])),
        ),
        (
            "too many protected paths",
            changed(|p| {
                p["protected_paths"] = (0..=PROTECTED_PATHS_MAX)
                    .map(|n| json!(format!("p{n}/")))
                    .collect()
            }),
        ),
        (
            "integrity rules belong to the settings",
            changed(|p| {
                p.as_object_mut().unwrap().insert(
                    "integrity".into(),
                    json!({ "inventory": "off", "skip_markers": "off", "protected_change": "flag" }),
                );
            }),
        ),
        (
            "unknown check field",
            changed(|p| p["checks"][0]["shell"] = json!(true)),
        ),
        ("version two", changed(|p| p["version"] = json!(2))),
        (
            "too many holdouts",
            changed(|p| {
                p["holdouts"] = (0..=HOLDOUTS_MAX)
                    .map(|n| {
                        holdout(
                            &format!("h{n}"),
                            json!({ format!("tests/h{n}.rs"): digest }),
                        )
                    })
                    .collect()
            }),
        ),
        (
            "holdout without files",
            changed(|p| p["holdouts"] = json!([holdout("h", json!({}))])),
        ),
        (
            "too many holdout files",
            changed(|p| {
                let files: serde_json::Map<String, Value> = (0..=HOLDOUT_FILES_MAX)
                    .map(|n| (format!("tests/h{n}.rs"), json!(digest)))
                    .collect();
                p["holdouts"] = json!([holdout("h", Value::Object(files))]);
            }),
        ),
        (
            "holdout file digest malformed",
            changed(|p| p["holdouts"] = json!([holdout("h", json!({ "tests/h.rs": "abc" }))])),
        ),
        (
            "holdout file escapes",
            changed(|p| p["holdouts"] = json!([holdout("h", json!({ "../h.rs": digest }))])),
        ),
        (
            "holdout id clashes with a check",
            changed(|p| p["holdouts"] = json!([holdout("unit", json!({ "tests/h.rs": digest }))])),
        ),
        (
            "over the byte bound",
            changed(|p| {
                let mut wide = unit_check("wide");
                wide["argv"] = json!(vec!["a".repeat(300); ARGV_MAX]);
                let mut wider = wide.clone();
                wider["id"] = json!("wider");
                p["checks"] = json!([wide, wider]);
            }),
        ),
    ];
    for (label, value) in &refused {
        let bytes = serde_json::to_vec(value).unwrap();
        assert_eq!(
            CheckProfile::import(&bytes),
            Err(code("checks_profile_invalid")),
            "{label}"
        );
        assert_eq!(
            case.propose(&case.alpha, value, AGENT),
            Err(code("checks_profile_invalid")),
            "{label}"
        );
    }
    assert_eq!(
        case.propose(
            &case.alpha,
            &changed(|p| p["holdouts"] = json!([holdout("h", json!({ "tests/h.rs": digest }))])),
            Actor::Owner
        ),
        Err(code("checks_holdout_missing"))
    );

    let too_big = vec![b'a'; HOLDOUT_FILE_BYTES_MAX + 1];
    for bytes in [&too_big[..], b""] {
        assert_eq!(
            store_holdout_file(&case.db, &case.alpha, bytes, Actor::Owner),
            Err(code("checks_holdout_invalid"))
        );
    }
    assert_eq!(
        store_holdout_file(&case.db, &case.alpha, b"assert!(true);", AGENT),
        Err(code("checks_owner_only"))
    );
    assert_eq!(case.count("SELECT COUNT(*) FROM check_profiles"), 0);
    assert_eq!(case.count("SELECT COUNT(*) FROM check_holdout_files"), 0);
    assert_eq!(case.count(PENDING_CHECK_QUESTIONS), 0);
}

#[test]
fn approving_a_new_profile_makes_the_old_one_not_current() {
    let case = Case::new();
    let stored =
        store_holdout_file(&case.db, &case.alpha, b"fn holdout() {}", Actor::Owner).unwrap();
    assert_eq!(stored, hex(b"fn holdout() {}"));
    let first = case
        .propose(
            &case.alpha,
            &changed(|p| {
                p["holdouts"] = json!([holdout("hidden", json!({ "tests/hidden.rs": stored }))])
            }),
            Actor::Owner,
        )
        .unwrap();
    let second = case
        .propose(
            &case.alpha,
            &profile(vec![unit_check("second")]),
            Actor::Owner,
        )
        .unwrap();
    assert_ne!(first.id, second.id);

    let a = approve(&case.db, &case.alpha, &first.id, Actor::Owner).unwrap();
    let checks = a.checks();
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0].spec.id, "unit");
    assert_eq!(checks[0].holdout, None);
    assert_eq!(checks[1].spec.id, "hidden");
    assert!(
        checks
            .iter()
            .all(|c| c.profile == a.id && c.approved_ms == a.approved_ms)
    );
    assert_eq!(
        holdout_files(&case.db, &case.alpha, &checks[1]).unwrap(),
        BTreeMap::from([("tests/hidden.rs".to_string(), b"fn holdout() {}".to_vec())])
    );
    assert!(
        holdout_files(&case.db, &case.alpha, &checks[0])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        holdout_files(&case.db, &case.beta, &checks[1]),
        Err(code("checks_holdout_missing"))
    );

    let b = approve(&case.db, &case.alpha, &second.id, Actor::Owner).unwrap();
    assert!(b.approved_ms > a.approved_ms);
    assert_eq!(
        current_profile(&case.db, &case.alpha).unwrap(),
        Some(b.clone())
    );
    assert_eq!(case.state(&first.id), ProfileState::Approved);
    assert_eq!(case.state(&second.id), ProfileState::Current);
    assert_eq!(
        approve(&case.db, &case.alpha, &second.id, Actor::Owner).unwrap(),
        b
    );

    let again = approve(&case.db, &case.alpha, &first.id, Actor::Owner).unwrap();
    assert_eq!(again.id, first.id);
    assert!(again.approved_ms > b.approved_ms);
    assert_eq!(case.state(&second.id), ProfileState::Approved);

    assert_eq!(
        approve(&case.db, &case.beta, &first.id, Actor::Owner),
        Err(code("checks_profile_unknown"))
    );
    assert_eq!(current_profile(&case.db, &case.beta).unwrap(), None);
    let unknown = ProfileId::new(hex(b"no such profile")).unwrap();
    assert_eq!(
        approve(&case.db, &case.alpha, &unknown, Actor::Owner),
        Err(code("checks_profile_unknown"))
    );

    revoke(&case.db, &case.alpha, &first.id, Actor::Owner).unwrap();
    assert_eq!(current_profile(&case.db, &case.alpha).unwrap(), None);
    assert_eq!(case.state(&first.id), ProfileState::Revoked);
    let revived = approve(&case.db, &case.alpha, &first.id, Actor::Owner).unwrap();
    assert!(revived.approved_ms > again.approved_ms);
    assert_eq!(case.state(&first.id), ProfileState::Current);
}

#[test]
fn the_project_override_wins_over_the_app_default() {
    let case = Case::new();
    let alpha = Scope::Project(case.alpha.clone());
    assert_eq!(
        settings::effective(&case.db, &case.alpha).unwrap(),
        ChecksSettings::default()
    );
    assert_eq!(ChecksSettings::default().gate, GateMode::Warn);
    assert_eq!(
        ChecksSettings::default().protected_change,
        ProtectedChange::Refuse
    );
    assert_eq!(
        settings::read(&case.db, &Scope::App).unwrap(),
        (ChecksChoice::default(), 0)
    );

    let app = ChecksChoice {
        gate: Some(GateMode::Require),
        parallel: Some(2),
        ..ChecksChoice::default()
    };
    assert_eq!(
        settings::write(&case.db, &Scope::App, &app, 0, Actor::Owner).unwrap(),
        1
    );
    let project = ChecksChoice {
        gate: Some(GateMode::Off),
        ..ChecksChoice::default()
    };
    assert_eq!(
        settings::write(&case.db, &alpha, &project, 0, Actor::Owner).unwrap(),
        1
    );
    let effective = settings::effective(&case.db, &case.alpha).unwrap();
    assert_eq!(effective.gate, GateMode::Off);
    assert_eq!(effective.parallel, 2);
    assert_eq!(effective.agent_requests, 6);
    assert_eq!(ChecksSettings::resolve(&app, &project), effective);
    let other = settings::effective(&case.db, &case.beta).unwrap();
    assert_eq!(other.gate, GateMode::Require);
    assert_eq!(other.parallel, 2);

    assert_eq!(
        settings::write(&case.db, &alpha, &app, 0, Actor::Owner),
        Err(code("checks_settings_stale"))
    );
    for actor in [AGENT, MASTER] {
        assert_eq!(
            settings::write(&case.db, &alpha, &ChecksChoice::default(), 1, actor),
            Err(code("checks_owner_only"))
        );
    }
    for invalid in [
        ChecksChoice {
            parallel: Some(5),
            ..ChecksChoice::default()
        },
        ChecksChoice {
            parallel: Some(0),
            ..ChecksChoice::default()
        },
        ChecksChoice {
            keep_failed_snapshots: Some(6),
            ..ChecksChoice::default()
        },
        ChecksChoice {
            agent_requests: Some(3),
            ..ChecksChoice::default()
        },
    ] {
        assert_eq!(
            settings::write(&case.db, &alpha, &invalid, 1, Actor::Owner),
            Err(code("checks_setting_invalid"))
        );
    }
    assert_eq!(settings::read(&case.db, &alpha).unwrap(), (project, 1));

    let listed = settings::choices();
    let keys: Vec<&str> = listed.iter().map(|choice| choice.key).collect();
    assert_eq!(
        keys,
        [
            "gate",
            "parallel",
            "keep_failed_snapshots",
            "agent_requests",
            "inventory",
            "skip_markers",
            "protected_change",
            "removed_tests"
        ]
    );
    let defaults = serde_json::to_value(ChecksSettings::default()).unwrap();
    assert_eq!(defaults.as_object().unwrap().len(), listed.len());
    assert_eq!(
        listed[0].options,
        [json!("off"), json!("warn"), json!("require")]
    );
    assert_eq!(listed[3].options, [json!(0), json!(2), json!(6), json!(12)]);
    let mut version = 1;
    for choice in &listed {
        assert_eq!(defaults[choice.key], choice.default, "{}", choice.key);
        assert!(choice.options.contains(&choice.default), "{}", choice.key);
        for option in &choice.options {
            let layer: ChecksChoice =
                serde_json::from_value(json!({ choice.key: option })).unwrap();
            version = settings::write(&case.db, &alpha, &layer, version, Actor::Owner).unwrap();
            let effective =
                serde_json::to_value(settings::effective(&case.db, &case.alpha).unwrap()).unwrap();
            assert_eq!(&effective[choice.key], option, "{} {option}", choice.key);
        }
    }
}

#[test]
fn the_same_profile_bytes_have_the_same_identity() {
    let case = Case::new();
    let written = serde_json::to_vec(&profile(vec![unit_check("unit")])).unwrap();
    let reordered = br#"{ "protected_paths": ["tests/acceptance/"], "checks": [ {
        "protected": false, "required": true, "when": ["before_accept"], "network": "off",
        "timeout_s": 1800, "format": "libtest", "cwd": ".",
        "argv": ["cargo", "test", "--locked", "--offline"], "toolchain": "rust",
        "kind": "unit", "id": "unit" } ], "version": 1 }"#;
    let parsed = CheckProfile::import(&written).unwrap();
    let id = parsed.identity().unwrap();
    let canonical = parsed.canonical().unwrap();
    assert_eq!(id.as_str(), hex(&canonical));
    assert_eq!(CheckProfile::import(&canonical).unwrap(), parsed);
    assert_eq!(
        CheckProfile::import(&written).unwrap().identity().unwrap(),
        id
    );
    assert_eq!(
        CheckProfile::import(reordered).unwrap().identity().unwrap(),
        id
    );
    let edited = changed(|p| p["checks"][0]["argv"][1] = json!("nextest"));
    assert_ne!(
        CheckProfile::import(&serde_json::to_vec(&edited).unwrap())
            .unwrap()
            .identity()
            .unwrap(),
        id
    );

    let first = propose(&case.db, &case.alpha, &written, Actor::Owner).unwrap();
    let second = propose(&case.db, &case.alpha, reordered, Actor::Owner).unwrap();
    assert_eq!(first.id, id);
    assert_eq!(second.id, id);
    assert_eq!(case.count("SELECT COUNT(*) FROM check_profiles"), 1);
    let body: Vec<u8> = case
        .db
        .query_row("SELECT body FROM check_profiles", [], |row| {
            row.get::<_, String>(0)
        })
        .map(String::into_bytes)
        .unwrap();
    assert_eq!(body, canonical);

    let x = store_holdout_file(&case.db, &case.alpha, b"x", Actor::Owner).unwrap();
    assert_eq!(
        store_holdout_file(&case.db, &case.alpha, b"x", Actor::Owner).unwrap(),
        x
    );
    let y = store_holdout_file(&case.db, &case.alpha, b"y", Actor::Owner).unwrap();
    assert_eq!(case.count("SELECT COUNT(*) FROM check_holdout_files"), 2);
    let with = |digest: &str| {
        let value =
            changed(|p| p["holdouts"] = json!([holdout("h", json!({ "tests/h.rs": digest }))]));
        CheckProfile::import(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .identity()
            .unwrap()
    };
    assert_ne!(with(x.as_str()), with(y.as_str()));
    assert_ne!(with(x.as_str()), id);
}
