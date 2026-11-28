use super::{Kept, PAGE_JSON_MAX, rows};
use crate::engine::EngineError;
use crate::engine::owner::OwnerSummary;
use crate::protocol::{ProjectId, ProjectSpec, RequestId, StoredProject};
use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
use rusqlite::Connection;
use sdk::generated::project_rows::ProjectRow;

/// An in-memory store with every table the engine opens and the test host's owner tables.
/// Output: the connection.
fn engine_store() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::state_log::fence::seed(&db);
    db
}

/// The stored body of one project. Input: its id, name and repository count. Output: the
/// JSON the engine writes.
fn body(id: &str, name: &str, repositories: usize) -> String {
    serde_json::to_string(&StoredProject {
        project_id: ProjectId::new(id).unwrap(),
        spec: ProjectSpec {
            name: name.to_owned(),
            repositories: (0..repositories).map(|n| format!("/r/{n}")).collect(),
        },
    })
    .unwrap()
}

/// Stores one project row as given. Input: the store, the row id and its body.
fn put(db: &Connection, id: &str, body: &str) {
    db.execute("INSERT INTO projects(id, body) VALUES (?1, ?2)", [id, body])
        .unwrap();
}

/// Reads one page in a fresh transaction. Input: the store, the id to read past and the
/// limit. Output: the page or its error.
fn read(db: &mut Connection, after: &str, limit: u16) -> Result<Kept, EngineError> {
    let tx = db.transaction().unwrap();
    rows(&tx, after, limit)
}

/// The SDK row of one project. Input: its id, name and count. Output: the row.
fn row(id: &str, name: &str, repository_count: u16) -> ProjectRow {
    ProjectRow {
        id: id.to_owned(),
        name: name.to_owned(),
        repository_count,
    }
}

/// The engine item of one project. Input: its id, name and count. Output: the item.
fn item(id: &str, name: &str, repository_count: usize) -> OwnerSummary {
    OwnerSummary::Project {
        project_id: ProjectId::new(id).unwrap(),
        name: name.to_owned(),
        repository_count,
    }
}

/// Two pages over three projects hold the same items, counts, order and `more` the owner
/// page read gave, and the SDK rows carry the same values.
#[test]
fn rows_page_is_the_2_4_4_projects_page() {
    let mut db = engine_store();
    put(&db, "alpha", &body("alpha", "Alpha", 0));
    put(&db, "beta", &body("beta", "Beta", 1));
    put(&db, "gamma", &body("gamma", "Gamma", 64));

    let first = read(&mut db, "", 2).unwrap();
    assert_eq!(
        first.items,
        vec![item("alpha", "Alpha", 0), item("beta", "Beta", 1)]
    );
    assert_eq!(
        first.page.rows,
        vec![row("alpha", "Alpha", 0), row("beta", "Beta", 1)]
    );
    assert!(first.page.more);

    let last = read(&mut db, "beta", 2).unwrap();
    assert_eq!(last.items, vec![item("gamma", "Gamma", 64)]);
    assert_eq!(last.page.rows, vec![row("gamma", "Gamma", 64)]);
    assert!(!last.page.more);
}

/// Each bad row fails the page with the code or text the owner page read gave, in its
/// order: the SQL bounds, the id check, the parse, then the invalid checks.
#[test]
fn rows_page_refuses_a_bad_row_like_2_4_4() {
    let not_json = "not json";
    let bad_identity = r#"{"project_id":"a/b","spec":{"name":"n","repositories":[]}}"#;
    let cases: Vec<(String, String, String)> = vec![
        (
            "a".repeat(129),
            body("x", "n", 0),
            "owner_row_bounds".to_owned(),
        ),
        (
            "big".to_owned(),
            body("big", &"n".repeat(70_000), 0),
            "owner_row_bounds".to_owned(),
        ),
        (
            "one".to_owned(),
            body("two", "n", 0),
            "owner_row_invalid".to_owned(),
        ),
        (
            "long".to_owned(),
            body("long", &"n".repeat(257), 0),
            "owner_row_invalid".to_owned(),
        ),
        (
            "many".to_owned(),
            body("many", "n", 65),
            "owner_row_invalid".to_owned(),
        ),
        (
            "junk".to_owned(),
            not_json.to_owned(),
            serde_json::from_str::<StoredProject>(not_json)
                .unwrap_err()
                .to_string(),
        ),
        (
            "a/b".to_owned(),
            body("x", "n", 0),
            RequestId::new("a/b").unwrap_err().to_string(),
        ),
        (
            "bad".to_owned(),
            bad_identity.to_owned(),
            serde_json::from_str::<StoredProject>(bad_identity)
                .unwrap_err()
                .to_string(),
        ),
    ];
    for (id, stored, expected) in cases {
        let mut db = engine_store();
        put(&db, &id, &stored);
        match read(&mut db, "", 32) {
            Err(EngineError(code)) => assert_eq!(code, expected, "row {id}"),
            Ok(kept) => panic!("row {id} read as {kept:?}"),
        }
    }
}

/// The row past the limit is only probed: an oversized second row neither fails the page
/// nor is parsed, and the page says more rows follow.
#[test]
fn rows_page_parses_only_rows_before_the_cut() {
    let mut db = engine_store();
    put(&db, "a", &body("a", "A", 0));
    put(&db, "b", &body("b", &"n".repeat(70_000), 0));

    let kept = read(&mut db, "", 1).unwrap();
    assert_eq!(kept.page.rows, vec![row("a", "A", 0)]);
    assert!(kept.page.more);
}

/// A full page of the largest items serializes to exactly `PAGE_JSON_MAX` bytes, so the
/// const bound under the frame cut holds for every page this read returns.
#[test]
fn worst_projects_page_fits_the_frame() {
    let worst: Vec<OwnerSummary> = (0..32)
        .map(|_| OwnerSummary::Project {
            project_id: ProjectId::new("a".repeat(128)).unwrap(),
            name: "\u{1}".repeat(256),
            repository_count: 64,
        })
        .collect();
    assert_eq!(serde_json::to_vec(&worst).unwrap().len(), PAGE_JSON_MAX);
}

/// No bundled plugin holds the projects read until the projects plugin ships.
#[test]
fn only_the_projects_plugin_holds_project_rows_read() {
    let holders: Vec<&str> = BUNDLED_GRANTS
        .iter()
        .filter(|grant| grant.permissions.contains(&"project_rows.read"))
        .map(|grant| grant.plugin)
        .collect();
    assert_eq!(holders, Vec::<&str>::new());
}
