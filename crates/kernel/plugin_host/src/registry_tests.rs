use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::OptionalExtension;

use crate::Refused;
use crate::registry::{
    Engine, FOLDER, MANIFEST_FILE, Switch, USER_PLUGINS_MAX, add, bundled, hold, list,
    loopback_ports, preview, ready, remove, review, set_enabled,
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
}

impl Case {
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
    Case { temp, root, db }
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

fn plugin_keys(case: &Case) -> Vec<PathBuf> {
    let keys = case.root.join("plugin-keys");
    let Ok(entries) = std::fs::read_dir(&keys) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries.map(|entry| entry.unwrap().path()).collect();
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
    assert_eq!(std::fs::read_to_string(&file).unwrap(), ACME);
    assert_eq!(
        (mode(&folder), mode(&plugin_folder), mode(&file)),
        (0o700, 0o700, 0o600)
    );
    assert_eq!(std::fs::read_dir(&plugin_folder).unwrap().count(), 1);
    let listing = list(&case.db, &case.engine()).unwrap();
    let acme = plugin(&listing, "acme");
    assert_eq!(
        (acme.origin, &acme.status, acme.enabled),
        (Origin::User, &PluginStatus::NeedsKey, true)
    );
    assert_eq!(acme.sha256, sha(ACME));
    assert_eq!(listing.plugins.last().unwrap().name, "acme");
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
        "app_plugin_name_taken"
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
        std::fs::read_dir(case.root.join(FOLDER)).unwrap().count(),
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
        std::fs::read(case.root.join(FOLDER).join("acme").join(MANIFEST_FILE)).unwrap(),
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
        std::fs::read_to_string(&legacy).unwrap(),
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
    assert_eq!(plugin(&listing, "near").status, PluginStatus::NeedsReview);
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

#[test]
fn bundled_plugins_turn_on_and_off_and_unknown_names_are_refused() {
    let case = case();
    set_enabled(&case.db, &case.engine(), "ollama", Switch::On).unwrap();
    set_enabled(&case.db, &case.engine(), "voyage", off()).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(plugin(&listing, "ollama").enabled);
    assert!(!plugin(&listing, "voyage").enabled);
    set_enabled(&case.db, &case.engine(), "ollama", off()).unwrap();
    assert!(!plugin(&list(&case.db, &case.engine()).unwrap(), "ollama").enabled);
    assert_eq!(
        code(set_enabled(&case.db, &case.engine(), "nope", Switch::On)),
        "app_plugin_not_found"
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "ollama")),
        "app_plugin_off"
    );
    assert_eq!(
        code(trust_file(
            &case.db,
            &case.engine(),
            "openai",
            &"0".repeat(64),
            1
        )),
        "app_plugin_bundled"
    );
}

#[test]
fn a_secret_binds_to_an_api_key_account_with_a_saved_key() {
    let case = case();
    trusted(&case, ACME);
    saved_account(&case, "acct-work", Some("key-acct-work"));
    saved_account(&case, "acct-nokey", Some("key-acct-nokey"));
    saved_account(&case, "acct-plain", None);
    let reference = KeyRef::new("key-acct-work").unwrap();
    test_replace(
        &case.root,
        KeyFolder::Account,
        &reference,
        b"sk-account-key-01".to_vec(),
    )
    .unwrap();
    let bind = |secret: &str, account: &str| {
        code(bind_account(
            &case.db,
            &case.engine(),
            "acme",
            secret,
            account,
        ))
    };
    assert_eq!(bind("api_key", "acct-gone"), "app_plugin_account_not_found");
    assert_eq!(bind("api_key", "../acct"), "app_plugin_account_not_found");
    assert_eq!(
        bind("api_key", "acct-nokey"),
        "app_plugin_account_has_no_key"
    );
    assert_eq!(
        bind("api_key", "acct-plain"),
        "app_plugin_account_has_no_key"
    );
    assert_eq!(bind("token", "acct-work"), "app_plugin_secret_not_declared");
    bind_account(&case.db, &case.engine(), "acme", "api_key", "acct-work").unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    let acme = plugin(&listing, "acme");
    assert_eq!(acme.status, PluginStatus::Ready);
    assert_eq!(
        acme.secrets[0].binding,
        Binding::Account {
            account: "acct-work".into(),
            name: "Work Acme".into()
        }
    );
    let loaded = ready(&case.db, &case.engine(), "acme").unwrap();
    assert_eq!(loaded.bindings, [Bound::Account(reference.clone())]);
    assert_eq!(loaded.bindings[0].folder(), KeyFolder::Account);
    let value = secret_value(&case.engine(), &loaded.bindings[0]).unwrap();
    assert_eq!(value.expose(), "sk-account-key-01");
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-own-plugin-01".to_vec(),
    )
    .unwrap();
    bind_account(&case.db, &case.engine(), "acme", "api_key", "acct-work").unwrap();
    assert!(plugin_keys(&case).is_empty());
}

/// A key reference is built only through the identity check, so a
/// stored or entered reference that is not one engine identity never becomes a binding.
#[test]
fn a_bad_reference_is_refused_at_construction() {
    let key = format!("plugin-key-{}", "a".repeat(64));
    assert_eq!(KeyRef::new(&key).unwrap().as_str(), key);
    for bad in ["", "..", "../keys", "a/b", "a b", "nul", &"x".repeat(129)] {
        assert!(KeyRef::new(bad).is_none(), "{bad:?}");
    }
    let case = case();
    trusted(&case, ACME);
    case.db
        .execute(
            "INSERT INTO app_plugin_secrets (plugin, secret, source, reference)
             VALUES ('acme', 'api_key', 'key', 'plugin-key-../x')",
            [],
        )
        .unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
    assert_eq!(
        code(ready(&case.db, &case.engine(), "acme")),
        "app_plugin_needs_key"
    );
}

#[test]
fn a_replaced_plugin_with_new_hosts_drops_its_bound_keys() {
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
    let same_hosts = ACME.replace("release = \"1.0.0\"", "release = \"1.0.1\"");
    add_text(&case, &same_hosts, true, 2).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::Key);
    let moved = same_hosts
        .replace(
            "https://api.acme.example/v1/embed",
            "https://other.example/v1/embed",
        )
        .replace("[\"api.acme.example\"]", "[\"other.example\"]");
    add_text(&case, &moved, true, 3).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
    assert!(plugin_keys(&case).is_empty());
    let path = case.root.join(FOLDER).join("acme").join(MANIFEST_FILE);
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-test-key-0002".to_vec(),
    )
    .unwrap();
    std::fs::write(&path, ACME).unwrap();
    trust_file(&case.db, &case.engine(), "acme", &sha(ACME), 4).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
}

#[test]
fn a_key_entered_for_a_plugin_is_a_private_file_replaced_in_place_and_deleted_on_unbind() {
    let case = case();
    trusted(&case, ACME);
    add_text(&case, NEAR, false, 2).unwrap();
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"pa-first-key-0001".to_vec(),
    )
    .unwrap();
    let files = plugin_keys(&case);
    assert_eq!(files.len(), 1);
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"pa-second-key-002".to_vec(),
    )
    .unwrap();
    assert_eq!(plugin_keys(&case), files);
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"pa-second-key-002");
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::Key);
    assert_eq!(plugin(&listing, "acme").status, PluginStatus::Ready);
    for (name, secret) in [("acme", "other"), ("near", "api_key")] {
        assert_eq!(
            code(set_key(
                &case.db,
                &case.engine(),
                name,
                secret,
                b"pa-other-key-0001".to_vec()
            )),
            "app_plugin_secret_not_declared"
        );
    }
    assert_eq!(
        code(set_key(
            &case.db,
            &case.engine(),
            "acme",
            "api_key",
            b"two words".to_vec()
        )),
        "app_plugin_key_invalid"
    );
    unbind(&case.db, &case.engine(), "acme", "api_key").unwrap();
    assert!(plugin_keys(&case).is_empty());
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").status, PluginStatus::NeedsKey);
}

#[test]
fn a_ready_plugin_loads_with_its_manifest_and_a_needs_key_or_off_one_is_refused() {
    let case = case();
    trusted(&case, NEAR);
    add_text(&case, ACME, false, 2).unwrap();
    set_enabled(&case.db, &case.engine(), "ollama", Switch::On).unwrap();
    let ollama = ready(&case.db, &case.engine(), "ollama").unwrap();
    assert_eq!(
        (ollama.manifest.name.as_str(), ollama.origin),
        ("ollama", Origin::Bundled)
    );
    assert!(ollama.bindings.is_empty());
    let near = ready(&case.db, &case.engine(), "near").unwrap();
    assert_eq!(
        (near.manifest.name.as_str(), near.origin),
        ("near", Origin::User)
    );
    assert!(near.bindings.is_empty());
    assert_eq!(
        code(ready(&case.db, &case.engine(), "acme")),
        "app_plugin_needs_key"
    );
    set_enabled(&case.db, &case.engine(), "near", off()).unwrap();
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_off"
    );
    assert_eq!(
        code(set_enabled(&case.db, &case.engine(), "nope", Switch::On)),
        "app_plugin_not_found"
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "missing")),
        "app_plugin_not_found"
    );
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
    assert_eq!(std::fs::read(&kept[0]).unwrap(), b"sk-kept-key-00001");
    assert!(plugins.join("acme").join(MANIFEST_FILE).is_file());
    let loaded = ready(&case.db, &case.engine(), "acme").unwrap();
    assert_eq!(loaded.bindings.len(), 1);
}

/// A key the owner entered for a plugin is never readable through the plugin
/// state read. The listing, its JSON, the three store tables and the loaded plugin hold no
/// key byte; the key comes back only through `secret_value`.
#[test]
fn a_saved_key_is_never_readable_through_the_plugin_state_read() {
    let case = case();
    trusted(&case, ACME);
    let key = "sk-never-in-state-0001";
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        key.as_bytes().to_vec(),
    )
    .unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::Key);
    assert!(!format!("{listing:?}").contains(key));
    assert!(!serde_json::to_string(&listing).unwrap().contains(key));
    for table in ["app_plugin_trust", "app_plugin_state", "app_plugin_secrets"] {
        let held = cells(&case.db, table);
        assert!(!held.is_empty(), "{table}");
        assert!(held.iter().all(|cell| !cell.contains(key)), "{table}");
    }
    let loaded = ready(&case.db, &case.engine(), "acme").unwrap();
    assert!(!format!("{loaded:?}").contains(key));
    let value = secret_value(&case.engine(), &loaded.bindings[0]).unwrap();
    assert_eq!(value.expose(), key);
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

/// Turning a plugin off writes the off state first: the next call is refused with
/// `app_plugin_off`, while a run already holding the plugin keeps it, is not stopped under
/// the default setting, and stays listed until it ends.
#[test]
fn turning_off_refuses_the_next_call_and_lets_a_running_one_finish() {
    let case = case();
    trusted(&case, NEAR);
    let usage = Usage::default();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (loaded, lease) = hold(
        &case.db,
        &case.engine(),
        &usage,
        "near",
        holder("r1"),
        counted(&calls),
    )
    .unwrap();
    assert_eq!(loaded.manifest.name, "near");
    set_enabled(
        &case.db,
        &case.engine(),
        "near",
        Switch::Off {
            usage: &usage,
            when_off: TurnOff::RefuseNewCalls,
        },
    )
    .unwrap();
    assert_eq!(
        code(hold(
            &case.db,
            &case.engine(),
            &usage,
            "near",
            holder("r2"),
            counted(&calls)
        )),
        "app_plugin_off"
    );
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_off"
    );
    assert_eq!(usage.users("near"), [holder("r1")]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(lease);
    assert!(usage.users("near").is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// With `stop_runs`, turning a plugin off stops each run holding it once, after the off
/// state is written; turning it off again stops none, and other plugins' runs go on.
#[test]
fn stop_runs_calls_each_holders_stop_once() {
    let case = case();
    trusted(&case, NEAR);
    add_text(
        &case,
        &NEAR.replace("plugin = \"near\"", "plugin = \"far\""),
        false,
        2,
    )
    .unwrap();
    let usage = Usage::default();
    let near_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let far_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let held = |name: &str, run: &str, calls: &std::sync::Arc<std::sync::atomic::AtomicUsize>| {
        hold(
            &case.db,
            &case.engine(),
            &usage,
            name,
            holder(run),
            counted(calls),
        )
        .unwrap()
        .1
    };
    let _first = held("near", "r1", &near_calls);
    let _second = held("near", "r2", &near_calls);
    let _far = held("far", "r3", &far_calls);
    let stop_runs = Switch::Off {
        usage: &usage,
        when_off: TurnOff::StopRuns,
    };
    set_enabled(&case.db, &case.engine(), "near", stop_runs).unwrap();
    assert_eq!(near_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        code(ready(&case.db, &case.engine(), "near")),
        "app_plugin_off"
    );
    set_enabled(&case.db, &case.engine(), "near", stop_runs).unwrap();
    assert_eq!(near_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(far_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(usage.users("far"), [holder("r3")]);
}

/// A trusted plugin whose plugin.toml changed is refused with `app_plugin_changed` from the
/// next call, and that change gives one notice naming the trusted and the current SHA-256:
/// a second call for the same change gives none, a further change gives a new one, and
/// approving it runs the plugin again.
#[test]
fn a_changed_plugin_notifies_once_per_change() {
    let case = case();
    trusted(&case, NEAR);
    let usage = Usage::default();
    let call = || {
        hold(
            &case.db,
            &case.engine(),
            &usage,
            "near",
            holder("r1"),
            Box::new(|| {}),
        )
    };
    drop(call().unwrap());
    assert!(usage.take_notices().is_empty());
    let path = case.root.join(FOLDER).join("near").join(MANIFEST_FILE);
    let first = NEAR.replace("dimensions = 128", "dimensions = 256");
    std::fs::write(&path, &first).unwrap();
    assert_eq!(code(call()), "app_plugin_changed");
    assert_eq!(code(call()), "app_plugin_changed");
    let notice = |current: &str| ChangeNotice {
        plugin: "near".to_owned(),
        trusted_sha256: sha(NEAR),
        current_sha256: sha(current),
    };
    assert_eq!(usage.take_notices(), [notice(&first)]);
    assert_eq!(code(call()), "app_plugin_changed");
    assert!(usage.take_notices().is_empty());
    let second = NEAR.replace("dimensions = 128", "dimensions = 512");
    std::fs::write(&path, &second).unwrap();
    assert_eq!(code(call()), "app_plugin_changed");
    assert_eq!(usage.take_notices(), [notice(&second)]);
    trust_file(&case.db, &case.engine(), "near", &sha(&second), 3).unwrap();
    drop(call().unwrap());
    assert!(usage.take_notices().is_empty());
    assert!(usage.users("near").is_empty());
}
