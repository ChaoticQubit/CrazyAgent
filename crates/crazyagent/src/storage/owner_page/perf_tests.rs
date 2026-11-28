use std::hint::black_box;
use std::time::Instant;

use crate::config::EffectiveConfig;
use crate::engine::owner::{Admission, OwnerData, OwnerPageCursor, OwnerView};
use crate::perf_kit::{Row, SAMPLES, line_ns};
use crate::protocol::{ProjectId, ProjectSpec, StoredProject};
use rusqlite::{Connection, Transaction};

/// The page size the desktop list reads.
const PAGE: u8 = 16;

/// The revision every page and cursor is read at; nothing writes during a row.
const REVISION: u64 = 0;

/// The most projects the desktop list keeps.
const LISTED: usize = 200;

/// The pages the desktop list reads to hold 200 projects 16 at a time.
const LIST_PAGES: usize = 13;

/// One owner page row, at the app layer: the read crosses the engine's storage and, where
/// a build routes it, the plugin and gateway. Input: its name. Output: the row.
fn row(name: &'static str) -> Row {
    Row {
        layer: 3,
        module: "app",
        name,
        batch: 1,
    }
}

/// An in-memory store with the engine's schema and projects `p000` up to the count, each
/// with a name and no repositories. Input: the count. Output: the connection.
fn store(count: usize) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    for n in 0..count {
        let id = format!("p{n:03}");
        let body = serde_json::to_string(&StoredProject {
            project_id: ProjectId::new(id.clone()).unwrap(),
            spec: ProjectSpec {
                name: format!("Project {n}"),
                repositories: vec![],
            },
        })
        .unwrap();
        db.execute(
            "INSERT INTO projects(id, body) VALUES (?1, ?2)",
            [&id, &body],
        )
        .unwrap();
    }
    db
}

/// The admission every row reads under. Output: it.
fn admission() -> Admission {
    Admission {
        db_instance: "perf-db".to_owned(),
        engine_epoch: "perf-epoch".to_owned(),
        owner_id: "perf-owner".to_owned(),
        owner_epoch: 1,
    }
}

/// Reads one 16-row projects page through the engine's owner page read. Inputs: the read
/// transaction, the config, the admission and the cursor (none for the first page).
/// Output: the rows read and the next cursor.
fn page(
    tx: &Transaction<'_>,
    config: &EffectiveConfig,
    a: &Admission,
    cursor: Option<&OwnerPageCursor>,
) -> (usize, Option<OwnerPageCursor>) {
    let read =
        crate::storage::read_owner_page(tx, config, a, OwnerView::Projects, cursor, PAGE, REVISION);
    match read.unwrap() {
        OwnerData::Page { items, next } => (items.len(), next),
        _ => panic!("a projects page"),
    }
}

/// Walks the list as the desktop does: 16-row pages by cursor until 200 are held or no
/// page follows. Inputs: the transaction, the config and the admission. Output: the rows
/// held and the pages read.
fn walk(tx: &Transaction<'_>, config: &EffectiveConfig, a: &Admission) -> (usize, usize) {
    let mut cursor = None;
    let mut held = 0;
    for reads in 1..=LIST_PAGES {
        let (rows, next) = page(tx, config, a, cursor.as_ref());
        held += rows;
        match next {
            Some(next) if held < LISTED => cursor = Some(next),
            _ => return (held, reads),
        }
    }
    panic!("the list read past {LIST_PAGES} pages");
}

/// Times one call per sample after a tenth of the samples as warm-up and prints its PERF
/// line in nanoseconds. Inputs: the row name and the call.
fn timed(name: &'static str, mut call: impl FnMut()) {
    let mut sample = || {
        let started = Instant::now();
        call();
        u64::try_from(started.elapsed().as_nanos()).expect("sample fits u64")
    };
    for _ in 0..SAMPLES / 10 {
        sample();
    }
    let mut samples: Vec<u64> = (0..SAMPLES).map(|_| sample()).collect();
    samples.sort_unstable();
    println!("{}", line_ns(&row(name), &samples));
}

/// Times the projects owner page read on a store with the engine's schema: one
/// 16-row page from the start with 1, 16 and 200 projects, and the desktop list's
/// 13-page walk over 200. Each answer is checked once before its row is timed. Prints one
/// PERF line per row and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn owner_projects_page() {
    let root = tempfile::tempdir().unwrap();
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let a = admission();
    for (name, count, rows) in [
        ("owner.page.projects.n1", 1, 1),
        ("owner.page.projects.n16", 16, 16),
        ("owner.page.projects.n200", LISTED, 16),
    ] {
        let mut db = store(count);
        let tx = db.transaction().unwrap();
        assert_eq!(page(&tx, &config, &a, None).0, rows);
        timed(name, || {
            black_box(page(&tx, &config, &a, None));
        });
    }
    let mut db = store(LISTED);
    let tx = db.transaction().unwrap();
    assert_eq!(walk(&tx, &config, &a), (LISTED, LIST_PAGES));
    timed("owner.list.projects.n200", || {
        black_box(walk(&tx, &config, &a));
    });
}
