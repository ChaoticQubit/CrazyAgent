use super::*;

/// Copies the committed 2.4.4 fixture, the engine.db a 2.4.4 engine (207d2f201) leaves after
/// one start and stop in an empty root, into a fresh engine root. Output: the root and the
/// fixture's bytes; panics when the fixture is missing or is not a 2.4 database.
fn fixture_v244() -> (Root, Vec<u8>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v244-engine.db");
    let bytes = std::fs::read(&path).unwrap_or_else(|cause| {
        panic!("the 2.4.4 fixture {} is missing ({cause})", path.display())
    });
    let root = Root::new();
    std::fs::write(root.database(), &bytes).unwrap();
    std::fs::set_permissions(root.database(), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(version(&root.connection()).unwrap(), V24_VERSION_MAX);
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
    (root, bytes)
}

/// The production steps with step 12, the first 2.5 step, replaced by one that fails, so the
/// upgrade stops after its backup and before any 2.5 change lands. Output: the steps.
fn failing_at_12() -> Vec<Step> {
    let steps: Vec<Step> = STEPS
        .iter()
        .map(|step| Step {
            to: step.to,
            run: if step.to == 12 { broken } else { step.run },
        })
        .collect();
    assert_eq!(steps.iter().filter(|step| step.to == 12).count(), 1);
    assert_eq!(steps.len(), STEPS.len());
    steps
}

/// V250-CATCHUP-244 fixture row: an upgrade to the tip of the 2.4.4 fixture with a worktree
/// row of each table runs every 2.5 step once. Only step 13 has rows to log, so one verified
/// snapshot run moves `state` one revision with one `events` row; every other 2.4.4 table
/// keeps its rows, the checked backup is pinned, and a replay equals the live tables.
#[test]
fn an_upgrade_of_the_2_4_4_fixture_replays_to_the_rows_it_held() {
    let (root, _) = fixture_v244();
    let sources = sources(&root);
    let mut db = root.connection();
    worktree_rows(&db);
    let before = rows(&db);
    let standing = standing_of(&db);
    let now = SystemTime::now();

    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        STEPS,
        now,
        check_copy,
    )
    .unwrap();

    assert_eq!(version(&db).unwrap(), latest(STEPS));
    let after = rows(&db);
    for (table, held) in before
        .iter()
        .filter(|(table, _)| !["state", "events"].contains(&table.as_str()))
    {
        assert_eq!(after.get(table), Some(held), "the 2.4.4 rows of {table}");
    }
    let moved = standing_of(&db);
    assert_eq!((moved.0, moved.1), (standing.0 + 1, standing.1 + 1));
    assert_eq!((&moved.2, &moved.3), (&standing.2, &standing.3));
    let names = names(&root);
    assert_eq!(names.len(), 1);
    assert_eq!(pinned_name(&root, now), Some(names[0].clone()));
    assert!(root.backups()[0].join(CHECKED).is_file());
    let done: Vec<(i64, String)> = db
        .prepare("SELECT seq, aggregate FROM state_log WHERE kind=?1 ORDER BY seq")
        .unwrap()
        .query_map([crate::state_log::SNAPSHOT_DONE], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let aggregates: Vec<&str> = done.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(aggregates, ["worktrees"], "one run, for the only rows held");
    for (seq, _) in &done {
        crate::state_log::verify_snapshot(&db, *seq as u64).unwrap();
    }
    let tx = db.transaction().unwrap();
    crate::worktrees::log::rebuild(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(standing_of(&db), moved, "replay equals the live tables");
}

/// V250-CATCHUP-244 fixture row: a failure injected in step 12 rolls the whole upgrade back,
/// so the 2.4.4 engine.db stays byte-identical at its 2.4 version, with its checked backup
/// kept beside it.
#[test]
fn a_failure_in_step_12_leaves_the_2_4_4_fixture_byte_identical() {
    let (root, bytes) = fixture_v244();
    let sources = sources(&root);
    let mut db = root.connection();

    let failed = upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &failing_at_12(),
        SystemTime::now(),
        check_copy,
    )
    .unwrap_err()
    .0;

    assert!(failed.contains("step 12 failed"), "{failed}");
    assert!(failed.contains("the database is unchanged"), "{failed}");
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    drop(db);
    assert_eq!(std::fs::read(root.database()).unwrap(), bytes);
    assert_eq!(names(&root).len(), 1);
    assert!(root.backups()[0].join(CHECKED).is_file());
}
