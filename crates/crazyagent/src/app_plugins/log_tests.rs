use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
use kernel_plugin_host::registry::{Switch, off_code_plugins};
use kernel_plugin_host::store::{KeyFolder, KeyRef};
use kernel_plugin_host::usage::{TurnOff, Usage};
use kernel_storage::state_log::replay::rows;
use rusqlite::Connection;
use rusqlite::types::Value;

use super::log::rebuild;
use super::{Command, KEYS, OffSet, engine, execute, initialize};
use crate::config::{EffectiveConfig, SETTINGS_VERSION, resolve_config_bytes};
use crate::state_log::fence::seed;

/// The agent-quit plugin's id, a bundled code plugin.
const QUIT: &str = BUNDLED_GRANTS[0].plugin;

/// A user plugin that declares one secret.
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

/// A user plugin that declares no secret.
const NEAR: &str = r#"plugin = "near"
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

/// The `state` row at revision 0 and the `events` table a store holds before any write.
const STATE: &str =
    "CREATE TABLE state (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
    CREATE TABLE events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
    INSERT INTO state VALUES (1,0);";

/// The plugin store tables, in the order the rows compare them.
const TABLES: [&str; 3] = ["app_plugin_trust", "app_plugin_state", "app_plugin_secrets"];

/// An engine root holding a file store with the plugin store tables, and the owner's
/// settings.
struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    db: Connection,
    config: EffectiveConfig,
}

/// A private engine root and its store, the test host started when `hosted`. Input:
/// whether an engine host runs. Output: the case.
fn case(hosted: bool) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = Connection::open(root.join("engine.db")).unwrap();
    db.execute_batch(STATE).unwrap();
    if hosted {
        seed(&db);
    }
    initialize(&db).unwrap();
    let settings = format!("version = {SETTINGS_VERSION}\n");
    let config = resolve_config_bytes(&settings, "").unwrap();
    Case {
        _temp: temp,
        root,
        db,
        config,
    }
}

/// A switch request. Inputs: the plugin and whether on. Output: the command.
fn switch(name: &str, enabled: bool) -> Command {
    Command::SetEnabled {
        name: name.into(),
        enabled,
        when_off: None,
    }
}

/// An entered key for acme's one secret. Input: the key bytes. Output: the command.
fn set_key(key: &[u8]) -> Command {
    Command::SetKey {
        name: "acme".into(),
        secret: "api_key".into(),
        key: key.to_vec(),
    }
}

/// Unbinds acme's one secret. Output: the command.
fn unbind() -> Command {
    Command::Unbind {
        name: "acme".into(),
        secret: "api_key".into(),
    }
}

/// Whether `bytes` holds `needle`. Inputs: both. Output: the answer.
fn holds(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

impl Case {
    /// Turns agent-quit off with the essential lock lifted, through an off set of this
    /// case's own, so the engine's shared set other rows read never shows it off.
    fn code_off(&self) {
        let usage = Usage::default();
        let off = Switch::Off {
            usage: &usage,
            when_off: TurnOff::RefuseNewCalls,
            essential_unlocked: true,
        };
        OffSet::new()
            .switch(&self.db, &engine(&self.root), QUIT, off)
            .unwrap();
    }

    /// Runs one App plugins request. Input: the command. Output: Ok, or the refusal code.
    fn run(&self, command: Command) -> Result<(), String> {
        execute(&self.db, &self.config, command)
            .map(drop)
            .map_err(|error| error.0)
    }

    /// Adds a user plugin from a picked folder holding the text. Inputs: the folder name,
    /// the text and whether it replaces one. Output: Ok, or the refusal code.
    fn add(&self, picked: &str, text: &str, replace: bool) -> Result<(), String> {
        let folder = self.root.join("picked").join(picked);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("plugin.toml"), text).unwrap();
        self.run(Command::Add {
            folder,
            sha256: kernel_common::hash(text.as_bytes()),
            replace,
        })
    }

    /// The plugin store tables, each ordered by all its columns. Output: their rows.
    fn tables(&self) -> Vec<Vec<Vec<Value>>> {
        TABLES
            .iter()
            .map(|table| rows(&self.db, table).unwrap())
            .collect()
    }

    /// Rebuilds the plugin store tables from the log and commits.
    fn rebuilt(&mut self) {
        let tx = self.db.transaction().unwrap();
        rebuild(&tx).unwrap();
        tx.commit().unwrap();
    }

    /// The `state` revision. Output: it.
    fn revision(&self) -> i64 {
        self.db
            .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Every logged plugin store event, as kind and body, in seq order; none while the
    /// store has no log. Output: the pairs.
    fn logged(&self) -> Vec<(String, Vec<u8>)> {
        let Ok(mut statement) = self
            .db
            .prepare("SELECT kind, body FROM state_log WHERE aggregate='plugins' ORDER BY seq")
        else {
            return Vec::new();
        };
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .filter(|(kind, _): &(String, Vec<u8>)| {
                kernel_plugin_host::store_log::KINDS.contains(&kind.as_str())
            })
            .collect()
    }

    /// The kinds of the logged plugin store events whose body names `plugin` as a JSON
    /// string. Input: the name. Output: the kinds in seq order.
    fn naming(&self, plugin: &str) -> Vec<String> {
        let quoted = format!("\"{plugin}\"");
        self.logged()
            .into_iter()
            .filter(|(_, body)| holds(body, quoted.as_bytes()))
            .map(|(kind, _)| kind)
            .collect()
    }

    /// The file of acme's bound entered key. Output: its path, none when no key is bound.
    fn key_file(&self) -> Option<PathBuf> {
        let reference: Option<String> = self
            .db
            .query_row(
                "SELECT reference FROM app_plugin_secrets
                 WHERE plugin='acme' AND secret='api_key' AND source='key'",
                [],
                |row| row.get(0),
            )
            .ok();
        reference.map(|reference| {
            self.root
                .join(crate::storage::accounts::PLUGIN_KEYS)
                .join(reference)
        })
    }
}

/// Every App plugins change leaves the three plugin store tables as replay of the
/// logged plugin store events rebuilds them.
#[test]
fn replaying_the_plugin_events_rebuilds_the_three_tables_equal_to_live() {
    let mut case = case(true);
    case.add("acme", ACME, false).unwrap();
    case.add("near", NEAR, false).unwrap();
    case.run(switch("near", false)).unwrap();
    case.run(switch("acme", false)).unwrap();
    case.run(switch("acme", true)).unwrap();
    case.code_off();
    case.run(set_key(b"sk-row-three-key-01")).unwrap();
    case.run(unbind()).unwrap();
    case.run(set_key(b"sk-row-three-key-02")).unwrap();
    let changed = ACME.replace("title = \"Acme embeddings\"", "title = \"Acme two\"");
    case.add("acme-two", &changed, true).unwrap();

    let kinds: Vec<String> = case.logged().into_iter().map(|(kind, _)| kind).collect();
    for kind in kernel_plugin_host::store_log::KINDS {
        if kind != "AppPluginForgotten" {
            assert!(kinds.iter().any(|logged| logged == kind), "{kind} logged");
        }
    }
    let live = case.tables();
    assert!(live.iter().all(|table| !table.is_empty()));
    case.rebuilt();
    assert_eq!(case.tables(), live, "replay equals the live tables");
}

/// No logged body holds an entered key's bytes, while the key itself reads back from its
/// key file.
#[test]
fn no_event_body_carries_a_secret_value() {
    let case = case(true);
    let marker = format!("sk-test-{}", "5e".repeat(32));
    case.add("acme", ACME, false).unwrap();
    case.run(set_key(marker.as_bytes())).unwrap();
    case.run(unbind()).unwrap();
    case.run(set_key(marker.as_bytes())).unwrap();
    case.run(switch("acme", false)).unwrap();

    let logged = case.logged();
    assert!(
        logged.iter().any(|(kind, _)| kind == "AppPluginBound"),
        "the binding is logged"
    );
    let bodies: Vec<Vec<u8>> = case
        .db
        .prepare("SELECT body FROM state_log")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for body in &bodies {
        assert!(!holds(body, marker.as_bytes()), "a body holds the key");
    }
    let file = case.key_file().unwrap();
    let reference = KeyRef::new(file.file_name().unwrap().to_str().unwrap()).unwrap();
    let read = (KEYS.read)(&case.root, KeyFolder::Plugin, &reference)
        .unwrap()
        .unwrap();
    assert_eq!(read.expose(), marker, "the key reads back from its file");
}

/// A change refused at the append by the store fence leaves neither a plugin store row
/// nor an event, and a switch to the state a plugin already has moves no revision.
#[test]
fn a_refused_append_leaves_neither_the_event_nor_the_row() {
    let unhosted = case(false);
    assert_eq!(
        unhosted.add("acme", NEAR, false),
        Err("owner_runtime_stale".to_owned())
    );
    assert!(unhosted.tables().iter().all(Vec::is_empty));
    assert!(unhosted.logged().is_empty());
    assert_eq!(unhosted.revision(), 0);

    let hosted = case(true);
    hosted.add("near", NEAR, false).unwrap();
    let revision = hosted.revision();
    assert!(revision > 0, "the add moved the revision");
    let logged = hosted.logged().len();
    hosted.run(switch("near", true)).unwrap();
    assert_eq!(hosted.revision(), revision);
    assert_eq!(hosted.logged().len(), logged);
}

/// A bundled code plugin turned off with the essential lock lifted is still off after the
/// tables are rebuilt from the log, kept under its code key with no bare row.
#[test]
fn an_off_code_plugin_survives_replay() {
    let mut case = case(true);
    case.code_off();
    case.rebuilt();
    let off = off_code_plugins(&case.db).unwrap();
    assert_eq!(off.into_iter().collect::<Vec<_>>(), [QUIT.to_owned()]);
    let bare: i64 = case
        .db
        .query_row(
            "SELECT COUNT(*) FROM app_plugin_state WHERE name=?1",
            [QUIT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bare, 0);
}

/// Removing a plugin, and the start pass's drop of an older plugin of the owner's under a
/// bundled plugin's name, leave only the removal event naming it; another plugin's events
/// and a code plugin's switch stay, and the rebuilt tables still equal the live ones.
#[test]
fn removing_a_plugin_redacts_its_earlier_events() {
    let mut case = case(true);
    case.run(switch("openai", false)).unwrap();
    case.add("acme", ACME, false).unwrap();
    case.add("near", NEAR, false).unwrap();
    case.run(set_key(b"sk-row-seven-key-01")).unwrap();
    case.code_off();
    case.db
        .execute(
            "INSERT INTO app_plugin_trust (name, sha256, hosts, trusted_ms) VALUES ('openai', ?1, '', 1)",
            ["0".repeat(64)],
        )
        .unwrap();

    case.run(Command::List).unwrap();
    case.run(Command::Remove {
        name: "acme".into(),
    })
    .unwrap();

    assert_eq!(case.naming("acme"), ["AppPluginForgotten"]);
    assert_eq!(case.naming("openai"), ["AppPluginForgotten"]);
    assert!(
        case.naming("near")
            .iter()
            .any(|kind| kind == "AppPluginTrusted"),
        "another plugin's events stay"
    );
    assert_eq!(case.naming(QUIT), ["AppPluginCodeSwitched"]);
    let live = case.tables();
    case.rebuilt();
    assert_eq!(case.tables(), live);
}

/// An entered key's file is deleted once the change that lets it go has committed: an
/// unbind, a re-approval whose hosts changed, and the plugin's removal; a key entered again
/// for a bound secret keeps its file.
#[test]
fn a_dropped_key_file_is_deleted_after_the_commit() {
    let case = case(true);
    case.add("acme", ACME, false).unwrap();
    case.run(set_key(b"sk-row-eight-key-01")).unwrap();
    let first = case.key_file().unwrap();
    assert!(first.exists());
    case.run(unbind()).unwrap();
    assert!(!first.exists(), "an unbind deletes the key file");

    case.run(set_key(b"sk-row-eight-key-02")).unwrap();
    let second = case.key_file().unwrap();
    case.run(set_key(b"sk-row-eight-key-03")).unwrap();
    assert_eq!(case.key_file().unwrap(), second);
    assert!(second.exists(), "a key entered again keeps its file");
    let moved = ACME.replace(
        "hosts = [\"api.acme.example\"]",
        "hosts = [\"api.acme.example\", \"eu.acme.example\"]",
    );
    assert_ne!(moved, ACME);
    case.add("acme-moved", &moved, true).unwrap();
    assert!(case.key_file().is_none());
    assert!(!second.exists(), "new hosts drop the key and its file");

    case.run(set_key(b"sk-row-eight-key-04")).unwrap();
    let third = case.key_file().unwrap();
    case.run(Command::Remove {
        name: "acme".into(),
    })
    .unwrap();
    assert!(!third.exists(), "a removal deletes the key file");
}

/// The plugin host modules that write the plugin store, or reach a write through their
/// start pass, as engine code names them.
const STORE_WRITERS: [&str; 4] = [
    "kernel_plugin_host::registry",
    "kernel_plugin_host::trust",
    "kernel_plugin_host::store",
    "grants::list",
];

/// Every Rust source under a folder, with its path. Input: the folder. Output: the files.
fn sources(folder: &std::path::Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let code = std::fs::read_to_string(&path).unwrap();
            found.push((path, code));
        }
    }
    found
}

/// Only App plugins code reaches the plugin store's writers, and all of it runs inside the
/// logged transaction, so no engine path writes a store row the log misses.
#[test]
fn only_app_plugins_code_reaches_the_plugin_store_writers() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let owned = src.join("app_plugins");
    let files = sources(&src);
    assert!(files.len() > 100, "the engine sources are found");
    let reaching: Vec<&PathBuf> = files
        .iter()
        .filter(|(_, code)| STORE_WRITERS.iter().any(|writer| code.contains(writer)))
        .map(|(path, _)| path)
        .collect();
    assert!(
        reaching.contains(&&src.join("app_plugins.rs")),
        "the scan sees the App plugins module"
    );
    let outside: Vec<&&PathBuf> = reaching
        .iter()
        .filter(|path| **path != &src.join("app_plugins.rs") && !path.starts_with(&owned))
        .collect();
    assert!(outside.is_empty(), "store writers reached from {outside:?}");
}
