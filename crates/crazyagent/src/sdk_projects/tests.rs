use super::{Kept, ListEntry, PAGE_JSON_MAX, PROJECTS_PLUGIN, RowsProvider, page_with, rows};
use crate::engine::EngineError;
use crate::engine::owner::OwnerSummary;
use crate::protocol::{ProjectId, ProjectSpec, RequestId, StoredProject};
use crate::sdk_slice::{EngineRefusals, bundled_grants};
use bus_gateway::{AllOn, Switches};
use crazyagent_plugin_projects as projects;
use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
use rusqlite::Connection;
use sdk::Refusal;
use sdk::generated::project_rows::{self, ProjectRow, Provide, RowsPage};

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

/// Only the bundled projects plugin holds the projects read.
#[test]
fn only_the_projects_plugin_holds_project_rows_read() {
    let holders: Vec<&str> = BUNDLED_GRANTS
        .iter()
        .filter(|grant| grant.permissions.contains(&"project_rows.read"))
        .map(|grant| grant.plugin)
        .collect();
    assert_eq!(holders, ["projects"]);
}

/// Switches with only the projects plugin turned off.
struct ProjectsOff;

impl Switches for ProjectsOff {
    fn is_on(&self, plugin: &str) -> bool {
        plugin != PROJECTS_PLUGIN
    }
}

/// A list entry that calls the projects read twice for one page. Inputs: as the plugin's
/// list. Output: the second page.
fn twice(host: &dyn sdk::Host, after: Option<String>, limit: u16) -> Result<RowsPage, Refusal> {
    project_rows::page(host, after.clone(), limit)?;
    project_rows::page(host, after, limit)
}

/// A list entry that reads past `z` in place of the page it was asked for. Inputs: as the
/// plugin's list. Output: the page it read.
fn substituted_after(
    host: &dyn sdk::Host,
    _: Option<String>,
    limit: u16,
) -> Result<RowsPage, Refusal> {
    project_rows::page(host, Some("z".to_owned()), limit)
}

/// A list entry that reads one row in place of the limit it was asked for. Inputs: as the
/// plugin's list. Output: the page it read.
fn substituted_limit(
    host: &dyn sdk::Host,
    after: Option<String>,
    _: u16,
) -> Result<RowsPage, Refusal> {
    project_rows::page(host, after, 1)
}

/// Reads one page through a plugin entry with the bundled grants in a fresh transaction.
/// Inputs: the store, the entry, the switches, the sink, the id to read past and the limit.
/// Output: the page or its error.
fn through(
    db: &mut Connection,
    entry: ListEntry,
    switches: &dyn Switches,
    refusals: &EngineRefusals,
    after: &str,
    limit: u16,
) -> Result<Kept, EngineError> {
    let tx = db.transaction().unwrap();
    page_with(
        entry,
        bundled_grants(),
        switches,
        refusals,
        &tx,
        after,
        limit,
    )
}

/// The error code of a read that must fail. Input: the read. Output: the code.
fn code(read: Result<Kept, EngineError>) -> String {
    match read {
        Err(EngineError(code)) => code,
        Ok(kept) => panic!("read as {kept:?}"),
    }
}

/// The page the bundled projects plugin returns through the gateway holds the same engine
/// items and SDK rows as the direct read, and an entry that calls the read twice for one
/// page is invalid.
#[test]
fn page_through_the_plugin_equals_the_direct_rows() {
    let mut db = engine_store();
    put(&db, "alpha", &body("alpha", "Alpha", 0));
    put(&db, "beta", &body("beta", "Beta", 1));
    put(&db, "gamma", &body("gamma", "Gamma", 64));
    for after in ["", "beta"] {
        let direct = read(&mut db, after, 2).unwrap();
        let served = through(
            &mut db,
            projects::list,
            &AllOn,
            &EngineRefusals::new(),
            after,
            2,
        )
        .unwrap();
        assert_eq!(served.items, direct.items, "after {after:?}");
        assert_eq!(served.page, direct.page, "after {after:?}");
    }
    assert_eq!(
        code(through(
            &mut db,
            twice,
            &AllOn,
            &EngineRefusals::new(),
            "",
            2
        )),
        "plugin_call_invalid"
    );
}

/// A page size outside 1 to 32 never reaches the store through the plugin: its own check
/// refuses, so no call is admitted and the page is invalid. The direct read answers the
/// owner page code for the same sizes.
#[test]
fn page_refuses_a_bad_limit_with_the_2_4_4_code() {
    let mut db = engine_store();
    put(&db, "alpha", &body("alpha", "Alpha", 0));
    for limit in [0, 33] {
        assert_eq!(code(read(&mut db, "", limit)), "owner_page_limit");
        assert_eq!(
            code(through(
                &mut db,
                projects::list,
                &AllOn,
                &EngineRefusals::new(),
                "",
                limit
            )),
            "plugin_call_invalid",
            "limit {limit}"
        );
    }
}

/// With the projects plugin turned off the page is refused, the refusal is counted, and
/// nothing is queued for the log.
#[test]
fn a_turned_off_projects_plugin_refuses_the_page_and_queues_nothing() {
    let mut db = engine_store();
    put(&db, "alpha", &body("alpha", "Alpha", 0));
    let refusals = EngineRefusals::new();
    assert_eq!(
        code(through(
            &mut db,
            projects::list,
            &ProjectsOff,
            &refusals,
            "",
            16
        )),
        "plugin_call_refused"
    );
    assert_eq!(refusals.count(PROJECTS_PLUGIN), 1);
    assert_eq!(refusals.pending_len(), 0);
}

/// The provider answers only the projects plugin: another caller is refused before any
/// read and is not counted, and the projects plugin gets the page.
#[test]
fn rows_provider_refuses_any_caller_but_projects() {
    let mut db = engine_store();
    put(&db, "alpha", &body("alpha", "Alpha", 0));
    let tx = db.transaction().unwrap();
    let provider = RowsProvider::new(&tx, "", 16);
    assert_eq!(
        provider.page("agent-quit", None, 16),
        Err(Refusal::Rejected {
            code: "project_rows_caller".to_owned()
        })
    );
    assert_eq!(provider.calls.get(), 0);
    let served = provider.page(PROJECTS_PLUGIN, None, 16).unwrap();
    assert_eq!(served.rows, vec![row("alpha", "Alpha", 0)]);
    assert!(!served.more);
    assert_eq!(provider.calls.get(), 1);
}

/// A plugin that reads another cursor or limit than the engine asked for gets no page:
/// the read is invalid, never the substituted page.
#[test]
fn a_plugin_that_reads_another_page_is_invalid() {
    let mut db = engine_store();
    put(&db, "alpha", &body("alpha", "Alpha", 0));
    put(&db, "beta", &body("beta", "Beta", 1));
    for entry in [substituted_after as ListEntry, substituted_limit] {
        assert_eq!(
            code(through(
                &mut db,
                entry,
                &AllOn,
                &EngineRefusals::new(),
                "",
                32
            )),
            "plugin_call_invalid"
        );
    }
    let tx = db.transaction().unwrap();
    let provider = RowsProvider::new(&tx, "", 32);
    assert_eq!(
        provider.page(PROJECTS_PLUGIN, Some("z".to_owned()), 32),
        Err(Refusal::Rejected {
            code: "project_rows_request".to_owned()
        })
    );
    assert_eq!(
        provider.page(PROJECTS_PLUGIN, None, 1),
        Err(Refusal::Rejected {
            code: "project_rows_request".to_owned()
        })
    );
    assert_eq!(provider.calls.get(), 0);
}

/// The page a fresh, uncached statement gives for the same arguments: the rows past
/// `after`, at most `limit`, and whether one more exists. Inputs: the transaction, the id
/// to read past and the limit. Output: the expected engine items and SDK page.
fn fresh(tx: &rusqlite::Transaction<'_>, after: &str, limit: u16) -> (Vec<OwnerSummary>, RowsPage) {
    let mut stmt = tx.prepare(super::SQL).unwrap();
    let found: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![after, i64::from(limit) + 1], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let more = found.len() > usize::from(limit);
    let kept = &found[..found.len().min(usize::from(limit))];
    let mut items = Vec::new();
    let mut rows = Vec::new();
    for (id, body) in kept {
        let p: StoredProject = serde_json::from_str(body).unwrap();
        let count = p.spec.repositories.len();
        items.push(item(id, &p.spec.name, count));
        rows.push(row(id, &p.spec.name, u16::try_from(count).unwrap()));
    }
    (items, RowsPage { rows, more })
}

/// On one transaction, page reads with changing arguments each give exactly the page a
/// fresh statement gives: no stale binding and no unreset statement carries over between
/// calls.
#[test]
fn the_cached_page_read_rebinds_each_call() {
    let mut db = engine_store();
    for (n, id) in ["alpha", "beta", "gamma"].iter().enumerate() {
        put(&db, id, &body(id, &format!("Project {id}"), n + 1));
    }
    let tx = db.transaction().unwrap();
    let first = rows(&tx, "", 1).unwrap();
    assert_eq!(first.page.rows.len(), 1);
    let after_first = first.page.rows[0].id.clone();
    for (after, limit) in [("", 1), (after_first.as_str(), 2), ("", 32), ("", 1)] {
        let got = rows(&tx, after, limit).unwrap();
        let (items, page) = fresh(&tx, after, limit);
        assert!(!page.rows.is_empty());
        assert_eq!(got.items, items, "items after {after:?} limit {limit}");
        assert_eq!(got.page, page, "page after {after:?} limit {limit}");
    }
}
