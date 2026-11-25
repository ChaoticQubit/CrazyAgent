use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crazyagent_win_sandbox::private;

use crate::Refused;
use crate::registry::{Engine, FOLDER, MANIFEST_FILE, add, list, ready, remove, remove_staged};
use crate::store::{AccountKey, KeyFolder, KeyRef, Keys, initialize};
use crate::views::{Listing, PluginStatus, PluginView};

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

const ID: &str = "00000000000000000000000000000000000000000000000000000000000000b2";

const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

fn no_saved(_: &Path, _: KeyFolder, _: &KeyRef) -> Result<bool, ()> {
    Ok(false)
}

fn no_account(_: &rusqlite::Connection, _: &KeyRef) -> Result<Option<AccountKey>, ()> {
    Ok(None)
}

fn no_replace(_: &Path, _: KeyFolder, _: &KeyRef, _: Vec<u8>) -> Result<(), ()> {
    Err(())
}

fn no_remove(_: &Path, _: KeyFolder, _: &KeyRef) -> Result<(), ()> {
    Ok(())
}

fn no_read(
    _: &Path,
    _: KeyFolder,
    _: &KeyRef,
) -> Result<Option<kernel_security::secret::SecretString>, ()> {
    Ok(None)
}

fn no_sweep(_: &Path, _: KeyFolder) -> Result<(), ()> {
    Ok(())
}

fn next_ref() -> Result<String, &'static str> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    Ok(format!("{:064x}", NEXT.fetch_add(1, Ordering::Relaxed)))
}

/// A key store that saves no key, since these rows add only a plugin that declares none.
const KEYS: Keys = Keys {
    saved: no_saved,
    account: no_account,
    replace: no_replace,
    remove: no_remove,
    read: no_read,
    sweep: no_sweep,
    new_ref: next_ref,
};

struct Case {
    temp: tempfile::TempDir,
    root: PathBuf,
    db: rusqlite::Connection,
    _held: crate::store_log::RowsHold,
}

impl Case {
    fn engine(&self) -> Engine<'_> {
        Engine {
            root: &self.root,
            keys: &KEYS,
            handed_out: None,
        }
    }

    fn plugins(&self) -> PathBuf {
        let folder = self.root.join(FOLDER);
        drop(private::dir(&folder).unwrap());
        folder
    }

    /// A folder outside the engine root holding one plugin.toml with the text. Inputs: a
    /// folder name unique in the row and the text. Output: the folder.
    fn outside(&self, name: &str, text: &str) -> PathBuf {
        let folder = self.temp.path().join(name);
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join(MANIFEST_FILE), text).unwrap();
        folder
    }
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("engine");
    std::fs::create_dir(&root).unwrap();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    Case {
        temp,
        root,
        db,
        _held: crate::store_log::rows_hold(),
    }
}

fn sha(text: &str) -> String {
    kernel_common::hash(text.as_bytes())
}

fn add_text(case: &Case, text: &str, replace: bool, now_ms: i64) -> Result<(), Refused> {
    let source = case.outside(&format!("picked-{now_ms}"), text);
    add(
        &case.db,
        &case.engine(),
        &source,
        &sha(text),
        replace,
        now_ms,
    )
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

fn code<T: std::fmt::Debug>(result: Result<T, Refused>) -> &'static str {
    result.expect_err("refused").code
}

fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Runs `mklink` through cmd.exe with each path as its own quoted argument. A path with `%`
/// or `!` is refused before cmd.exe starts, because cmd.exe expands them inside quotes; one
/// with a forward slash, which mklink reads as a switch, the quote itself or `&`, `|`, `^`,
/// `<`, `>` is refused as well, so a path is never read as a command; `(` and `)` pass.
/// Inputs: the switch, if any, the link and the target. Output: whether mklink made the
/// link; a path that is not UTF-8 or a cmd.exe that cannot start stops the row naming it.
fn mklink(switch: Option<&str>, link: &Path, target: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    let quoted = |path: &Path| {
        let text = path
            .to_str()
            .unwrap_or_else(|| panic!("{} is not UTF-8", path.display()));
        assert!(
            !text.contains(['/', '"', '&', '|', '^', '<', '>', '%', '!']),
            "{text}"
        );
        format!("\"{text}\"")
    };
    let mut command = Command::new("cmd");
    command.args(["/D", "/C", "mklink"]);
    if let Some(switch) = switch {
        command.arg(switch);
    }
    command
        .raw_arg(quoted(link))
        .raw_arg(quoted(target))
        .status()
        .unwrap_or_else(|error| panic!("mklink {} {}: {error}", link.display(), target.display()))
        .success()
}

/// A junction under a folder whose name holds parentheses is made by `mklink` and leads to
/// its target: inside the quotes cmd.exe reads `(` and `)` as part of the path.
#[test]
fn windows_mklink_takes_a_path_with_parentheses() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("scratch folder: {error}"));
    let parent = temp.path().join("a (1)");
    let target = parent.join("target");
    fixture(&target, std::fs::create_dir_all);
    let marker = target.join("marker");
    fixture(&marker, |path| std::fs::write(path, b"marker"));
    let link = parent.join("link");

    let made = mklink(Some("/J"), &link, &target);

    assert!(made, "junction {}", link.display());
    assert_eq!(fixture(&link.join("marker"), std::fs::read), b"marker");
}

/// Makes a junction at `link` to the folder `target` with `mklink /J`.
fn junction(link: &Path, target: &Path) {
    assert!(
        mklink(Some("/J"), link, target),
        "junction {}",
        link.display()
    );
}

/// Makes a symbolic link at `link` to `target`, a folder or a file, falling back to
/// `mklink` when the std call is refused.
fn symlink(link: &Path, target: &Path, folder: bool) {
    let made = if folder {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    if made.is_err() {
        let switch = folder.then_some("/D");
        assert!(mklink(switch, link, target), "symlink {}", link.display());
    }
}

/// An added plugin lists with the text it was added with and loads, a second add under its
/// name is refused, and the plugins folder, the plugin folder and its plugin.toml are all
/// private to the user.
#[test]
fn windows_lists_reads_and_writes_a_user_plugin() {
    let case = case();
    add_text(&case, NEAR, false, 1).unwrap();

    let listing = list(&case.db, &case.engine()).unwrap();
    let near = plugin(&listing, "near");
    assert_eq!(near.sha256, sha(NEAR));
    assert!(
        !matches!(near.status, PluginStatus::Refused { .. }),
        "{near:?}"
    );
    ready(&case.db, &case.engine(), "near").unwrap();
    assert!(add_text(&case, NEAR, false, 2).is_err());

    let plugins = case.root.join(FOLDER);
    let folder = private::open_dir(&plugins).unwrap();
    private::check(&folder).unwrap();
    private::check(&private::open_dir(&plugins.join("near")).unwrap()).unwrap();
    let manifest = plugins.join("near").join(MANIFEST_FILE);
    private::check(&private::read_only(&manifest).unwrap()).unwrap();
    assert_eq!(fixture(&manifest, std::fs::read_to_string), NEAR);
}

/// A plugins folder that is a junction or a directory link is refused as unsafe on the
/// list and on an add, and the folder it points to is neither read nor written.
#[test]
fn windows_refuses_a_junction_or_link_as_the_plugins_folder() {
    for linked in [true, false] {
        let case = case();
        let target = case.temp.path().join("elsewhere");
        drop(private::dir(&target).unwrap());
        let held = target.join("near");
        std::fs::create_dir(&held).unwrap();
        std::fs::write(held.join(MANIFEST_FILE), NEAR).unwrap();
        let plugins = case.root.join(FOLDER);
        if linked {
            symlink(&plugins, &target, true);
        } else {
            junction(&plugins, &target);
        }

        assert_eq!(
            code(list(&case.db, &case.engine())),
            "app_plugin_folder_unsafe"
        );
        let other = NEAR.replace("plugin = \"near\"", "plugin = \"other\"");
        assert_eq!(
            code(add_text(&case, &other, false, 1)),
            "app_plugin_folder_unsafe"
        );

        assert_eq!(names(&target), ["near"]);
        assert_eq!(
            std::fs::read_to_string(held.join(MANIFEST_FILE)).unwrap(),
            NEAR
        );
    }
}

/// A plugin entry that is a junction or a directory link is listed refused as unsafe, the
/// plugin.toml it points to is never loaded, and removing it deletes the link alone.
#[test]
fn windows_refuses_a_junction_or_link_as_a_plugin_folder() {
    let case = case();
    let plugins = case.plugins();
    let target = case.outside("target", NEAR);
    junction(&plugins.join("joined"), &target);
    symlink(&plugins.join("linked"), &target, true);

    let listing = list(&case.db, &case.engine()).unwrap();
    for name in ["joined", "linked"] {
        assert_eq!(
            refused_code(plugin(&listing, name)),
            "app_plugin_file_unsafe",
            "{name}"
        );
        assert_eq!(
            code(ready(&case.db, &case.engine(), name)),
            "app_plugin_refused"
        );
        remove(&case.db, &case.engine(), name).unwrap();
        assert!(
            std::fs::symlink_metadata(plugins.join(name)).is_err(),
            "{name}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(target.join(MANIFEST_FILE)).unwrap(),
        NEAR
    );
}

/// A plugin.toml that is a file link is refused as unsafe, and a junction beside a
/// plugin.toml refuses its folder for holding another entry; the targets stay untouched.
#[test]
fn windows_refuses_a_link_inside_a_plugin_folder() {
    let case = case();
    let plugins = case.plugins();
    let target = case.outside("target", NEAR);
    let near = plugins.join("near");
    std::fs::create_dir(&near).unwrap();
    symlink(
        &near.join(MANIFEST_FILE),
        &target.join(MANIFEST_FILE),
        false,
    );
    let local = plugins.join("local");
    std::fs::create_dir(&local).unwrap();
    let text = NEAR.replace("plugin = \"near\"", "plugin = \"local\"");
    std::fs::write(local.join(MANIFEST_FILE), &text).unwrap();
    junction(&local.join("lib"), &target);

    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(
        refused_code(plugin(&listing, "near")),
        "app_plugin_file_unsafe"
    );
    assert_eq!(
        refused_code(plugin(&listing, "local")),
        "plugin_folder_extra_entry"
    );
    assert_eq!(names(&target), [MANIFEST_FILE]);
    assert_eq!(
        std::fs::read_to_string(target.join(MANIFEST_FILE)).unwrap(),
        NEAR
    );
}

/// Replacing a trusted plugin leaves one folder under its name holding the new text, no
/// staged or retired folder, and the new text trusted, so it waits for no review.
#[test]
fn windows_a_replace_keeps_one_folder_and_settles_trust() {
    let case = case();
    add_text(&case, NEAR, false, 1).unwrap();
    let text = NEAR.replace("title = \"Near\"", "title = \"Near two\"");

    add_text(&case, &text, true, 2).unwrap();

    let plugins = case.root.join(FOLDER);
    assert_eq!(names(&plugins), ["near"]);
    assert_eq!(
        std::fs::read_to_string(plugins.join("near").join(MANIFEST_FILE)).unwrap(),
        text
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    let near = plugin(&listing, "near");
    assert_eq!(near.sha256, sha(&text));
    assert_ne!(near.status, PluginStatus::NeedsReview);
    assert!(
        !matches!(near.status, PluginStatus::Changed { .. }),
        "{:?}",
        near.status
    );
}

/// A replace while another program holds the old folder without sharing delete is
/// refused as unavailable: the old plugin.toml reads as before and no staged folder is
/// left.
#[test]
fn windows_a_replace_over_a_held_old_folder_refuses_and_keeps_it() {
    let case = case();
    add_text(&case, NEAR, false, 1).unwrap();
    let plugins = case.root.join(FOLDER);
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(plugins.join("near"))
        .unwrap();
    let text = NEAR.replace("title = \"Near\"", "title = \"Near two\"");

    assert_eq!(
        code(add_text(&case, &text, true, 2)),
        "app_plugin_folder_unavailable"
    );

    drop(held);
    assert_eq!(names(&plugins), ["near"]);
    assert_eq!(
        std::fs::read_to_string(plugins.join("near").join(MANIFEST_FILE)).unwrap(),
        NEAR
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    assert_eq!(plugin(&listing, "near").sha256, sha(NEAR));
}

/// Deleting a retired folder that holds a junction to a folder outside deletes the
/// junction and the retired folder, and every file in the outside folder stays.
#[test]
fn windows_deleting_a_retired_folder_unlinks_a_junction_in_it_and_keeps_its_target() {
    let case = case();
    let plugins = case.plugins();
    let outside = case.outside("outside", NEAR);
    std::fs::write(outside.join("notes.txt"), b"kept").unwrap();
    let retired = plugins.join(format!(".old-{ID}"));
    std::fs::create_dir(&retired).unwrap();
    std::fs::write(retired.join(MANIFEST_FILE), NEAR).unwrap();
    junction(&retired.join("lib"), &outside);

    remove_staged(&case.root).unwrap();

    assert!(names(&plugins).is_empty(), "{:?}", names(&plugins));
    assert_eq!(names(&outside), ["notes.txt", MANIFEST_FILE]);
    assert_eq!(
        std::fs::read_to_string(outside.join(MANIFEST_FILE)).unwrap(),
        NEAR
    );
}

/// A replace cut after the old folder was moved aside and before the new one moved in
/// leaves no folder under the plugin's name; the start sweep moves the old folder back,
/// deletes the staged copy, and the plugin lists as it was trusted.
#[test]
fn windows_the_sweep_restores_a_retired_folder_cut_between_renames() {
    let case = case();
    add_text(&case, NEAR, false, 1).unwrap();
    let plugins = case.root.join(FOLDER);
    std::fs::rename(
        plugins.join("near"),
        plugins.join(format!(".old-near.{ID}")),
    )
    .unwrap();
    let staged = plugins.join(format!(".add-{ID}"));
    std::fs::create_dir(&staged).unwrap();
    std::fs::write(staged.join(MANIFEST_FILE), "new text").unwrap();

    remove_staged(&case.root).unwrap();

    assert_eq!(names(&plugins), ["near"]);
    assert_eq!(
        std::fs::read_to_string(plugins.join("near").join(MANIFEST_FILE)).unwrap(),
        NEAR
    );
    let listing = list(&case.db, &case.engine()).unwrap();
    let near = plugin(&listing, "near");
    assert_eq!(near.sha256, sha(NEAR));
    assert!(
        !matches!(near.status, PluginStatus::Refused { .. }),
        "{near:?}"
    );
}

/// Reads a fixture a row checks. Inputs: its path and the read. Output: what the read
/// returned; a failed read stops the row naming the path and the I/O error.
fn fixture<'p, T>(path: &'p Path, read: impl FnOnce(&'p Path) -> std::io::Result<T>) -> T {
    read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Leaves read-only as the one attribute of a file or folder, as an unzipped or copied
/// plugin can carry it, with `attrib`, the path with backslashes only.
fn read_only(path: &Path) {
    assert!(!path.to_string_lossy().contains('/'));
    let status = Command::new("attrib")
        .args(["-A", "+R"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "attrib {}", path.display());
    assert!(std::fs::metadata(path).unwrap().permissions().readonly());
}

/// Puts a read-only file in a plugin folder and a read-only folder holding a read-only
/// file below it. Input: the plugin folder.
fn read_only_entries(folder: &Path) {
    std::fs::write(folder.join("notes.txt"), b"copied").unwrap();
    read_only(&folder.join("notes.txt"));
    let lib = folder.join("lib");
    std::fs::create_dir(&lib).unwrap();
    std::fs::write(lib.join("data.bin"), b"copied").unwrap();
    read_only(&lib.join("data.bin"));
    read_only(&lib);
}

/// Removing a plugin whose folder holds read-only files and a read-only folder deletes
/// the whole folder: nothing stays aside, and the start sweep finds nothing to retry.
#[test]
fn windows_a_read_only_file_does_not_block_removing_a_plugin() {
    let case = case();
    add_text(&case, NEAR, false, 1).unwrap();
    let plugins = case.root.join(FOLDER);
    read_only_entries(&plugins.join("near"));

    remove(&case.db, &case.engine(), "near").unwrap();

    assert!(names(&plugins).is_empty(), "{:?}", names(&plugins));
    remove_staged(&case.root).unwrap();
    assert!(names(&plugins).is_empty(), "{:?}", names(&plugins));
}

/// A replace over a plugin folder that holds read-only files and a read-only folder
/// keeps one folder with the new text and leaves no retired folder aside.
#[test]
fn windows_a_replace_over_a_folder_with_a_read_only_file_leaves_nothing_aside() {
    let case = case();
    add_text(&case, NEAR, false, 1).unwrap();
    let plugins = case.root.join(FOLDER);
    read_only_entries(&plugins.join("near"));
    let text = NEAR.replace("title = \"Near\"", "title = \"Near two\"");

    add_text(&case, &text, true, 2).unwrap();

    assert_eq!(names(&plugins), ["near"]);
    assert_eq!(
        std::fs::read_to_string(plugins.join("near").join(MANIFEST_FILE)).unwrap(),
        text
    );
    remove_staged(&case.root).unwrap();
    assert_eq!(names(&plugins), ["near"]);
}

/// Gives every account write on one entry, the entry alone and nothing made in it later.
/// Input: the path.
fn everyone_writes(path: &Path) {
    let status = Command::new("icacls")
        .arg(path)
        .args(["/grant", "*S-1-1-0:(W)"])
        .status()
        .unwrap();
    assert!(status.success(), "icacls {}", path.display());
}

/// A plugin folder, or its plugin.toml, that another account may write is listed refused as
/// unsafe: a folder is refused before its plugin.toml is opened, so one with that file gone
/// is still refused as unsafe and not as a missing manifest. What the app made passes.
#[test]
fn windows_a_plugin_folder_another_account_can_write_is_refused() {
    for folder in [true, false] {
        let case = case();
        add_text(&case, NEAR, false, 1).unwrap();
        let listing = list(&case.db, &case.engine()).unwrap();
        let near = plugin(&listing, "near");
        assert!(
            !matches!(near.status, PluginStatus::Refused { .. }),
            "{near:?}"
        );
        let path = case.root.join(FOLDER).join("near");
        let manifest = path.join(MANIFEST_FILE);
        if folder {
            std::fs::remove_file(&manifest).unwrap();
            everyone_writes(&path);
        } else {
            everyone_writes(&manifest);
        }

        let listing = list(&case.db, &case.engine()).unwrap();
        assert_eq!(
            refused_code(plugin(&listing, "near")),
            "app_plugin_file_unsafe",
            "folder {folder}"
        );
        assert!(ready(&case.db, &case.engine(), "near").is_err());
    }
}

/// A read-only file in a plugin folder that is also a name of a file outside the plugins
/// folder is never changed: replacing the plugin and the start sweep leave the outside file
/// read-only with its bytes, the shared attribute never cleared.
#[test]
fn windows_a_read_only_file_hard_linked_from_outside_keeps_its_attribute() {
    let case = case();
    add_text(&case, NEAR, false, 1).unwrap();
    let outside = case.temp.path().join("outside.txt");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::hard_link(
        &outside,
        case.root.join(FOLDER).join("near").join("linked.txt"),
    )
    .unwrap();
    read_only(&outside);
    let text = NEAR.replace("title = \"Near\"", "title = \"Near two\"");

    add_text(&case, &text, true, 2).unwrap();
    let _ = remove_staged(&case.root);

    assert!(
        std::fs::metadata(&outside)
            .unwrap()
            .permissions()
            .readonly(),
        "the outside file keeps its read-only attribute"
    );
    assert_eq!(fixture(&outside, std::fs::read), b"outside");
}
