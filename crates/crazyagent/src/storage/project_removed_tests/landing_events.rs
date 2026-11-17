use super::*;
use crate::landing_log::{self, AgentBranch, Approved, Base, Change, OwnerEdit, Project, Snoozed};
use crate::state_log::TOMBSTONE;

/// The project-keyed landing tables the row compares live against a landing rebuild.
const LANDING_TABLES: [&str; 2] = ["github_publish_prompt", "verification_current"];

/// V250-L9 (leader 10:46, QUESTION-1046 A): Create demo clears an earlier id, so the id's
/// landing events are tombstoned in the demo transaction under the owner fence with its
/// rows, and a landing rebuild of the log brings none back; the other project keeps its.
#[test]
fn a_cleared_ids_landing_events_are_tombstoned_and_a_landing_rebuild_brings_none_back() {
    let mut s = store();
    landing_changes(&s.db, REMOVED);
    landing_changes(&s.db, OTHER);
    let before = landing_events(&s.db, "project");
    assert_eq!(before.len(), 6, "three landing events per project");
    let cleared = ProjectId::new(REMOVED).unwrap();
    let owner = fence::Fence::Owner(s.started.owners[0].admission.clone());
    let tx = s.db.transaction().unwrap();
    crate::project_setup::removed::clear(&tx, &cleared, &owner).unwrap();
    tx.commit().unwrap();

    let after = logged(&s.db);
    for (seq, project, body) in &before {
        let (_, _, kind, _, now) = after.iter().find(|event| event.0 == *seq).unwrap();
        if project == REMOVED {
            assert_eq!(now.as_slice(), TOMBSTONE, "{kind} of {project} is redacted");
        } else {
            assert_eq!(now, body, "{kind} of {project} stays");
        }
    }
    let copy = s.root.path().join("landing-rebuilt.db");
    s.db.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    let rebuilt = Connection::open(&copy).unwrap();
    landing_log::rebuild(&rebuilt).unwrap();
    for table in LANDING_TABLES {
        assert_eq!(
            rows(&rebuilt, table).unwrap(),
            rows(&s.db, table).unwrap(),
            "{table} rebuilds equal"
        );
        let held: i64 = rebuilt
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE project=?1"),
                [REMOVED],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(held, 0, "{table} holds no row of the cleared id");
    }
}

/// Logs a Not now and a Don't ask again to the project's publish prompt and an approval of
/// its baseline `b-<project>` through the landing log. Inputs: the store and the project.
/// Output: none.
fn landing_changes(db: &Connection, project: &str) {
    let changes = [
        Change::PublishPromptSnoozed(Snoozed {
            project: project.into(),
            until_ms: 5,
        }),
        Change::PublishPromptNever(Project {
            project: project.into(),
        }),
        Change::BaselineApproved(Approved {
            project: project.into(),
            baseline: format!("b-{project}"),
        }),
    ];
    for change in &changes {
        landing_log::write(db, change).unwrap();
    }
}

/// Every landing event in log order, as its seq, the value its body names at `field` and
/// the body. Inputs: the store and the field. Output: the events.
fn landing_events(db: &Connection, field: &str) -> Vec<(i64, String, Vec<u8>)> {
    logged(db)
        .into_iter()
        .filter(|(_, aggregate, _, _, _)| aggregate == Aggregate::Landing.name())
        .map(|(seq, _, _, _, body)| {
            let named: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let value = named[field].as_str().unwrap().to_owned();
            (seq, value, body)
        })
        .collect()
}

/// The agent worktrees the Delete row names: [`REMOVED`]'s own, [`OTHER`]'s own, and one
/// both projects name.
const GONE: &str = "/repo/.crazyagent/worktrees/agent-10";
const STAYS: &str = "/repo/.crazyagent/worktrees/agent-20";
const SHARED: &str = "/repo/.crazyagent/worktrees/agent-30";

/// The worktree-keyed landing tables the Delete row compares live against a rebuild.
const WORKTREE_TABLES: [&str; 3] = [
    "landing_agent_branches",
    "landing_bases",
    "landing_owner_edits",
];

/// rv-sec3 #415 F1: Delete everything forgets the agent worktrees only its project names:
/// every landing event naming one is tombstoned and its agent branch, base and owner edit
/// rows go in the delete's transaction, so no landing event names it and a landing rebuild
/// equals the live tables; the other project's worktree, and a path both name, keep theirs.
#[test]
fn a_deleted_projects_worktree_landing_events_are_tombstoned_and_a_rebuild_brings_none_back() {
    let mut s = store();
    crate::landing::number_change_sets(&s.db).unwrap();
    for (agent, project, worktree) in [
        (10, REMOVED, GONE),
        (20, OTHER, STAYS),
        (30, REMOVED, SHARED),
        (31, OTHER, SHARED),
    ] {
        s.db.execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
             VALUES (?1,?2,'/repo',?3,'/repo/.git')",
            params![agent, project, worktree],
        )
        .unwrap();
        worktree_changes(&s.db, worktree, agent);
    }
    let before = landing_events(&s.db, "worktree");
    assert_eq!(before.len(), 12, "three landing events per worktree row");
    let mutation = OwnerMutation::DeleteProject {
        project_id: ProjectId::new(REMOVED).unwrap(),
        name: REMOVED.into(),
    };
    let request = request(&mut s.db, "delete-project", mutation);
    let done = Ok(Vec::new());
    let delete = Some(Effect::Delete(&done));
    let admission = &s.started.owners[0].admission;
    let applied =
        owner_apply_with(&mut s.db, &s.config, &s.path, admission, &request, delete).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);

    let after = logged(&s.db);
    for (seq, worktree, body) in &before {
        let (_, _, kind, _, now) = after.iter().find(|event| event.0 == *seq).unwrap();
        if worktree == GONE {
            assert_eq!(
                now.as_slice(),
                TOMBSTONE,
                "{kind} of {worktree} is redacted"
            );
        } else {
            assert_eq!(now, body, "{kind} of {worktree} stays");
        }
    }
    let named = |body: &[u8]| body.windows(GONE.len()).any(|part| part == GONE.as_bytes());
    assert!(after.iter().all(|(_, _, _, _, body)| !named(body)));
    worktree_rebuild_equals_live(&s);
}

/// Logs an agent branch, a base and an owner edit for a worktree through the landing log.
/// Inputs: the store, the worktree and its agent id, which names the branch. Output: none.
fn worktree_changes(db: &Connection, worktree: &str, agent: i64) {
    let changes = [
        Change::AgentBranchSet(AgentBranch {
            worktree: worktree.into(),
            repository: "/repo".into(),
            branch: format!("crazyagent/task-{agent}"),
        }),
        Change::BaseSet(Base {
            worktree: worktree.into(),
            base: "a".repeat(40),
        }),
        Change::OwnerEditKept(OwnerEdit {
            worktree: worktree.into(),
            path: "src/main.rs".into(),
            at_ms: 5,
        }),
    ];
    for change in &changes {
        landing_log::write(db, change).unwrap();
    }
}

/// Checks that [`GONE`] holds no row in the worktree-keyed landing tables while the other
/// two worktrees keep theirs, and that a landing rebuild of a copy of the store equals the
/// live tables. Input: the store after the Delete. Output: none.
fn worktree_rebuild_equals_live(s: &Store) {
    let copy = s.root.path().join("worktree-rebuilt.db");
    s.db.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    let rebuilt = Connection::open(&copy).unwrap();
    landing_log::rebuild(&rebuilt).unwrap();
    for table in WORKTREE_TABLES {
        assert_eq!(
            rows(&rebuilt, table).unwrap(),
            rows(&s.db, table).unwrap(),
            "{table} rebuilds equal"
        );
        for (worktree, held) in [(GONE, 0), (STAYS, 1), (SHARED, 1)] {
            let count: i64 =
                s.db.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE worktree=?1"),
                    [worktree],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, held, "{table} rows of {worktree}");
        }
    }
}

/// Every landing kind names a project, an agent worktree, or neither path, and exactly one
/// of them, so a new kind is classed for removal before it can name a path no removal
/// forgets (rv-sec3 #415 F1). Input: none. Output: none.
#[test]
fn every_landing_kind_is_classed_for_removal_once() {
    let neither = [
        landing_log::OWNER_EDITS_FORGOTTEN,
        landing_log::VERIFICATION_FORGOTTEN,
    ];
    for kind in landing_log::KINDS {
        let classes = [
            landing_log::PROJECT_KINDS.contains(&kind),
            landing_log::WORKTREE_KINDS.contains(&kind),
            neither.contains(&kind),
        ];
        assert_eq!(classes.iter().filter(|class| **class).count(), 1, "{kind}");
    }
    let classed = landing_log::PROJECT_KINDS.len() + landing_log::WORKTREE_KINDS.len();
    assert_eq!(classed + neither.len(), landing_log::KINDS.len());
}
