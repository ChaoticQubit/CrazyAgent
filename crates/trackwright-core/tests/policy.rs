use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;
use std::path::Path;
use trackwright_core::criteria::{self, NewEvidence};
use trackwright_core::events;
use trackwright_core::host::{
    self, Authority, CheckView, Evidence, EvidenceState, HostEvent, LinkKind, LinkState, Links,
    TicketView,
};
use trackwright_core::model::{
    Actor, AgentEditPolicy, CheckKind, ClosePolicy, CreatePolicy, CriterionState, EvidenceResult,
    OpKind, PolicyKey, Priority, ProjectKey, Refusal, ShortId, TicketKind, TicketStatus, codes,
};
use trackwright_core::policy::{self, Role};
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, NewTicket, Store, StoreError};

/// A host shaped like an engine: branches and change sets live in its own tables in the
/// tracker's database and are read in the core's transaction; authority and every other
/// check kind follow the standalone host.
struct EngineLike;

const ENGINE_KINDS: [LinkKind; 2] = [
    LinkKind {
        name: "branch",
        label: "Branch",
    },
    LinkKind {
        name: "change_set",
        label: "Change set",
    },
];

const ENGINE_TABLES: &str = "
CREATE TABLE host_branches(
    project TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (project, name)
);
CREATE TABLE host_change_sets(
    project TEXT NOT NULL,
    id TEXT NOT NULL,
    accepted INTEGER NOT NULL,
    PRIMARY KEY (project, id)
);";

impl Links for EngineLike {
    fn kinds(&self) -> &[LinkKind] {
        &ENGINE_KINDS
    }

    fn resolve(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal> {
        let sql = match kind {
            "branch" => "SELECT project, 0 FROM host_branches WHERE name = ?1",
            "change_set" => "SELECT project, accepted FROM host_change_sets WHERE id = ?1",
            _ => {
                return Err(Refusal::new(
                    "link_kind_unknown",
                    format!("this host links no {kind}"),
                ));
            }
        };
        let found: Option<(String, i64)> = tx
            .query_row(sql, [target], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(|error| Refusal::new("host_failed", error.to_string()))?;
        match found {
            None => Err(Refusal::new("link_broken", format!("no {kind} {target}"))),
            Some((owner, _)) if owner != project.as_str() => Err(Refusal::new(
                "link_cross_project",
                format!("{kind} {target} belongs to another project"),
            )),
            Some((_, accepted)) => Ok(LinkState {
                live: true,
                label: target.to_owned(),
                state: if accepted == 1 { "accepted" } else { "open" }.to_owned(),
            }),
        }
    }
}

impl Evidence for EngineLike {
    fn check(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        check: &CheckView,
    ) -> Result<EvidenceState, Refusal> {
        if check.kind != CheckKind::Change {
            return Standalone.check(tx, project, check);
        }
        let accepted: Option<i64> = tx
            .query_row(
                "SELECT accepted FROM host_change_sets WHERE project = ?1 AND id = ?2",
                params![project.as_str(), check.reference],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| Refusal::new("host_failed", error.to_string()))?;
        Ok(match accepted {
            Some(1) => EvidenceState {
                satisfied: true,
                detail: "accepted".to_owned(),
            },
            Some(_) => EvidenceState {
                satisfied: false,
                detail: "not accepted yet".to_owned(),
            },
            None => EvidenceState {
                satisfied: false,
                detail: "gone".to_owned(),
            },
        })
    }
}

impl Authority for EngineLike {
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        ticket: Option<&TicketView>,
    ) -> Result<(), Refusal> {
        Standalone.actor_may(tx, project, actor, op, ticket)
    }

    fn actors_verified(&self) -> bool {
        true
    }
}

fn opened(dir: &Path) -> Store {
    Store::open(&dir.join("tracker").join("tracker.db")).unwrap()
}

fn actor(name: &str) -> Actor {
    Actor::new(name).unwrap()
}

fn owner() -> Actor {
    actor("owner")
}

fn key() -> ProjectKey {
    ProjectKey::new("CA").unwrap()
}

fn project(tx: &Transaction<'_>) {
    store::create_project(tx, &owner(), &key(), "CrazyAgent", 1_000).unwrap();
}

fn ticket(tx: &Transaction<'_>, by: &Actor, title: &str) -> ShortId {
    let new = NewTicket {
        title: title.to_owned(),
        body: String::new(),
        kind: TicketKind::Feature,
        status: TicketStatus::Todo,
        priority: Priority::new(2).unwrap(),
    };
    store::create_ticket(tx, by, &key(), &new, 1_100)
        .unwrap()
        .short_id
}

fn refused<T: std::fmt::Debug>(result: Result<T, StoreError>) -> &'static str {
    match result {
        Err(StoreError::Refused(refusal)) => refusal.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn count(tx: &Transaction<'_>, table: &str) -> i64 {
    tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn proof(
    id: &ShortId,
    criterion: u32,
    kind: CheckKind,
    check: &str,
    result: EvidenceResult,
) -> NewEvidence {
    NewEvidence {
        ticket: id.clone(),
        criterion,
        kind,
        check: check.to_owned(),
        result,
        reference: format!("run of {check}"),
    }
}

fn set_policy(tx: &Transaction<'_>, setting: PolicyKey, value: serde_json::Value) {
    let version = policy::describe(tx, &key())
        .unwrap()
        .into_iter()
        .find(|entry| entry.key == setting)
        .unwrap()
        .version;
    let set = policy::set(
        tx,
        &Standalone,
        &owner(),
        &key(),
        setting,
        &value,
        version,
        1_050,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(set).unwrap()[setting.as_str()], value);
}

fn engine_tables(tx: &Transaction<'_>) {
    tx.execute_batch(ENGINE_TABLES).unwrap();
}

#[test]
fn the_last_passing_evidence_closes_the_ticket_and_records_why() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    assert_eq!(
        policy::load(&tx, &key()).unwrap().close,
        ClosePolicy::Evidence
    );
    let id = ticket(&tx, &owner(), "Close on the last proof");
    let agent = actor("agent:w1");
    let build = criteria::add_criterion(&tx, &Standalone, &owner(), &id, "The build passes", 1_200)
        .unwrap();
    let review = criteria::add_criterion(
        &tx,
        &Standalone,
        &agent,
        &id,
        "The owner reviewed the change",
        1_210,
    )
    .unwrap();
    criteria::add_check(
        &tx,
        &Standalone,
        &owner(),
        &id,
        build.position,
        CheckKind::Ci,
        "ci:build",
        1_220,
    )
    .unwrap();
    criteria::add_check(
        &tx,
        &Standalone,
        &agent,
        &id,
        review.position,
        CheckKind::Manual,
        "review:owner",
        1_230,
    )
    .unwrap();

    let (_, first) = criteria::record_evidence(
        &tx,
        &Standalone,
        &agent,
        &proof(
            &id,
            build.position,
            CheckKind::Ci,
            "ci:build",
            EvidenceResult::Pass,
        ),
        1_300,
    )
    .unwrap();
    assert_eq!(first.criterion.state, CriterionState::Met);
    assert_eq!(first.ticket.status, TicketStatus::Todo);
    assert_eq!(first.ticket.closed_by, None);

    let (recorded, last) = criteria::record_evidence(
        &tx,
        &Standalone,
        &owner(),
        &proof(
            &id,
            review.position,
            CheckKind::Manual,
            "review:owner",
            EvidenceResult::Pass,
        ),
        1_400,
    )
    .unwrap();
    assert_eq!(recorded.result, EvidenceResult::Pass);
    assert_eq!(recorded.recorded_by, owner());
    assert_eq!(recorded.reference, "run of review:owner");
    assert_eq!(last.criterion.state, CriterionState::Met);
    let closed = store::ticket(&tx, &id).unwrap();
    assert_eq!(closed, last.ticket);
    assert_eq!(closed.status, TicketStatus::Done);
    assert_eq!(closed.closed_by, Some(Actor::host("evidence")));
    assert_eq!(closed.closed_ms, Some(1_400));
    let reason = closed.close_reason.clone().unwrap();
    assert!(reason.contains("2 of 2 criteria met or waived"), "{reason}");
    assert!(reason.contains("manual check review:owner"), "{reason}");

    let history = events::history(&tx, &id).unwrap();
    let close = &history[history.len() - 1];
    assert_eq!(close.op, events::TICKET_UPDATE);
    assert_eq!(close.actor, Actor::host("evidence"));
    assert_eq!(close.after.as_ref().unwrap()["status"], "done");
    let met = &history[history.len() - 2];
    assert_eq!(met.op, events::CRITERION_SET);
    assert_eq!(met.actor, Actor::host("evidence"));
    assert_eq!(met.after.as_ref().unwrap()["state"], "met");
    let evidence = &history[history.len() - 3];
    assert_eq!(evidence.op, events::EVIDENCE_RECORD);
    assert_eq!(evidence.actor, owner());

    let late = criteria::record_evidence(
        &tx,
        &Standalone,
        &agent,
        &proof(
            &id,
            build.position,
            CheckKind::Ci,
            "ci:build",
            EvidenceResult::Fail,
        ),
        1_500,
    );
    assert_eq!(refused(late), codes::TICKET_CLOSED);
    assert_eq!(count(&tx, "tk_evidence"), 2);
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
    tx.commit().unwrap();
}

#[test]
fn a_failing_evidence_reopens_a_met_criterion() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let id = ticket(&tx, &owner(), "Reopen on a failing run");
    let agent = actor("agent:w1");
    let tests = criteria::add_criterion(
        &tx,
        &Standalone,
        &owner(),
        &id,
        "Unit tests and lint pass",
        1_200,
    )
    .unwrap();
    let docs = criteria::add_criterion(&tx, &Standalone, &owner(), &id, "The docs name it", 1_210)
        .unwrap();
    for check in ["ci:unit", "ci:lint"] {
        criteria::add_check(
            &tx,
            &Standalone,
            &owner(),
            &id,
            tests.position,
            CheckKind::Ci,
            check,
            1_220,
        )
        .unwrap();
    }
    let record = |check: &str, result: EvidenceResult, at_ms: i64| {
        criteria::record_evidence(
            &tx,
            &Standalone,
            &agent,
            &proof(&id, tests.position, CheckKind::Ci, check, result),
            at_ms,
        )
        .unwrap()
        .1
    };

    let unit = record("ci:unit", EvidenceResult::Pass, 1_300);
    assert_eq!(unit.criterion.state, CriterionState::Open);
    let lint = record("ci:lint", EvidenceResult::Pass, 1_310);
    assert_eq!(lint.criterion.state, CriterionState::Met);
    let met_version = lint.criterion.version;

    let failed = record("ci:lint", EvidenceResult::Fail, 1_400);
    assert_eq!(failed.criterion.state, CriterionState::Open);
    assert_eq!(failed.criterion.version, met_version + 1);
    assert_eq!(failed.ticket.status, TicketStatus::Todo);
    assert_eq!(failed.ticket.closed_by, None);
    let reopened = events::history(&tx, &id).unwrap().pop().unwrap();
    assert_eq!(reopened.op, events::CRITERION_SET);
    assert_eq!(reopened.actor, Actor::host("evidence"));
    assert_eq!(reopened.before.as_ref().unwrap()["state"], "met");
    assert_eq!(reopened.after.as_ref().unwrap()["state"], "open");

    let by_hand = criteria::set_criterion(
        &tx,
        &Standalone,
        &owner(),
        &id,
        tests.position,
        failed.criterion.version,
        CriterionState::Met,
        1_450,
    );
    assert_eq!(refused(by_hand), codes::CRITERION_HAS_CHECKS);
    let waive_by_agent = criteria::set_criterion(
        &tx,
        &Standalone,
        &agent,
        &id,
        docs.position,
        docs.version,
        CriterionState::Waived,
        1_460,
    );
    assert_eq!(refused(waive_by_agent), codes::OWNER_ONLY);

    let again = record("ci:lint", EvidenceResult::Pass, 1_500);
    assert_eq!(again.criterion.state, CriterionState::Met);
    assert_eq!(again.ticket.status, TicketStatus::Todo);
    assert_eq!(count(&tx, "tk_evidence"), 4);
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
    tx.commit().unwrap();
}

#[test]
fn each_close_option_allows_and_refuses_the_right_actors() {
    let others = [
        actor("agent:w1"),
        actor("master:s1"),
        actor("engine:landing"),
    ];
    let options = [
        (ClosePolicy::Agents, None),
        (ClosePolicy::Evidence, Some(codes::CLOSE_NEEDS_EVIDENCE)),
        (ClosePolicy::Owner, Some(codes::CLOSE_OWNER_ONLY)),
    ];
    assert_eq!(options.len(), ClosePolicy::ALL.len());
    for (option, refusal) in options {
        let dir = tempfile::tempdir().unwrap();
        let mut store = opened(dir.path());
        let tx = store.transaction().unwrap();
        project(&tx);
        set_policy(&tx, PolicyKey::Close, json!(option.as_str()));
        assert_eq!(policy::load(&tx, &key()).unwrap().close, option);

        for other in &others {
            let id = ticket(&tx, &owner(), "Close by hand");
            let events_before = count(&tx, "tk_events");
            let result = policy::close(
                &tx,
                &Standalone,
                other,
                &id,
                1,
                TicketStatus::Done,
                Some("finished"),
                1_500,
            );
            match refusal {
                None => {
                    let closed = result.unwrap();
                    assert_eq!(closed.status, TicketStatus::Done);
                    assert_eq!(closed.closed_by.as_ref(), Some(other));
                    assert_eq!(closed.close_reason.as_deref(), Some("finished"));
                }
                Some(code) => {
                    assert_eq!(refused(result), code, "{option} {other}");
                    assert_eq!(count(&tx, "tk_events"), events_before);
                    assert_eq!(store::ticket(&tx, &id).unwrap().status, TicketStatus::Todo);
                }
            }
        }

        let id = ticket(&tx, &owner(), "The owner always closes");
        let closed = policy::close(
            &tx,
            &Standalone,
            &owner(),
            &id,
            1,
            TicketStatus::Cancelled,
            Some("not needed"),
            1_600,
        )
        .unwrap();
        assert_eq!(closed.status, TicketStatus::Cancelled);
        assert_eq!(closed.closed_by, Some(owner()));
        let twice = policy::close(
            &tx,
            &Standalone,
            &owner(),
            &id,
            closed.version,
            TicketStatus::Done,
            None,
            1_610,
        );
        assert_eq!(refused(twice), codes::TICKET_CLOSED);
        let reopened = policy::reopen(
            &tx,
            &Standalone,
            &owner(),
            &id,
            closed.version,
            TicketStatus::Todo,
            1_700,
        )
        .unwrap();
        assert_eq!(reopened.status, TicketStatus::Todo);
        assert_eq!(reopened.closed_by, None);
        assert_eq!(reopened.close_reason, None);

        let id = ticket(&tx, &owner(), "Proven by a manual check");
        let criterion =
            criteria::add_criterion(&tx, &Standalone, &owner(), &id, "Checked by hand", 1_800)
                .unwrap();
        criteria::add_check(
            &tx,
            &Standalone,
            &owner(),
            &id,
            criterion.position,
            CheckKind::Manual,
            "look:release-notes",
            1_810,
        )
        .unwrap();
        criteria::record_evidence(
            &tx,
            &Standalone,
            &others[0],
            &proof(
                &id,
                criterion.position,
                CheckKind::Manual,
                "look:release-notes",
                EvidenceResult::Pass,
            ),
            1_900,
        )
        .unwrap();
        let proven = store::ticket(&tx, &id).unwrap();
        if option == ClosePolicy::Evidence {
            assert_eq!(proven.status, TicketStatus::Done);
            assert_eq!(proven.closed_by, Some(Actor::host("evidence")));
        } else {
            assert_eq!(proven.status, TicketStatus::Todo, "{option}");
            assert_eq!(proven.closed_by, None);
        }
        tx.commit().unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let set_by = |who: &Actor, value: serde_json::Value, version: i64| {
        policy::set(
            &tx,
            &Standalone,
            who,
            &key(),
            PolicyKey::Close,
            &value,
            version,
            2_000,
        )
    };
    assert_eq!(
        refused(set_by(&actor("agent:w1"), json!("agents"), 0)),
        codes::OWNER_ONLY
    );
    assert_eq!(
        refused(set_by(&actor("engine:landing"), json!("agents"), 0)),
        codes::OWNER_ONLY
    );
    assert_eq!(
        refused(set_by(&owner(), json!("everyone"), 0)),
        codes::POLICY_VALUE_INVALID
    );
    assert_eq!(
        refused(set_by(&owner(), json!("agents"), 3)),
        codes::STALE_VERSION
    );
    assert_eq!(count(&tx, "tk_policy"), 0);
    set_by(&owner(), json!("owner"), 0).unwrap();
    assert_eq!(policy::load(&tx, &key()).unwrap().close, ClosePolicy::Owner);
    let close = policy::describe(&tx, &key())
        .unwrap()
        .into_iter()
        .find(|entry| entry.key == PolicyKey::Close)
        .unwrap();
    assert_eq!(close.version, 1);
    assert_eq!(close.value, json!("owner"));
    assert_eq!(close.default, json!("evidence"));
    assert_eq!(
        close.options,
        vec![json!("agents"), json!("evidence"), json!("owner")]
    );
    assert_eq!(
        refused(set_by(&owner(), json!("agents"), 0)),
        codes::STALE_VERSION
    );
    tx.commit().unwrap();
}

#[test]
fn each_create_and_agent_edit_option_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    let agent = actor("agent:w1");
    assert_eq!(policy::role(&owner()), Role::Owner);
    assert_eq!(policy::role(&actor("engine:landing")), Role::Engine);
    assert_eq!(policy::role(&agent), Role::Agent);
    assert_eq!(policy::role(&actor("master:s1")), Role::Agent);

    assert_eq!(CreatePolicy::ALL.len(), 3);
    for option in CreatePolicy::ALL.iter().copied() {
        set_policy(&tx, PolicyKey::Create, json!(option.as_str()));
        let policy = policy::load(&tx, &key()).unwrap();
        assert_eq!(policy.create, option);
        Standalone
            .actor_may(&tx, &key(), &owner(), OpKind::Create, None)
            .unwrap();
        assert_eq!(
            policy::start_status(&policy, &owner(), TicketStatus::Backlog),
            TicketStatus::Backlog
        );
        let agent_may = Standalone.actor_may(&tx, &key(), &agent, OpKind::Create, None);
        match option {
            CreatePolicy::Direct => {
                agent_may.unwrap();
                assert_eq!(
                    policy::start_status(&policy, &agent, TicketStatus::Backlog),
                    TicketStatus::Backlog
                );
            }
            CreatePolicy::Triage => {
                agent_may.unwrap();
                assert_eq!(
                    policy::start_status(&policy, &agent, TicketStatus::Todo),
                    TicketStatus::Triage
                );
            }
            CreatePolicy::OwnerOnly => {
                assert_eq!(agent_may.unwrap_err().code, codes::CREATE_OWNER_ONLY);
            }
        }
    }

    assert_eq!(AgentEditPolicy::ALL.len(), 3);
    for option in AgentEditPolicy::ALL.iter().copied() {
        set_policy(&tx, PolicyKey::AgentEdit, json!(option.as_str()));
        let theirs = ticket(&tx, &owner(), "The owner's ticket");
        let own = ticket(&tx, &agent, "The agent's own ticket");
        let view = TicketView::of(&store::ticket(&tx, &theirs).unwrap());
        let edit = Standalone.actor_may(&tx, &key(), &agent, OpKind::Edit, Some(&view));
        let moved = Standalone.actor_may(&tx, &key(), &agent, OpKind::Move, Some(&view));
        let links_before = count(&tx, "tk_links");
        let criteria_before = count(&tx, "tk_criteria");
        let events_before = count(&tx, "tk_events");
        let target = format!("feat/{option}");
        let link = host::link(&tx, &Standalone, &agent, &theirs, "branch", &target, 1_300);
        let criterion =
            criteria::add_criterion(&tx, &Standalone, &agent, &theirs, "It works", 1_310);
        match option {
            AgentEditPolicy::Any => {
                edit.unwrap();
                moved.unwrap();
                assert_eq!(link.unwrap().1.state, "unknown");
                criterion.unwrap();
                assert_eq!(count(&tx, "tk_events"), events_before + 2);
            }
            AgentEditPolicy::StatusAndCriteria => {
                assert_eq!(edit.unwrap_err().code, codes::AGENT_EDIT_REFUSED);
                moved.unwrap();
                assert_eq!(refused(link), codes::AGENT_EDIT_REFUSED);
                assert_eq!(count(&tx, "tk_links"), links_before);
                criterion.unwrap();
                assert_eq!(count(&tx, "tk_events"), events_before + 1);
            }
            AgentEditPolicy::NoEdits => {
                assert_eq!(edit.unwrap_err().code, codes::AGENT_EDIT_REFUSED);
                assert_eq!(moved.unwrap_err().code, codes::AGENT_EDIT_REFUSED);
                assert_eq!(refused(link), codes::AGENT_EDIT_REFUSED);
                assert_eq!(refused(criterion), codes::AGENT_EDIT_REFUSED);
                assert_eq!(count(&tx, "tk_links"), links_before);
                assert_eq!(count(&tx, "tk_criteria"), criteria_before);
                assert_eq!(count(&tx, "tk_events"), events_before);
            }
        }
        host::link(&tx, &Standalone, &agent, &own, "branch", &target, 1_400).unwrap();
        criteria::add_criterion(&tx, &Standalone, &agent, &own, "Mine works", 1_410).unwrap();
        host::link(&tx, &Standalone, &owner(), &theirs, "pr", &target, 1_420).unwrap();
        let own_view = TicketView::of(&store::ticket(&tx, &own).unwrap());
        Standalone
            .actor_may(&tx, &key(), &agent, OpKind::Edit, Some(&own_view))
            .unwrap();
        Standalone
            .actor_may(
                &tx,
                &key(),
                &actor("engine:landing"),
                OpKind::Edit,
                Some(&view),
            )
            .unwrap();
    }
    tx.commit().unwrap();
}

#[test]
fn a_host_refusal_from_links_blocks_the_link_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    engine_tables(&tx);
    tx.execute(
        "INSERT INTO host_branches(project, name) VALUES ('CA', 'feat/at4'), ('OT', 'feat/other')",
        [],
    )
    .unwrap();
    let id = ticket(&tx, &owner(), "Link only what the host knows");
    let links_before = count(&tx, "tk_links");
    let events_before = count(&tx, "tk_events");
    for (kind, target, code) in [
        ("branch", "feat/missing", "link_broken"),
        ("branch", "feat/other", "link_cross_project"),
        ("memory", "m-1", "link_kind_unknown"),
    ] {
        let result = host::link(&tx, &EngineLike, &owner(), &id, kind, target, 1_200);
        assert_eq!(refused(result), code, "{kind} {target}");
        assert_eq!(count(&tx, "tk_links"), links_before);
        assert_eq!(count(&tx, "tk_events"), events_before);
    }
    let hidden = host::link(
        &tx,
        &EngineLike,
        &owner(),
        &id,
        "branch",
        "feat/a\u{202e}b",
        1_210,
    );
    assert_eq!(refused(hidden), codes::TEXT_HIDDEN_CHARACTERS);
    assert_eq!(count(&tx, "tk_events"), events_before);

    let (link, state) =
        host::link(&tx, &EngineLike, &owner(), &id, "branch", "feat/at4", 1_300).unwrap();
    assert_eq!(link.kind, "branch");
    assert_eq!(link.target, "feat/at4");
    assert_eq!(link.created_by, owner());
    assert!(state.live);
    assert_eq!(state.label, "feat/at4");
    assert_eq!(count(&tx, "tk_links"), links_before + 1);
    assert_eq!(count(&tx, "tk_events"), events_before + 1);
    let unlinked = host::unlink(&tx, &EngineLike, &owner(), &id, "branch", "feat/at4", 1_400);
    assert_eq!(unlinked.unwrap().target, "feat/at4");
    assert_eq!(count(&tx, "tk_links"), links_before);
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
    tx.commit().unwrap();
}

#[test]
fn notify_reevaluates_only_tickets_linked_to_that_target() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = opened(dir.path());
    let tx = store.transaction().unwrap();
    project(&tx);
    engine_tables(&tx);
    tx.execute(
        "INSERT INTO host_change_sets(project, id, accepted) VALUES ('CA', 'CS-1', 0), ('CA', 'CS-2', 0)",
        [],
    )
    .unwrap();
    let proven_by = |title: &str, change: &str, linked: bool, extra: bool| {
        let id = ticket(&tx, &owner(), title);
        let criterion =
            criteria::add_criterion(&tx, &EngineLike, &owner(), &id, "The change lands", 1_200)
                .unwrap();
        let check = criteria::add_check(
            &tx,
            &EngineLike,
            &owner(),
            &id,
            criterion.position,
            CheckKind::Change,
            change,
            1_210,
        )
        .unwrap();
        assert_eq!(check.reference, change);
        if extra {
            criteria::add_criterion(&tx, &EngineLike, &owner(), &id, "The owner tried it", 1_220)
                .unwrap();
        }
        if linked {
            let (_, state) =
                host::link(&tx, &EngineLike, &owner(), &id, "change_set", change, 1_230).unwrap();
            assert_eq!(state.state, "open");
        }
        id
    };
    let linked = proven_by("Linked to CS-1", "CS-1", true, false);
    let other = proven_by("Linked to CS-2", "CS-2", true, false);
    let unlinked = proven_by("Checks CS-1 without a link", "CS-1", false, false);
    let partial = proven_by("Linked to CS-1 with more to prove", "CS-1", true, true);
    for id in [&linked, &other, &unlinked, &partial] {
        let ticket = store::ticket(&tx, id).unwrap();
        assert_eq!(ticket.status, TicketStatus::Todo);
    }
    tx.execute("UPDATE host_change_sets SET accepted = 1", [])
        .unwrap();
    let event = |target: &str| HostEvent {
        kind: "change_set".to_owned(),
        target: target.to_owned(),
    };

    let touched = criteria::notify(&tx, &EngineLike, &event("CS-1"), 2_000).unwrap();
    assert_eq!(touched, vec![linked.clone(), partial.clone()]);
    let closed = store::ticket(&tx, &linked).unwrap();
    assert_eq!(closed.status, TicketStatus::Done);
    assert_eq!(closed.closed_by, Some(Actor::host("evidence")));
    assert_eq!(closed.closed_ms, Some(2_000));
    let reason = closed.close_reason.unwrap();
    assert!(reason.contains("1 of 1 criteria met or waived"), "{reason}");
    assert!(reason.contains("change_set CS-1"), "{reason}");
    let half = store::ticket(&tx, &partial).unwrap();
    assert_eq!(half.status, TicketStatus::Todo);
    let first = criteria::criteria(&tx, &partial).unwrap();
    assert_eq!(first[0].state, CriterionState::Met);
    assert_eq!(first[1].state, CriterionState::Open);
    for id in [&other, &unlinked] {
        assert_eq!(store::ticket(&tx, id).unwrap().status, TicketStatus::Todo);
        assert_eq!(
            criteria::criteria(&tx, id).unwrap()[0].state,
            CriterionState::Open
        );
    }

    let events_before = count(&tx, "tk_events");
    let again = criteria::notify(&tx, &EngineLike, &event("CS-1"), 2_100).unwrap();
    assert_eq!(again, vec![partial.clone()]);
    assert_eq!(count(&tx, "tk_events"), events_before);
    let nobody = criteria::notify(&tx, &EngineLike, &event("CS-9"), 2_150).unwrap();
    assert!(nobody.is_empty());

    let second = criteria::notify(&tx, &EngineLike, &event("CS-2"), 2_200).unwrap();
    assert_eq!(second, vec![other.clone()]);
    assert_eq!(
        store::ticket(&tx, &other).unwrap().status,
        TicketStatus::Done
    );
    assert_eq!(
        store::ticket(&tx, &unlinked).unwrap().status,
        TicketStatus::Todo
    );
    assert_eq!(
        events::replay(&tx, &key()).unwrap(),
        store::state(&tx, &key()).unwrap()
    );
    tx.commit().unwrap();
}
