use std::path::{Path, PathBuf};

use crate::registry::{FOLDER, MANIFEST_FILE, remove_staged};

const ID: &str = "00000000000000000000000000000000000000000000000000000000000000a1";

fn plugins_folder(root: &Path) -> PathBuf {
    let folder = root.join(FOLDER);
    #[cfg(windows)]
    drop(crazyagent_win_sandbox::private::dir(&folder).unwrap());
    #[cfg(not(windows))]
    std::fs::create_dir_all(&folder).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    folder
}

fn folder_with(parent: &Path, name: &str, text: &str) -> PathBuf {
    let folder = parent.join(name);
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join(MANIFEST_FILE), text).unwrap();
    folder
}

fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A replace cut between moving the old folder aside and moving the new one in leaves no
/// folder under the plugin's name. The start sweep moves the old folder back under that
/// name and deletes the staged copy, so the plugin the owner had is kept.
#[test]
fn a_replace_cut_between_its_renames_is_restored_by_the_start_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let plugins = plugins_folder(temp.path());
    folder_with(&plugins, &format!(".add-{ID}"), "new text");
    folder_with(&plugins, &format!(".old-acme.{ID}"), "old text");

    remove_staged(temp.path()).unwrap();

    assert_eq!(names(&plugins), ["acme"]);
    assert_eq!(
        std::fs::read_to_string(plugins.join("acme").join(MANIFEST_FILE)).unwrap(),
        "old text"
    );
}

/// A plain file left under a staged name by an older version does not stop the start
/// sweep: it is deleted, and so is every staged or retired folder read after it.
#[test]
fn a_plain_staged_file_is_deleted_and_the_sweep_goes_on_past_it() {
    let temp = tempfile::tempdir().unwrap();
    let plugins = plugins_folder(temp.path());
    std::fs::write(plugins.join(".add-0"), b"old store copy").unwrap();
    folder_with(&plugins, ".add-1", "staged");
    folder_with(&plugins, ".old-2", "retired with no name");
    std::fs::write(plugins.join(".add-3"), b"old store copy").unwrap();
    folder_with(&plugins, "kept", "kept");

    remove_staged(temp.path()).unwrap();

    assert_eq!(names(&plugins), ["kept"]);
}

/// A retired folder beside the live plugin of its name, whose text the trust row holds, is
/// a leftover of a finished replace. The start sweeps delete it even when it holds a nested
/// folder, and keep the plugin of that name as it is.
#[test]
fn a_retired_leftover_with_a_nested_folder_is_deleted_and_the_plugin_kept() {
    let temp = tempfile::tempdir().unwrap();
    let plugins = plugins_folder(temp.path());
    folder_with(&plugins, "acme", "current text");
    let retired = folder_with(&plugins, &format!(".old-acme.{ID}"), "old text");
    std::fs::create_dir(retired.join("assets")).unwrap();
    std::fs::write(retired.join("assets").join("logo.txt"), b"logo").unwrap();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    crate::store::initialize(&db).unwrap();
    db.execute(
        "INSERT INTO app_plugin_trust VALUES ('acme', ?1, '', 1)",
        [kernel_common::hash(b"current text")],
    )
    .unwrap();

    remove_staged(temp.path()).unwrap();
    crate::registry::sweep_replaced(&db, temp.path()).unwrap();

    assert_eq!(names(&plugins), ["acme"]);
    assert_eq!(
        std::fs::read_to_string(plugins.join("acme").join(MANIFEST_FILE)).unwrap(),
        "current text"
    );
}

/// A retired folder whose name carries no valid plugin name, or none at all, cannot be
/// put back, so the sweep deletes it and never creates a plugin under an invalid name.
#[test]
fn a_retired_folder_with_no_valid_name_is_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let plugins = plugins_folder(temp.path());
    folder_with(&plugins, &format!(".old-{ID}"), "no name");
    folder_with(&plugins, &format!(".old-Bad_Name.{ID}"), "bad name");
    folder_with(&plugins, &format!(".old-.{ID}"), "empty name");

    remove_staged(temp.path()).unwrap();

    assert!(names(&plugins).is_empty(), "{:?}", names(&plugins));
}
