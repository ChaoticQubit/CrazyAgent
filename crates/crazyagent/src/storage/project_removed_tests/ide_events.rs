use super::*;
use crate::ide::log::{
    self, Change, FileUnlocked, IdeAsked, RunModeSet, WorkspaceForgotten, WorkspaceWritten,
};
use crate::state_log::TOMBSTONE;
use rusqlite::types::Value;

/// The four ide tables, each compared live against an ide-only rebuild of the log.
const IDE_TABLES: [&str; 4] = [
    "ide_asks",
    "ide_run_settings",
    "ide_unlocked",
    "ide_workspaces",
];

/// Logs one change of each ide kind for a project through the ide log, its file unlocked
/// for the project's working agent `first + 2`, the one agent a removal keeps. Inputs: the
/// store, the project and its first agent id. Output: none.
fn ide_changes(s: &Store, project: &str, first: i64) {
    let named = || ProjectId::new(project).unwrap();
    let changes = [
        Change::Asked(IdeAsked {
            project: named(),
            entry: RequestId::new("fact").unwrap(),
            version: 1,
            decision: crate::ide::bridge::Decision::Approve,
            asked_ms: 1,
        }),
        Change::RunModeSet(RunModeSet {
            project: named(),
            mode: crate::ide::bridge::run_command::RunMode::Off,
        }),
        Change::FileUnlocked(FileUnlocked {
            project: named(),
            agent: first + 2,
            path: log::WorktreePath::try_from("src/lib.rs".to_owned()).unwrap(),
            turns: 0,
        }),
        Change::WorkspaceForgotten(WorkspaceForgotten { project: named() }),
        Change::WorkspaceWritten(WorkspaceWritten {
            project: named(),
            show_all: true,
        }),
    ];
    assert_eq!(changes.len(), log::KINDS.len(), "one change of each kind");
    for change in &changes {
        let tx = log::begin(&s.db).unwrap();
        log::record(&tx, &s.started.host, 1, change).unwrap();
        tx.commit().unwrap();
    }
}

/// The ide events of the log as seq, kind, the project their body names and the body, in
/// seq order; every body must still be a change. Input: the store. Output: the events.
fn ide_events(db: &Connection) -> Vec<(i64, String, String, Vec<u8>)> {
    logged(db)
        .into_iter()
        .filter(|(_, aggregate, ..)| aggregate.as_str() == Aggregate::Ide.name())
        .map(|(seq, _, kind, _, body)| {
            let fields: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let project = fields["project"].as_str().unwrap().to_owned();
            (seq, kind, project, body)
        })
        .collect()
}

/// The rows a project holds in each table of [`IDE_TABLES`], `ide_unlocked` counted by the
/// agent that holds the files. Inputs: the store, the project and the agent. Output: the
/// counts in table order.
fn held_ide(db: &Connection, project: &str, agent: i64) -> [i64; 4] {
    let count =
        |sql: &str, key: Value| -> i64 { db.query_row(sql, [key], |row| row.get(0)).unwrap() };
    let named = || Value::from(project.to_owned());
    [
        count("SELECT count(*) FROM ide_asks WHERE project=?1", named()),
        count(
            "SELECT count(*) FROM ide_run_settings WHERE project=?1",
            named(),
        ),
        count(
            "SELECT count(*) FROM ide_unlocked WHERE agent=?1",
            Value::from(agent),
        ),
        count(
            "SELECT count(*) FROM ide_workspaces WHERE project=?1",
            named(),
        ),
    ]
}

/// v2.5 L7, rv-sec2 #309 N1 and code F3: Remove from dashboard tombstones every earlier ide
/// event of the removed project, whatever its kind, the unlocked file's path with it, and
/// keeps the other project's; its projection drops the project's rows from the four ide
/// tables, so an ide-only rebuild of the log brings none of them back and equals the live
/// tables.
#[test]
fn a_removed_projects_ide_events_are_tombstoned_and_an_ide_rebuild_brings_none_back() {
    let mut s = store();
    seed(&mut s.db, REMOVED, 10);
    seed(&mut s.db, OTHER, 20);
    s.db.execute_batch(crate::ide::bridge::memory::ASKS_SCHEMA)
        .unwrap();
    ide_changes(&s, REMOVED, 10);
    ide_changes(&s, OTHER, 20);
    let before = ide_events(&s.db);
    let all: BTreeSet<&str> = log::KINDS.into_iter().collect();
    for project in [REMOVED, OTHER] {
        let kinds: BTreeSet<&str> = before
            .iter()
            .filter(|(_, _, named, _)| named == project)
            .map(|(_, kind, _, _)| kind.as_str())
            .collect();
        assert_eq!(kinds, all, "{project} has one event of each ide kind");
    }
    assert_eq!(before.len(), 2 * log::KINDS.len());
    assert_eq!(held_ide(&s.db, REMOVED, 12), [1; 4]);

    let request = remove_request(&mut s.db);
    let admission = &s.started.owners[0].admission;
    let applied = owner_apply(&mut s.db, &s.config, &s.path, admission, &request).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);

    let after = logged(&s.db);
    for (seq, kind, project, body) in &before {
        let (_, _, _, _, now) = after.iter().find(|event| event.0 == *seq).unwrap();
        if project == REMOVED {
            assert_eq!(now.as_slice(), TOMBSTONE, "{kind} of {project} is redacted");
        } else {
            assert_eq!(now, body, "{kind} of {project} stays");
        }
    }
    let (_, aggregate, kind, _, body) = after.last().unwrap();
    assert_eq!(
        (aggregate.as_str(), kind.as_str()),
        (Aggregate::Projects.name(), event::KIND),
        "the removal is the last event and is kept"
    );
    assert_eq!(body.as_slice(), br#"{"project":"removed","kept":true}"#);
    assert_eq!(held_ide(&s.db, REMOVED, 12), [0; 4], "no ide row stays");
    assert_eq!(
        held_ide(&s.db, OTHER, 22),
        [1; 4],
        "the other project keeps its rows"
    );

    let copy = s.root.path().join("ide-rebuilt.db");
    s.db.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    let rebuilt = Connection::open(&copy).unwrap();
    log::rebuild(&rebuilt).unwrap();
    for table in IDE_TABLES {
        assert_eq!(
            rows(&rebuilt, table).unwrap(),
            rows(&s.db, table).unwrap(),
            "{table} rebuilds equal"
        );
    }
    assert_eq!(held_ide(&rebuilt, REMOVED, 12), [0; 4]);
}
