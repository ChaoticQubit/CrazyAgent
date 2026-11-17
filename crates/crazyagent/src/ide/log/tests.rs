use super::*;
use crate::ide::bridge::Decision;
use crate::ide::bridge::memory::{ASKS_SCHEMA, ask};
use crate::ide::bridge::run_command::{RunMode, set_mode};
use crate::ide::readonly::{self, unlock};
use crate::ide::workspace::tests::{Case, add_project};
use crate::ide::workspace::{self, AGENTS_SHOWN_MAX, follow, show_all, write};
use crate::protocol::{ProjectId, RequestId};
use crate::state_log::fence::{host_started, host_stopped};
use crate::state_log::replay::{redact, rows};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

const NOW_MS: i64 = 1_000_000;
const TABLE_NAMES: [&str; 4] = [
    "ide_asks",
    "ide_run_settings",
    "ide_unlocked",
    "ide_workspaces",
];

/// The refusal code of a result, none when it succeeded. Input: the result. Output: the code.
fn code<T>(result: Result<T, EngineError>) -> Option<String> {
    result.err().map(|error| error.0)
}

/// The project and switch of each followed workspace, by project; the file text is a cache
/// outside the log. Input: the store. Output: the rows.
fn followed(db: &Connection) -> Vec<Vec<Value>> {
    let mut statement = db
        .prepare("SELECT project, show_all FROM ide_workspaces ORDER BY project")
        .unwrap();
    statement
        .query_map([], |row| Ok(vec![row.get(0)?, row.get(1)?]))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The rows of the four ide tables a replay must give back, in [`TABLE_NAMES`] order.
/// Input: the store. Output: each table's rows.
fn tables(db: &Connection) -> Vec<Vec<Vec<Value>>> {
    let mut tables: Vec<_> = TABLE_NAMES[..3]
        .iter()
        .map(|table| rows(db, table).unwrap())
        .collect();
    tables.push(followed(db));
    assert_eq!(tables.len(), TABLE_NAMES.len());
    tables
}

/// How many rows a table holds, none for a table not created yet. Inputs: the store and
/// the table. Output: the count.
fn count(db: &Connection, table: &str) -> i64 {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return 0;
    }
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// The kind and body of every logged ide event, in log order. Input: the store. Output:
/// the events.
fn events(db: &Connection) -> Vec<(String, serde_json::Value)> {
    if count(db, "state_log") == 0 {
        return Vec::new();
    }
    let mut statement = db
        .prepare("SELECT kind, body FROM state_log WHERE aggregate='ide' ORDER BY seq")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (kind, body) = row.unwrap();
            (kind, serde_json::from_slice(&body).unwrap())
        })
        .collect()
}

/// The case project's workspace file. Input: the case. Output: its path.
fn workspace_file(case: &Case) -> std::path::PathBuf {
    let file = case.file();
    assert!(file.ends_with(workspace::FILE));
    file
}

/// The case project's stored switch and cached file text, none when it is not followed.
/// Input: the case. Output: the row.
fn cached(case: &Case) -> Option<(bool, String)> {
    case.db
        .query_row(
            "SELECT show_all, body FROM ide_workspaces WHERE project=?1",
            [case.project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
}

/// Runs every logged ide write once through its own function: two asks, a Run setting,
/// two unlocks, the project's workspace written then switched to Show all, and a second
/// project's workspace written, then forgotten by `follow` once that project is gone.
/// Input: the case. Output: none.
fn every_write(case: &mut Case) {
    let agent = case.agent("sub", "working");
    let (db, host, project) = (&case.db, &case.host, &case.project);
    let entry = |name: &str| RequestId::new(name).unwrap();
    ask(
        db,
        host,
        project,
        &entry("fact"),
        1,
        Decision::Approve,
        NOW_MS,
    )
    .unwrap();
    ask(
        db,
        host,
        project,
        &entry("other"),
        2,
        Decision::Reject,
        NOW_MS + 1,
    )
    .unwrap();
    set_mode(db, host, project, RunMode::Off).unwrap();
    unlock(db, host, project, agent.id, "src/lib.rs").unwrap();
    unlock(db, host, project, agent.id, "docs/a.md").unwrap();
    write(&case.root, host, project).unwrap();
    show_all(&case.root, host, project, true).unwrap();
    let other = case.folder.with_file_name("beta");
    std::fs::create_dir(&other).unwrap();
    add_project(db, "beta", &other);
    write(&case.root, host, &ProjectId::new("beta").unwrap()).unwrap();
    let gone = db
        .execute("DELETE FROM project_setups WHERE project='beta'", [])
        .unwrap();
    assert_eq!(gone, 1);
    follow(db, &case.root).unwrap();
    assert_eq!(count(db, "ide_workspaces"), 1, "beta is no longer followed");
}

/// The acceptance replay row (plan 2.2 dev.2): after every logged ide write, the four
/// tables rebuilt from the log alone equal the live tables; with one event redacted the
/// rebuild differs, so the rows come from the log and not from what was left in place.
#[test]
fn the_ide_tables_rebuilt_from_the_log_equal_the_live_tables() {
    let mut case = Case::new();
    every_write(&mut case);
    let live = tables(&case.db);
    assert!(live.iter().all(|table| !table.is_empty()));
    assert_eq!(events(&case.db).len(), 9, "one event per logged write");
    rebuild(&case.db).unwrap();
    assert_eq!(tables(&case.db), live);
    let seq: i64 = case
        .db
        .query_row(
            "SELECT seq FROM state_log WHERE kind=?1",
            [RUN_MODE_SET],
            |row| row.get(0),
        )
        .unwrap();
    let tx = case.db.unchecked_transaction().unwrap();
    let fence = Fence::Engine(case.host.clone());
    redact(&tx, u64::try_from(seq).unwrap(), &fence).unwrap();
    tx.commit().unwrap();
    rebuild(&case.db).unwrap();
    let skipped = tables(&case.db);
    assert_ne!(skipped, live);
    assert!(
        skipped[1].is_empty(),
        "the redacted Run setting is not rebuilt"
    );
    assert_eq!(skipped[0], live[0]);
}

/// A writer whose engine host stopped is refused at append and leaves no row, no event, no
/// revision and no workspace file, for each logged ide write; the running host writes.
#[test]
fn a_stopped_host_refuses_every_ide_writer_and_leaves_nothing() {
    let mut case = Case::new();
    let agent = case.agent("sub", "working");
    let gone = HostStamp {
        db_instance: case.host.db_instance.clone(),
        engine_epoch: "6".repeat(64),
    };
    host_started(&gone);
    host_stopped(&gone);
    let refused = Some("state_log_fence_out_of_process".to_owned());
    let (db, project) = (&case.db, &case.project);
    let revision = || -> i64 {
        db.query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    let before = revision();
    let fact = RequestId::new("fact").unwrap();
    let asked = ask(db, &gone, project, &fact, 1, Decision::Approve, NOW_MS);
    assert_eq!(code(asked), refused);
    assert_eq!(code(set_mode(db, &gone, project, RunMode::Off)), refused);
    assert_eq!(
        code(unlock(db, &gone, project, agent.id, "src/lib.rs")),
        refused
    );
    assert_eq!(code(write(&case.root, &gone, project)), refused);
    assert_eq!(code(show_all(&case.root, &gone, project, true)), refused);
    for table in TABLE_NAMES {
        assert_eq!(count(db, table), 0, "{table} holds no row");
    }
    assert!(events(db).is_empty());
    assert_eq!(revision(), before, "a refused write moves no revision");
    assert!(std::fs::symlink_metadata(workspace_file(&case)).is_err());
    write(&case.root, &case.host, project).unwrap();
    assert_eq!(events(db).len(), 1, "the running host's write is logged");
}

/// `follow` with no engine host running in the store writes nothing: no file, no row and no
/// event; once the host runs again it rewrites the changed workspace, logging nothing for a
/// rewrite that keeps the switch.
#[test]
fn follow_without_a_running_host_writes_nothing() {
    let mut case = Case::new();
    case.agent("sub", "working");
    write(&case.root, &case.host, &case.project).unwrap();
    let file = workspace_file(&case);
    let text = std::fs::read_to_string(&file).unwrap();
    let logged = events(&case.db).len();
    let offline = case
        .db
        .execute("UPDATE owner_meta SET runtime_state='offline'", [])
        .unwrap();
    assert_eq!(offline, 1);
    let second = case.agent("sub", "working");
    follow(&case.db, &case.root).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_eq!(cached(&case), Some((false, text.clone())));
    assert_eq!(events(&case.db).len(), logged);
    let active = case
        .db
        .execute("UPDATE owner_meta SET runtime_state='active'", [])
        .unwrap();
    assert_eq!(active, 1);
    follow(&case.db, &case.root).unwrap();
    let rewritten = std::fs::read_to_string(&file).unwrap();
    assert!(rewritten.contains(second.worktree.to_str().unwrap()));
    assert_eq!(cached(&case), Some((false, rewritten)));
    assert_eq!(events(&case.db).len(), logged);
}

/// The ide snapshot run of rows that predate the log replays to the rows it was taken
/// from: a table split over several chunks comes back whole, a table not created yet comes
/// back empty, the file text stays out of the run, and the run replaces whatever the
/// tables held before it.
#[test]
fn an_ide_snapshot_run_replays_to_the_rows_it_was_taken_from() {
    let db = Connection::open_in_memory().unwrap();
    let host = seed(&db);
    for schema in [ASKS_SCHEMA, readonly::SCHEMA, workspace::SCHEMA] {
        db.execute_batch(schema).unwrap();
    }
    db.execute(
        "INSERT INTO ide_asks VALUES ('alpha','fact',1,'approve',?1)",
        [NOW_MS],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ide_workspaces VALUES ('alpha',1,'{\"folders\":[]}')",
        [],
    )
    .unwrap();
    let long = "p".repeat(200);
    for index in 0..600 {
        db.execute(
            "INSERT INTO ide_unlocked VALUES (7,?1,3)",
            [format!("{long}/{index}")],
        )
        .unwrap();
    }
    let taken = vec![
        rows(&db, "ide_asks").unwrap(),
        Vec::new(),
        rows(&db, "ide_unlocked").unwrap(),
        followed(&db),
    ];
    let chunks = snapshot(&db).unwrap();
    let of = |table: &str| chunks.iter().filter(|chunk| chunk.table == table).count();
    assert!(
        of("ide_unlocked") >= 2,
        "the unlocked rows split over chunks"
    );
    assert_eq!((of("ide_asks"), of("ide_run_settings")), (1, 1));
    let workspaces = chunks.iter().find(|chunk| chunk.table == "ide_workspaces");
    assert_eq!(
        workspaces.unwrap().rows,
        vec![serde_json::json!({"project": "alpha", "show_all": 1})]
    );
    {
        let tx = begin(&db).unwrap();
        let mut appender = Appender::own(&tx, Fence::Engine(host)).unwrap();
        appender.snapshot(Aggregate::Ide, 1, chunks).unwrap();
        drop(appender);
        tx.commit().unwrap();
    }
    db.execute(
        "INSERT INTO ide_asks VALUES ('alpha','stray',1,'reject',?1)",
        [NOW_MS],
    )
    .unwrap();
    db.execute("DELETE FROM ide_unlocked WHERE path LIKE '%/1%'", [])
        .unwrap();
    let mut projection = Projection::default();
    replay::replay(&db, Aggregate::Ide, 0, |event| projection.apply(&db, event)).unwrap();
    assert_eq!(tables(&db), taken);
}

/// Each ide event body holds exactly its kind's keys, the project at the top level of every
/// one so a redaction finds a project's events by that key, and no file text; every body
/// reads back as its change.
#[test]
fn every_ide_event_body_holds_only_its_kind_keys() {
    let mut case = Case::new();
    every_write(&mut case);
    let keys: BTreeMap<&str, Vec<&str>> = BTreeMap::from([
        (
            IDE_ASKED,
            vec!["asked_ms", "decision", "entry", "project", "version"],
        ),
        (RUN_MODE_SET, vec!["mode", "project"]),
        (FILE_UNLOCKED, vec!["agent", "path", "project", "turns"]),
        (WORKSPACE_WRITTEN, vec!["project", "show_all"]),
        (WORKSPACE_FORGOTTEN, vec!["project"]),
    ]);
    let events = events(&case.db);
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for (kind, body) in &events {
        let mut found: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        found.sort_unstable();
        assert_eq!(found, keys[kind.as_str()], "{kind}");
        assert!(body["project"].is_string(), "{kind} names its project");
        *kinds.entry(kind.as_str()).or_default() += 1;
    }
    assert_eq!(
        kinds,
        BTreeMap::from([
            (FILE_UNLOCKED, 2),
            (IDE_ASKED, 2),
            (RUN_MODE_SET, 1),
            (WORKSPACE_FORGOTTEN, 1),
            (WORKSPACE_WRITTEN, 3),
        ])
    );
    let read = replay::replay(&case.db, Aggregate::Ide, 0, |event| {
        Change::parse(event).map(|_| ())
    });
    assert!(read.is_ok());
}

/// A store rebuilt from the log keeps each followed workspace's switch with an empty file
/// text, and the next follow writes the file again and caches its text without logging,
/// so the cache never serves an empty or stale text (Q3 B).
#[test]
fn a_rebuilt_workspace_cache_is_written_again_on_the_next_follow() {
    let mut case = Case::new();
    case.agent("sub", "working");
    show_all(&case.root, &case.host, &case.project, true).unwrap();
    let file = workspace_file(&case);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.is_empty());
    assert_eq!(cached(&case), Some((true, text.clone())));
    let inode = std::fs::metadata(&file).unwrap().ino();
    rebuild(&case.db).unwrap();
    assert_eq!(cached(&case), Some((true, String::new())));
    let logged = events(&case.db).len();
    follow(&case.db, &case.root).unwrap();
    assert_eq!(cached(&case), Some((true, text.clone())));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_ne!(
        std::fs::metadata(&file).unwrap().ino(),
        inode,
        "written again"
    );
    assert_eq!(events(&case.db).len(), logged);
}

/// The cap and Show all stay after the store is rebuilt from the log, as after an engine
/// restart: the default opens the newest twelve agents, Show all every one (L7 Q7 c, the
/// in-process half of the settings rows).
#[test]
fn show_all_and_the_cap_stay_after_a_rebuild_from_the_log() {
    let mut case = Case::new();
    for _ in 0..AGENTS_SHOWN_MAX + 2 {
        case.agent("sub", "working");
    }
    let (root, host, project) = (case.root.clone(), case.host.clone(), case.project.clone());
    let written = write(&root, &host, &project).unwrap();
    assert_eq!((written.hidden, written.show_all), (2, false));
    rebuild(&case.db).unwrap();
    let written = write(&root, &host, &project).unwrap();
    assert_eq!(
        (written.hidden, written.show_all),
        (2, false),
        "the cap stays"
    );
    show_all(&root, &host, &project, true).unwrap();
    rebuild(&case.db).unwrap();
    let written = write(&root, &host, &project).unwrap();
    assert_eq!(
        (written.hidden, written.show_all),
        (0, true),
        "Show all is kept"
    );
}

/// rv-sec2 L7 r2 N2: each `Change` variant's kind is in [`KINDS`] at the variant's place, so
/// a removal redacts every kind and the L1-N1 row holds a body rule for each. The match has
/// no wildcard: a new variant fails to build here until it has an arm, and a sample of it
/// then fails until [`KINDS`] lists its kind.
#[test]
fn every_change_kind_is_in_kinds_at_its_variants_place() {
    let project = ProjectId::new("app").unwrap();
    let changes = [
        Change::Asked(IdeAsked {
            project: project.clone(),
            entry: RequestId::new("m1").unwrap(),
            version: 1,
            decision: Decision::Approve,
            asked_ms: NOW_MS,
        }),
        Change::RunModeSet(RunModeSet {
            project: project.clone(),
            mode: RunMode::Off,
        }),
        Change::FileUnlocked(FileUnlocked {
            project: project.clone(),
            agent: 1,
            path: WorktreePath::try_from("src/a.rs".to_owned()).unwrap(),
            turns: 0,
        }),
        Change::WorkspaceWritten(WorkspaceWritten {
            project: project.clone(),
            show_all: false,
        }),
        Change::WorkspaceForgotten(WorkspaceForgotten { project }),
    ];
    let mut seen = [false; KINDS.len()];
    for change in &changes {
        let at = match change {
            Change::Asked(_) => 0,
            Change::RunModeSet(_) => 1,
            Change::FileUnlocked(_) => 2,
            Change::WorkspaceWritten(_) => 3,
            Change::WorkspaceForgotten(_) => 4,
        };
        assert!(!seen[at], "one sample of each variant");
        seen[at] = true;
        assert_eq!(change.kind(), KINDS[at], "{change:?}");
    }
    assert!(
        seen.iter().all(|sampled| *sampled),
        "a sample of every variant"
    );
}
