use super::*;
use crate::landing_log::{self, Approved, Change, Project, Snoozed};
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
    let before = landing_events(&s.db);
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

/// Every landing event in log order, as its seq, the project its body names and the body.
/// Input: the store. Output: the events.
fn landing_events(db: &Connection) -> Vec<(i64, String, Vec<u8>)> {
    logged(db)
        .into_iter()
        .filter(|(_, aggregate, _, _, _)| aggregate == Aggregate::Landing.name())
        .map(|(seq, _, _, _, body)| {
            let named: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let project = named["project"].as_str().unwrap().to_owned();
            (seq, project, body)
        })
        .collect()
}
