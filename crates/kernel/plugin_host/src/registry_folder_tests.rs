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

const STUCK_ID: &str = "00000000000000000000000000000000000000000000000000000000000000c3";

fn stuck_ref() -> Result<String, &'static str> {
    Ok(STUCK_ID.to_owned())
}

/// A key store whose every new id is the same, so a row can take the nameless name a
/// removal would move its folder to.
const STUCK: Keys = Keys {
    new_ref: stuck_ref,
    ..KEYS
};

/// Holds the nameless folder name the fixed id leads to with a file in it, so a rename onto
/// it fails until the start sweep deletes it. Input: the plugins folder.
fn take_nameless(folder: &Path) {
    let taken = folder.join(format!(".old-{STUCK_ID}"));
    std::fs::create_dir(&taken).unwrap();
    std::fs::write(taken.join("note.txt"), b"taken").unwrap();
}

/// Nests folders below a plugin folder deeper than a delete goes down, so deleting it
/// fails. Input: the plugin folder.
fn too_deep(plugin: &Path) {
    let mut deep = plugin.to_path_buf();
    for level in 0..8 {
        deep = deep.join(format!("level-{level}"));
        std::fs::create_dir(&deep).unwrap();
    }
}

/// The start sweeps in the order an engine start runs them. Input: the case. Output: the
/// removal sweep's refusal, else the replace sweep's answer.
fn start_sweep(case: &Case) -> Result<(), Refused> {
    sweep_staged(&case.engine());
    let replaced = crate::registry::sweep_replaced(&case.db, &case.root);
    let removed = crate::registry::sweep_removals(&case.db, &case.engine());
    removed.and(replaced)
}

/// A committed removal whose folder can neither lose the plugin name nor be deleted stays
/// removed: the start sweep never puts it back under the name.
#[test]
fn a_removed_plugin_whose_retired_folder_cannot_move_is_not_restored_at_start() {
    let case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    too_deep(&folder.join("acme"));
    take_nameless(&folder);
    let stuck = Engine {
        keys: &STUCK,
        ..case.engine()
    };

    assert!(remove(&case.db, &stuck, "acme").is_err());
    let _ = start_sweep(&case);

    let names = names_in(&folder);
    assert!(!folder.join("acme").exists(), "{names:?}");
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(listing.plugins.iter().all(|plugin| plugin.name != "acme"));
}

/// A removal that stopped before its rows were deleted leaves the folder aside with the
/// rows still there, and the start sweep puts it back as it was.
#[test]
fn a_removal_that_crashed_before_its_commit_is_restored_at_start() {
    let case = case();
    trusted(&case, ACME);
    let before = plugin(&list(&case.db, &case.engine()).unwrap(), "acme")
        .status
        .clone();
    let folder = plugins_folder(&case);
    std::fs::rename(
        folder.join("acme"),
        folder.join(format!(".rm-acme.{STUCK_ID}")),
    )
    .unwrap();

    start_sweep(&case).unwrap();

    assert_eq!(names_in(&folder), ["acme"]);
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").status, before);
}

/// Removing a plugin with no rows never moves its folder aside under its name, so a folder
/// that cannot be deleted stays where it was instead of waiting to be put back.
#[test]
fn a_rowless_plugin_removal_never_moves_it_aside() {
    let case = case();
    drop_in(&case, "acme", ACME);
    let folder = plugins_folder(&case);
    too_deep(&folder.join("acme"));
    take_nameless(&folder);
    let stuck = Engine {
        keys: &STUCK,
        ..case.engine()
    };

    assert!(remove(&case.db, &stuck, "acme").is_err());

    let names = names_in(&folder);
    assert!(
        names
            .iter()
            .all(|name| !name.starts_with(".old-acme.") && !name.starts_with(".rm-acme.")),
        "{names:?}"
    );
    assert!(folder.join("acme").join(MANIFEST_FILE).is_file());
}

/// A folder a committed removal left aside, found beside a plugin added again under the
/// same name, is deleted at start and never replaces the live folder.
#[test]
fn a_stuck_removal_folder_beside_a_re_added_plugin_is_deleted_at_start() {
    let case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    let stuck = folder.join(format!(".rm-acme.{STUCK_ID}"));
    std::fs::create_dir(&stuck).unwrap();
    std::fs::write(stuck.join(MANIFEST_FILE), "old text").unwrap();

    start_sweep(&case).unwrap();

    assert_eq!(names_in(&folder), ["acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        ACME
    );
}

/// Replaces acme with a new text inside a capture whose transaction then rolls back, and
/// ends its folder steps as the engine does after a rollback. Inputs: the case, the key
/// store the replace runs with, the new text, and what to do between the commit-less
/// write and the rollback.
fn replace_rolled_back(case: &mut Case, keys: &'static Keys, text: &str, between: impl FnOnce()) {
    case.release();
    let source = picked(case, "rolled-back", text);
    let root = case.root.clone();
    let engine = Engine {
        root: &root,
        keys,
        handed_out: None,
    };
    case.db.execute_batch("BEGIN").unwrap();
    let (added, mut captured) = crate::store_log::capture(&case.db, || {
        add(&case.db, &engine, &source, &sha(text), true, 2)
    });
    added.unwrap();
    between();
    case.db.execute_batch("ROLLBACK").unwrap();
    for step in std::mem::take(&mut captured.folders).into_iter().rev() {
        step.roll_back();
    }
    case.hold();
}

/// An id source that gives one id, then fails, so the replace's write takes its id and
/// any later call errs.
fn one_ref() -> Result<String, &'static str> {
    static GIVEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if GIVEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("app_plugin_folder_unavailable");
    }
    Ok("00000000000000000000000000000000000000000000000000000000000000d1".to_owned())
}

/// A key store whose id source gives one id, then fails.
const ONE_REF: Keys = Keys {
    new_ref: one_ref,
    ..KEYS
};

/// A replace rolled back when the id source has nothing more to give puts the original
/// folder back under the plugin name, leaving no retired or staged folder.
#[test]
fn a_replace_rolled_back_with_no_id_source_puts_the_original_back() {
    let mut case = case();
    trusted(&case, ACME);
    let text = ACME.replace("Acme embeddings", "Acme embeddings two");

    replace_rolled_back(&mut case, &ONE_REF, &text, || {});

    let folder = plugins_folder(&case);
    assert_eq!(names_in(&folder), ["acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        ACME
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").sha256, sha(ACME));
}

/// A replace rolled back whose new folder cannot leave the plugin name keeps the original
/// through the next start: one start sweep deletes the folder that blocked the move, then
/// puts the original back under the name in place of the new folder.
#[test]
fn a_replace_rollback_whose_new_folder_cannot_move_keeps_the_original_through_the_sweep() {
    let mut case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    let text = ACME.replace("Acme embeddings", "Acme embeddings two");

    replace_rolled_back(&mut case, &STUCK, &text, || take_nameless(&folder));
    sweep_staged(&case.engine());

    assert_eq!(names_in(&folder), ["acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        ACME
    );
}

/// Two folders owed back to one plugin name are left as they are by the start sweep, with
/// the live folder under the name, since the sweep cannot tell which one is the original.
#[test]
fn two_folders_owed_to_one_name_are_left_by_the_sweep() {
    let case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    let owed = [
        ".back-acme.00000000000000000000000000000000000000000000000000000000000000e1",
        ".back-acme.00000000000000000000000000000000000000000000000000000000000000e2",
    ];
    for name in owed {
        std::fs::create_dir(folder.join(name)).unwrap();
        std::fs::write(folder.join(name).join(MANIFEST_FILE), NEAR).unwrap();
    }

    sweep_staged(&case.engine());

    let mut names = names_in(&folder);
    names.sort();
    assert_eq!(names, [owed[0], owed[1], "acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        ACME
    );
}

/// Two folders owed to one plugin name stay ambiguous when a sweep's batch holds only one of
/// them: the count of owed folders comes from the whole plugins folder, so neither goes
/// back and the live folder keeps the name.
#[test]
fn an_owed_folder_past_the_sweep_batch_still_counts_for_its_name() {
    let case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    let owed = [
        ".back-acme.00000000000000000000000000000000000000000000000000000000000000e1",
        ".back-acme.00000000000000000000000000000000000000000000000000000000000000e2",
    ];
    for name in owed {
        std::fs::create_dir(folder.join(name)).unwrap();
        std::fs::write(folder.join(name).join(MANIFEST_FILE), NEAR).unwrap();
    }

    crate::registry::remove_staged_within(&case.root, 1).unwrap();

    let mut names = names_in(&folder);
    names.sort();
    assert_eq!(names, [owed[0], owed[1], "acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        ACME
    );
}

/// Leaves a replace of acme rolled back whose new folder could not leave the name: the
/// original is owed back as `.back-acme.<STUCK_ID>` and `.old-<STUCK_ID>` still blocks the
/// move. Inputs: the case and the plugins folder. Output: none.
fn stuck_rollback(case: &mut Case, folder: &Path) {
    let text = ACME.replace("Acme embeddings", "Acme embeddings two");
    replace_rolled_back(case, &STUCK, &text, || take_nameless(folder));
    assert!(folder.join(format!(".back-acme.{STUCK_ID}")).exists());
}

/// Checks that the folder under acme is the one the trust rows trust: the plugins folder
/// holds only acme, its plugin.toml is `text`, and the listing trusts that text's hash.
/// Inputs: the case, the plugins folder and the text. Output: none.
fn trusted_on_disk(case: &Case, folder: &Path, text: &str) {
    assert_eq!(names_in(folder), ["acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        text
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    let view = plugin(&listing, "acme");
    assert_eq!(view.sha256, sha(text));
    assert_ne!(view.status, PluginStatus::NeedsReview);
    assert!(
        !matches!(view.status, PluginStatus::Changed { .. }),
        "{:?}",
        view.status
    );
}

/// A replace of a plugin whose earlier rolled-back replace still owes its original folder,
/// and whose owed folder cannot go back yet, is refused with nothing written, and the next
/// start puts the trusted original back.
#[test]
fn a_replace_after_a_stuck_rollback_is_refused_and_a_start_keeps_the_trusted_folder() {
    let mut case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    stuck_rollback(&mut case, &folder);
    let third = ACME.replace("Acme embeddings", "Acme embeddings three");

    let replaced = add_text(&case, &third, true, 3);

    assert_eq!(code(replaced), "app_plugin_folder_unavailable");
    sweep_staged(&case.engine());
    trusted_on_disk(&case, &folder, ACME);
}

/// A replace of a plugin whose earlier rolled-back replace still owes its original folder
/// first puts that original back, then replaces it, so the next start keeps the folder the
/// trust rows now trust.
#[test]
fn a_replace_after_a_rollback_owed_folder_recovers_it_first() {
    let mut case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    stuck_rollback(&mut case, &folder);
    std::fs::remove_dir_all(folder.join(format!(".old-{STUCK_ID}"))).unwrap();
    let third = ACME.replace("Acme embeddings", "Acme embeddings three");

    add_text(&case, &third, true, 3).unwrap();

    sweep_staged(&case.engine());
    trusted_on_disk(&case, &folder, &third);
}

/// A removal of a plugin whose earlier rolled-back replace still owes its original folder
/// stays removed through the next start: the owed folder is settled before the removal, so
/// the start sweep has nothing to put back.
#[test]
fn a_removal_after_a_stuck_rollback_stays_removed_through_a_start() {
    let mut case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    stuck_rollback(&mut case, &folder);
    std::fs::remove_dir_all(folder.join(format!(".old-{STUCK_ID}"))).unwrap();

    remove(&case.db, &case.engine(), "acme").unwrap();

    start_sweep(&case).unwrap();
    assert!(names_in(&folder).is_empty(), "{:?}", names_in(&folder));
}

/// Puts an entry named like a folder owed to acme that is not a folder: a regular file, or
/// on unix a link to a folder outside the plugins folder holding a plugin.toml. Inputs: the
/// plugins folder, the entry name and whether it is a link. Output: the link's target, when
/// one was made.
fn owed_named_entry(folder: &Path, name: &str, link: bool) -> Option<PathBuf> {
    let entry = folder.join(name);
    if !link {
        std::fs::write(&entry, NEAR).unwrap();
        return None;
    }
    let target = folder.parent().unwrap().join(format!("{name}-target"));
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join(MANIFEST_FILE), NEAR).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &entry).unwrap();
    Some(target)
}

/// A file or link named like a folder owed to acme is not owed: a replace that rolls back
/// leaves the original acme in place, and the next start unlinks the entry without
/// following it.
#[test]
fn a_back_named_file_never_moves_the_live_folder() {
    let links: &[bool] = if cfg!(unix) { &[false, true] } else { &[false] };
    for &link in links {
        let mut case = case();
        trusted(&case, ACME);
        let folder = plugins_folder(&case);
        let entry = ".back-acme.00000000000000000000000000000000000000000000000000000000000000e5";
        let target = owed_named_entry(&folder, entry, link);
        let two = ACME.replace("Acme embeddings", "Acme embeddings two");

        replace_rolled_back(&mut case, &KEYS, &two, || {});

        assert_eq!(
            fixture(
                &folder.join("acme").join(MANIFEST_FILE),
                std::fs::read_to_string
            ),
            ACME,
            "link {link}"
        );
        start_sweep(&case).unwrap();
        assert_eq!(names_in(&folder), ["acme"], "link {link}");
        if let Some(target) = target {
            assert!(target.join(MANIFEST_FILE).exists());
        }
    }
}

/// A file named like a folder owed to acme does not count as a second owed folder: the start
/// sweep puts the one owed folder back under acme and unlinks the file.
#[test]
fn a_back_named_file_does_not_make_an_owed_folder_ambiguous() {
    let case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    let owed = ".back-acme.00000000000000000000000000000000000000000000000000000000000000a1";
    let original = ACME.replace("Acme embeddings", "Acme embeddings original");
    std::fs::create_dir(folder.join(owed)).unwrap();
    std::fs::write(folder.join(owed).join(MANIFEST_FILE), &original).unwrap();
    owed_named_entry(
        &folder,
        ".back-acme.00000000000000000000000000000000000000000000000000000000000000b1",
        false,
    );

    crate::registry::remove_staged(&case.root).unwrap();

    assert_eq!(names_in(&folder), ["acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        original
    );
}

/// Leaves acme as a replace cut after both its renames and before its commit: the trusted
/// folder moved aside to `.old-acme.<STUCK_ID>` and `live` under acme. Inputs: the case and
/// the live text. Output: the plugins folder.
fn cut_after_renames(case: &Case, live: &str) -> PathBuf {
    let folder = plugins_folder(case);
    std::fs::rename(
        folder.join("acme"),
        folder.join(format!(".old-acme.{STUCK_ID}")),
    )
    .unwrap();
    drop_in(case, "acme", live);
    folder
}

/// Checks that the start sweep kept both folders of a cut replace: `live` under acme and
/// the trusted ACME under its retired name. Inputs: the plugins folder and the live text.
/// Output: none.
fn both_kept(folder: &Path, live: &str) {
    let retired = format!(".old-acme.{STUCK_ID}");
    let mut names = names_in(folder);
    names.sort();
    assert_eq!(names, [retired.as_str(), "acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        live
    );
    assert_eq!(
        fixture(
            &folder.join(&retired).join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        ACME
    );
}

/// Sets the trusted SHA-256 of acme straight in its trust row. Inputs: the case and the
/// text whose hash the row then holds. Output: none.
fn trust_row_holds(case: &Case, text: &str) {
    let changed = case
        .db
        .execute(
            "UPDATE app_plugin_trust SET sha256 = ?1 WHERE name = 'acme'",
            [sha(text)],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

/// A replace cut after both its renames and before its commit leaves the new folder under
/// the name and the original aside, with the trust row still the original's: the start
/// sweep puts the original back in place of the new folder.
#[test]
fn a_replace_cut_after_its_renames_keeps_the_original_on_the_start_sweep() {
    let case = case();
    trusted(&case, ACME);
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");
    let folder = cut_after_renames(&case, &two);

    start_sweep(&case).unwrap();

    trusted_on_disk(&case, &folder, ACME);
}

/// A replace rolled back while the plugins folder cannot be opened leaves the original
/// aside beside the new folder: once the folder opens again, the start sweep puts the
/// original back.
#[test]
fn a_replace_rolled_back_with_the_plugins_folder_unopenable_keeps_the_original() {
    let mut case = case();
    trusted(&case, ACME);
    let folder = plugins_folder(&case);
    let shut = |mode: u32| {
        let permissions =
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(mode);
        std::fs::set_permissions(&folder, permissions).unwrap();
    };
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");

    replace_rolled_back(&mut case, &KEYS, &two, || shut(0o000));
    shut(0o700);

    let names = names_in(&folder);
    assert!(
        names.iter().any(|name| name.starts_with(".old-acme.")),
        "{names:?}"
    );
    start_sweep(&case).unwrap();
    trusted_on_disk(&case, &folder, ACME);
}

/// A retired folder beside the live one of its name, where the trust row holds neither
/// text, is kept with the live folder, since which one the owner trusted is not known.
#[test]
fn a_retired_folder_neither_text_trusts_is_kept_with_the_live_one() {
    let case = case();
    trusted(&case, ACME);
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");
    let three = ACME.replace("Acme embeddings", "Acme embeddings three");
    let folder = cut_after_renames(&case, &two);
    trust_row_holds(&case, &three);

    start_sweep(&case).unwrap();

    both_kept(&folder, &two);
}

/// A retired folder beside the live one of its name is kept with it when the trust rows do
/// not read, so nothing is deleted without them.
#[test]
fn a_retired_folder_is_kept_when_the_rows_do_not_read() {
    let case = case();
    trusted(&case, ACME);
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");
    let folder = cut_after_renames(&case, &two);
    case.db
        .execute_batch("ALTER TABLE app_plugin_trust RENAME TO app_plugin_trust_away")
        .unwrap();

    let _ = start_sweep(&case);

    both_kept(&folder, &two);
}

/// A committed replace whose older folder could not lose the plugin name leaves it aside
/// beside the live folder the trust row holds: the start sweep still deletes it.
#[test]
fn a_committed_replace_leftover_beside_its_live_plugin_is_still_deleted() {
    let case = case();
    trusted(&case, ACME);
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");
    let folder = cut_after_renames(&case, &two);
    trust_row_holds(&case, &two);

    start_sweep(&case).unwrap();

    trusted_on_disk(&case, &folder, &two);
}

/// A retired folder whose text the trust row holds is kept with the live folder of its name
/// when the live plugin.toml does not read, so a live folder is never deleted unread.
#[test]
fn a_retired_folder_is_kept_when_the_live_one_does_not_read() {
    let case = case();
    trusted(&case, ACME);
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");
    let folder = cut_after_renames(&case, &two);
    std::fs::write(folder.join("acme").join("notes.txt"), b"extra").unwrap();

    start_sweep(&case).unwrap();

    both_kept(&folder, &two);
}

/// A plugin removed while a retired folder of its name was kept beside it stays removed
/// through the next start: the removal takes the plugin name off the retired folder, so the
/// start sweep has nothing to put back.
#[test]
fn a_removed_plugin_with_a_kept_retired_folder_stays_removed_through_a_start() {
    let case = case();
    trusted(&case, ACME);
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");
    let three = ACME.replace("Acme embeddings", "Acme embeddings three");
    let folder = cut_after_renames(&case, &two);
    trust_row_holds(&case, &three);
    start_sweep(&case).unwrap();
    both_kept(&folder, &two);

    remove(&case.db, &case.engine(), "acme").unwrap();
    start_sweep(&case).unwrap();

    let names = names_in(&folder);
    assert!(
        names.iter().all(|name| name != "acme" && !name.starts_with(".old-acme.")),
        "{names:?}"
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    assert!(listing.plugins.iter().all(|plugin| plugin.name != "acme"));
}

/// A removal whose retired folder of the same name cannot lose the plugin name is refused
/// before the live folder or any row changes, so the plugin stays as it was.
#[test]
fn a_removal_whose_retired_folder_cannot_lose_the_name_is_refused_with_nothing_moved() {
    let case = case();
    trusted(&case, ACME);
    let two = ACME.replace("Acme embeddings", "Acme embeddings two");
    let folder = cut_after_renames(&case, &two);
    take_nameless(&folder);

    let removed = remove(&case.db, &case.engine(), "acme");

    assert_eq!(code(removed), "app_plugin_folder_unavailable");
    let retired = format!(".old-acme.{STUCK_ID}");
    let nameless = format!(".old-{STUCK_ID}");
    let mut names = names_in(&folder);
    names.sort();
    assert_eq!(names, [nameless.as_str(), retired.as_str(), "acme"]);
    assert_eq!(
        fixture(
            &folder.join("acme").join(MANIFEST_FILE),
            std::fs::read_to_string
        ),
        two
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "acme").name, "acme");
}
