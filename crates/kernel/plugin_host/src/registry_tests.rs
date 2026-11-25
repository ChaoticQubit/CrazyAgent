use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::OptionalExtension;

use crate::Refused;
use crate::registry::{
    Engine, FOLDER, MANIFEST_FILE, Switch, USER_PLUGINS_MAX, add, bundled, hold, list,
    loopback_ports, off_code_plugins, preview, ready, remove, review, set_enabled,
};
use crate::store::{
    Bound, KeyFolder, KeyRef, Keys, bind_account, initialize, secret_value, set_key, sweep_staged,
    unbind,
};
use crate::trust::trust_file;
use crate::usage::{ChangeNotice, Holder, TurnOff, Usage};
use crate::views::{Binding, Listing, Origin, PluginStatus, PluginView, Preview, RunsView};

const ACME: &str = r#"plugin = "acme"
tier = "t2"
kind = "embedding"
version = 1
permissions = []
title = "Acme embeddings"
release = "1.0.0"

[http]
url = "https://api.acme.example/v1/embed"
hosts = ["api.acme.example"]
secrets = ["api_key"]
vectors = "data.*.embedding"
dimensions = 256

[http.headers]
Authorization = "Bearer {{secret.api_key}}"

[http.body]
input = "{{inputs}}"
"#;

pub(crate) const NEAR: &str = r#"plugin = "near"
tier = "t2"
kind = "embedding"
version = 1
permissions = []
title = "Near"
release = "1"

[http]
url = "http://127.0.0.1:9000/embed"
hosts = ["127.0.0.1"]
vectors = "embeddings.*"
dimensions = 128

[http.body]
input = "{{inputs}}"
"#;

const BROKEN: &str = "version = 1\nkind = = 2\n";

fn key_path(root: &Path, folder: KeyFolder, reference: &KeyRef) -> PathBuf {
    root.join(folder_name(folder)).join(reference.as_str())
}

fn folder_name(folder: KeyFolder) -> &'static str {
    match folder {
        KeyFolder::Account => "account-keys",
        KeyFolder::Plugin => "plugin-keys",
    }
}

fn test_saved(root: &Path, folder: KeyFolder, reference: &KeyRef) -> Result<bool, ()> {
    Ok(key_path(root, folder, reference).is_file())
}

fn test_account(
    db: &rusqlite::Connection,
    id: &KeyRef,
) -> Result<Option<crate::store::AccountKey>, ()> {
    let row: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT name, key FROM test_accounts WHERE id=?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| ())?;
    Ok(row.map(|(name, key)| (name, key.and_then(|key| KeyRef::new(&key)))))
}

fn test_replace(
    root: &Path,
    folder: KeyFolder,
    reference: &KeyRef,
    key: Vec<u8>,
) -> Result<(), ()> {
    let path = key_path(root, folder, reference);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|_| ())?;
    std::fs::write(&path, key).map_err(|_| ())
}

fn test_remove(root: &Path, folder: KeyFolder, reference: &KeyRef) -> Result<(), ()> {
    match std::fs::remove_file(key_path(root, folder, reference)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(()),
    }
}

fn test_read(
    root: &Path,
    folder: KeyFolder,
    reference: &KeyRef,
) -> Result<Option<kernel_security::secret::SecretString>, ()> {
    match std::fs::read(key_path(root, folder, reference)) {
        Ok(bytes) => kernel_security::secret::SecretString::new(bytes)
            .map(Some)
            .map_err(|_| ()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

fn test_sweep(root: &Path, folder: KeyFolder) -> Result<(), ()> {
    let Ok(entries) = std::fs::read_dir(root.join(folder_name(folder))) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        if entry.file_name().to_string_lossy().starts_with(".replace-") {
            std::fs::remove_file(entry.path()).map_err(|_| ())?;
        }
    }
    Ok(())
}

fn test_new_ref() -> Result<String, &'static str> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    Ok(format!("{:064x}", NEXT.fetch_add(1, Ordering::Relaxed)))
}

/// A key store over plain files under the engine root, standing in for the engine's own,
/// which keeps its own rows.
const KEYS: Keys = Keys {
    saved: test_saved,
    account: test_account,
    replace: test_replace,
    remove: test_remove,
    read: test_read,
    sweep: test_sweep,
    new_ref: test_new_ref,
};

pub(crate) struct Case {
    pub(crate) temp: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) db: rusqlite::Connection,
    held: Option<crate::store_log::RowsHold>,
}

impl Case {
    /// Ends this row's store hold, so the next store call runs outside any capture.
    fn release(&mut self) {
        assert!(self.held.take().is_some());
    }

    /// Takes the store hold again after [`Case::release`].
    fn hold(&mut self) {
        assert!(self.held.is_none());
        self.held = Some(crate::store_log::rows_hold());
    }

    pub(crate) fn engine(&self) -> Engine<'_> {
        Engine {
            root: &self.root,
            keys: &KEYS,
            handed_out: None,
        }
    }
}

pub(crate) fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("engine");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    Case {
        temp,
        root,
        db,
        held: Some(crate::store_log::rows_hold()),
    }
}

fn sha(text: &str) -> String {
    kernel_common::hash(text.as_bytes())
}

fn plugin<'a>(listing: &'a Listing, name: &str) -> &'a PluginView {
    listing
        .plugins
        .iter()
        .find(|plugin| plugin.name == name)
        .unwrap_or_else(|| panic!("no plugin {name}"))
}

fn refused_code(view: &PluginView) -> String {
    match &view.status {
        PluginStatus::Refused { problem } => problem.code.clone(),
        other => panic!("{}: {other:?}", view.name),
    }
}

/// A folder the owner picks to add, outside the engine root, holding one plugin.toml.
/// Inputs: the case, a folder name unique in the row and the text. Output: the folder.
fn picked(case: &Case, folder: &str, text: &str) -> PathBuf {
    let source = case.temp.path().join(format!("picked-{folder}"));
    let _ = std::fs::remove_dir_all(&source);
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join(MANIFEST_FILE), text).unwrap();
    source
}

fn plugins_folder(case: &Case) -> PathBuf {
    let folder = case.root.join(FOLDER);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

/// Drops a plugin folder straight into the plugins folder, as an owner copying it by hand.
/// Inputs: the case, the folder name and the text. Output: its plugin.toml path.
fn drop_in(case: &Case, name: &str, text: &str) -> PathBuf {
    let folder = plugins_folder(case).join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = folder.join(MANIFEST_FILE);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

/// Turning a plugin off with the default setting and no run holding it.
pub(crate) fn off() -> Switch<'static> {
    static IDLE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();
    Switch::Off {
        usage: IDLE.get_or_init(Usage::default),
        when_off: TurnOff::RefuseNewCalls,
        essential_unlocked: false,
    }
}

fn holder(run: &str) -> Holder {
    Holder {
        run_id: run.to_owned(),
        label: format!("run {run}"),
    }
}

fn counted(calls: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Box<dyn FnOnce() + Send> {
    let calls = std::sync::Arc::clone(calls);
    Box::new(move || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })
}

fn code<T: std::fmt::Debug>(result: Result<T, Refused>) -> &'static str {
    result.expect_err("refused").code
}

fn add_text(case: &Case, text: &str, replace: bool, now_ms: i64) -> Result<(), Refused> {
    let source = picked(case, &format!("add-{now_ms}"), text);
    add(
        &case.db,
        &case.engine(),
        &source,
        &sha(text),
        replace,
        now_ms,
    )
}

pub(crate) fn trusted(case: &Case, text: &str) {
    add_text(case, text, false, 1).unwrap();
}

/// Reads a fixture a row checks. Inputs: its path and the read. Output: what the read
/// returned; a failed read stops the row naming the path and the I/O error.
fn fixture<'p, T>(path: &'p Path, read: impl FnOnce(&'p Path) -> std::io::Result<T>) -> T {
    read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// A fixture read that fails stops the row with the path it read and the I/O error.
#[test]
#[should_panic(expected = "missing.toml: No such file")]
fn a_failed_fixture_read_names_its_path_and_error() {
    let case = case();
    fixture(&case.root.join("missing.toml"), std::fs::read);
}

/// The entry names in a folder, read through the fixture helper. An entry or a name that
/// does not read stops the row with the folder and what failed. Input: the folder. Output:
/// the names.
fn names_in(folder: &Path) -> Vec<String> {
    fixture(folder, std::fs::read_dir)
        .map(
            |entry| match entry.map(|entry| entry.file_name().into_string()) {
                Ok(Ok(name)) => name,
                failed => panic!("{}: {failed:?}", folder.display()),
            },
        )
        .collect()
}

fn plugin_keys(case: &Case) -> Vec<PathBuf> {
    let keys = case.root.join("plugin-keys");
    let Ok(entries) = std::fs::read_dir(&keys) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("{}: {error}", keys.display()))
                .path()
        })
        .collect();
    files.sort();
    files
}

fn saved_account(case: &Case, id: &str, key: Option<&str>) {
    case.db
        .execute(
            "CREATE TABLE IF NOT EXISTS test_accounts (id TEXT PRIMARY KEY, name TEXT, key TEXT)",
            [],
        )
        .unwrap();
    case.db
        .execute(
            "INSERT INTO test_accounts (id, name, key) VALUES (?1, 'Work Acme', ?2)",
            rusqlite::params![id, key],
        )
        .unwrap();
}

fn cells(db: &rusqlite::Connection, table: &str) -> Vec<String> {
    let mut statement = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|index| {
                    row.get::<_, rusqlite::types::Value>(index)
                        .map(|value| format!("{value:?}"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .concat()
}

/// The three bundled HTTP embedding plugins list first, in screen
/// order, and every one starts off.
#[test]
fn bundled_plugins_list_first_and_all_start_off() {
    let case = case();
    let listing = list(&case.db, &case.engine()).unwrap();
    let names: Vec<&str> = listing
        .plugins
        .iter()
        .map(|plugin| plugin.name.as_str())
        .collect();
    assert_eq!(names, ["openai", "voyage", "ollama"]);
    assert!(!listing.more);
    assert!(
        listing
            .plugins
            .iter()
            .all(|plugin| plugin.origin == Origin::Bundled && !plugin.enabled)
    );
    let openai = plugin(&listing, "openai");
    assert_eq!(
        (&openai.status, openai.enabled),
        (&PluginStatus::NeedsKey, false)
    );
    assert_eq!(openai.secrets[0].binding, Binding::None);
    let ollama = plugin(&listing, "ollama");
    assert_eq!(
        (&ollama.status, ollama.enabled),
        (&PluginStatus::Ready, false)
    );
    assert!(!case.root.join(FOLDER).exists());
}

/// The bundled plugin.toml files are trusted by shipping, need no review, write no trust
/// row and start off; turning one on needs no review either.
#[test]
fn bundled_plugins_are_trusted_and_start_off() {
    let case = case();
    assert_eq!(bundled().len(), 3);
    for (name, text) in bundled() {
        let manifest = crate::embedding::parse(text).unwrap();
        assert_eq!(&manifest.name, name);
        let view = plugin(&list(&case.db, &case.engine()).unwrap(), name).clone();
        assert_ne!(view.status, PluginStatus::NeedsReview, "{name}");
        assert!(!view.enabled, "{name}");
    }
    set_enabled(&case.db, &case.engine(), "ollama", Switch::On).unwrap();
    assert!(plugin(&list(&case.db, &case.engine()).unwrap(), "ollama").enabled);
    assert!(cells(&case.db, "app_plugin_trust").is_empty());
}

/// What an older plugin of the owner's left under a name a bundled plugin now has is
/// dropped the next time the store is used.
#[test]
fn leftovers_of_an_older_plugin_under_a_bundled_name_are_dropped() {
    let case = case();
    let reference = KeyRef::new("plugin-key-leftover").unwrap();
    test_replace(
        &case.root,
        KeyFolder::Plugin,
        &reference,
        b"sk-leftover-key-01".to_vec(),
    )
    .unwrap();
    case.db
        .execute_batch(&format!(
            "INSERT INTO app_plugin_trust (name, sha256, hosts, trusted_ms)
             VALUES ('openai', '{}', 'old.example', 1);
             INSERT INTO app_plugin_state (name, enabled) VALUES ('openai', 1);
             INSERT INTO app_plugin_secrets (plugin, secret, source, reference)
             VALUES ('openai', 'api_key', 'key', 'plugin-key-leftover');",
            "0".repeat(64)
        ))
        .unwrap();
    assert_eq!(plugin_keys(&case).len(), 1);
    let listing = list(&case.db, &case.engine()).unwrap();
    let openai = plugin(&listing, "openai");
    assert_eq!(
        (openai.origin, &openai.status, openai.enabled),
        (Origin::Bundled, &PluginStatus::NeedsKey, false)
    );
    assert_eq!(openai.secrets[0].binding, Binding::None);
    for table in ["app_plugin_trust", "app_plugin_state", "app_plugin_secrets"] {
        assert!(cells(&case.db, table).is_empty(), "{table}");
    }
    assert!(plugin_keys(&case).is_empty());
}

#[test]
fn a_preview_shows_the_trust_screen_and_saves_nothing() {
    let case = case();
    let source = picked(&case, "acme", ACME);
    let Preview::Ready { trust } = preview(&case.db, &case.engine(), &source).unwrap() else {
        panic!("ready");
    };
    assert_eq!(trust.name, "acme");
    assert_eq!(trust.sha256, sha(ACME));
    assert_eq!(trust.secrets, ["api_key"]);
    assert!(!trust.replaces && !trust.changed && trust.grew_hosts.is_empty());
    assert!(!case.root.join(FOLDER).exists());
    let broken = picked(&case, "broken", BROKEN);
    let Preview::Refused { problem } = preview(&case.db, &case.engine(), &broken).unwrap() else {
        panic!("refused");
    };
    assert_eq!(
        (problem.code.as_str(), problem.line),
        ("plugin_manifest_invalid", Some(2))
    );
    let bundled_name = picked(
        &case,
        "openai",
        &ACME.replace("plugin = \"acme\"", "plugin = \"openai\""),
    );
    let Preview::Refused { problem } = preview(&case.db, &case.engine(), &bundled_name).unwrap()
    else {
        panic!("refused");
    };
    assert_eq!(problem.code, "app_plugin_name_taken");
}

#[test]
fn add_saves_a_private_file_trusts_it_and_turns_it_on() {
    let case = case();
    add_text(&case, ACME, false, 1_000).unwrap();
    let folder = case.root.join(FOLDER);
    let plugin_folder = folder.join("acme");
    let file = plugin_folder.join(MANIFEST_FILE);
    assert_eq!(fixture(&file, std::fs::read), ACME.as_bytes());
    assert_eq!(
        (mode(&folder), mode(&plugin_folder), mode(&file)),
        (0o700, 0o700, 0o600)
    );
    assert_eq!(fixture(&plugin_folder, std::fs::read_dir).count(), 1);
    let listing = list(&case.db, &case.engine()).unwrap();
    let acme = plugin(&listing, "acme");
    assert_eq!(
        (acme.origin, &acme.status, acme.enabled),
        (Origin::User, &PluginStatus::NeedsKey, true)
    );
    assert_eq!(acme.sha256, sha(ACME));
    assert_eq!(listing.plugins.last().unwrap().name, "acme");
}

/// A trust screen read before another add put a folder under the same name still says it
/// adds a new plugin; the add it leads to is refused as a used name, not as a built-in's,
/// so the owner reviews it again and is offered the replace.
#[test]
fn a_trust_screen_gone_stale_by_a_new_folder_is_refused_as_name_used() {
    let case = case();
    let source = picked(&case, "acme", ACME);
    let Preview::Ready { trust } = preview(&case.db, &case.engine(), &source).unwrap() else {
        panic!("ready");
    };
    assert!(!trust.replaces);
    add_text(&case, ACME, false, 1).unwrap();

    let stale = add(
        &case.db,
        &case.engine(),
        &source,
        &trust.sha256,
        trust.replaces,
        2,
    );

    assert_eq!(code(stale), "app_plugin_name_used");
}

#[test]
fn add_refuses_a_changed_text_and_a_taken_name_unless_replacing() {
    let case = case();
    let source = picked(&case, "acme", ACME);
    assert_eq!(
        code(add(&case.db, &case.engine(), &source, &sha(NEAR), false, 1)),
        "app_plugin_changed"
    );
    add_text(&case, ACME, false, 1).unwrap();
    assert_eq!(
        code(add_text(&case, ACME, false, 2)),
        "app_plugin_name_used"
    );
    let newer = ACME.replace("release = \"1.0.0\"", "release = \"1.1.0\"");
    let Preview::Ready { trust } =
        preview(&case.db, &case.engine(), &picked(&case, "newer", &newer)).unwrap()
    else {
        panic!("ready");
    };
    assert!(trust.replaces);
    add_text(&case, &newer, true, 3).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").version, "1.1.0");
    assert_eq!(
        fixture(&case.root.join(FOLDER), std::fs::read_dir).count(),
        1,
        "no staged or retired folder is left"
    );
    let bundled = ACME.replace("plugin = \"acme\"", "plugin = \"openai\"");
    assert_eq!(
        code(add_text(&case, &bundled, true, 4)),
        "app_plugin_name_taken"
    );
    assert_eq!(
        code(add_text(&case, BROKEN, false, 5)),
        "app_plugin_refused"
    );
}

/// Add copies the bytes the owner reviewed and nothing else: a plugin.toml changed after
/// its trust screen is refused with nothing copied, and a picked folder holding another
/// entry, a link, or that is a link is refused.
#[test]
fn add_copies_only_the_reviewed_content() {
    let case = case();
    let source = picked(&case, "acme", ACME);
    let Preview::Ready { trust } = preview(&case.db, &case.engine(), &source).unwrap() else {
        panic!("ready");
    };
    let changed = ACME.replace("dimensions = 256", "dimensions = 512");
    std::fs::write(source.join(MANIFEST_FILE), &changed).unwrap();
    assert_eq!(
        code(add(
            &case.db,
            &case.engine(),
            &source,
            &trust.sha256,
            false,
            1
        )),
        "app_plugin_changed"
    );
    assert!(!case.root.join(FOLDER).join("acme").exists());
    std::fs::write(source.join(MANIFEST_FILE), ACME).unwrap();
    std::fs::write(source.join("run.sh"), "echo").unwrap();
    assert_eq!(
        code(add(&case.db, &case.engine(), &source, &sha(ACME), false, 2)),
        "app_plugin_refused"
    );
    std::fs::remove_file(source.join("run.sh")).unwrap();
    let linked = case.temp.path().join("picked-linked");
    std::os::unix::fs::symlink(&source, &linked).unwrap();
    assert_eq!(
        code(add(&case.db, &case.engine(), &linked, &sha(ACME), false, 3)),
        "app_plugin_refused"
    );
    let inner = picked(&case, "inner-link", "");
    std::fs::remove_file(inner.join(MANIFEST_FILE)).unwrap();
    std::os::unix::fs::symlink(source.join(MANIFEST_FILE), inner.join(MANIFEST_FILE)).unwrap();
    assert_eq!(
        code(add(&case.db, &case.engine(), &inner, &sha(ACME), false, 4)),
        "app_plugin_refused"
    );
    assert!(!case.root.join(FOLDER).join("acme").exists());
    add(&case.db, &case.engine(), &source, &sha(ACME), false, 5).unwrap();
    assert_eq!(
        fixture(
            &case.root.join(FOLDER).join("acme").join(MANIFEST_FILE),
            std::fs::read
        ),
        ACME.as_bytes()
    );
}

/// A plugin folder holds its plugin.toml only: any other entry refuses it on the list with
/// `plugin_folder_extra_entry`, and the picked folder with the same on its trust screen.
#[test]
fn a_plugin_folder_with_another_entry_is_refused() {
    let case = case();
    let path = drop_in(&case, "near", NEAR);
    let folder = path.parent().unwrap();
    for extra in ["notes.txt", "lib"] {
        if extra == "lib" {
            std::fs::create_dir(folder.join(extra)).unwrap();
        } else {
            std::fs::write(folder.join(extra), "x").unwrap();
        }
        let listing = list(&case.db, &case.engine()).unwrap();
        assert_eq!(
            refused_code(plugin(&listing, "near")),
            "plugin_folder_extra_entry",
            "{extra}"
        );
        assert_eq!(
            code(ready(&case.db, &case.engine(), "near")),
            "app_plugin_refused"
        );
        if extra == "lib" {
            std::fs::remove_dir(folder.join(extra)).unwrap();
        } else {
            std::fs::remove_file(folder.join(extra)).unwrap();
        }
    }
    let source = picked(&case, "extra", NEAR);
    std::fs::write(source.join("weights.bin"), "x").unwrap();
    let Preview::Refused { problem } = preview(&case.db, &case.engine(), &source).unwrap() else {
        panic!("refused");
    };
    assert_eq!(problem.code, "plugin_folder_extra_entry");
}

/// A `.yaml` plugin file a version before plugin folders wrote is listed refused with
/// `app_plugin_format_old`, never read or run, and kept on disk until the owner removes it.
#[test]
fn a_left_over_yaml_plugin_is_listed_refused_and_kept() {
    let case = case();
    let legacy = plugins_folder(&case).join("acme.yaml");
    std::fs::write(&legacy, "schema: 1\nkind: embedding\nname: acme\n").unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    let acme = plugin(&listing, "acme");
    assert_eq!(
        (acme.origin, refused_code(acme).as_str()),
        (Origin::User, "app_plugin_format_old")
    );
    assert!(acme.runs.is_none());
    assert_eq!(
        code(ready(&case.db, &case.engine(), "acme")),
        "app_plugin_refused"
    );
    assert!(legacy.is_file());
    assert_eq!(
        fixture(&legacy, std::fs::read_to_string),
        "schema: 1\nkind: embedding\nname: acme\n"
    );
    remove(&case.db, &case.engine(), "acme").unwrap();
    assert!(!legacy.exists());
}

#[test]
fn a_dropped_in_file_needs_review_until_trusted_and_again_after_a_change() {
    let case = case();
    let path = drop_in(&case, "near", NEAR);
    let listing = list(&case.db, &case.engine()).unwrap();
    let near = plugin(&listing, "near");
    assert_eq!(
        (&near.status, near.enabled),
        (&PluginStatus::NeedsReview, false)
    );
    assert_eq!(
        code(set_enabled(&case.db, &case.engine(), "near", Switch::On)),
        "app_plugin_needs_review"
    );
    let Preview::Ready { trust } = review(&case.db, &case.engine(), "near").unwrap() else {
        panic!("ready");
    };
    assert_eq!(trust.sha256, sha(NEAR));
    assert_eq!(
        code(trust_file(&case.db, &case.engine(), "near", &sha(ACME), 1)),
        "app_plugin_changed"
    );
    trust_file(&case.db, &case.engine(), "near", &trust.sha256, 1).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        (
            &plugin(&listing, "near").status,
            plugin(&listing, "near").enabled
        ),
        (&PluginStatus::Ready, true)
    );
    std::fs::write(&path, NEAR.replace("dimensions = 128", "dimensions = 256")).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        plugin(&listing, "near").status,
        PluginStatus::Changed {
            trusted_sha256: sha(NEAR)
        }
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_changed"
    );
}

/// A plugin whose plugin.toml changed after the owner trusted it is off for every call
/// until the owner approves the new text; approving it runs it again.
#[test]
fn a_changed_plugin_is_refused_until_approved_again() {
    let case = case();
    trusted(&case, NEAR);
    ready(&case.db, &case.engine(), "near").unwrap();
    let path = case.root.join(FOLDER).join("near").join(MANIFEST_FILE);
    let changed = NEAR.replace("dimensions = 128", "dimensions = 256");
    std::fs::write(&path, &changed).unwrap();
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_changed"
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(!plugin(&listing, "near").enabled);
    assert_eq!(
        code(trust_file(&case.db, &case.engine(), "near", &sha(NEAR), 2)),
        "app_plugin_changed"
    );
    trust_file(&case.db, &case.engine(), "near", &sha(&changed), 3).unwrap();
    assert_eq!(
        ready(&case.db, &case.engine(), "near")
            .unwrap()
            .manifest
            .http
            .dimensions,
        256
    );
}

/// A re-review says the text changed and lists the hosts the trusted version did not
/// reach, first in the host list; a first review reads unchanged with nothing grown.
#[test]
fn a_review_lists_what_grew_first() {
    let case = case();
    trusted(&case, NEAR);
    let Preview::Ready { trust } = review(&case.db, &case.engine(), "near").unwrap() else {
        panic!("ready");
    };
    assert!(!trust.changed && trust.grew_hosts.is_empty());
    let moved = NEAR
        .replace("http://127.0.0.1:9000/embed", "http://localhost:9000/embed")
        .replace("hosts = [\"127.0.0.1\"]", "hosts = [\"localhost\"]");
    std::fs::write(
        case.root.join(FOLDER).join("near").join(MANIFEST_FILE),
        &moved,
    )
    .unwrap();
    let Preview::Ready { trust } = review(&case.db, &case.engine(), "near").unwrap() else {
        panic!("ready");
    };
    assert!(trust.changed);
    assert_eq!(trust.grew_hosts, ["localhost"]);
    let RunsView::Http { hosts, .. } = &trust.runs else {
        panic!("http");
    };
    assert_eq!(hosts.first().map(String::as_str), Some("localhost"));
    let same_hosts = NEAR.replace("dimensions = 128", "dimensions = 256");
    std::fs::write(
        case.root.join(FOLDER).join("near").join(MANIFEST_FILE),
        &same_hosts,
    )
    .unwrap();
    let Preview::Ready { trust } = review(&case.db, &case.engine(), "near").unwrap() else {
        panic!("ready");
    };
    assert!(trust.changed && trust.grew_hosts.is_empty());
}

#[test]
fn refused_and_unsafe_files_are_listed_with_their_problem_and_other_files_ignored() {
    let case = case();
    drop_in(&case, "bad", BROKEN);
    drop_in(&case, "other", NEAR);
    drop_in(
        &case,
        "ollama",
        &NEAR.replace("plugin = \"near\"", "plugin = \"ollama\""),
    );
    drop_in(
        &case,
        "local",
        &NEAR.replace("plugin = \"near\"", "plugin = \"local\""),
    );
    let folder = plugins_folder(&case);
    std::fs::write(folder.join("notes.txt"), "x").unwrap();
    drop_in(&case, ".hidden", NEAR);
    std::os::unix::fs::symlink(folder.join("other"), folder.join("linked")).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        refused_code(plugin(&listing, "bad")),
        "plugin_manifest_invalid"
    );
    assert_eq!(
        refused_code(plugin(&listing, "other")),
        "app_plugin_file_name_mismatch"
    );
    assert_eq!(
        refused_code(plugin(&listing, "linked")),
        "app_plugin_file_unsafe"
    );
    let users: Vec<&str> = listing
        .plugins
        .iter()
        .filter(|plugin| plugin.origin == Origin::User)
        .map(|plugin| plugin.name.as_str())
        .collect();
    assert_eq!(users, ["bad", "linked", "local", "ollama", "other"]);
    let user = |name: &str| {
        listing
            .plugins
            .iter()
            .find(|plugin| plugin.origin == Origin::User && plugin.name == name)
            .unwrap()
    };
    assert!(matches!(
        &user("ollama").status,
        PluginStatus::Refused { problem } if problem.code == "app_plugin_name_taken"
    ));
    assert!(user("ollama").runs.is_none());
    assert_eq!(user("local").status, PluginStatus::NeedsReview);
    assert!(plugin(&listing, "bad").runs.is_none());
}

#[test]
fn an_unsafe_folder_is_refused() {
    let case = case();
    drop_in(&case, "near", NEAR);
    let folder = case.root.join(FOLDER);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        code(list(&case.db, &case.engine())),
        "app_plugin_folder_unsafe"
    );
    assert_eq!(
        code(add_text(&case, ACME, false, 1)),
        "app_plugin_folder_unsafe"
    );
}

#[test]
fn the_folder_lists_at_most_its_cap_and_says_there_are_more() {
    let case = case();
    for index in 0..=USER_PLUGINS_MAX {
        let name = format!("p{index:03}");
        drop_in(
            &case,
            &name,
            &NEAR.replace("plugin = \"near\"", &format!("plugin = \"{name}\"")),
        );
    }
    let listing = list(&case.db, &case.engine()).unwrap();
    let users = listing
        .plugins
        .iter()
        .filter(|plugin| plugin.origin == Origin::User)
        .count();
    assert_eq!(users, USER_PLUGINS_MAX);
    assert!(listing.more);
}

/// An add the listing would not show, or that would push a listed plugin out of it, is
/// refused before anything is written; replacing a listed plugin still works.
#[test]
fn an_add_past_the_folder_cap_is_refused_with_nothing_written() {
    let case = case();
    let named = |name: &str| ACME.replace("plugin = \"acme\"", &format!("plugin = \"{name}\""));
    for index in 0..USER_PLUGINS_MAX {
        let name = format!("p{index:03}");
        drop_in(&case, &name, &named(&name));
    }
    let trust_before = cells(&case.db, "app_plugin_trust");
    let state_before = cells(&case.db, "app_plugin_state");
    for (name, now_ms) in [("p064", 1), ("a000", 2)] {
        assert_eq!(
            code(add_text(&case, &named(name), false, now_ms)),
            "app_plugin_past_cap"
        );
        assert!(!case.root.join(FOLDER).join(name).exists());
    }
    assert_eq!(cells(&case.db, "app_plugin_trust"), trust_before);
    assert_eq!(cells(&case.db, "app_plugin_state"), state_before);
    let newer = named("p010").replace("release = \"1.0.0\"", "release = \"1.1.0\"");
    add_text(&case, &newer, true, 3).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "p010").version, "1.1.0");
    assert!(!listing.more);
}

#[test]
fn remove_deletes_a_user_plugin_with_its_keys_and_refuses_bundled_and_unknown_ones() {
    let case = case();
    add_text(&case, ACME, false, 1).unwrap();
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-test-key-0001".to_vec(),
    )
    .unwrap();
    remove(&case.db, &case.engine(), "acme").unwrap();
    assert!(!case.root.join(FOLDER).join("acme").exists());
    assert!(plugin_keys(&case).is_empty());
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(listing.plugins.iter().all(|plugin| plugin.name != "acme"));
    assert_eq!(
        code(remove(&case.db, &case.engine(), "openai")),
        "app_plugin_bundled"
    );
    let shadowed = drop_in(
        &case,
        "ollama",
        &NEAR.replace("plugin = \"near\"", "plugin = \"ollama\""),
    );
    remove(&case.db, &case.engine(), "ollama").unwrap();
    assert!(!shadowed.exists() && !shadowed.parent().unwrap().exists());
    assert_eq!(
        code(remove(&case.db, &case.engine(), "ollama")),
        "app_plugin_bundled"
    );
    assert_eq!(
        code(remove(&case.db, &case.engine(), "missing")),
        "app_plugin_not_found"
    );
    add_text(&case, ACME, false, 2).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
}

/// A removal storage cannot commit changes nothing: its transaction ends, the plugin and its
/// key stay, and the same removal goes through once storage is free.
#[test]
fn a_removal_storage_cannot_commit_ends_its_transaction_and_keeps_the_plugin() {
    let mut case = case();
    let file = case.temp.path().join("store.db");
    case.db = rusqlite::Connection::open(&file).unwrap();
    initialize(&case.db).unwrap();
    case.db.busy_timeout(std::time::Duration::ZERO).unwrap();
    add_text(&case, ACME, false, 1).unwrap();
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-test-key-0001".to_vec(),
    )
    .unwrap();
    let reader = rusqlite::Connection::open(&file).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let tables: i64 = reader
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        .unwrap();
    assert!(tables > 0, "the reader holds the database");
    assert!(remove(&case.db, &case.engine(), "acme").is_err());
    assert!(
        case.db.is_autocommit(),
        "the failed removal ended its transaction"
    );
    reader.execute_batch("COMMIT").unwrap();
    assert!(case.root.join(FOLDER).join("acme").exists());
    assert_eq!(plugin_keys(&case).len(), 1);
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(listing.plugins.iter().any(|plugin| plugin.name == "acme"));
    remove(&case.db, &case.engine(), "acme").unwrap();
    assert!(plugin_keys(&case).is_empty());
}

/// A plugin entry that is a link is removed as the link itself: the folder it points to
/// and the plugin.toml inside stay untouched.
#[test]
fn remove_deletes_a_link_entry_and_leaves_its_target() {
    let case = case();
    let target = picked(&case, "target", NEAR);
    let linked = plugins_folder(&case).join("linked");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    remove(&case.db, &case.engine(), "linked").unwrap();
    assert!(std::fs::symlink_metadata(&linked).is_err());
    let manifest = target.join(MANIFEST_FILE);
    match std::fs::read_to_string(&manifest) {
        Ok(text) => assert_eq!(text, NEAR),
        Err(error) => panic!("{}: {error}", manifest.display()),
    }
}

#[test]
fn every_loopback_plugin_port_is_listed_for_the_sandbox_and_an_unsafe_folder_fails_closed() {
    let case = case();
    assert_eq!(loopback_ports(&case.engine()).unwrap(), [11434]);
    drop_in(&case, "near", NEAR);
    drop_in(
        &case,
        "studio",
        &NEAR
            .replace("plugin = \"near\"", "plugin = \"studio\"")
            .replace(
                "http://127.0.0.1:9000/embed",
                "http://localhost:1234/v1/embeddings",
            )
            .replace("[\"127.0.0.1\"]", "[\"localhost\"]"),
    );
    drop_in(&case, "acme", ACME);
    drop_in(&case, "bad", BROKEN);
    assert_eq!(loopback_ports(&case.engine()).unwrap(), [1234, 9000, 11434]);
    let folder = case.root.join(FOLDER);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        code(loopback_ports(&case.engine())),
        "app_plugin_folder_unsafe"
    );
}

#[test]
fn a_port_zero_plugin_and_files_past_the_cap_are_refused_while_the_ports_still_list() {
    let case = case();
    drop_in(
        &case,
        "zero",
        &NEAR
            .replace("plugin = \"near\"", "plugin = \"zero\"")
            .replace("127.0.0.1:9000", "127.0.0.1:0"),
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        refused_code(plugin(&listing, "zero")),
        "app_plugin_port_invalid"
    );
    assert_eq!(loopback_ports(&case.engine()).unwrap(), [11434]);
    let cap = USER_PLUGINS_MAX;
    for index in 0..=cap {
        let name = format!("p{index:03}");
        drop_in(
            &case,
            &name,
            &NEAR
                .replace("plugin = \"near\"", &format!("plugin = \"{name}\""))
                .replace(":9000", &format!(":{}", 20_000 + index)),
        );
    }
    let ports = loopback_ports(&case.engine()).unwrap();
    assert_eq!(ports.len(), cap + 1);
    assert!(ports.contains(&(20_000 + cap as u16 - 1)));
    assert!(!ports.contains(&(20_000 + cap as u16)));
    assert_eq!(
        code(ready(&case.db, &case.engine(), &format!("p{cap:03}"))),
        "app_plugin_refused"
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "p000")),
        "app_plugin_needs_review"
    );
}

fn near_on(port: u16) -> String {
    NEAR.replace("plugin = \"near\"", "plugin = \"handed\"")
        .replace("127.0.0.1:9000", &format!("127.0.0.1:{port}"))
}

/// A loopback plugin port inside the
/// ports the system hands out refuses that plugin alone, with a plain reason naming the
/// range; ports outside it, and hosts that are not loopback, stay usable.
#[test]
fn a_loopback_plugin_port_inside_the_handed_out_range_is_refused_alone_with_the_range_named() {
    let usual = 32_768..=60_999;
    let handed = |text: &str| {
        let found = crate::registry::user_found("handed".into(), Ok(text.as_bytes().to_vec()));
        crate::registry::outside_handed_out(found, Some(&usual)).parsed
    };
    for port in [32_768, 40_000, 60_999] {
        let problem = handed(&near_on(port)).expect_err("inside the range");
        assert_eq!(problem.code, "app_plugin_port_handed_out");
        assert_eq!(problem.field.as_deref(), Some("http.url"));
        assert_eq!(
            problem.message,
            format!(
                "http.url: port {port} is one this Linux hands out by itself (ports 32768 to \
                 60999), so an agent could listen on it; run the plugin on a port outside that \
                 range, or have an administrator move net.ipv4.ip_local_port_range away from \
                 port {port}."
            )
        );
    }
    for port in [9_000, 32_767, 61_000] {
        assert!(handed(&near_on(port)).is_ok(), "{port}");
    }
    let remote = ACME.replace("api.acme.example/v1", "api.acme.example:40000/v1");
    assert!(handed(&remote.replace("plugin = \"acme\"", "plugin = \"handed\"")).is_ok());
    let found = crate::registry::user_found("handed".into(), Ok(near_on(40_000).into_bytes()));
    assert!(
        crate::registry::outside_handed_out(found, None)
            .parsed
            .is_ok(),
        "no range read, no plugin refused"
    );
}

/// A plugin on a port the engine says the system hands out reads Refused with the
/// reason in the listing and the trust screen, never runs, and is left out of the ports a
/// launch keeps free; the other plugins keep their ports. The engine passes the range in.
#[test]
fn a_plugin_on_a_handed_out_port_is_refused_and_left_out_of_the_kept_ports() {
    let case = case();
    let range = 32_768..=60_999;
    let port = *range.start();
    let engine = Engine {
        handed_out: Some(range.clone()),
        ..case.engine()
    };
    drop_in(&case, "near", NEAR);
    drop_in(&case, "handed", &near_on(port));
    let listing = list(&case.db, &engine).unwrap();
    match &plugin(&listing, "handed").status {
        PluginStatus::Refused { problem } => {
            assert_eq!(problem.code, "app_plugin_port_handed_out");
            let named = format!("ports {} to {}", range.start(), range.end());
            assert!(problem.message.contains(&named), "{}", problem.message);
        }
        other => panic!("expected a refused plugin, got {other:?}"),
    }
    assert_eq!(plugin(&listing, "near").status, PluginStatus::NeedsReview);
    assert_eq!(loopback_ports(&engine).unwrap(), [9000, 11434]);
    assert_eq!(
        code(ready(&case.db, &engine, "handed")),
        "app_plugin_refused"
    );
    let other = picked(
        &case,
        "other",
        &near_on(port).replace("plugin = \"handed\"", "plugin = \"other\""),
    );
    match preview(&case.db, &engine, &other).unwrap() {
        Preview::Refused { problem } => assert_eq!(problem.code, "app_plugin_port_handed_out"),
        other => panic!("expected the trust screen to refuse, got {other:?}"),
    }
}

/// The handed-out range is read again when the owner confirms an add: a port the range
/// took between the trust screen and the add is refused with nothing written, and a plugin
/// on a port outside the range installs.
#[test]
fn an_add_rechecks_the_handed_out_range_before_writing() {
    let case = case();
    let range = 32_768..=60_999;
    let engine = Engine {
        handed_out: Some(range.clone()),
        ..case.engine()
    };
    let text = near_on(*range.start());
    let source = picked(&case, "handed", &text);
    assert!(matches!(
        preview(&case.db, &case.engine(), &source).unwrap(),
        Preview::Ready { .. }
    ));
    let trust_before = cells(&case.db, "app_plugin_trust");
    let state_before = cells(&case.db, "app_plugin_state");
    assert_eq!(
        code(add(&case.db, &engine, &source, &sha(&text), false, 1)),
        "app_plugin_port_handed_out"
    );
    assert!(!case.root.join(FOLDER).join("handed").exists());
    assert_eq!(cells(&case.db, "app_plugin_trust"), trust_before);
    assert_eq!(cells(&case.db, "app_plugin_state"), state_before);
    let allowed = near_on(9000);
    let source = picked(&case, "allowed", &allowed);
    add(&case.db, &engine, &source, &sha(&allowed), false, 2).unwrap();
    assert!(case.root.join(FOLDER).join("handed").exists());
}

/// With no handed-out range (macOS keeps every loopback plugin port free of agents in
/// its profile), a plugin on any port stays usable and listed for the sandbox.
#[test]
fn with_no_handed_out_range_a_plugin_on_any_port_stays_usable() {
    let case = case();
    drop_in(&case, "handed", &near_on(50_000));
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "handed").status, PluginStatus::NeedsReview);
    assert_eq!(loopback_ports(&case.engine()).unwrap(), [11434, 50000]);
    let other = picked(
        &case,
        "other",
        &near_on(50_000).replace("plugin = \"handed\"", "plugin = \"other\""),
    );
    assert!(matches!(
        preview(&case.db, &case.engine(), &other).unwrap(),
        Preview::Ready { .. }
    ));
}

/// A crash between a staged write and its rename leaves a `.replace-<id>` key
/// copy in a key folder, or an `.add-<id>` or `.old-<id>` folder in the plugins folder. The
/// start sweep deletes them and keeps every saved key and plugin.
#[test]
fn the_start_sweep_deletes_staged_leftovers_and_keeps_saved_keys_and_files() {
    let case = case();
    trusted(&case, ACME);
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-kept-key-00001".to_vec(),
    )
    .unwrap();
    let kept = plugin_keys(&case);
    assert_eq!(kept.len(), 1);
    let accounts = case.root.join("account-keys");
    std::fs::create_dir(&accounts).unwrap();
    let plugins = case.root.join(FOLDER);
    let leftovers = [
        case.root.join("plugin-keys").join(".replace-crashed"),
        accounts.join(".replace-crashed"),
    ];
    for leftover in &leftovers {
        std::fs::write(leftover, b"sk-leftover-copy").unwrap();
    }
    let staged = [plugins.join(".add-crashed"), plugins.join(".old-crashed")];
    for folder in &staged {
        std::fs::create_dir(folder).unwrap();
        std::fs::write(folder.join(MANIFEST_FILE), ACME).unwrap();
    }

    sweep_staged(&case.engine());

    for leftover in leftovers.iter().chain(&staged) {
        assert!(!leftover.exists(), "{}", leftover.display());
    }
    assert_eq!(plugin_keys(&case), kept);
    assert_eq!(fixture(&kept[0], std::fs::read), b"sk-kept-key-00001");
    assert!(plugins.join("acme").join(MANIFEST_FILE).is_file());
    let loaded = ready(&case.db, &case.engine(), "acme").unwrap();
    assert_eq!(loaded.bindings.len(), 1);
}

/// The trust row binds the SHA-256 of the text the owner reviewed.
#[test]
fn the_trust_row_binds_the_sha256_of_the_reviewed_text() {
    let case = case();
    let path = drop_in(&case, "near", NEAR);
    trust_file(&case.db, &case.engine(), "near", &sha(NEAR), 1).unwrap();
    let held = || -> String {
        case.db
            .query_row(
                "SELECT sha256 FROM app_plugin_trust WHERE name='near'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(held(), sha(NEAR));
    ready(&case.db, &case.engine(), "near").unwrap();
    let changed = NEAR.replace("dimensions = 128", "dimensions = 256");
    std::fs::write(&path, &changed).unwrap();
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_changed"
    );
    assert_eq!(
        code(set_enabled(&case.db, &case.engine(), "near", Switch::On)),
        "app_plugin_changed"
    );
    case.db
        .execute(
            "UPDATE app_plugin_trust SET sha256=?1 WHERE name='near'",
            [sha(&changed)],
        )
        .unwrap();
    ready(&case.db, &case.engine(), "near").unwrap();
    std::fs::write(&path, NEAR).unwrap();
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_changed"
    );
    assert_eq!(held(), sha(&changed));
}

#[path = "registry_key_tests.rs"]
mod keys;

#[path = "registry_switch_tests.rs"]
mod switch;

#[path = "registry_folder_tests.rs"]
mod folder;

/// A plugin whose folder cannot be moved aside is not removed: its rows, its key and its
/// folder all stay, so it is listed as it was, and the next removal goes through.
#[test]
fn a_removal_whose_folder_cannot_move_keeps_the_rows_and_the_key() {
    fn no_ref() -> Result<String, &'static str> {
        Err("app_plugin_folder_unavailable")
    }
    const NO_REF: Keys = Keys {
        new_ref: no_ref,
        ..KEYS
    };
    let case = case();
    add_text(&case, ACME, false, 1).unwrap();
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-test-key-0001".to_vec(),
    )
    .unwrap();
    let stuck = Engine {
        keys: &NO_REF,
        ..case.engine()
    };
    assert!(remove(&case.db, &stuck, "acme").is_err());
    assert_eq!(plugin_keys(&case).len(), 1);
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").status, PluginStatus::Ready);
    remove(&case.db, &case.engine(), "acme").unwrap();
    assert!(plugin_keys(&case).is_empty());
    assert!(!plugins_folder(&case).join("acme").exists());
}

/// A removal whose rows cannot be deleted puts the folder back under its name and keeps the
/// key, leaving no retired folder behind.
#[test]
fn a_removal_whose_rows_fail_puts_the_folder_back() {
    let case = case();
    add_text(&case, ACME, false, 1).unwrap();
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-test-key-0001".to_vec(),
    )
    .unwrap();
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER held BEFORE DELETE ON app_plugin_trust
             BEGIN SELECT RAISE(ABORT, 'held'); END;",
        )
        .unwrap();
    assert!(remove(&case.db, &case.engine(), "acme").is_err());
    let folder = plugins_folder(&case);
    let names = names_in(&folder);
    assert_eq!(names, ["acme"]);
    assert_eq!(plugin_keys(&case).len(), 1);
    case.db.execute_batch("DROP TRIGGER held").unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").status, PluginStatus::Ready);
}

/// A retired folder left beside the live plugin of its name, which cannot be deleted, loses
/// the plugin name on the start sweep, so a later removal never brings it back.
#[test]
fn a_retired_folder_beside_its_live_plugin_loses_its_name_on_the_sweep() {
    let case = case();
    add_text(&case, ACME, false, 1).unwrap();
    let folder = plugins_folder(&case);
    let retired = folder.join(format!(".old-acme.{}", "ab".repeat(32)));
    std::fs::create_dir_all(retired.join("a/b/c/d/e/f")).unwrap();
    std::fs::write(retired.join(MANIFEST_FILE), ACME).unwrap();
    let _ = crate::registry::remove_staged(&case.root);
    let left = names_in(&folder);
    assert!(
        !left.iter().any(|name| name.starts_with(".old-acme.")),
        "{left:?}"
    );
    remove(&case.db, &case.engine(), "acme").unwrap();
    let _ = crate::registry::remove_staged(&case.root);
    assert!(
        !folder.join("acme").exists(),
        "the old folder never comes back"
    );
}

/// A removal outside any capture is refused before anything is written: the folder stays
/// under its name with no removal folder beside it, and the rows read as before.
#[test]
fn a_removal_outside_any_capture_is_refused_and_leaves_the_folder_and_rows() {
    let mut case = case();
    add_text(&case, ACME, false, 1).unwrap();
    let before = plugin(&list(&case.db, &case.engine()).unwrap(), "acme")
        .status
        .clone();
    case.release();
    assert_eq!(
        code(remove(&case.db, &case.engine(), "acme")),
        "app_plugins_unavailable"
    );
    assert_eq!(names_in(&plugins_folder(&case)), ["acme"]);
    case.hold();
    let after = plugin(&list(&case.db, &case.engine()).unwrap(), "acme")
        .status
        .clone();
    assert_eq!(after, before);
}

/// An add outside any capture is refused before its folder is written, so no folder is
/// ever left with no trust record.
#[test]
fn an_add_outside_any_capture_is_refused_and_writes_no_folder() {
    let mut case = case();
    case.release();
    assert_eq!(
        code(add_text(&case, ACME, false, 1)),
        "app_plugins_unavailable"
    );
    assert!(names_in(&plugins_folder(&case)).is_empty());
    case.hold();
    assert!(
        list(&case.db, &case.engine())
            .unwrap()
            .plugins
            .iter()
            .all(|view| view.name != "acme")
    );
}

/// A key entered outside any capture is refused before the key is saved, so no key file is
/// ever left that no binding row names.
#[test]
fn a_key_set_outside_any_capture_is_refused_and_saves_no_key_file() {
    let mut case = case();
    add_text(&case, ACME, false, 1).unwrap();
    case.release();
    let refused = set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-test-key-0002".to_vec(),
    );
    assert_eq!(code(refused), "app_plugins_unavailable");
    assert!(plugin_keys(&case).is_empty());
    case.hold();
}
