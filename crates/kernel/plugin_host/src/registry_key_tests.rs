use super::*;
use kernel_security::secret::SecretString;

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
    assert_eq!(fixture(&files[0], std::fs::read), b"pa-second-key-002");
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

/// A binding change whose database write fails leaves the old binding and its entered key
/// file in place, so the plugin still loads and reads the key it had.
#[test]
fn a_failed_binding_write_keeps_the_old_key_working() {
    let case = case();
    trusted(&case, ACME);
    saved_account(&case, "acct-work", Some("key-acct-work"));
    test_replace(
        &case.root,
        KeyFolder::Account,
        &KeyRef::new("key-acct-work").unwrap(),
        b"sk-account-key-01".to_vec(),
    )
    .unwrap();
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-own-plugin-01".to_vec(),
    )
    .unwrap();
    let before = cells(&case.db, "app_plugin_secrets");
    assert_eq!(plugin_keys(&case).len(), 1);
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_insert BEFORE INSERT ON app_plugin_secrets
             BEGIN SELECT RAISE(ABORT, 'refused'); END;
             CREATE TEMP TRIGGER refuse_update BEFORE UPDATE ON app_plugin_secrets
             BEGIN SELECT RAISE(ABORT, 'refused'); END;
             CREATE TEMP TRIGGER refuse_delete BEFORE DELETE ON app_plugin_secrets
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    assert!(bind_account(&case.db, &case.engine(), "acme", "api_key", "acct-work").is_err());
    assert!(unbind(&case.db, &case.engine(), "acme", "api_key").is_err());
    assert_eq!(cells(&case.db, "app_plugin_secrets"), before);
    assert_eq!(plugin_keys(&case).len(), 1);
    let loaded = ready(&case.db, &case.engine(), "acme").unwrap();
    assert_eq!(loaded.bindings[0].folder(), KeyFolder::Plugin);
    let value = secret_value(&case.engine(), &loaded.bindings[0]).unwrap();
    assert_eq!(value.expose(), "sk-own-plugin-01");
}

/// A new key entered for a secret already bound to an entered key replaces that key in
/// place and writes no binding row, so a failing binding write cannot fail it, and the
/// plugin reads the new key.
#[test]
fn a_new_key_at_the_same_reference_needs_no_binding_write() {
    let case = case();
    trusted(&case, ACME);
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-own-plugin-01".to_vec(),
    )
    .unwrap();
    let before = cells(&case.db, "app_plugin_secrets");
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_insert BEFORE INSERT ON app_plugin_secrets
             BEGIN SELECT RAISE(ABORT, 'refused'); END;
             CREATE TEMP TRIGGER refuse_update BEFORE UPDATE ON app_plugin_secrets
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-own-plugin-02".to_vec(),
    )
    .unwrap();
    assert_eq!(cells(&case.db, "app_plugin_secrets"), before);
    assert_eq!(plugin_keys(&case).len(), 1);
    let loaded = ready(&case.db, &case.engine(), "acme").unwrap();
    let value = secret_value(&case.engine(), &loaded.bindings[0]).unwrap();
    assert_eq!(value.expose(), "sk-own-plugin-02");
}

/// A key store read, as [`Keys::read`] takes it.
type KeyRead = fn(&Path, KeyFolder, &KeyRef) -> Result<Option<SecretString>, ()>;

/// Replaces acme's entered key in place with an engine whose key store reads with `read`,
/// after a first key was saved. Inputs: the case and the read. Output: the replace's
/// result.
fn replace_key_reading(case: &Case, read: KeyRead) -> Result<(), Refused> {
    trusted(case, ACME);
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"pa-old-key-000001".to_vec(),
    )
    .unwrap();
    let keys = Keys { read, ..KEYS };
    let engine = Engine {
        keys: &keys,
        ..case.engine()
    };
    set_key(
        &case.db,
        &engine,
        "acme",
        "api_key",
        b"pa-new-key-000002".to_vec(),
    )
}

/// A key replace whose read of the fresh key file fails deletes that file and leaves the
/// old key in its own file.
#[test]
fn a_key_commit_whose_read_fails_deletes_the_fresh_key_file() {
    fn failing(_: &Path, _: KeyFolder, _: &KeyRef) -> Result<Option<SecretString>, ()> {
        Err(())
    }
    let case = case();
    let replaced = replace_key_reading(&case, failing);
    assert_eq!(code(replaced), "app_plugin_key_store_unavailable");
    let files = plugin_keys(&case);
    assert_eq!(files.len(), 1);
    assert_eq!(fixture(&files[0], std::fs::read), b"pa-old-key-000001");
}

/// A key replace whose read of the fresh key file finds nothing deletes that file and
/// leaves the old key in its own file.
#[test]
fn a_key_commit_whose_read_finds_nothing_deletes_the_fresh_key_file() {
    fn nothing(_: &Path, _: KeyFolder, _: &KeyRef) -> Result<Option<SecretString>, ()> {
        Ok(None)
    }
    let case = case();
    let replaced = replace_key_reading(&case, nothing);
    assert_eq!(code(replaced), "app_plugin_key_store_unavailable");
    let files = plugin_keys(&case);
    assert_eq!(files.len(), 1);
    assert_eq!(fixture(&files[0], std::fs::read), b"pa-old-key-000001");
}
