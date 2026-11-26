use super::*;
use std::os::unix::fs::PermissionsExt;

/// No settings texts. Input: none. Output: an empty list.
fn no_texts() -> Result<Vec<(&'static str, String)>, StorageError> {
    Ok(Vec::new())
}

/// A database with one table of one row in a private folder. Input: none. Output: the
/// folder, the database path and its connection.
fn database() -> (tempfile::TempDir, PathBuf, Connection) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join(DATABASE);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE kept (value TEXT); INSERT INTO kept VALUES ('one');")
        .unwrap();
    (root, path, db)
}

#[test]
fn only_a_real_folder_of_this_user_counts_as_a_backup() {
    let root = tempfile::tempdir().unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"").unwrap();
    assert!(own_folder(root.path()));
    assert!(!own_folder(&link));
    assert!(!own_folder(&file));
    assert!(!own_folder(&root.path().join("missing")));
    assert_eq!(
        own_folder(Path::new("/")),
        rustix::process::getuid().as_raw() == 0
    );
}

#[test]
fn backup_names_use_a_sortable_utc_stamp() {
    assert_eq!(stamp(UNIX_EPOCH).unwrap(), "19700101T000000000Z");
    assert_eq!(
        stamp(UNIX_EPOCH + Duration::from_secs(951_868_800)).unwrap(),
        "20000301T000000000Z"
    );
    assert_eq!(
        stamp(UNIX_EPOCH + Duration::from_millis(1_709_251_199_999)).unwrap(),
        "20240229T235959999Z"
    );
    assert!(stamp(UNIX_EPOCH + Duration::from_secs(253_402_300_800)).is_err());
    assert_eq!(
        backup_time("1-to-2-20240229T235959999Z"),
        Some("20240229T235959999Z")
    );
    for other in [
        "keep-me",
        "1-to-2-",
        "x-to-2-20240229T235959999Z",
        "1-to-2-20240229T235959999",
        "1-to-2-2024022XT235959999Z",
        "1-to-2-2024022\u{e9}T35959999Z",
    ] {
        assert_eq!(backup_time(other), None, "{other}");
    }
}

#[test]
fn a_backup_failure_record_counts_only_as_the_engine_writes_it() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join(BACKUP_FAILURE_FILE);
    let write = |text: &[u8], mode: u32| {
        let _ = std::fs::remove_file(&record);
        std::fs::write(&record, text).unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    write(b"storage_backup_failed", 0o600);
    assert_eq!(backup_failure(root.path()), Some("storage_backup_failed"));
    write(b"storage_backup_unsafe", 0o644);
    assert_eq!(backup_failure(root.path()), None);
    write(b"storage_backup_unsafe: /Users/someone", 0o600);
    assert_eq!(backup_failure(root.path()), None);
    std::fs::remove_file(&record).unwrap();
    let target = root.path().join("elsewhere");
    std::fs::write(&target, b"storage_backup_failed").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&target, &record).unwrap();
    assert_eq!(backup_failure(root.path()), None);
}

#[test]
fn a_backup_keeps_every_settings_text_it_is_given_beside_the_database() {
    let (_root, path, db) = database();
    let texts = || {
        Ok(vec![
            ("global.toml", "a = 1\n".to_owned()),
            ("project.toml", "b = 2\n".to_owned()),
        ])
    };
    let made = backup(&db, &path, &texts, (1, 2), SystemTime::now()).unwrap();
    assert_eq!(
        std::fs::read_to_string(made.join("global.toml")).unwrap(),
        "a = 1\n"
    );
    assert_eq!(
        std::fs::read_to_string(made.join("project.toml")).unwrap(),
        "b = 2\n"
    );
    for name in [DATABASE, CHECKED, "global.toml", "project.toml"] {
        let mode = std::fs::metadata(made.join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{name}");
    }
}

#[test]
fn the_real_backup_refuses_a_copy_the_live_database_outgrew() {
    let (root, path, db) = database();
    let texts = || {
        db.execute("INSERT INTO kept VALUES ('two')", []).unwrap();
        Ok(Vec::new())
    };
    let refused = backup(&db, &path, &texts, (1, 2), SystemTime::now()).unwrap_err();
    assert_eq!(
        refused.0,
        "storage_backup_failed: the backup copy holds 1 rows in kept where engine.db holds 2"
    );
    let left: Vec<_> = std::fs::read_dir(root.path().join(BACKUPS))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_backup_without_settings_texts_holds_the_database_and_its_check() {
    let (_root, path, db) = database();
    let made = backup(&db, &path, &no_texts, (1, 2), SystemTime::now()).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&made)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [CHECKED, DATABASE]);
}

/// Runs one backup whose settings texts name a file the folder already holds or one that
/// leaves it. Input: the name. Output: none; the kernel's assert ends the row.
fn backup_named(name: &'static str) {
    let (_root, path, db) = database();
    let texts = || Ok(vec![(name, String::new())]);
    let _ = backup(&db, &path, &texts, (1, 2), SystemTime::now());
}

#[test]
#[should_panic(expected = "a settings text is one plain file name beside engine.db and checked")]
fn a_settings_text_named_engine_db_is_refused() {
    backup_named("engine.db");
}

#[test]
#[should_panic(expected = "a settings text is one plain file name beside engine.db and checked")]
fn a_settings_text_named_checked_is_refused() {
    backup_named("checked");
}

#[test]
#[should_panic(expected = "a settings text is one plain file name beside engine.db and checked")]
fn a_settings_text_in_another_folder_is_refused() {
    backup_named("a/b");
}

#[test]
fn a_private_file_makes_one_full_flush() {
    let root = tempfile::tempdir().unwrap();
    take_flushes();
    private_file(&root.path().join("one"), b"bytes", Flush::Full).unwrap();
    assert_eq!(take_flushes(), (1, 0));
    assert_eq!(std::fs::read(root.path().join("one")).unwrap(), b"bytes");
}
