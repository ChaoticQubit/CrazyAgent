use super::*;
use crate::accounts::{AccountDetails, Provider};
use crate::app_plugins::{Binding, Origin, PluginStatus, Preview};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use std::os::unix::fs::PermissionsExt;

const ACME: &str = "schema: 1
kind: embedding
name: acme
title: Acme embeddings
version: 1.0.0
http:
  url: https://api.acme.example/v1/embed
  hosts: [api.acme.example]
  secrets: [api_key]
  headers:
    Authorization: \"Bearer {{secret.api_key}}\"
  body:
    input: \"{{inputs}}\"
  vectors: data.*.embedding
  dimensions: 256
";

const NEAR: &str = "schema: 1
kind: embedding
name: near
title: Near
version: 1
http:
  url: http://127.0.0.1:9000/embed
  hosts: [127.0.0.1]
  body:
    input: \"{{inputs}}\"
  vectors: embeddings.*
  dimensions: 128
";

struct Case {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    db: rusqlite::Connection,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    Case {
        _temp: temp,
        root,
        db,
    }
}

fn sha(text: &str) -> String {
    crate::app_plugins::sha256_hex(text.as_bytes())
}

fn plugin<'a>(
    listing: &'a crate::app_plugins::Listing,
    name: &str,
) -> &'a crate::app_plugins::PluginView {
    listing
        .plugins
        .iter()
        .find(|plugin| plugin.name == name)
        .unwrap_or_else(|| panic!("no plugin {name}"))
}

fn drop_in(case: &Case, file: &str, text: &str) -> std::path::PathBuf {
    let folder = case.root.join(crate::app_plugins::FOLDER);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = folder.join(file);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn code<T: std::fmt::Debug>(result: Result<T, EngineError>) -> String {
    result.expect_err("refused").0
}

fn trusted(case: &Case, text: &str) {
    add(&case.db, &case.root, text, &sha(text), false, 1).unwrap();
}

fn plugin_keys(case: &Case) -> Vec<std::path::PathBuf> {
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    std::fs::read_dir(&keys)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect()
}

fn saved_account(case: &Case, id: &str, auth: ModelAuthentication) {
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    let connection = ModelConnection {
        id: reference(id),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference(id),
        endpoint: ModelEndpoint::Http {
            url: "https://api.acme.example/v1".into(),
        },
        model: ModelReference {
            id: "m".into(),
            author: reference("openai-compatible"),
        },
        auth,
    };
    let details = AccountDetails {
        name: "Work Acme".into(),
        provider: Provider::OpenaiCompatible,
    };
    let body =
        serde_json::to_vec(&serde_json::json!({"connection": connection, "details": details}))
            .unwrap();
    case.db
        .execute(
            "INSERT INTO accounts(id, body) VALUES (?1, ?2)",
            rusqlite::params![id, body],
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

#[test]
fn bundled_plugins_list_first_with_only_the_local_one_on() {
    let case = case();
    let listing = list(&case.db, &case.root).unwrap();
    let names: Vec<&str> = listing
        .plugins
        .iter()
        .map(|plugin| plugin.name.as_str())
        .collect();
    assert_eq!(names, ["local", "openai", "voyage", "ollama"]);
    assert!(!listing.more);
    let local = plugin(&listing, "local");
    assert_eq!(
        (local.origin, &local.status, local.enabled),
        (Origin::Bundled, &PluginStatus::Ready, true)
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
    assert!(!case.root.join(crate::app_plugins::FOLDER).exists());
}

/// H4: what an older plugin of the owner's left under a name a bundled plugin now has is
/// dropped the next time the store is used: its binding with the key entered for it, its
/// switch and its trust record. The bundled plugin then lists as shipped.
#[test]
fn leftovers_of_an_older_plugin_under_a_bundled_name_are_dropped() {
    let case = case();
    let reference = CredentialRef::new("plugin-key-leftover").unwrap();
    crate::storage::accounts::replace_key_in(
        &case.root,
        crate::storage::accounts::PLUGIN_KEYS,
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
    let listing = list(&case.db, &case.root).unwrap();
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
    let Preview::Ready { trust } = preview(&case.root, ACME).unwrap() else {
        panic!("ready");
    };
    assert_eq!(trust.name, "acme");
    assert_eq!(trust.sha256, sha(ACME));
    assert_eq!(trust.secrets, ["api_key"]);
    assert!(!trust.replaces);
    assert!(!case.root.join(crate::app_plugins::FOLDER).exists());
    let Preview::Refused { problem } = preview(&case.root, "a: 1\n\tb: 2\n").unwrap() else {
        panic!("refused");
    };
    assert_eq!(
        (problem.code.as_str(), problem.line),
        ("app_plugin_yaml_tab", Some(2))
    );
    let bundled_name = ACME.replace("name: acme", "name: local");
    let Preview::Refused { problem } = preview(&case.root, &bundled_name).unwrap() else {
        panic!("refused");
    };
    assert_eq!(problem.code, "app_plugin_name_taken");
}

#[test]
fn add_saves_a_private_file_trusts_it_and_turns_it_on() {
    let case = case();
    add(&case.db, &case.root, ACME, &sha(ACME), false, 1_000).unwrap();
    let folder = case.root.join(crate::app_plugins::FOLDER);
    let file = folder.join("acme.yaml");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), ACME);
    assert_eq!((mode(&folder), mode(&file)), (0o700, 0o600));
    let listing = list(&case.db, &case.root).unwrap();
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
    assert_eq!(
        code(add(&case.db, &case.root, ACME, &sha(NEAR), false, 1)),
        "app_plugin_changed"
    );
    add(&case.db, &case.root, ACME, &sha(ACME), false, 1).unwrap();
    assert_eq!(
        code(add(&case.db, &case.root, ACME, &sha(ACME), false, 2)),
        "app_plugin_name_taken"
    );
    let newer = ACME.replace("version: 1.0.0", "version: 1.1.0");
    let Preview::Ready { trust } = preview(&case.root, &newer).unwrap() else {
        panic!("ready");
    };
    assert!(trust.replaces);
    add(&case.db, &case.root, &newer, &sha(&newer), true, 3).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").version, "1.1.0");
    let bundled = ACME.replace("name: acme", "name: openai");
    assert_eq!(
        code(add(&case.db, &case.root, &bundled, &sha(&bundled), true, 4)),
        "app_plugin_name_taken"
    );
    assert_eq!(
        code(add(
            &case.db,
            &case.root,
            "a: 1\n\tb\n",
            &sha("a: 1\n\tb\n"),
            false,
            5
        )),
        "app_plugin_refused"
    );
}

#[test]
fn a_dropped_in_file_needs_review_until_trusted_and_again_after_a_change() {
    let case = case();
    let path = drop_in(&case, "near.yaml", NEAR);
    let listing = list(&case.db, &case.root).unwrap();
    let near = plugin(&listing, "near");
    assert_eq!(
        (&near.status, near.enabled),
        (&PluginStatus::NeedsReview, false)
    );
    assert_eq!(
        code(set_enabled(&case.db, &case.root, "near", true)),
        "app_plugin_needs_review"
    );
    let Preview::Ready { trust } = review(&case.root, "near").unwrap() else {
        panic!("ready");
    };
    assert_eq!(trust.sha256, sha(NEAR));
    assert_eq!(
        code(trust_file(&case.db, &case.root, "near", &sha(ACME), 1)),
        "app_plugin_changed"
    );
    trust_file(&case.db, &case.root, "near", &trust.sha256, 1).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(
        (
            &plugin(&listing, "near").status,
            plugin(&listing, "near").enabled
        ),
        (&PluginStatus::Ready, true)
    );
    std::fs::write(&path, NEAR.replace("dimensions: 128", "dimensions: 256")).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "near").status, PluginStatus::NeedsReview);
    assert_eq!(
        code(ready(&case.db, &case.root, "near")),
        "app_plugin_needs_review"
    );
}

#[test]
fn refused_and_unsafe_files_are_listed_with_their_problem_and_other_files_ignored() {
    let case = case();
    drop_in(&case, "bad.yaml", "a: 1\n\tb: 2\n");
    drop_in(&case, "other.yaml", NEAR);
    drop_in(
        &case,
        "local.yaml",
        &NEAR.replace("name: near", "name: local"),
    );
    drop_in(&case, "notes.txt", "x");
    drop_in(&case, ".hidden.yaml", NEAR);
    let folder = case.root.join(crate::app_plugins::FOLDER);
    std::os::unix::fs::symlink(folder.join("other.yaml"), folder.join("linked.yaml")).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    let refused = |name: &str| match &plugin(&listing, name).status {
        PluginStatus::Refused { problem } => problem.code.clone(),
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(refused("bad"), "app_plugin_yaml_tab");
    assert_eq!(refused("other"), "app_plugin_file_name_mismatch");
    assert_eq!(refused("linked"), "app_plugin_file_unsafe");
    let users: Vec<&str> = listing
        .plugins
        .iter()
        .filter(|plugin| plugin.origin == Origin::User)
        .map(|plugin| plugin.name.as_str())
        .collect();
    assert_eq!(users, ["bad", "linked", "local", "other"]);
    let user_local = listing
        .plugins
        .iter()
        .find(|plugin| plugin.origin == Origin::User && plugin.name == "local")
        .unwrap();
    assert!(matches!(
        &user_local.status,
        PluginStatus::Refused { problem } if problem.code == "app_plugin_name_taken"
    ));
    assert!(user_local.runs.is_none());
    assert!(plugin(&listing, "bad").runs.is_none());
}

#[test]
fn an_unsafe_folder_is_refused() {
    let case = case();
    drop_in(&case, "near.yaml", NEAR);
    let folder = case.root.join(crate::app_plugins::FOLDER);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code(list(&case.db, &case.root)), "app_plugin_folder_unsafe");
    assert_eq!(
        code(add(&case.db, &case.root, ACME, &sha(ACME), false, 1)),
        "app_plugin_folder_unsafe"
    );
}

#[test]
fn the_folder_lists_at_most_its_cap_and_says_there_are_more() {
    let case = case();
    for index in 0..=crate::app_plugins::USER_PLUGINS_MAX {
        let name = format!("p{index:03}");
        drop_in(
            &case,
            &format!("{name}.yaml"),
            &NEAR.replace("name: near", &format!("name: {name}")),
        );
    }
    let listing = list(&case.db, &case.root).unwrap();
    let users = listing
        .plugins
        .iter()
        .filter(|plugin| plugin.origin == Origin::User)
        .count();
    assert_eq!(users, crate::app_plugins::USER_PLUGINS_MAX);
    assert!(listing.more);
}

#[test]
fn remove_deletes_a_user_plugin_with_its_keys_and_refuses_bundled_and_unknown_ones() {
    let case = case();
    add(&case.db, &case.root, ACME, &sha(ACME), false, 1).unwrap();
    set_key(
        &case.db,
        &case.root,
        "acme",
        "api_key",
        b"sk-test-key-0001".to_vec(),
    )
    .unwrap();
    remove(&case.db, &case.root, "acme").unwrap();
    assert!(
        !case
            .root
            .join(crate::app_plugins::FOLDER)
            .join("acme.yaml")
            .exists()
    );
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);
    let listing = list(&case.db, &case.root).unwrap();
    assert!(listing.plugins.iter().all(|plugin| plugin.name != "acme"));
    assert_eq!(
        code(remove(&case.db, &case.root, "openai")),
        "app_plugin_bundled"
    );
    let shadowed = drop_in(
        &case,
        "local.yaml",
        &NEAR.replace("name: near", "name: local"),
    );
    remove(&case.db, &case.root, "local").unwrap();
    assert!(!shadowed.exists());
    assert_eq!(
        code(remove(&case.db, &case.root, "local")),
        "app_plugin_bundled"
    );
    assert_eq!(
        code(remove(&case.db, &case.root, "missing")),
        "app_plugin_not_found"
    );
    add(&case.db, &case.root, ACME, &sha(ACME), false, 2).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
}

#[test]
fn bundled_plugins_turn_on_and_off_and_unknown_names_are_refused() {
    let case = case();
    set_enabled(&case.db, &case.root, "ollama", true).unwrap();
    set_enabled(&case.db, &case.root, "local", false).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert!(plugin(&listing, "ollama").enabled);
    assert!(!plugin(&listing, "local").enabled);
    assert_eq!(
        code(set_enabled(&case.db, &case.root, "nope", true)),
        "app_plugin_not_found"
    );
    assert_eq!(code(ready(&case.db, &case.root, "local")), "app_plugin_off");
    assert_eq!(
        code(trust_file(
            &case.db,
            &case.root,
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
    crate::storage::accounts::initialize(&case.db).unwrap();
    trusted(&case, ACME);
    let keyed = |id: &str| ModelAuthentication::ApiKey {
        credential_ref: CredentialRef::new(format!("key-{id}")).unwrap(),
    };
    saved_account(&case, "acct-work", keyed("acct-work"));
    saved_account(&case, "acct-nokey", keyed("acct-nokey"));
    saved_account(&case, "acct-plain", ModelAuthentication::None {});
    let reference = CredentialRef::new("key-acct-work").unwrap();
    crate::storage::accounts::replace_key_in(
        &case.root,
        crate::storage::accounts::ACCOUNT_KEYS,
        &reference,
        b"sk-account-key-01".to_vec(),
    )
    .unwrap();
    let bind = |secret: &str, account: &str| {
        code(bind_account(&case.db, &case.root, "acme", secret, account))
    };
    assert_eq!(bind("api_key", "acct-gone"), "app_plugin_account_not_found");
    assert_eq!(
        bind("api_key", "acct-nokey"),
        "app_plugin_account_has_no_key"
    );
    assert_eq!(
        bind("api_key", "acct-plain"),
        "app_plugin_account_has_no_key"
    );
    assert_eq!(bind("token", "acct-work"), "app_plugin_secret_not_declared");
    bind_account(&case.db, &case.root, "acme", "api_key", "acct-work").unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    let acme = plugin(&listing, "acme");
    assert_eq!(acme.status, PluginStatus::Ready);
    assert_eq!(
        acme.secrets[0].binding,
        Binding::Account {
            account: "acct-work".into(),
            name: "Work Acme".into()
        }
    );
    let loaded = ready(&case.db, &case.root, "acme").unwrap();
    assert_eq!(loaded.bindings, [Bound::Account(reference.clone())]);
    let value = secret_value(&case.root, &loaded.bindings[0]).unwrap();
    assert_eq!(value.expose(), "sk-account-key-01");
    set_key(
        &case.db,
        &case.root,
        "acme",
        "api_key",
        b"sk-own-plugin-01".to_vec(),
    )
    .unwrap();
    bind_account(&case.db, &case.root, "acme", "api_key", "acct-work").unwrap();
    assert!(plugin_keys(&case).is_empty());
}

#[test]
fn a_replaced_plugin_with_new_hosts_drops_its_bound_keys() {
    let case = case();
    add(&case.db, &case.root, ACME, &sha(ACME), false, 1).unwrap();
    set_key(
        &case.db,
        &case.root,
        "acme",
        "api_key",
        b"sk-test-key-0001".to_vec(),
    )
    .unwrap();
    let same_hosts = ACME.replace("version: 1.0.0", "version: 1.0.1");
    add(
        &case.db,
        &case.root,
        &same_hosts,
        &sha(&same_hosts),
        true,
        2,
    )
    .unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::Key);
    let moved = same_hosts
        .replace(
            "https://api.acme.example/v1/embed",
            "https://other.example/v1/embed",
        )
        .replace("[api.acme.example]", "[other.example]");
    add(&case.db, &case.root, &moved, &sha(&moved), true, 3).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);
    let path = case.root.join(crate::app_plugins::FOLDER).join("acme.yaml");
    set_key(
        &case.db,
        &case.root,
        "acme",
        "api_key",
        b"sk-test-key-0002".to_vec(),
    )
    .unwrap();
    std::fs::write(&path, ACME).unwrap();
    trust_file(&case.db, &case.root, "acme", &sha(ACME), 4).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
}

#[test]
fn a_key_entered_for_a_plugin_is_a_private_file_replaced_in_place_and_deleted_on_unbind() {
    let case = case();
    trusted(&case, ACME);
    trusted(&case, NEAR);
    set_key(
        &case.db,
        &case.root,
        "acme",
        "api_key",
        b"pa-first-key-0001".to_vec(),
    )
    .unwrap();
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    let files: Vec<_> = std::fs::read_dir(&keys)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!((mode(&keys), mode(&files[0])), (0o700, 0o600));
    set_key(
        &case.db,
        &case.root,
        "acme",
        "api_key",
        b"pa-second-key-002".to_vec(),
    )
    .unwrap();
    let again: Vec<_> = std::fs::read_dir(&keys)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(again, files);
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"pa-second-key-002");
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::Key);
    assert_eq!(plugin(&listing, "acme").status, PluginStatus::Ready);
    assert_eq!(
        code(set_key(
            &case.db,
            &case.root,
            "acme",
            "other",
            b"pa-other-key-0001".to_vec()
        )),
        "app_plugin_secret_not_declared"
    );
    assert_eq!(
        code(set_key(
            &case.db,
            &case.root,
            "near",
            "api_key",
            b"pa-other-key-0001".to_vec()
        )),
        "app_plugin_secret_not_declared"
    );
    unbind(&case.db, &case.root, "acme", "api_key").unwrap();
    assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").status, PluginStatus::NeedsKey);
}

#[test]
fn a_ready_plugin_loads_with_its_manifest_and_a_needs_key_or_off_one_is_refused() {
    let case = case();
    trusted(&case, NEAR);
    trusted(&case, ACME);
    let local = ready(&case.db, &case.root, "local").unwrap();
    assert_eq!(
        (local.manifest.name.as_str(), local.origin),
        ("local", Origin::Bundled)
    );
    assert!(local.bindings.is_empty());
    let near = ready(&case.db, &case.root, "near").unwrap();
    assert_eq!(
        (near.manifest.name.as_str(), near.origin),
        ("near", Origin::User)
    );
    assert!(near.bindings.is_empty());
    assert_eq!(
        code(ready(&case.db, &case.root, "acme")),
        "app_plugin_needs_key"
    );
    set_enabled(&case.db, &case.root, "near", false).unwrap();
    assert_eq!(code(ready(&case.db, &case.root, "near")), "app_plugin_off");
    assert_eq!(
        code(set_enabled(&case.db, &case.root, "nope", true)),
        "app_plugin_not_found"
    );
    assert_eq!(
        code(ready(&case.db, &case.root, "missing")),
        "app_plugin_not_found"
    );
}

#[test]
fn every_loopback_plugin_port_is_listed_for_the_sandbox_and_an_unsafe_folder_fails_closed() {
    let case = case();
    assert_eq!(loopback_ports(&case.root).unwrap(), [11434]);
    drop_in(&case, "near.yaml", NEAR);
    drop_in(
        &case,
        "studio.yaml",
        &NEAR
            .replace("name: near", "name: studio")
            .replace(
                "http://127.0.0.1:9000/embed",
                "http://localhost:1234/v1/embeddings",
            )
            .replace("[127.0.0.1]", "[localhost]"),
    );
    drop_in(&case, "acme.yaml", ACME);
    drop_in(&case, "bad.yaml", "a: 1\n\tb: 2\n");
    assert_eq!(loopback_ports(&case.root).unwrap(), [1234, 9000, 11434]);
    let folder = case.root.join(crate::app_plugins::FOLDER);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code(loopback_ports(&case.root)), "app_plugin_folder_unsafe");
}

#[test]
fn a_port_zero_plugin_and_files_past_the_cap_are_refused_while_the_ports_still_list() {
    let case = case();
    drop_in(
        &case,
        "zero.yaml",
        &NEAR
            .replace("name: near", "name: zero")
            .replace("127.0.0.1:9000", "127.0.0.1:0"),
    );
    let listing = list(&case.db, &case.root).unwrap();
    match &plugin(&listing, "zero").status {
        PluginStatus::Refused { problem } => assert_eq!(problem.code, "app_plugin_port_invalid"),
        other => panic!("expected a refused plugin, got {other:?}"),
    }
    assert_eq!(loopback_ports(&case.root).unwrap(), [11434]);
    let cap = crate::app_plugins::USER_PLUGINS_MAX;
    for index in 0..=cap {
        let name = format!("p{index:03}");
        drop_in(
            &case,
            &format!("{name}.yaml"),
            &NEAR
                .replace("name: near", &format!("name: {name}"))
                .replace(":9000", &format!(":{}", 20_000 + index)),
        );
    }
    let ports = loopback_ports(&case.root).unwrap();
    assert_eq!(ports.len(), cap + 1);
    assert!(ports.contains(&(20_000 + cap as u16 - 1)));
    assert!(!ports.contains(&(20_000 + cap as u16)));
    assert_eq!(
        code(ready(&case.db, &case.root, &format!("p{cap:03}"))),
        "app_plugin_refused"
    );
    assert_eq!(
        code(ready(&case.db, &case.root, "p000")),
        "app_plugin_needs_review"
    );
}

fn near_on(port: u16) -> String {
    NEAR.replace("name: near", "name: handed")
        .replace("127.0.0.1:9000", &format!("127.0.0.1:{port}"))
}

/// Q-8 (DEC-156 15:35 Unresolved, leader 19:12 refuse): a loopback plugin port inside the
/// ports Linux hands out refuses that plugin alone, with a plain reason naming the range;
/// ports outside it, and hosts that are not loopback, stay usable.
#[test]
fn a_loopback_plugin_port_inside_the_handed_out_range_is_refused_alone_with_the_range_named() {
    let usual = 32_768..=60_999;
    let handed = |text: &str| {
        let found = user_found("handed".into(), Ok(text.as_bytes().to_vec()));
        outside_handed_out(found, Some(&usual)).parsed
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
    assert!(handed(&remote.replace("name: acme", "name: handed")).is_ok());
    let found = user_found("handed".into(), Ok(near_on(40_000).into_bytes()));
    assert!(
        outside_handed_out(found, None).parsed.is_ok(),
        "no range read, no plugin refused"
    );
}

/// Q-8: on Linux a plugin on a port this Linux hands out reads Refused with the reason in
/// the listing and the trust screen, never runs, and is left out of the ports a launch keeps
/// free, so it never stops an open-network launch; the other plugins keep their ports.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_a_plugin_on_a_port_linux_hands_out_is_refused_and_left_out_of_the_kept_ports() {
    let range = crate::sandbox::linux::read_picked_ports().expect("this Linux names its range");
    assert!(
        *range.start() > 11_434,
        "the test needs the bundled port below the range"
    );
    let port = *range.start();
    let case = case();
    drop_in(&case, "near.yaml", NEAR);
    drop_in(&case, "handed.yaml", &near_on(port));
    let listing = list(&case.db, &case.root).unwrap();
    match &plugin(&listing, "handed").status {
        PluginStatus::Refused { problem } => {
            assert_eq!(problem.code, "app_plugin_port_handed_out");
            let named = format!("ports {} to {}", range.start(), range.end());
            assert!(problem.message.contains(&named), "{}", problem.message);
        }
        other => panic!("expected a refused plugin, got {other:?}"),
    }
    assert_eq!(plugin(&listing, "near").status, PluginStatus::NeedsReview);
    assert_eq!(loopback_ports(&case.root).unwrap(), [9000, 11434]);
    assert_eq!(
        code(ready(&case.db, &case.root, "handed")),
        "app_plugin_refused"
    );
    match preview(
        &case.root,
        &near_on(port).replace("name: handed", "name: other"),
    )
    .unwrap()
    {
        Preview::Refused { problem } => assert_eq!(problem.code, "app_plugin_port_handed_out"),
        other => panic!("expected the trust screen to refuse, got {other:?}"),
    }
}

/// Q-8: macOS keeps every loopback plugin port free of agents in its profile, so a plugin on
/// a port inside the system's own range stays usable and listed for the sandbox.
#[cfg(not(target_os = "linux"))]
#[test]
fn off_linux_a_plugin_on_a_port_the_system_hands_out_stays_usable() {
    let case = case();
    drop_in(&case, "handed.yaml", &near_on(50_000));
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "handed").status, PluginStatus::NeedsReview);
    assert_eq!(loopback_ports(&case.root).unwrap(), [11434, 50000]);
    assert!(matches!(
        preview(
            &case.root,
            &near_on(50_000).replace("name: handed", "name: other")
        )
        .unwrap(),
        Preview::Ready { .. }
    ));
}

/// H2 plan row: a key the owner entered for a plugin is never readable through the plugin
/// state read. The listing, its JSON, the three store tables and the loaded plugin hold no
/// key byte; the key lives only in its owner-only 0600 file and comes back only through
/// `secret_value`.
#[test]
fn a_saved_key_is_never_readable_through_the_plugin_state_read() {
    let case = case();
    trusted(&case, ACME);
    let key = "sk-never-in-state-0001";
    set_key(
        &case.db,
        &case.root,
        "acme",
        "api_key",
        key.as_bytes().to_vec(),
    )
    .unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::Key);
    assert!(!format!("{listing:?}").contains(key));
    assert!(!serde_json::to_string(&listing).unwrap().contains(key));
    for table in ["app_plugin_trust", "app_plugin_state", "app_plugin_secrets"] {
        let held = cells(&case.db, table);
        assert!(!held.is_empty(), "{table}");
        assert!(held.iter().all(|cell| !cell.contains(key)), "{table}");
    }
    let loaded = ready(&case.db, &case.root, "acme").unwrap();
    assert!(!format!("{loaded:?}").contains(key));
    let files = plugin_keys(&case);
    assert_eq!(files.len(), 1);
    assert_eq!(mode(&files[0]), 0o600);
    assert_eq!(std::fs::read(&files[0]).unwrap(), key.as_bytes());
    let value = secret_value(&case.root, &loaded.bindings[0]).unwrap();
    assert_eq!(value.expose(), key);
}

/// H2 plan row: the trust row binds the SHA-256 of the text the owner reviewed. The row
/// holds that hash; a changed file needs review again until a row holds the new hash; and
/// the reviewed text read back once more is refused while the row holds another hash.
#[test]
fn the_trust_row_binds_the_sha256_of_the_reviewed_text() {
    let case = case();
    let path = drop_in(&case, "near.yaml", NEAR);
    trust_file(&case.db, &case.root, "near", &sha(NEAR), 1).unwrap();
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
    ready(&case.db, &case.root, "near").unwrap();
    let changed = NEAR.replace("dimensions: 128", "dimensions: 256");
    std::fs::write(&path, &changed).unwrap();
    assert_eq!(
        code(ready(&case.db, &case.root, "near")),
        "app_plugin_needs_review"
    );
    assert_eq!(
        code(set_enabled(&case.db, &case.root, "near", true)),
        "app_plugin_needs_review"
    );
    case.db
        .execute(
            "UPDATE app_plugin_trust SET sha256=?1 WHERE name='near'",
            [sha(&changed)],
        )
        .unwrap();
    ready(&case.db, &case.root, "near").unwrap();
    std::fs::write(&path, NEAR).unwrap();
    assert_eq!(
        code(ready(&case.db, &case.root, "near")),
        "app_plugin_needs_review"
    );
    assert_eq!(held(), sha(&changed));
}
