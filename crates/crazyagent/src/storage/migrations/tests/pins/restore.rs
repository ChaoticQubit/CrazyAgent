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
    let last = late - Duration::from_millis(1);
    assert_eq!(restorable_until(root.path(), last), Some(late));
    assert_eq!(restorable_until(root.path(), late), None);
    assert_eq!(
        restorable_until(root.path(), late + Duration::from_millis(1)),
        None
    );

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
    crate::team::initialize(&db).unwrap();
    drop(crate::research::jobs::ResearchJobs::open(&root.database()).unwrap());
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
    assert_eq!(replaced(&root), vec![copy.clone()]);
    assert!(copy.join(CHECKED).is_file());
    assert!(copy.join(DATABASE).is_file());
}

#[test]
fn a_second_restore_brings_back_the_2_4_data_written_after_the_first() {
    let start = SystemTime::now();
    let (root, _) = upgraded(start);
    restored(&root, start + HOUR).unwrap();
    let mut db = root.connection();
    let removed = db
        .execute(
            "DELETE FROM projects WHERE rowid = (SELECT MIN(rowid) FROM projects)",
            [],
        )
        .unwrap();
    assert_eq!(removed, 1, "the P5 fixture holds its project");
    let used = rows(&db);
    let sources = sources(&root);
    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        start + HOUR * 2,
        check_copy,
    )
    .unwrap();
    drop(db);
    assert_eq!(names(&root).len(), 2);

    restored(&root, start + HOUR * 3).unwrap();

    let db = root.connection();
    assert_eq!(version(&db).unwrap(), V24_VERSION_MAX);
    assert!(!probed(&db));
    assert_eq!(rows(&db), used);
    assert_eq!(replaced(&root).len(), 2);
}

#[test]
fn a_failed_restore_changes_nothing_and_keeps_no_copy() {
    let start = SystemTime::now();
    let (root, _) = upgraded(start);
    let after = rows(&root.connection());
    let pin = root.backups()[0].join(DATABASE);
    let opened = Connection::open(&pin).unwrap();
    opened
        .pragma_update(None, "user_version", latest(STEPS) + 1)
        .unwrap();
    drop(opened);

    let refused = restored(&root, start + HOUR).unwrap_err().0;

    assert!(refused.starts_with(RESTORE_FAILED), "{refused}");
    assert!(!refused.contains("stays in"), "{refused}");
    assert_eq!(rows(&root.connection()), after);
    assert!(probed(&root.connection()));
    assert!(replaced(&root).is_empty());
    assert_eq!(names(&root).len(), 1);
}

/// The names one key folder holds, in the engine root or in a restore's dated copy, sorted.
/// Inputs: the folder holding the key folders and the key folder. Output: the names, none
/// for a missing folder.
fn key_files(base: &Path, folder: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(base.join(folder)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// V250-L5-N5: a restore moves every key file only 2.5 rows named, a plugin key a 2.5
/// binding held and a key of an account added after the upgrade, into the same key folders
/// of the dated copy it keeps of the 2.5 data (DEC-192), private as they were, and deletes
/// the staged copies in both key folders, while the key of an account the 2.4 copy holds
/// stays where it is.
#[test]
fn a_restore_moves_the_keys_only_2_5_rows_name_into_its_copy_and_keeps_a_2_4_accounts_key() {
    use crate::storage::accounts::{self, ACCOUNT_KEYS, KEY_FOLDERS, PLUGIN_KEYS};
    let start = SystemTime::now();
    let (root, _) = fixture();
    let old = root.connection();
    accounts::initialize(&old).unwrap();
    old.execute(
        "INSERT INTO accounts (id, body) VALUES ('restore-old', ?1)",
        [br#"{"credential_ref":"restore-old-key"}"#.as_slice()],
    )
    .unwrap();
    drop(old);
    let sources = sources(&root);
    let mut db = root.connection();
    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        start,
        check_copy,
    )
    .unwrap();
    crate::app_plugins::store::initialize(&db).unwrap();
    db.execute(
        "INSERT INTO app_plugin_secrets (plugin, secret, source, reference) \
         VALUES ('openai', 'api_key', 'key', 'plugin-key-restore')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO accounts (id, body) VALUES ('restore-new', ?1)",
        [br#"{"credential_ref":"restore-new-key"}"#.as_slice()],
    )
    .unwrap();
    drop(db);
    for (folder, name) in [
        (PLUGIN_KEYS, "plugin-key-restore"),
        (ACCOUNT_KEYS, "restore-old-key"),
        (ACCOUNT_KEYS, "restore-new-key"),
    ] {
        let reference = crate::protocol::CredentialRef::new(name).unwrap();
        accounts::replace_key_in(root.path(), folder, &reference, b"sk-restore".to_vec()).unwrap();
    }
    for folder in KEY_FOLDERS {
        let staged = root.path().join(folder).join(".replace-restore");
        std::fs::write(&staged, "sk-staged").unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(
        key_files(root.path(), PLUGIN_KEYS),
        [".replace-restore", "plugin-key-restore"]
    );
    assert_eq!(
        key_files(root.path(), ACCOUNT_KEYS),
        [".replace-restore", "restore-new-key", "restore-old-key"]
    );

    let copy = restored(&root, start + HOUR).unwrap();

    assert_eq!(version(&root.connection()).unwrap(), V24_VERSION_MAX);
    assert!(key_files(root.path(), PLUGIN_KEYS).is_empty());
    assert_eq!(key_files(root.path(), ACCOUNT_KEYS), ["restore-old-key"]);
    assert_eq!(mode(&root.path().join(PLUGIN_KEYS)), 0o700);
    assert_eq!(key_files(&copy, PLUGIN_KEYS), ["plugin-key-restore"]);
    assert_eq!(key_files(&copy, ACCOUNT_KEYS), ["restore-new-key"]);
    for (folder, name) in [
        (PLUGIN_KEYS, "plugin-key-restore"),
        (ACCOUNT_KEYS, "restore-new-key"),
    ] {
        let moved = copy.join(folder).join(name);
        assert_eq!(mode(&copy.join(folder)), 0o700);
        assert_eq!(mode(&moved), 0o600);
        assert_eq!(std::fs::read(&moved).unwrap(), b"sk-restore");
    }
}

/// Saves one account row as the accounts store writes it: a local connection with the given
/// authentication, and its details. Inputs: the database, the id and the authentication.
fn save_account(db: &Connection, id: &str, auth: crate::config::ModelAuthentication) {
    let reference = |value: &str| crate::protocol::CredentialRef::new(value).unwrap();
    let account = crate::config::ModelConnection {
        id: reference(id),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference("local-account"),
        endpoint: crate::config::ModelEndpoint::Http {
            url: "http://127.0.0.1:8000/v1".into(),
        },
        model: crate::config::ModelReference {
            id: "m".into(),
            author: reference("openai-compatible"),
        },
        auth,
    };
    let details = crate::accounts::AccountDetails {
        name: "Lab".into(),
        provider: crate::accounts::Provider::OpenaiCompatible,
    };
    let body = serde_json::to_vec(&serde_json::json!({"connection": account, "details": details}))
        .unwrap();
    db.execute(
        "INSERT INTO accounts (id, body) VALUES (?1, ?2)",
        rusqlite::params![id, body],
    )
    .unwrap();
}

/// The folder name the engine gives an account's profile slot or generated home: the
/// SHA-256 of its reference in hex. Input: the reference. Output: the name.
fn slot_name(reference: &str) -> String {
    format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(reference.as_bytes())
    )
}

/// Makes one private file holding a dummy secret, its 0700 folders included. Input: the
/// file. Output: none.
fn private_secret(file: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(file.parent().unwrap())
        .unwrap();
    std::fs::write(file, "secret-dummy").unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// V250-L5-N5B: a restore deletes the generated SDK home holding the key of an API key account
/// only the replaced 2.5 data holds, and the sign-in files of a native account only it holds,
/// keeping the rest of that slot, while the home of an account the 2.4 copy holds stays.
#[test]
fn a_restore_deletes_the_sdk_home_and_sign_in_of_accounts_only_2_5_holds() {
    use crate::config::ModelAuthentication;
    let reference = |value: &str| crate::protocol::CredentialRef::new(value).unwrap();
    let start = SystemTime::now();
    let (root, _) = fixture();
    let old = root.connection();
    crate::storage::accounts::initialize(&old).unwrap();
    let old_key = ModelAuthentication::ApiKey {
        credential_ref: reference("restore-old-key"),
    };
    save_account(&old, "restore-old", old_key);
    drop(old);
    let sources = sources(&root);
    let mut db = root.connection();
    upgrade_at(
        &mut db,
        &root.database(),
        Some(&sources),
        &steps(&[probe]),
        start,
        check_copy,
    )
    .unwrap();
    let new_key = ModelAuthentication::ApiKey {
        credential_ref: reference("restore-new-key"),
    };
    save_account(&db, "restore-new", new_key);
    let native = ModelAuthentication::NativeManaged {
        profile_ref: reference("restore-new-profile"),
    };
    save_account(&db, "restore-native", native);
    drop(db);
    let homes = root.path().join("configured-sdk-homes");
    let kept_home = homes.join(slot_name("restore-old-key"));
    let new_home = homes.join(slot_name("restore-new-key"));
    for home in [&kept_home, &new_home] {
        private_secret(&home.join(".backboard/config.json"));
        private_secret(&home.join(".backboard/keys.json"));
    }
    let slot = root
        .path()
        .join("native-profiles")
        .join(slot_name("restore-new-profile"));
    let sign_ins = [
        slot.join("home/auth.json"),
        slot.join("home/.credentials.json"),
        slot.join("home/Library/Keychains/login.keychain-db"),
    ];
    let identity = slot.join("identity.json");
    for file in sign_ins.iter().chain([&identity]) {
        private_secret(file);
    }

    restored(&root, start + HOUR).unwrap();

    assert_eq!(version(&root.connection()).unwrap(), V24_VERSION_MAX);
    assert!(kept_home.join(".backboard/keys.json").exists());
    assert!(!new_home.exists());
    for file in &sign_ins {
        assert!(!file.exists(), "{}", file.display());
    }
    assert!(identity.exists());
}
