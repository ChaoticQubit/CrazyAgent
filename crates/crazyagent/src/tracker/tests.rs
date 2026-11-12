use super::host::{
    ACTOR_OTHER_PROJECT, EVIDENCE_ENGINE_ONLY, EVIDENCE_OWNER_ONLY, LINK_CROSS_PROJECT,
    POLICY_IN_SETTINGS,
};
use super::settings::{CiEvidence, ManualEvidence, Scope, TicketOverride};
use super::*;
use crate::landing::{AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use trackwright_core::criteria::{self, NewEvidence};
use trackwright_core::model::{
    AgentEditPolicy, CheckKind, ClosePolicy, CreatePolicy, EvidenceResult, PolicyKey, ShortId,
    TicketStatus,
};
use trackwright_core::ops;
use trackwright_core::policy;
use trackwright_core::requests::Request;
use trackwright_core::store::{self, StoreError};

const NOW: i64 = 1_800_000_000_000;

/// One repository with an owner home that holds no global git config, and an engine root
/// whose `engine.db` holds every authority schema, the change set store and the tracker's
/// tables, with two engine projects attached as `CA` and `CB`.
struct Case {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    db: Connection,
}

/// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
/// Inputs: the owner home, the folder and the arguments. Output: standard output.
fn git_in(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

fn key(text: &str) -> ProjectKey {
    ProjectKey::new(text).unwrap()
}

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("home"), root.join("engine"));
        for folder in [&repo, &home, &engine] {
            std::fs::create_dir(folder).unwrap();
        }
        git_in(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        git_in(&home, &repo, &["config", "user.name", "Repo Owner"]);
        git_in(
            &home,
            &repo,
            &["config", "user.email", "owner@example.invalid"],
        );
        std::fs::write(repo.join("README"), b"one\n").unwrap();
        git_in(&home, &repo, &["add", "README"]);
        git_in(&home, &repo, &["commit", "-q", "-m", "Start"]);
        let db = crate::landing::open(&engine).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        let case = Self {
            _tmp: tmp,
            home,
            repo,
            db,
        };
        let tx = case.tx();
        attach(&tx, &project("project-1"), &key("CA"), "One", NOW).unwrap();
        attach(&tx, &project("project-2"), &key("CB"), "Two", NOW).unwrap();
        tx.commit().unwrap();
        case
    }

    fn tx(&self) -> Transaction<'_> {
        Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate).unwrap()
    }

    fn worktree(&self, id: i64) -> PathBuf {
        self.repo.parent().unwrap().join(format!("agent-{id}"))
    }

    fn git_dir(&self, id: i64) -> PathBuf {
        self.repo.join(".git/worktrees").join(format!("agent-{id}"))
    }

    /// Adds one team agent of a project the way the runtime records it: a real worktree on
    /// its own branch `crazyagent/agent-<id>` with its start recorded, and its team rows.
    /// Inputs: the id, the project and the role.
    fn agent(&self, id: i64, owner: &ProjectId, role: &str) {
        let worktree = self.worktree(id);
        let branch = format!("crazyagent/agent-{id}");
        git_in(
            &self.home,
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        crate::landing::record_worktree_start(&self.db, &self.repo, &worktree, &self.git_dir(id))
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,created_ms,heartbeat_ms)
                 VALUES (?1,?2,?3,'codex',0,'local-a','working',?4,?4)",
                params![id, owner.as_str(), role, NOW],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    owner.as_str(),
                    self.repo.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    self.git_dir(id).to_str().unwrap()
                ],
            )
            .unwrap();
    }

    /// Ends a turn in an agent's worktree as the runtime does, opening its change set.
    /// Inputs: the agent and its project. Output: the change set.
    fn turn_end(&self, id: i64, owner: &ProjectId) -> ChangeSetId {
        std::fs::write(self.worktree(id).join("README"), b"one\ntwo\n").unwrap();
        let workspace = Workspace {
            project: owner.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: id.to_string(),
                label: format!("Agent {id}"),
            },
            repository: self.repo.clone(),
            worktree: self.worktree(id),
            git_dir: self.git_dir(id),
        };
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: format!("turn-{id}"),
            prompt_line: "Add the second line".into(),
        };
        crate::landing::record_turn_end(&self.db, &workspace, &turn)
            .unwrap()
            .unwrap()
    }

    fn head_of(&self, id: &ChangeSetId, owner: &ProjectId) -> String {
        crate::landing::list(&self.db, owner)
            .unwrap()
            .into_iter()
            .find(|view| &view.id == id)
            .unwrap()
            .head
    }

    fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

/// Runs one catalog op through the core with the engine host, as the engine's tools and
/// owner protocol do. Inputs: the transaction, the engine-set actor, the op and its
/// arguments. Output: the reply envelope.
fn call(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> Value {
    let bytes = serde_json::to_vec(&args).unwrap();
    let request = Request::parse(op, &bytes).unwrap_or_else(|refusal| panic!("{op}: {refusal}"));
    let reply = ops::dispatch(tx, &Host, &actor(who), request);
    let value: Value = serde_json::from_str(reply.json()).unwrap();
    assert_eq!(value["ok"].as_bool(), Some(reply.is_ok()));
    value
}

fn ok(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> Value {
    let value = call(tx, who, op, args);
    assert_eq!(value["ok"], true, "{who} {op} succeeds: {value}");
    value
}

fn refused(tx: &Transaction<'_>, who: &str, op: &str, args: Value) -> String {
    let value = call(tx, who, op, args);
    assert_eq!(value["ok"], false, "{who} {op} is refused: {value}");
    value["code"].as_str().unwrap().to_owned()
}

fn create(tx: &Transaction<'_>, who: &str, key: &str, title: &str) -> (String, i64) {
    let created = ok(tx, who, "create", json!({"project": key, "title": title}));
    (
        created["data"]["id"].as_str().unwrap().to_owned(),
        created["data"]["version"].as_i64().unwrap(),
    )
}

fn store_refusal<T: std::fmt::Debug>(result: Result<T, StoreError>) -> &'static str {
    match result {
        Err(StoreError::Refused(refusal)) => refusal.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn set(tx: &Transaction<'_>, scope: &Scope, values: TicketOverride) -> u64 {
    let version = settings::stored(tx, scope).unwrap().version;
    settings::set(tx, scope, &values, version, NOW)
        .unwrap()
        .unwrap()
}

/// Creates an owner ticket in `CA` with one criterion that has one check. Inputs: the
/// transaction, the title, the check's kind and reference. Output: the ticket and the
/// criterion's position.
fn checked(tx: &Transaction<'_>, title: &str, kind: CheckKind, reference: &str) -> (ShortId, u32) {
    let (id, _) = create(tx, "owner", "CA", title);
    let id = ShortId::parse(&id).unwrap();
    let criterion =
        criteria::add_criterion(tx, &Host, &actor("owner"), &id, "The check passed", NOW).unwrap();
    criteria::add_check(
        tx,
        &Host,
        &actor("owner"),
        &id,
        criterion.position,
        kind,
        reference,
        NOW + 1,
    )
    .unwrap();
    (id, criterion.position)
}

/// Records a passing result on a ticket's check through the core with the engine host.
/// Inputs: the transaction, the actor, the ticket, the criterion, the check's kind and
/// reference. Output: the core's result.
fn record(
    tx: &Transaction<'_>,
    who: &str,
    id: &ShortId,
    criterion: u32,
    kind: CheckKind,
    reference: &str,
) -> Result<(), StoreError> {
    criteria::record_evidence(
        tx,
        &Host,
        &actor(who),
        &NewEvidence {
            ticket: id.clone(),
            criterion,
            kind,
            check: reference.to_owned(),
            result: EvidenceResult::Pass,
            reference: Some(format!("seen by {who}")),
        },
        NOW + 10,
    )
    .map(|_| ())
}

fn closed_by(tx: &Transaction<'_>, id: &ShortId) -> Option<Actor> {
    store::ticket(tx, id).unwrap().closed_by
}

#[test]
fn an_agent_link_to_another_projects_worktree_is_refused_in_engine_db() {
    let case = Case::new();
    let (one, two) = (project("project-1"), project("project-2"));
    case.agent(1, &one, "sub");
    case.agent(2, &two, "sub");
    let tx = case.tx();
    set(
        &tx,
        &Scope::Project(one.clone()),
        TicketOverride {
            agent_edit: Some(AgentEditPolicy::Any),
            ..TicketOverride::default()
        },
    );
    let (id, _) = create(&tx, "owner", "CA", "Wire the host");
    let own = case.worktree(1).to_str().unwrap().to_owned();
    let other = case.worktree(2).to_str().unwrap().to_owned();
    let links_before = case.count("SELECT count(*) FROM tk_links");
    let events_before = case.count("SELECT count(*) FROM tk_events");

    for who in ["agent:1", "owner"] {
        let code = refused(
            &tx,
            who,
            "link",
            json!({"id": id, "kind": "worktree", "target": other}),
        );
        assert_eq!(code, LINK_CROSS_PROJECT, "{who}");
        let code = refused(
            &tx,
            who,
            "link",
            json!({"id": id, "kind": "branch", "target": "crazyagent/agent-2"}),
        );
        assert_eq!(code, LINK_CROSS_PROJECT, "{who}");
    }
    let code = refused(
        &tx,
        "agent:2",
        "link",
        json!({"id": id, "kind": "worktree", "target": own}),
    );
    assert_eq!(code, ACTOR_OTHER_PROJECT);
    assert_eq!(case.count("SELECT count(*) FROM tk_links"), links_before);
    assert_eq!(case.count("SELECT count(*) FROM tk_events"), events_before);

    ok(
        &tx,
        "agent:1",
        "link",
        json!({"id": id, "kind": "worktree", "target": own}),
    );
    ok(
        &tx,
        "agent:1",
        "link",
        json!({"id": id, "kind": "branch", "target": "crazyagent/agent-1"}),
    );
    let shown = ok(&tx, "agent:1", "show", json!({"id": id}));
    let links = shown["data"]["links"].as_array().unwrap();
    assert_eq!(links.len(), 2, "{shown}");
    assert!(links.iter().all(|link| link["live"] == true), "{shown}");
    tx.commit().unwrap();
    assert_eq!(
        case.count("SELECT count(*) FROM tk_links WHERE kind='worktree'"),
        1
    );
}

#[test]
fn a_removed_change_set_shows_gone_and_the_ticket_still_reads() {
    let case = Case::new();
    let one = project("project-1");
    case.agent(1, &one, "sub");
    let change = case.turn_end(1, &one);
    let tx = case.tx();
    let (id, _) = create(&tx, "owner", "CA", "Land the readme");
    ok(
        &tx,
        "owner",
        "link",
        json!({"id": id, "kind": "change_set", "target": change.as_str()}),
    );
    let shown = ok(&tx, "owner", "show", json!({"id": id}));
    let link = &shown["data"]["links"][0];
    assert_eq!(link["live"], true, "{shown}");
    assert_eq!(link["state"], "pending", "{shown}");
    tx.commit().unwrap();

    let head = case.head_of(&change, &one);
    crate::landing::discard(&case.db, &change, &head, u64::try_from(NOW).unwrap()).unwrap();
    let after_window = u64::try_from(NOW).unwrap() + crate::landing::RESTORE_WINDOW_MS + 1;
    assert_eq!(crate::landing::prune(&case.db, after_window).unwrap(), 1);

    let tx = case.tx();
    let shown = ok(&tx, "owner", "show", json!({"id": id}));
    assert_eq!(
        shown["data"]["ticket"]["title"], "Land the readme",
        "{shown}"
    );
    let link = &shown["data"]["links"][0];
    assert_eq!(link["kind"], "change_set");
    assert_eq!(link["target"], change.as_str());
    assert_eq!(link["live"], false, "{shown}");
    assert_eq!(link["state"], "gone", "{shown}");
    let listed = ok(&tx, "owner", "list", json!({"project": "CA"}));
    assert_eq!(listed["data"]["tickets"][0]["id"], id.as_str());
    tx.commit().unwrap();
}

#[test]
fn the_project_override_wins_over_the_app_default() {
    let case = Case::new();
    let (one, two) = (project("project-1"), project("project-2"));
    case.agent(1, &one, "sub");
    case.agent(2, &two, "sub");
    let tx = case.tx();
    assert_eq!(
        settings::effective(&tx, &one).unwrap(),
        settings::TicketSettings::default()
    );
    let app = set(
        &tx,
        &Scope::App,
        TicketOverride {
            close: Some(ClosePolicy::Owner),
            create: Some(CreatePolicy::Direct),
            ..TicketOverride::default()
        },
    );
    assert_eq!(app, 1);
    let overridden = set(
        &tx,
        &Scope::Project(one.clone()),
        TicketOverride {
            close: Some(ClosePolicy::Agents),
            ..TicketOverride::default()
        },
    );
    assert_eq!(overridden, 1);

    let first = settings::effective(&tx, &one).unwrap();
    assert_eq!(first.policy.close, ClosePolicy::Agents, "the override wins");
    assert_eq!(
        first.policy.create,
        CreatePolicy::Direct,
        "the app value fills the rest"
    );
    assert_eq!(first.policy.agent_edit, AgentEditPolicy::StatusAndCriteria);
    assert_eq!(first.manual_evidence, ManualEvidence::Owner);
    let second = settings::effective(&tx, &two).unwrap();
    assert_eq!(second.policy.close, ClosePolicy::Owner);
    assert_eq!(second.policy.create, CreatePolicy::Direct);
    assert_eq!(
        policy::load(&tx, &key("CA")).unwrap().close,
        ClosePolicy::Agents
    );
    assert_eq!(
        policy::load(&tx, &key("CA")).unwrap().create,
        CreatePolicy::Direct
    );
    assert_eq!(
        policy::load(&tx, &key("CB")).unwrap().close,
        ClosePolicy::Owner
    );

    let (mine, version) = create(&tx, "agent:1", "CA", "Close it myself");
    ok(
        &tx,
        "agent:1",
        "close",
        json!({"id": mine, "reason": "done in agent 1", "version": version}),
    );
    let closed = store::ticket(&tx, &ShortId::parse(&mine).unwrap()).unwrap();
    assert_eq!(closed.status, TicketStatus::Done);
    assert_eq!(closed.closed_by, Some(actor("agent:1")));
    let (theirs, version) = create(&tx, "agent:2", "CB", "Wait for the owner");
    let code = refused(
        &tx,
        "agent:2",
        "close",
        json!({"id": theirs, "reason": "done in agent 2", "version": version}),
    );
    assert_eq!(
        code, "close_owner_only",
        "the policy refuses, not the membership check"
    );
    let ticket = store::ticket(&tx, &ShortId::parse(&theirs).unwrap()).unwrap();
    assert_eq!(
        ticket.status,
        TicketStatus::Backlog,
        "direct create starts in the backlog"
    );

    let stale = settings::set(
        &tx,
        &Scope::Project(one.clone()),
        &TicketOverride::default(),
        0,
        NOW,
    )
    .unwrap();
    assert_eq!(
        stale.unwrap_err().current,
        1,
        "a stale override write lands nothing"
    );
    assert_eq!(
        policy::load(&tx, &key("CA")).unwrap().close,
        ClosePolicy::Agents
    );
    set(&tx, &Scope::Project(one.clone()), TicketOverride::default());
    assert_eq!(
        settings::effective(&tx, &one).unwrap().policy.close,
        ClosePolicy::Owner,
        "Use app default"
    );
    assert_eq!(
        policy::load(&tx, &key("CA")).unwrap().close,
        ClosePolicy::Owner
    );
    tx.commit().unwrap();
}

#[test]
fn an_agent_cannot_record_owner_evidence() {
    let case = Case::new();
    let one = project("project-1");
    case.agent(1, &one, "sub");
    case.agent(3, &one, "master");
    let tx = case.tx();
    let (manual, review) = (CheckKind::Manual, "review:owner");

    let (id, position) = checked(&tx, "Reviewed by the owner", manual, review);
    let rows = case.count("SELECT count(*) FROM tk_evidence");
    for who in ["agent:1", "master:3", "engine:review"] {
        assert_eq!(
            store_refusal(record(&tx, who, &id, position, manual, review)),
            EVIDENCE_OWNER_ONLY,
            "{who}"
        );
    }
    assert_eq!(case.count("SELECT count(*) FROM tk_evidence"), rows);
    assert_eq!(closed_by(&tx, &id), None);
    record(&tx, "owner", &id, position, manual, review).unwrap();
    let closed = store::ticket(&tx, &id).unwrap();
    assert_eq!(closed.status, TicketStatus::Done);
    assert_eq!(closed.closed_by, Some(Actor::host("evidence")));

    set(
        &tx,
        &Scope::Project(one.clone()),
        TicketOverride {
            manual_evidence: Some(ManualEvidence::Agents),
            ..TicketOverride::default()
        },
    );
    let (id, position) = checked(&tx, "Reviewed by an agent", manual, review);
    record(&tx, "agent:1", &id, position, manual, review).unwrap();
    assert_eq!(closed_by(&tx, &id), Some(Actor::host("evidence")));
    tx.commit().unwrap();
}

#[test]
fn an_agent_cannot_record_its_own_ci_evidence() {
    let case = Case::new();
    let one = project("project-1");
    case.agent(1, &one, "sub");
    let tx = case.tx();
    let (ci, build) = (CheckKind::Ci, "ci:build");
    assert_eq!(
        settings::effective(&tx, &one).unwrap().ci_evidence,
        CiEvidence::Engine,
        "the proposed default"
    );

    let (id, position) = checked(&tx, "Built by CI", ci, build);
    let rows = case.count("SELECT count(*) FROM tk_evidence");
    assert_eq!(
        store_refusal(record(&tx, "agent:1", &id, position, ci, build)),
        EVIDENCE_ENGINE_ONLY
    );
    assert_eq!(case.count("SELECT count(*) FROM tk_evidence"), rows);
    assert_eq!(closed_by(&tx, &id), None, "no CI run, no close");
    record(&tx, "engine:ci", &id, position, ci, build).unwrap();
    assert_eq!(closed_by(&tx, &id), Some(Actor::host("evidence")));
    let (id, position) = checked(&tx, "Built and read by the owner", ci, build);
    record(&tx, "owner", &id, position, ci, build).unwrap();
    assert_eq!(closed_by(&tx, &id), Some(Actor::host("evidence")));

    set(
        &tx,
        &Scope::Project(one.clone()),
        TicketOverride {
            ci_evidence: Some(CiEvidence::Owner),
            ..TicketOverride::default()
        },
    );
    let (id, position) = checked(&tx, "Only the owner reads CI", ci, build);
    let rows = case.count("SELECT count(*) FROM tk_evidence");
    for who in ["engine:ci", "agent:1"] {
        assert_eq!(
            store_refusal(record(&tx, who, &id, position, ci, build)),
            EVIDENCE_OWNER_ONLY,
            "{who}"
        );
    }
    assert_eq!(case.count("SELECT count(*) FROM tk_evidence"), rows);
    record(&tx, "owner", &id, position, ci, build).unwrap();
    assert_eq!(closed_by(&tx, &id), Some(Actor::host("evidence")));

    set(
        &tx,
        &Scope::Project(one.clone()),
        TicketOverride {
            ci_evidence: Some(CiEvidence::Agents),
            ..TicketOverride::default()
        },
    );
    let (id, position) = checked(&tx, "Agents report CI", ci, build);
    record(&tx, "agent:1", &id, position, ci, build).unwrap();
    assert_eq!(closed_by(&tx, &id), Some(Actor::host("evidence")));
    tx.commit().unwrap();
}

#[test]
fn the_core_policy_set_op_is_refused_so_ticket_settings_stay_the_one_writer() {
    let case = Case::new();
    let one = project("project-1");
    case.agent(1, &one, "sub");
    let tx = case.tx();
    let before = policy::load(&tx, &key("CA")).unwrap();
    let rows = case.count("SELECT count(*) FROM tk_policy");
    let events = case.count("SELECT count(*) FROM tk_events");
    for who in ["owner", "agent:1", "engine:settings"] {
        let code = refused(
            &tx,
            who,
            "policy_set",
            json!({"project": "CA", "key": "close", "value": "agents"}),
        );
        assert_eq!(code, POLICY_IN_SETTINGS, "{who}");
    }
    assert_eq!(
        store_refusal(policy::set(
            &tx,
            &Host,
            &actor("owner"),
            &key("CA"),
            PolicyKey::Close,
            "agents",
            0,
            NOW
        )),
        POLICY_IN_SETTINGS
    );
    assert_eq!(policy::load(&tx, &key("CA")).unwrap(), before);
    assert_eq!(case.count("SELECT count(*) FROM tk_policy"), rows);
    assert_eq!(case.count("SELECT count(*) FROM tk_events"), events);

    set(
        &tx,
        &Scope::Project(one.clone()),
        TicketOverride {
            close: Some(ClosePolicy::Agents),
            ..TicketOverride::default()
        },
    );
    assert_eq!(
        policy::load(&tx, &key("CA")).unwrap().close,
        ClosePolicy::Agents,
        "the ticket settings still reach the core"
    );
    tx.commit().unwrap();
}
