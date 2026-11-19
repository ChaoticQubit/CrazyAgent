use super::*;
use crate::projects_home::icloud::SKIPPED_LISTED_MAX;
use crate::projects_home::reinstall::{Left, OFFERS_MAX};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// A record naming these folders, with no command and no counted rest.
fn record(paths: &[&str]) -> Kept {
    Kept {
        left: paths
            .iter()
            .map(|path| Left {
                path: (*path).into(),
                hint: None,
            })
            .collect(),
        more: 0,
    }
}

/// The landed folder of job `n` in a projects home.
fn folder(n: usize) -> PathBuf {
    PathBuf::from(format!("/Users/o/CrazyAgent/p{n}"))
}

/// Slice 3: before any job kept one there is no offer; a done job's record is kept at its
/// folder, a later one there replaces it, and one that left nothing deletes it.
#[test]
fn an_offer_is_kept_replaced_and_deleted_at_its_folder() {
    let mut db = Connection::open_in_memory().unwrap();
    assert_eq!(at(&db, &folder(1)).unwrap(), None);
    keep(&mut db, &folder(1), None, &record(&["node_modules"])).unwrap();
    assert_eq!(
        at(&db, &folder(1)).unwrap(),
        Some(record(&["node_modules"]))
    );
    let both = record(&["web/node_modules", "api/.venv"]);
    keep(&mut db, &folder(1), None, &both).unwrap();
    assert_eq!(at(&db, &folder(1)).unwrap(), Some(both));
    keep(&mut db, &folder(1), None, &Kept::default()).unwrap();
    assert_eq!(at(&db, &folder(1)).unwrap(), None);
}

/// Slice 3: a project move drops the offer at the project's old folder, whether or not it
/// keeps one at its new folder.
#[test]
fn a_project_move_drops_the_offer_at_its_old_folder() {
    let mut db = Connection::open_in_memory().unwrap();
    keep(&mut db, &folder(1), None, &record(&["node_modules"])).unwrap();
    keep(&mut db, &folder(2), Some(&folder(1)), &record(&["dist"])).unwrap();
    assert_eq!(at(&db, &folder(1)).unwrap(), None);
    assert_eq!(at(&db, &folder(2)).unwrap(), Some(record(&["dist"])));
    keep(&mut db, &folder(3), Some(&folder(2)), &Kept::default()).unwrap();
    assert_eq!(at(&db, &folder(2)).unwrap(), None);
    assert_eq!(at(&db, &folder(3)).unwrap(), None);
}

/// Slice 3: past `OFFERS_MAX` offers the oldest goes first, and a replaced offer counts as
/// the newest.
#[test]
fn past_the_bound_the_oldest_offer_goes_first() {
    let mut db = Connection::open_in_memory().unwrap();
    for n in 0..=OFFERS_MAX {
        keep(&mut db, &folder(n), None, &record(&["node_modules"])).unwrap();
    }
    assert_eq!(at(&db, &folder(0)).unwrap(), None);
    assert_eq!(
        at(&db, &folder(1)).unwrap(),
        Some(record(&["node_modules"]))
    );
    keep(&mut db, &folder(1), None, &record(&["dist"])).unwrap();
    keep(&mut db, &folder(OFFERS_MAX + 1), None, &record(&["build"])).unwrap();
    assert_eq!(at(&db, &folder(1)).unwrap(), Some(record(&["dist"])));
    assert_eq!(at(&db, &folder(2)).unwrap(), None);
    let kept: i64 = db
        .query_row("SELECT COUNT(*) FROM projects_home_reinstall", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(kept, OFFERS_MAX as i64);
}

/// Slice 3: removing a project forgets the offer at its folder inside the removal's
/// transaction; a folder whose name is not UTF-8 holds no offer and is passed over.
#[test]
fn a_removed_projects_folder_forgets_its_offer() {
    let mut db = Connection::open_in_memory().unwrap();
    let tx = db.transaction().unwrap();
    forget(&tx, &folder(1)).unwrap();
    tx.commit().unwrap();
    keep(&mut db, &folder(1), None, &record(&["node_modules"])).unwrap();
    keep(&mut db, &folder(2), None, &record(&["node_modules"])).unwrap();
    let tx = db.transaction().unwrap();
    forget(&tx, &folder(1)).unwrap();
    forget(&tx, Path::new(OsStr::from_bytes(b"/Users/o/\xff"))).unwrap();
    tx.commit().unwrap();
    assert_eq!(at(&db, &folder(1)).unwrap(), None);
    assert_eq!(
        at(&db, &folder(2)).unwrap(),
        Some(record(&["node_modules"]))
    );
}

/// Slice 3: a stored record past its bounds, or not a record at all, reads as
/// `reinstall_offer_unreadable` instead of reaching a client.
#[test]
fn a_stored_record_out_of_its_bounds_reads_as_unreadable() {
    let mut db = Connection::open_in_memory().unwrap();
    keep(&mut db, &folder(1), None, &record(&["node_modules"])).unwrap();
    assert_eq!(
        at(&db, &folder(1)).unwrap(),
        Some(record(&["node_modules"]))
    );
    let wide = record(&["node_modules"; SKIPPED_LISTED_MAX + 1]);
    for body in [serde_json::to_string(&wide).unwrap(), "[]".to_owned()] {
        db.execute("UPDATE projects_home_reinstall SET kept=?1", [&body])
            .unwrap();
        let read = at(&db, &folder(1));
        assert!(
            matches!(&read, Err(e) if e.0 == "reinstall_offer_unreadable"),
            "{read:?}"
        );
    }
}
