use super::*;

/// The P5 fixture upgraded by the probe step at a time, then given 2.5 data the way an owner
/// would after the upgrade: a second probe row. Every connection is closed on return, as the
/// restore needs. Input: the upgrade time. Output: the root and the 2.4 fixture's rows.
fn upgraded(at: SystemTime) -> (Root, BTreeMap<String, Vec<String>>) {
    let (root, _) = fixture();
    let before = rows(&root.connection());
    let sources = sources(&root);
    let mut db = root.connection();
    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        at,
        check_copy,
    )
    .unwrap();
    let written = db
        .execute(
            "INSERT INTO migration_probe VALUES (?1)",
            [latest(STEPS) + 2],
        )
        .unwrap();
    assert_eq!(written, 1);
    assert_eq!(names(&root).len(), 1);
    (root, before)
}

/// The rows each table holds. Input: every row by table. Output: the count by table.
fn counts(rows: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, usize> {
    rows.iter()
        .map(|(table, values)| (table.clone(), values.len()))
        .collect()
}

/// The copies restores kept under backups/, oldest first. Input: the root. Output: the folders.
fn replaced(root: &Root) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.path().join(BACKUPS)) else {
        return Vec::new();
    };
    let prefix = format!("{REPLACED}-");
    let mut found: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.starts_with(&prefix)
        })
        .collect();
    found.sort();
    found
}

/// Restores the root's pinned backup at a time, as the engine does once it has stopped.
/// Inputs: the root and the time. Output: the restore's result.
fn restored(root: &Root, at: SystemTime) -> Result<PathBuf, EngineError> {
    let sources = sources(root);
    restore_pinned_at(&root.database(), Some(&sources), at)
}

#[test]
fn a_restore_after_thirty_days_is_refused_and_changes_nothing() {
    let start = SystemTime::now();
    let (root, _) = upgraded(start);
    let after = rows(&root.connection());
    let taken = stamp_time(backup_time(&names(&root)[0]).unwrap()).unwrap();
    let late = taken + DAY * PINNED_DAYS;
    assert_eq!(restorable_until(root.path(), start), Some(late));
    assert_eq!(restorable_until(root.path(), late), None);

    let refused = restored(&root, late).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_UNAVAILABLE), "{refused}");
    assert_eq!(rows(&root.connection()), after);
    assert!(replaced(&root).is_empty());
    assert_eq!(names(&root).len(), 1);
}

#[test]
fn a_restore_is_refused_while_an_agent_runs() {
    let start = SystemTime::now();
    let (root, _) = upgraded(start);
    let db = root.connection();
    db.execute(
        "INSERT INTO team_agents (project, role, harness, tools, status, created_ms, \
         heartbeat_ms) VALUES ('restore-project', 'sub', 'claude', 0, 'running', 1, 1)",
        [],
    )
    .unwrap();
    drop(db);
    let after = rows(&root.connection());

    let refused = restored(&root, start + HOUR).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_AGENTS_RUNNING), "{refused}");
    assert_eq!(rows(&root.connection()), after);
    assert!(replaced(&root).is_empty());
    let db = root.connection();
    db.execute(
        "UPDATE team_agents SET status = 'stopped' WHERE project = 'restore-project'",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO research_jobs (project, kind, harness, tools, status, created_ms) \
         VALUES ('restore-project', 'research', 'claude', 0, 'running', 1)",
        [],
    )
    .unwrap();
    drop(db);
    let refused = restored(&root, start + HOUR).unwrap_err().0;
    assert!(refused.starts_with(RESTORE_AGENTS_RUNNING), "{refused}");
    assert!(replaced(&root).is_empty());
    assert!(probed(&root.connection()));
}

#[test]
fn a_restore_is_refused_while_another_connection_is_open() {
    let start = SystemTime::now();
    let (root, _) = upgraded(start);
    let after = rows(&root.connection());
    let reader = root.connection();
    reader.execute_batch("BEGIN").unwrap();
    let held: i64 = reader
        .query_row("SELECT COUNT(*) FROM migration_probe", [], |row| row.get(0))
        .unwrap();
    assert_eq!(held, 2);

    let refused = restored(&root, start + HOUR).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_BUSY), "{refused}");
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    assert_eq!(rows(&root.connection()), after);
    assert!(replaced(&root).is_empty());
    restored(&root, start + HOUR).unwrap();
    assert_eq!(version(&root.connection()).unwrap(), V24_VERSION_MAX);
}

#[test]
fn a_restore_brings_back_the_2_4_2_counts() {
    let start = SystemTime::now();
    let (root, before) = upgraded(start);

    restored(&root, start + HOUR).unwrap();

    let db = root.connection();
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    assert!(!probed(&db));
    let now = rows(&db);
    assert_eq!(counts(&now), counts(&before));
    assert_eq!(now, before);
}

#[test]
fn a_restore_keeps_one_dated_private_copy_of_the_2_5_data_it_replaces() {
    let start = SystemTime::now();
    let (root, _) = upgraded(start);
    let replaced_rows = rows(&root.connection());
    assert_eq!(replaced_rows["migration_probe"].len(), 2);
    let at = start + HOUR;

    let copy = restored(&root, at).unwrap();

    assert_eq!(replaced(&root), vec![copy.clone()]);
    let name = copy.file_name().unwrap().to_str().unwrap();
    assert_eq!(name, format!("{REPLACED}-{}", stamp(at).unwrap()));
    assert_eq!(backup_time(name), None);
    assert_eq!(mode(&copy), 0o700);
    assert_eq!(mode(&copy.join(DATABASE)), 0o600);
    assert_eq!(mode(&copy.join(CHECKED)), 0o600);
    let copied = Connection::open(copy.join(DATABASE)).unwrap();
    let kept_rows = rows(&copied);
    assert_eq!(counts(&kept_rows), counts(&replaced_rows));
    assert_eq!(kept_rows, replaced_rows);
    let global = std::fs::read_to_string(copy.join("global.toml")).unwrap();
    assert_eq!(global, GLOBAL);
    let project = std::fs::read_to_string(copy.join("project.toml")).unwrap();
    assert_eq!(project, PROJECT);
    assert_eq!(names(&root).len(), 1);
}

#[test]
fn a_later_upgrades_rotation_leaves_the_restore_copy() {
    let start = SystemTime::now();
    let (root, _) = upgraded(start);
    let copy = restored(&root, start + HOUR).unwrap();
    let pin = names(&root)[0].clone();
    let sources = sources(&root);
    let mut db = root.connection();
    let extra: [StepRun; 7] = [probe, kept, kept, kept, kept, kept, kept];

    for index in 0..extra.len() {
        let now = start + HOUR * (2 + u32::try_from(index).unwrap());
        upgrade_at(
            &mut db,
            &root.database(),
            Some(&sources),
            &steps(&extra[..=index]),
            now,
            check_copy,
        )
        .unwrap();
    }

    let names = names(&root);
    assert_eq!(names.len(), BACKUPS_KEPT + 1, "{names:?}");
    assert_eq!(names[0], pin);
    assert_eq!(replaced(&root), vec![copy.clone()]);
    assert!(copy.join(CHECKED).is_file());
    assert!(copy.join(DATABASE).is_file());
}
