use super::*;
use crate::protocol::{Outcome, ProjectSpec};
use std::path::PathBuf;

/// A fresh engine database with every table the engine makes. Output: its folder, which
/// must outlive the connection, and the connection.
fn database() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let db = Connection::open(directory.path().join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    (directory, db)
}

fn id(text: &str) -> ProjectId {
    ProjectId::new(text).unwrap()
}

fn live(db: &Connection, project: &str) {
    db.execute("INSERT INTO projects VALUES (?1,'{}')", [project])
        .unwrap();
}

fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// The table a V3 build keeps standing connector allows in, made here as that build makes
/// it, so these rows hold for tables this build does not know.
const STANDING_ALLOWS: &str =
    "CREATE TABLE connector_write_allowed (project TEXT NOT NULL, connector TEXT NOT NULL);";

#[test]
fn a_returnable_removal_answers_its_id_only_for_its_exact_folder() {
    let (_directory, db) = database();
    record(&db, &id("my-app"), Some("/h/my-app"), true).unwrap();
    assert_eq!(returning(&db, "/h/my-app").unwrap(), Some(id("my-app")));
    for other in ["/h/my-app/", "/h/My-app", "/h/my-app ", "/o/my-app", "/h"] {
        assert_eq!(
            returning(&db, other).unwrap(),
            None,
            "{other:?} is another folder"
        );
    }
    record(&db, &id("old-app"), Some("/h/old-app"), false).unwrap();
    assert_eq!(
        returning(&db, "/h/old-app").unwrap(),
        None,
        "a project deleted with everything never comes back"
    );
    assert!(held(&db, &id("old-app")).unwrap());
    record(&db, &id("no-folder"), None, true).unwrap();
    assert!(held(&db, &id("no-folder")).unwrap());
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM projects_removed WHERE project='no-folder' AND returns=0"
        ),
        1,
        "a removal with no recorded folder can never come back"
    );
}

#[test]
fn an_id_a_removal_holds_is_stepped_over() {
    let (_directory, db) = database();
    let folder = Path::new("/o/My App");
    record(&db, &id("my-app"), Some("/h/my-app"), true).unwrap();
    assert!(held(&db, &id("MY-APP")).unwrap(), "ASCII case is ignored");
    assert_eq!(free_id(&db, folder).unwrap(), Ok(id("my-app-2")));
    record(&db, &id("my-app-2"), None, false).unwrap();
    assert_eq!(free_id(&db, folder).unwrap(), Ok(id("my-app-3")));
    assert_eq!(
        free_id(&db, Path::new("/o/other")).unwrap(),
        Ok(id("other"))
    );
}

/// rv-code2 RS1-4: a live id belongs to another folder, since a folder's own live project is
/// answered before any id is picked, so it is stepped over as a held one is.
#[test]
fn a_live_candidate_of_another_folder_is_stepped_over_too() {
    let (_directory, db) = database();
    record(&db, &id("my-app"), Some("/h/my-app"), true).unwrap();
    live(&db, "my-app-2");
    assert_eq!(
        free_id(&db, Path::new("/o/my-app")).unwrap(),
        Ok(id("my-app-3"))
    );
    live(&db, "Your-App");
    assert_eq!(
        free_id(&db, Path::new("/o/your-app")).unwrap(),
        Ok(id("your-app-2")),
        "a live id differing only by ASCII case is the same id"
    );
}

/// rv-code2 RS1-4: a stepped id is an ordinary name, so a folder whose own name it is still
/// gets a project, the next free `-n` of that name, never a refusal.
#[test]
fn a_folder_named_like_a_stepped_id_still_gets_a_project() {
    let (_directory, db) = database();
    record(&db, &id("my-app"), Some("/h/my-app"), true).unwrap();
    let stepped = free_id(&db, Path::new("/o/my-app")).unwrap().unwrap();
    assert_eq!(stepped, id("my-app-2"));
    live(&db, stepped.as_str());
    assert_eq!(
        free_id(&db, Path::new("/p/my-app-2")).unwrap(),
        Ok(id("my-app-2-2"))
    );
}

#[test]
fn ids_with_rows_that_outlive_their_project_are_backfilled_as_held() {
    let (_directory, db) = database();
    live(&db, "live-app");
    db.execute_batch(&format!(
        "{STANDING_ALLOWS}
         INSERT INTO connector_write_allowed VALUES ('gone-app','linear'),('live-app','linear');
         INSERT INTO memory_library(id,project,origin,text) VALUES
            ('m1','old-app','{{\"kind\":\"written\"}}','Deploys go out on Tuesdays.'),
            ('m2',NULL,'{{\"kind\":\"written\"}}','A note for every project.');"
    ))
    .unwrap();
    hold_left(&db).unwrap();
    for gone in ["old-app", "gone-app"] {
        assert!(held(&db, &id(gone)).unwrap(), "{gone} is held");
        assert_eq!(
            count(
                &db,
                &format!(
                    "SELECT count(*) FROM projects_removed WHERE project='{gone}' AND returns=0"
                )
            ),
            1,
            "{gone} has no recorded folder, so it never comes back"
        );
    }
    assert!(!held(&db, &id("live-app")).unwrap(), "a live project's rows hold nothing");
    assert_eq!(
        free_id(&db, Path::new("/h/old-app")).unwrap(),
        Ok(id("old-app-2"))
    );
    assert_eq!(
        free_id(&db, Path::new("/h/gone-app")).unwrap(),
        Ok(id("gone-app-2"))
    );
    let recorded = count(&db, "SELECT count(*) FROM projects_removed");
    hold_left(&db).unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM projects_removed"),
        recorded,
        "a second start adds nothing"
    );
}

/// rv-code2 RS1-1: the backfill reads every table once, at the first open of a database, and
/// every later open skips it, so engine start never scans the project tables again. A row
/// left after that first open is not read.
#[test]
fn the_backfill_runs_once_and_every_later_open_skips_it() {
    let (_directory, db) = database();
    db.execute_batch(
        "INSERT INTO memory_library(id,project,origin,text) VALUES
            ('m1','old-app','{\"kind\":\"written\"}','Deploys go out on Tuesdays.');",
    )
    .unwrap();
    assert!(matches!(hold_left(&db).unwrap(), Some(n) if n >= 1));
    assert!(held(&db, &id("old-app")).unwrap(), "the first open holds old-app");
    db.execute_batch(
        "INSERT INTO memory_library(id,project,origin,text) VALUES
            ('m2','late-app','{\"kind\":\"written\"}','Written after the first open.');",
    )
    .unwrap();
    let recorded = count(&db, "SELECT count(*) FROM projects_removed");
    assert_eq!(hold_left(&db).unwrap(), None, "a later open skips the backfill");
    assert_eq!(count(&db, "SELECT count(*) FROM projects_removed"), recorded);
    assert!(!held(&db, &id("late-app")).unwrap());
    assert_eq!(hold_left(&db).unwrap(), None);
}

/// Leader 01:11, run-once vetoed alone: an id with rows left after the backfill ran, as a
/// build without the removal record leaves them, is stepped over as if held, whatever the
/// case of the rows' id; a leftover onboarding row alone is not, since starting onboarding
/// replaces or refuses that row itself.
#[test]
fn a_leftover_written_after_the_backfill_still_blocks_its_id() {
    let (_directory, db) = database();
    assert!(hold_left(&db).unwrap().is_some());
    db.execute_batch(
        "INSERT INTO memory_library(id,project,origin,text) VALUES
            ('m1','late-app','{\"kind\":\"written\"}','Written after the first open.'),
            ('m2','Loud-App','{\"kind\":\"written\"}','Written under capitals.');
         INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
            VALUES ('draft-app','/gone/draft-app','pool-team','done','done','{}',1,1);",
    )
    .unwrap();
    let recorded = count(&db, "SELECT count(*) FROM projects_removed");
    assert_eq!(hold_left(&db).unwrap(), None);
    assert_eq!(
        free_id(&db, Path::new("/p/late-app")).unwrap(),
        Ok(id("late-app-2")),
        "a leftover written after the backfill still blocks its id"
    );
    assert_eq!(
        free_id(&db, Path::new("/p/loud-app")).unwrap(),
        Ok(id("loud-app-2"))
    );
    assert_eq!(
        free_id(&db, Path::new("/p/draft-app")).unwrap(),
        Ok(id("draft-app"))
    );
    assert!(!held(&db, &id("late-app")).unwrap(), "the probe holds nothing itself");
    assert_eq!(count(&db, "SELECT count(*) FROM projects_removed"), recorded);
}

/// rv-sa241 N2: the backfill never fills the record past `REMOVED_MAX`, so leftover ids of an
/// old database can't leave Remove and Delete everything refused for good.
#[test]
fn the_backfill_stops_at_the_record_bound() {
    let (_directory, db) = database();
    db.execute(
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{})
             INSERT INTO projects_removed(project,folder,returns,removed_ms)
             SELECT 'gone-'||i, NULL, 0, 0 FROM n",
            REMOVED_MAX - 2
        ),
        [],
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO memory_library(id,project,origin,text) VALUES
            ('m1','left-1','{\"kind\":\"written\"}','One.'),
            ('m2','left-2','{\"kind\":\"written\"}','Two.'),
            ('m3','left-3','{\"kind\":\"written\"}','Three.'),
            ('m4','left-4','{\"kind\":\"written\"}','Four.'),
            ('m5','left-5','{\"kind\":\"written\"}','Five.');",
    )
    .unwrap();
    assert_eq!(hold_left(&db).unwrap(), Some(2));
    assert_eq!(
        count(&db, "SELECT count(*) FROM projects_removed"),
        REMOVED_MAX,
        "the backfill fills the record up to its bound and no further"
    );
    assert_eq!(hold_left(&db).unwrap(), None, "a bound backfill still runs once");
}

#[test]
fn the_suffix_keeps_the_id_within_64_bytes() {
    let (_directory, db) = database();
    let folder = PathBuf::from(format!("/h/{}", "a".repeat(70)));
    let base = crate::onboarding::project_id_for(&folder).unwrap();
    assert_eq!(base.as_str().len(), 64);
    record(&db, &base, None, false).unwrap();
    assert_eq!(
        free_id(&db, &folder).unwrap(),
        Ok(id(&format!("{}-2", "a".repeat(62))))
    );
    for n in 2..=SUFFIX_MAX {
        let next = free_id(&db, &folder).unwrap().unwrap();
        assert!(next.as_str().len() <= 64, "{next:?}");
        assert!(next.as_str().ends_with(&format!("-{n}")), "{next:?}");
        record(&db, &next, None, false).unwrap();
    }
    assert_eq!(
        free_id(&db, &folder).unwrap(),
        Err("onboarding_project_ids_used")
    );
    let dashed = PathBuf::from(format!("/h/{}-{}", "b".repeat(61), "c".repeat(9)));
    let dashed_base = crate::onboarding::project_id_for(&dashed).unwrap();
    record(&db, &dashed_base, None, false).unwrap();
    assert_eq!(
        free_id(&db, &dashed).unwrap(),
        Ok(id(&format!("{}-2", "b".repeat(61)))),
        "a cut that ends in a dash loses the dash"
    );
}

#[test]
fn clear_takes_every_row_of_the_id() {
    let (_directory, db) = database();
    let demo = id(crate::demo::PROJECT_ID);
    record(&db, &demo, Some("/h/crazyagent-demo"), true).unwrap();
    db.execute_batch(&format!(
        "{STANDING_ALLOWS}
         INSERT INTO connector_write_allowed VALUES ('crazyagent-demo','linear'),('kept-app','linear');
         INSERT INTO memory_library(id,project,origin,text) VALUES
            ('m1','crazyagent-demo','{{\"kind\":\"written\"}}','An old demo note.'),
            ('m2','kept-app','{{\"kind\":\"written\"}}','Another project note.');"
    ))
    .unwrap();
    assert!(held(&db, &demo).unwrap());
    clear(&db, &demo).unwrap();
    assert!(!held(&db, &demo).unwrap());
    assert_eq!(returning(&db, "/h/crazyagent-demo").unwrap(), None);
    for table in ["connector_write_allowed", "memory_library"] {
        assert_eq!(
            count(
                &db,
                &format!("SELECT count(*) FROM {table} WHERE project='crazyagent-demo'")
            ),
            0,
            "{table} keeps no row of the cleared id"
        );
        assert_eq!(
            count(
                &db,
                &format!("SELECT count(*) FROM {table} WHERE project='kept-app'")
            ),
            1,
            "{table} keeps another project's row"
        );
    }
}

#[test]
fn a_new_project_at_a_recorded_folder_ends_that_return() {
    let (_directory, db) = database();
    record(&db, &id("my-app"), Some("/h/my-app"), true).unwrap();
    assert_eq!(returning(&db, "/h/my-app").unwrap(), Some(id("my-app")));
    claim(&db, &id("project-1"), "/h/my-app").unwrap();
    assert_eq!(returning(&db, "/h/my-app").unwrap(), None);
    assert!(held(&db, &id("my-app")).unwrap(), "its id stays held");

    record(&db, &id("our-app"), Some("/h/our-app"), true).unwrap();
    claim(&db, &id("our-app"), "/h/our-app").unwrap();
    assert!(!held(&db, &id("our-app")).unwrap(), "a project back is held no more");

    record(&db, &id("first-app"), Some("/h/same"), true).unwrap();
    record(&db, &id("second-app"), Some("/h/same"), true).unwrap();
    assert_eq!(
        returning(&db, "/h/same").unwrap(),
        Some(id("second-app")),
        "the newer removal of a folder is the one it brings back"
    );
    assert!(held(&db, &id("first-app")).unwrap());
}

#[test]
fn a_full_record_refuses_room_and_a_record_past_it_still_lands() {
    let (_directory, db) = database();
    assert_eq!(room(&db).unwrap(), Ok(()));
    db.execute(
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{REMOVED_MAX})
             INSERT INTO projects_removed(project,folder,returns,removed_ms)
             SELECT 'gone-'||i, NULL, 0, 0 FROM n"
        ),
        [],
    )
    .unwrap();
    assert_eq!(room(&db).unwrap(), Err("projects_removed_full"));
    record(&db, &id("past-the-bound"), Some("/h/past"), true).unwrap();
    assert!(
        held(&db, &id("past-the-bound")).unwrap(),
        "a Delete everything whose folders reached the Trash is always recorded"
    );
}

#[test]
fn create_project_refuses_an_id_a_removal_holds() {
    let (_directory, mut db) = database();
    record(&db, &id("my-app"), Some("/h/my-app"), true).unwrap();
    record(&db, &id(crate::demo::PROJECT_ID), None, false).unwrap();
    let spec = ProjectSpec {
        name: "My app".into(),
        repositories: vec![],
    };
    let tx = db.transaction().unwrap();
    for held_id in ["my-app", "My-App", crate::demo::PROJECT_ID] {
        assert_eq!(
            crate::storage::create_project(&tx, &id(held_id), &spec).unwrap(),
            Outcome::Rejected {
                code: "project_id_removed".into()
            },
            "{held_id} is held"
        );
    }
    assert_eq!(
        crate::storage::create_project(&tx, &id("other-app"), &spec).unwrap(),
        Outcome::Applied
    );
    assert_eq!(
        count(&tx, "SELECT count(*) FROM projects"),
        1,
        "only the free id is recorded"
    );
}
