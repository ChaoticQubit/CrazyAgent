use super::*;

/// A trusted plugin whose plugin.toml changed lists as Changed with the SHA-256 it was
/// trusted at, and noting the listing keeps one notice per change: listing it again adds
/// none, and once it runs again a later change notifies again.
#[test]
fn a_changed_plugin_lists_as_changed_and_notifies_once_per_change() {
    let case = case();
    trusted(&case, NEAR);
    let usage = Usage::default();
    let path = case.root.join(FOLDER).join("near").join(MANIFEST_FILE);
    let changed = NEAR.replace("dimensions = 128", "dimensions = 256");
    std::fs::write(&path, &changed).unwrap();

    let listing = list(&case.db, &case.engine()).unwrap();
    usage.note_listed(&listing);
    usage.note_listed(&list(&case.db, &case.engine()).unwrap());

    assert_eq!(
        usage.take_notices(),
        [ChangeNotice {
            plugin: "near".into(),
            trusted_sha256: sha(NEAR),
            current_sha256: sha(&changed),
        }]
    );
    assert!(usage.take_notices().is_empty());
    usage.note_listed(&list(&case.db, &case.engine()).unwrap());
    assert!(usage.take_notices().is_empty());

    trust_file(&case.db, &case.engine(), "near", &sha(&changed), 2).unwrap();
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "near").status, PluginStatus::Ready);
    usage.note_listed(&listing);
    std::fs::write(&path, NEAR).unwrap();
    usage.note_listed(&list(&case.db, &case.engine()).unwrap());
    assert_eq!(usage.take_notices().len(), 1);
}

/// A database write that fails during a remove leaves the plugin as it was: its folder,
/// its trust, its binding and the key the owner entered for it, so it still loads.
#[test]
fn a_failed_remove_write_keeps_the_plugin_and_its_key() {
    let case = case();
    trusted(&case, ACME);
    set_key(
        &case.db,
        &case.engine(),
        "acme",
        "api_key",
        b"sk-kept-on-fail-01".to_vec(),
    )
    .unwrap();
    let keys = plugin_keys(&case);
    assert_eq!(keys.len(), 1);
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_remove BEFORE DELETE ON app_plugin_trust
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();

    assert!(remove(&case.db, &case.engine(), "acme").is_err());

    case.db.execute_batch("DROP TRIGGER refuse_remove").unwrap();
    assert!(case.db.is_autocommit());
    assert_eq!(plugin_keys(&case), keys);
    assert_eq!(std::fs::read(&keys[0]).unwrap(), b"sk-kept-on-fail-01");
    assert!(
        case.root
            .join(FOLDER)
            .join("acme")
            .join(MANIFEST_FILE)
            .is_file()
    );
    let loaded = ready(&case.db, &case.engine(), "acme").unwrap();
    assert_eq!(loaded.bindings.len(), 1);
}

/// Replacing a plugin whose old folder holds more than the sweep may delete still
/// succeeds: the new plugin.toml is in place and listed, and the old folder is left aside
/// under a retired name instead of failing the add after it was published.
#[test]
fn a_replace_over_an_old_folder_it_cannot_delete_still_succeeds() {
    let case = case();
    let old = drop_in(&case, "acme", ACME);
    let mut deep = old.parent().unwrap().to_path_buf();
    for level in 0..8 {
        deep = deep.join(format!("level-{level}"));
        std::fs::create_dir(&deep).unwrap();
    }
    std::fs::write(deep.join("note.txt"), b"deep").unwrap();
    let text = ACME.replace("Acme embeddings", "Acme embeddings two");

    add_text(&case, &text, true, 2).unwrap();

    assert_eq!(std::fs::read_to_string(&old).unwrap(), text);
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").sha256, sha(&text));
    let left = left_over(&case);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(!left[0].starts_with(".old-acme."), "{left:?}");

    remove(&case.db, &case.engine(), "acme").unwrap();
    sweep_staged(&case.engine());

    assert!(!plugins_folder(&case).join("acme").exists());
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(listing.plugins.iter().all(|plugin| plugin.name != "acme"));
}

/// The staged and retired entries in the plugins folder, by name.
fn left_over(case: &Case) -> Vec<String> {
    let mut left: Vec<String> = std::fs::read_dir(plugins_folder(case))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".old-") || name.starts_with(".add-"))
        .collect();
    left.sort();
    left
}

/// Removing a plugin whose folder holds more than a delete may take still removes it: its
/// rows go, and its folder leaves the plugin's name, so it is never listed again, even
/// after the start sweep.
#[test]
fn a_removed_plugin_with_a_folder_it_cannot_delete_stays_removed() {
    let case = case();
    trusted(&case, ACME);
    let mut deep = plugins_folder(&case).join("acme");
    for level in 0..8 {
        deep = deep.join(format!("level-{level}"));
        std::fs::create_dir(&deep).unwrap();
    }

    remove(&case.db, &case.engine(), "acme").unwrap();
    sweep_staged(&case.engine());

    assert!(!plugins_folder(&case).join("acme").exists());
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(listing.plugins.iter().all(|plugin| plugin.name != "acme"));
    assert!(
        left_over(&case)
            .iter()
            .all(|name| !name.starts_with(".old-acme."))
    );
}

/// Replacing a trusted plugin whose folder holds a subfolder succeeds: the new text is
/// what is trusted, so the plugin is not left waiting for a review, and no folder stays
/// under the old name.
#[test]
fn replacing_a_plugin_whose_folder_holds_a_subfolder_succeeds_and_settles_trust() {
    let case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case).join("acme");
    std::fs::create_dir(folder.join("assets")).unwrap();
    std::fs::write(folder.join("assets").join("note.txt"), b"kept aside").unwrap();
    let text = ACME.replace("Acme embeddings", "Acme embeddings two");

    add_text(&case, &text, true, 2).unwrap();

    assert_eq!(
        std::fs::read_to_string(folder.join(MANIFEST_FILE)).unwrap(),
        text
    );
    assert!(!folder.join("assets").exists());
    let listing = list(&case.db, &case.engine()).unwrap();
    let view = plugin(&listing, "acme");
    assert_eq!(view.sha256, sha(&text));
    assert_ne!(view.status, PluginStatus::NeedsReview);
    assert!(
        !matches!(view.status, PluginStatus::Changed { .. }),
        "{:?}",
        view.status
    );
    let left = left_over(&case);
    assert!(
        left.iter()
            .all(|name| !name.starts_with(".old-acme.") && !name.starts_with(".add-")),
        "{left:?}"
    );
}
