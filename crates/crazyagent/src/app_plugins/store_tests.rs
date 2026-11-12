use super::*;
use crate::app_plugins::{Binding, Origin, PluginStatus, Preview};
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
    Case {
        _temp: temp,
        root,
        db: rusqlite::Connection::open_in_memory().unwrap(),
    }
}

fn sha(text: &str) -> String {
    crate::semantic::sha256_hex(text.as_bytes())
}

fn plugin<'a>(listing: &'a crate::app_plugins::Listing, name: &str) -> &'a crate::app_plugins::PluginView {
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
    std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn code<T: std::fmt::Debug>(result: Result<T, EngineError>) -> String {
    result.expect_err("refused").0
}

#[test]
fn bundled_plugins_list_first_with_only_the_local_one_on() {
    let case = case();
    let listing = list(&case.db, &case.root).unwrap();
    let names: Vec<&str> = listing.plugins.iter().map(|plugin| plugin.name.as_str()).collect();
    assert_eq!(names, ["local", "openai", "voyage", "ollama"]);
    assert!(!listing.more);
    let local = plugin(&listing, "local");
    assert_eq!(
        (local.origin, &local.status, local.enabled),
        (Origin::Bundled, &PluginStatus::Ready, true)
    );
    let openai = plugin(&listing, "openai");
    assert_eq!((&openai.status, openai.enabled), (&PluginStatus::NeedsKey, false));
    assert_eq!(openai.secrets[0].binding, Binding::None);
    let ollama = plugin(&listing, "ollama");
    assert_eq!((&ollama.status, ollama.enabled), (&PluginStatus::Ready, false));
    assert!(!case.root.join(crate::app_plugins::FOLDER).exists());
}

#[test]
fn a_preview_shows_the_trust_screen_and_saves_nothing() {
    let case = case();
    let Preview::Ready { trust } = preview(&case.db, &case.root, ACME).unwrap() else {
        panic!("ready");
    };
    assert_eq!(trust.name, "acme");
    assert_eq!(trust.sha256, sha(ACME));
    assert_eq!(trust.secrets, ["api_key"]);
    assert!(!trust.replaces);
    assert!(!case.root.join(crate::app_plugins::FOLDER).exists());
    let Preview::Refused { problem } = preview(&case.db, &case.root, "a: 1\n\tb: 2\n").unwrap() else {
        panic!("refused");
    };
    assert_eq!((problem.code.as_str(), problem.line), ("app_plugin_yaml_tab", Some(2)));
    let bundled_name = ACME.replace("name: acme", "name: local");
    let Preview::Refused { problem } = preview(&case.db, &case.root, &bundled_name).unwrap() else {
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
    let Preview::Ready { trust } = preview(&case.db, &case.root, &newer).unwrap() else {
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
        code(add(&case.db, &case.root, "a: 1\n\tb\n", &sha("a: 1\n\tb\n"), false, 5)),
        "app_plugin_refused"
    );
}

#[test]
fn a_dropped_in_file_needs_review_until_trusted_and_again_after_a_change() {
    let case = case();
    let path = drop_in(&case, "near.yaml", NEAR);
    let listing = list(&case.db, &case.root).unwrap();
    let near = plugin(&listing, "near");
    assert_eq!((&near.status, near.enabled), (&PluginStatus::NeedsReview, false));
    assert_eq!(
        code(set_enabled(&case.db, &case.root, "near", true)),
        "app_plugin_needs_review"
    );
    let Preview::Ready { trust } = review(&case.db, &case.root, "near").unwrap() else {
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
        (&plugin(&listing, "near").status, plugin(&listing, "near").enabled),
        (&PluginStatus::Ready, true)
    );
    std::fs::write(&path, NEAR.replace("dimensions: 128", "dimensions: 256")).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "near").status, PluginStatus::NeedsReview);
    assert_eq!(code(ready(&case.db, &case.root, "near")), "app_plugin_needs_review");
}

#[test]
fn refused_and_unsafe_files_are_listed_with_their_problem_and_other_files_ignored() {
    let case = case();
    drop_in(&case, "bad.yaml", "a: 1\n\tb: 2\n");
    drop_in(&case, "other.yaml", NEAR);
    drop_in(&case, "local.yaml", &NEAR.replace("name: near", "name: local"));
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
    assert!(matches!(&user_local.status, PluginStatus::Refused { problem } if problem.code == "app_plugin_name_taken"));
    assert!(user_local.runs.is_none());
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
        drop_in(&case, &format!("{name}.yaml"), &NEAR.replace("name: near", &format!("name: {name}")));
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
fn remove_deletes_a_user_plugin_and_refuses_bundled_and_unknown_ones() {
    let case = case();
    add(&case.db, &case.root, ACME, &sha(ACME), false, 1).unwrap();
    set_key(&case.db, &case.root, "acme", "api_key", b"sk-test-key-0001".to_vec()).unwrap();
    remove(&case.db, &case.root, "acme").unwrap();
    assert!(!case.root.join(crate::app_plugins::FOLDER).join("acme.yaml").exists());
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);
    let listing = list(&case.db, &case.root).unwrap();
    assert!(listing.plugins.iter().all(|plugin| plugin.name != "acme"));
    assert_eq!(code(remove(&case.db, &case.root, "openai")), "app_plugin_bundled");
    assert_eq!(code(remove(&case.db, &case.root, "missing")), "app_plugin_not_found");
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
    assert_eq!(code(set_enabled(&case.db, &case.root, "nope", true)), "app_plugin_not_found");
    assert_eq!(code(ready(&case.db, &case.root, "local")), "app_plugin_off");
}

#[test]
fn a_secret_binds_to_an_api_key_account_with_a_saved_key() {
    let case = case();
    crate::storage::accounts::initialize(&case.db).unwrap();
    let keyed = |id: &str| serde_json::json!({"method": "api_key", "credential_ref": format!("key-{id}")});
    for (id, auth) in [
        ("acct-work", keyed("acct-work")),
        ("acct-nokey", keyed("acct-nokey")),
        ("acct-plain", serde_json::json!({"method": "none"})),
    ] {
        let body = serde_json::json!({
            "connection": {
                "id": id,
                "provider": "openai",
                "harness": "rcli",
                "protocol": "openai-chat",
                "account_ref": id,
                "endpoint": {"kind": "http", "url": "https://api.openai.com/v1"},
                "model": {"id": "gpt-test", "author": "openai"},
                "auth": auth,
            },
            "details": {"name": "Work OpenAI", "provider": "openai_api"},
        });
        case.db
            .execute(
                "INSERT INTO accounts (id, body) VALUES (?1, ?2)",
                rusqlite::params![id, serde_json::to_vec(&body).unwrap()],
            )
            .unwrap();
    }
    let reference = crate::protocol::CredentialRef::new("key-acct-work").unwrap();
    crate::storage::accounts::replace_key_in(
        &case.root,
        crate::storage::accounts::ACCOUNT_KEYS,
        &reference,
        b"sk-account-key-01".to_vec(),
    )
    .unwrap();
    assert_eq!(
        code(bind_account(&case.db, &case.root, "openai", "api_key", "acct-gone")),
        "app_plugin_account_not_found"
    );
    assert_eq!(
        code(bind_account(&case.db, &case.root, "openai", "api_key", "acct-nokey")),
        "app_plugin_account_has_no_key"
    );
    assert_eq!(
        code(bind_account(&case.db, &case.root, "openai", "api_key", "acct-plain")),
        "app_plugin_account_has_no_key"
    );
    assert_eq!(
        code(bind_account(&case.db, &case.root, "openai", "token", "acct-work")),
        "app_plugin_secret_not_declared"
    );
    bind_account(&case.db, &case.root, "openai", "api_key", "acct-work").unwrap();
    set_enabled(&case.db, &case.root, "openai", true).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    let openai = plugin(&listing, "openai");
    assert_eq!(openai.status, PluginStatus::Ready);
    assert_eq!(
        openai.secrets[0].binding,
        Binding::Account {
            account: "acct-work".into(),
            name: "Work OpenAI".into()
        }
    );
    let loaded = ready(&case.db, &case.root, "openai").unwrap();
    assert_eq!(loaded.bindings, [Bound::Account(reference.clone())]);
    let value = secret_value(&case.root, &loaded.bindings[0]).unwrap();
    assert_eq!(value.expose(), "sk-account-key-01");
    set_key(&case.db, &case.root, "openai", "api_key", b"sk-own-plugin-01".to_vec()).unwrap();
    bind_account(&case.db, &case.root, "openai", "api_key", "acct-work").unwrap();
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);
}

#[test]
fn a_replaced_plugin_with_new_hosts_drops_its_bound_keys() {
    let case = case();
    add(&case.db, &case.root, ACME, &sha(ACME), false, 1).unwrap();
    set_key(&case.db, &case.root, "acme", "api_key", b"sk-test-key-0001".to_vec()).unwrap();
    let same_hosts = ACME.replace("version: 1.0.0", "version: 1.0.1");
    add(&case.db, &case.root, &same_hosts, &sha(&same_hosts), true, 2).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::Key);
    let moved = same_hosts
        .replace("https://api.acme.example/v1/embed", "https://other.example/v1/embed")
        .replace("[api.acme.example]", "[other.example]");
    add(&case.db, &case.root, &moved, &sha(&moved), true, 3).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);
    let path = case.root.join(crate::app_plugins::FOLDER).join("acme.yaml");
    set_key(&case.db, &case.root, "acme", "api_key", b"sk-test-key-0002".to_vec()).unwrap();
    std::fs::write(&path, ACME).unwrap();
    trust_file(&case.db, &case.root, "acme", &sha(ACME), 4).unwrap();
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "acme").secrets[0].binding, Binding::None);
}

#[test]
fn a_key_entered_for_a_plugin_is_a_private_file_replaced_in_place_and_deleted_on_unbind() {
    let case = case();
    set_key(&case.db, &case.root, "voyage", "api_key", b"pa-first-key-0001".to_vec()).unwrap();
    let keys = case.root.join(crate::storage::accounts::PLUGIN_KEYS);
    let files: Vec<_> = std::fs::read_dir(&keys).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(files.len(), 1);
    assert_eq!((mode(&keys), mode(&files[0])), (0o700, 0o600));
    set_key(&case.db, &case.root, "voyage", "api_key", b"pa-second-key-002".to_vec()).unwrap();
    let again: Vec<_> = std::fs::read_dir(&keys).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(again, files);
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"pa-second-key-002");
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "voyage").secrets[0].binding, Binding::Key);
    assert_eq!(plugin(&listing, "voyage").status, PluginStatus::Ready);
    assert_eq!(
        code(set_key(&case.db, &case.root, "voyage", "other", b"pa-other-key-0001".to_vec())),
        "app_plugin_secret_not_declared"
    );
    assert_eq!(
        code(set_key(&case.db, &case.root, "local", "api_key", b"pa-other-key-0001".to_vec())),
        "app_plugin_secret_not_declared"
    );
    unbind(&case.db, &case.root, "voyage", "api_key").unwrap();
    assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);
    let listing = list(&case.db, &case.root).unwrap();
    assert_eq!(plugin(&listing, "voyage").status, PluginStatus::NeedsKey);
}

#[test]
fn a_ready_plugin_loads_with_its_manifest_and_a_needs_key_one_is_refused() {
    let case = case();
    let local = ready(&case.db, &case.root, "local").unwrap();
    assert_eq!(local.manifest.name, "local");
    assert!(local.bindings.is_empty());
    set_enabled(&case.db, &case.root, "voyage", true).unwrap();
    assert_eq!(code(ready(&case.db, &case.root, "voyage")), "app_plugin_needs_key");
    assert_eq!(code(ready(&case.db, &case.root, "missing")), "app_plugin_not_found");
}
