use super::*;
use crate::activity::{ActivityKind, NewActivity};
use crate::coord::settings::CoordSettings;
use crate::coord::tool_switches::{self, Switch};
use crate::ide::bridge::run_command::{self, RunMode};
use crate::scheduling::Strategy;
use crate::storage::versioned;

/// Each table of the second part of a removal's projection (v2.5 L14b) with the column
/// that names the project there: `agent` holds the project's agents, `id` its schedule
/// override. Activity, coordination, GitHub rule proposals, the IDE Run setting, the
/// schedule and usage, which moves a removed project's counts to `<removed>`.
const AREAS: [(&str, &str); 13] = [
    ("project_activity", "project"),
    ("project_activity_backfill", "project"),
    ("project_activity_logged", "project"),
    ("coord_settings", "project"),
    ("coord_activity", "agent"),
    ("coord_tool_use", "agent"),
    ("coord_tool_hours", "agent"),
    ("tool_switches", "scope_project"),
    ("github_rule_requests", "project"),
    ("ide_run_settings", "project"),
    ("account_strategies", "id"),
    ("usage_tokens", "project"),
    ("usage_turns", "project"),
];

/// The tables the engine's own open writes in these areas (activity `initialize`, GitHub
/// `start_store`, usage `initialize` with its row counts), which stay off the log.
const OPENED: [&str; 7] = [
    "project_activity",
    "project_activity_backfill",
    "project_activity_logged",
    "github_rule_requests",
    "usage_tokens",
    "usage_turns",
    "usage_row_counts",
];

/// A live store after a Remove from dashboard of [`REMOVED`], with the copies taken just
/// before it and what [`OTHER`] and the schedule held then.
struct Removed {
    store: Store,
    copy: PathBuf,
    missed: PathBuf,
    other: Vec<(&'static str, i64)>,
    schedule: u64,
}

/// Runs the writes the engine's open makes in these areas: the authority schemas with
/// activity's backfill, the GitHub proposals' start and the usage store's. Input: the
/// store. Output: none.
fn open_as_engine(db: &Connection) {
    initialize_authority_schemas(db).unwrap();
    crate::github::proposals::start_store(db).unwrap();
    crate::usage::initialize(db).unwrap();
}

/// Gives a project one row in every table of [`AREAS`] through the writers that own them,
/// or as their tables hold them once created; its coordination counts belong to its
/// stopped agent `first` and its working agent `first + 2`. Inputs: the store, the project
/// and its first agent id. Output: none.
fn seed_areas(db: &Connection, project: &str, first: i64) {
    let id = ProjectId::new(project).unwrap();
    let phase = NewActivity {
        kind: ActivityKind::Phase,
        actor: "Master",
        agent: None,
        goal: None,
        text: "building",
        change_set: None,
    };
    crate::activity::record_logged(db, &id, (first, false), &phase, 10)
        .unwrap()
        .unwrap();
    crate::activity::backfill(db, &id).unwrap();
    crate::coord::settings::write(db, &id, &CoordSettings::default()).unwrap();
    for agent in [first, first + 2] {
        db.execute(
            "INSERT INTO coord_activity(agent,last_seen) VALUES (?1,1)",
            [agent],
        )
        .unwrap();
        db.execute(
            "INSERT INTO coord_tool_use(agent,tool,count,last) VALUES (?1,'graphify',1,1)",
            [agent],
        )
        .unwrap();
        db.execute(
            "INSERT INTO coord_tool_hours(hour_ms,agent,tool,count) VALUES (0,?1,'graphify',1)",
            [agent],
        )
        .unwrap();
    }
    tool_switches::set(
        db,
        Some(&id),
        tool_switches::CRAZYAGENT_SERVER,
        "spawn",
        Switch::Off,
    )
    .unwrap();
    assert!(
        !crate::github::proposals::waiting(db),
        "an ended and told proposal leaves the apply pass no work"
    );
    db.execute(
        "INSERT INTO github_rule_requests(project,asker,rule,turn_on,why,repo,status,told,created_ms)
         VALUES (?1,'agent:1','require_review',1,'safer','owner/repo','refused',1,1)",
        [project],
    )
    .unwrap();
    let host = crate::ide::log::active_host(db)
        .unwrap()
        .expect("the store's host runs");
    run_command::set_mode(db, &host, &id, RunMode::Off).unwrap();
    db.execute(
        "INSERT INTO account_strategies(scope,id,strategy) VALUES ('project',?1,?2)",
        params![project, Strategy::Balanced.code()],
    )
    .unwrap();
    usage_row(db, project);
    db.execute(
        "INSERT INTO usage_turns(hour_ms,project,agent,turns) VALUES (0,?1,'1',1)",
        [project],
    )
    .unwrap();
}

/// Adds one hour of token counts for a project key, the same hour, run and model for every
/// key, so a removed project's row adds into the one already kept under `<removed>`.
/// Inputs: the store and the key. Output: none.
fn usage_row(db: &Connection, key: &str) {
    db.execute(
        "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,model,
         input,output,cache_read,cache_write,cost_nanos)
         VALUES (0,'agent',?1,'pool','account','1','codex','session','model',1,1,0,0,0)",
        [key],
    )
    .unwrap();
}

/// The rows a project holds in each table of [`AREAS`]; in the agent tables, the rows of
/// its stopped and failed agents, the ones a removal takes. Inputs: the store, the project
/// and its first agent id. Output: (table, rows) per table.
fn held_areas(db: &Connection, project: &str, first: i64) -> Vec<(&'static str, i64)> {
    AREAS
        .iter()
        .map(|(table, column)| {
            let count = match *column {
                "agent" => db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE agent IN (?1, ?1 + 1)"),
                    [first],
                    |row| row.get(0),
                ),
                "id" => db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE scope='project' AND id=?1"),
                    [project],
                    |row| row.get(0),
                ),
                _ => db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE {column}=?1"),
                    [project],
                    |row| row.get(0),
                ),
            };
            (*table, count.unwrap())
        })
        .collect()
}

/// The schedule's version, which a removal that drops a schedule override moves once.
/// Input: the store. Output: the version.
fn schedule_version(db: &Connection) -> u64 {
    versioned::current(db, &versioned::account_schedule_item())
        .unwrap()
        .version
}

/// Opens a store holding [`REMOVED`] (agents 10 to 12) and [`OTHER`] (agents 20 to 22) with
/// rows in every area of both parts of the projection, as the engine's open leaves it,
/// copies it twice, then removes [`REMOVED`] from the dashboard through the owner path.
/// Output: the removed store.
fn removed_store() -> Removed {
    let mut s = store();
    crate::usage::initialize(&s.db).unwrap();
    seed(&mut s.db, REMOVED, 10);
    seed(&mut s.db, OTHER, 20);
    seed_areas(&s.db, REMOVED, 10);
    seed_areas(&s.db, OTHER, 20);
    usage_row(&s.db, crate::usage::REMOVED_PROJECTS);
    open_as_engine(&s.db);
    for (project, first) in [(REMOVED, 10), (OTHER, 20)] {
        for (table, rows) in held_areas(&s.db, project, first) {
            assert!(rows > 0, "{project} is seeded in {table}");
        }
    }
    let other = held_areas(&s.db, OTHER, 20);
    let copy = s.root.path().join("areas-copy.db");
    let missed = s.root.path().join("areas-missed.db");
    for path in [&copy, &missed] {
        s.db.execute("VACUUM INTO ?1", [path.to_str().unwrap()])
            .unwrap();
    }
    let schedule = schedule_version(&s.db);
    let request = remove_request(&mut s.db);
    let admission = &s.started.owners[0].admission;
    let applied = owner_apply(&mut s.db, &s.config, &s.path, admission, &request).unwrap();
    assert_eq!(applied.result, OwnerResult::Applied);
    Removed {
        store: s,
        copy,
        missed,
        other,
        schedule,
    }
}

/// v2.5 L14b: the removal's projection also clears the project's activity, coordination,
/// GitHub rule proposals, IDE Run setting and schedule override, and moves its usage to
/// `<removed>`, so replaying the log into a copy taken before the removal rebuilds every
/// such table equal to the live store, the schedule version included; the other project
/// keeps all of its rows, and a projection that skips the removal does not rebuild equal.
/// The coordination counts of the stopped agent rebuild equal only when coordination is
/// forgotten before the projection deletes that agent.
#[test]
fn a_replayed_removal_leaves_the_project_no_row_in_activity_coord_github_ide_schedule_or_usage() {
    let r = removed_store();
    let live = &r.store.db;
    let events: Vec<_> = logged(live)
        .into_iter()
        .filter(|(_, aggregate, ..)| aggregate.as_str() == "projects")
        .collect();
    assert_eq!(events.len(), 1, "one removal, one projects event");
    let seq = events[0].0 as u64;
    assert!(
        held_areas(live, REMOVED, 10)
            .iter()
            .all(|(_, rows)| *rows == 0),
        "{:?}",
        held_areas(live, REMOVED, 10)
    );
    assert_eq!(held_areas(live, OTHER, 20), r.other);
    assert_eq!(schedule_version(live), r.schedule + 1);

    let rebuilt = rebuild(live, &r.copy, None);
    for table in AREAS
        .iter()
        .map(|(table, _)| *table)
        .chain(["usage_row_counts"])
    {
        assert_eq!(
            rows(&rebuilt, table).unwrap(),
            rows(live, table).unwrap(),
            "{table} rebuilds equal"
        );
    }
    assert_eq!(
        held_areas(&rebuilt, REMOVED, 10),
        held_areas(live, REMOVED, 10)
    );
    assert_eq!(schedule_version(&rebuilt), schedule_version(live));

    let skipped = rebuild(live, &r.missed, Some(seq));
    assert!(
        AREAS
            .iter()
            .any(|(table, _)| rows(&skipped, table).unwrap() != rows(live, table).unwrap()),
        "a projection that skips the removal must not rebuild equal"
    );
}

/// v2.5 L14b, leader 03:10 Q1 and 03:23: the engine's open writes in these areas stay off
/// the log, so a rebuild opened as the engine opens equals the live store opened again in
/// every table the open writes. The rebuild takes the live store's projects, which join the
/// log with L15, so the open's backfill finds the same live projects on both.
#[test]
fn a_rebuild_opened_as_the_engine_opens_equals_the_live_store_opened_again() {
    let r = removed_store();
    let live = &r.store.db;
    let rebuilt = rebuild(live, &r.copy, None);
    rebuilt
        .execute("DELETE FROM projects WHERE id=?1", [REMOVED])
        .unwrap();
    open_as_engine(&rebuilt);
    open_as_engine(live);
    for table in OPENED {
        assert_eq!(
            rows(&rebuilt, table).unwrap(),
            rows(live, table).unwrap(),
            "{table} equal after both open"
        );
    }
}
