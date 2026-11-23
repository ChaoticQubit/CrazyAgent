use super::finding_seed;
use super::last_good::{FILE_BYTES_MAX, NAME as SIDE_FILE};
use super::tests::engine_with_projects;
use kernel_processes::finding::Places;
use kernel_processes::finding::test_support::{at_roots, forget_last_good};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The first line of the side file, naming its format and version.
const MAGIC: &[u8] = b"crazyagent places-last-good 1\n";

/// A repository the rows register under an install root, where a failed read with no last
/// good read trusts every path as installed. It need not exist: the read keeps the folder as
/// recorded.
const REPO: &str = "/usr/local/src/ca-last-good-row";

/// A tool inside [`REPO`]. Output: its path.
fn tool() -> PathBuf {
    Path::new(REPO).join("bin/tool")
}

/// A scratch folder holding a data folder `engine` that is not made yet. Output: the
/// scratch folder, kept alive by the caller, and the data folder's real path.
fn data_folder() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::Builder::new()
        .prefix("tools-last-good-")
        .tempdir()
        .unwrap_or_else(|error| panic!("temp folder: {error}"));
    let real = folder
        .path()
        .canonicalize()
        .unwrap_or_else(|error| panic!("{}: {error}", folder.path().display()));
    (folder, real.join("engine"))
}

/// The engine's places at a data folder, reading its engine.db now. Input: the data
/// folder. Output: the places.
fn read(root: &Path) -> Places {
    at_roots(Some(root), None, finding_seed())
}

/// Removes a data folder's engine.db with its WAL files. Input: the data folder.
fn remove_database(root: &Path) {
    for name in ["engine.db", "engine.db-wal", "engine.db-shm"] {
        let file = root.join(name);
        match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                panic!("{}: {error}", file.display())
            }
            _ => {}
        }
    }
}

/// Replaces a data folder's engine.db with text that is no database, so every read of the
/// set-up projects fails, and drops the folder's in-memory last good read, as a new engine
/// process starts with none. Input: the data folder.
fn restart_unreadable(root: &Path) {
    remove_database(root);
    let database = root.join("engine.db");
    std::fs::write(&database, "not a database, just text long enough to read")
        .unwrap_or_else(|error| panic!("{}: {error}", database.display()));
    forget_last_good(root);
}

/// The side file's bytes for a list of folders: the first line, then each folder's bytes
/// followed by a NUL. Input: the folders. Output: the bytes.
fn side_bytes(folders: &[&Path]) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = MAGIC.to_vec();
    for folder in folders {
        bytes.extend_from_slice(folder.as_os_str().as_bytes());
        bytes.push(0);
    }
    bytes
}

/// Writes a file with the given bytes and mode. Inputs: the file, the bytes and the mode.
fn plant(file: &Path, bytes: &[u8], mode: u32) {
    std::fs::write(file, bytes).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
}

/// A repository the engine registered under an install root stays one agents can write
/// after the engine restarts while engine.db cannot be read: the good read left the list in
/// a 0600 side file in the data folder, and the next process's failed first read takes it.
/// With the side file gone, the same failed read trusts the install root again.
#[test]
fn a_registered_repository_under_an_install_root_stays_refused_after_a_restart() {
    let (_folder, root) = data_folder();
    engine_with_projects(&root, &[Path::new(REPO)]);
    assert!(read(&root).agent_writable(&tool()));
    let side = root.join(SIDE_FILE);
    let metadata = std::fs::symlink_metadata(&side)
        .unwrap_or_else(|error| panic!("{}: {error}", side.display()));
    assert!(metadata.is_file(), "{}", side.display());
    assert_eq!(metadata.mode() & 0o777, 0o600);
    restart_unreadable(&root);
    assert!(
        read(&root).agent_writable(&tool()),
        "the side file outlives the process"
    );
    forget_last_good(&root);
    std::fs::remove_file(&side).unwrap_or_else(|error| panic!("{}: {error}", side.display()));
    assert!(
        !read(&root).agent_writable(&tool()),
        "with no side file the failed first read gains nothing"
    );
}

/// A side file only adds refusals: one naming a folder makes a tool under it agents' to
/// write during a failed read, and a tool elsewhere gets the same answer as with no file.
#[test]
fn a_planted_side_file_refuses_only_under_the_folders_it_names() {
    let other = Path::new("/usr/local/src/ca-last-good-other/bin/tool");
    let (_bare_folder, bare) = data_folder();
    let (_planted_folder, planted) = data_folder();
    for root in [&bare, &planted] {
        engine_with_projects(root, &[]);
        restart_unreadable(root);
    }
    plant(
        &planted.join(SIDE_FILE),
        &side_bytes(&[Path::new(REPO)]),
        0o600,
    );
    let without = read(&bare);
    let with = read(&planted);
    assert!(!without.agent_writable(&tool()));
    assert!(with.agent_writable(&tool()));
    assert_eq!(without.agent_writable(other), with.agent_writable(other));
    assert!(!with.agent_writable(other));
}

/// A side file is ignored, giving the answer of no file, when it is a link, others can
/// read or write it, it has a second name, it is past the byte bound, or its text is not
/// the format: a wrong first line, a relative folder or an empty one.
#[test]
fn an_unsafe_or_malformed_side_file_is_ignored() {
    let (_keep, kept) = data_folder();
    std::fs::create_dir_all(&kept).unwrap_or_else(|error| panic!("{}: {error}", kept.display()));
    let good = side_bytes(&[Path::new(REPO)]);
    let elsewhere = kept.join("held");
    plant(&elsewhere, &good, 0o600);
    let mut wrong_magic = b"crazyagent places-last-good 2\n".to_vec();
    wrong_magic.extend_from_slice(&good[MAGIC.len()..]);
    let relative = side_bytes(&[Path::new(REPO), Path::new("usr/local/src/repo")]);
    let mut empty = good.clone();
    empty.push(0);
    let cases: Vec<(&str, Box<dyn Fn(&Path)>)> = vec![
        (
            "link",
            Box::new(|side: &Path| {
                std::os::unix::fs::symlink(&elsewhere, side)
                    .unwrap_or_else(|error| panic!("{}: {error}", side.display()))
            }),
        ),
        (
            "group write",
            Box::new(|side: &Path| plant(side, &good, 0o620)),
        ),
        (
            "other read",
            Box::new(|side: &Path| plant(side, &good, 0o604)),
        ),
        (
            "second name",
            Box::new(|side: &Path| {
                let twin = kept.join("twin");
                plant(&twin, &good, 0o600);
                std::fs::hard_link(&twin, side)
                    .unwrap_or_else(|error| panic!("{}: {error}", side.display()))
            }),
        ),
        (
            "past the bound",
            Box::new(|side: &Path| {
                plant(side, &good, 0o600);
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(side)
                    .and_then(|file| file.set_len(FILE_BYTES_MAX + 1))
                    .unwrap_or_else(|error| panic!("{}: {error}", side.display()))
            }),
        ),
        (
            "wrong first line",
            Box::new(|side: &Path| plant(side, &wrong_magic, 0o600)),
        ),
        (
            "relative folder",
            Box::new(|side: &Path| plant(side, &relative, 0o600)),
        ),
        (
            "empty folder",
            Box::new(|side: &Path| plant(side, &empty, 0o600)),
        ),
    ];
    for (name, make) in &cases {
        let (_folder, root) = data_folder();
        engine_with_projects(&root, &[]);
        restart_unreadable(&root);
        make(&root.join(SIDE_FILE));
        assert!(!read(&root).agent_writable(&tool()), "{name}");
    }
}

/// A good read keeps the side file current and writes it only when the list changed: no
/// project and no file is no write, and the data folder is never made; the same list again,
/// in this process or the next, leaves the file as it was; a list without the repository
/// drops it from the file.
#[test]
fn the_side_file_is_written_only_when_the_good_read_changed() {
    let (_folder, root) = data_folder();
    assert!(!read(&root).agent_writable(&tool()));
    assert!(!root.exists(), "no read makes the data folder");
    engine_with_projects(&root, &[]);
    assert!(!read(&root).agent_writable(&tool()));
    let side = root.join(SIDE_FILE);
    assert!(
        !side.exists(),
        "an empty list and no file are the same list"
    );

    remove_database(&root);
    engine_with_projects(&root, &[Path::new(REPO)]);
    assert!(read(&root).agent_writable(&tool()));
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(&side)
        .and_then(|file| file.set_modified(old))
        .unwrap_or_else(|error| panic!("{}: {error}", side.display()));
    let modified = |file: &Path| {
        std::fs::metadata(file)
            .and_then(|metadata| metadata.modified())
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()))
    };
    assert!(read(&root).agent_writable(&tool()));
    assert_eq!(modified(&side), old, "the same list in this process");
    forget_last_good(&root);
    assert!(read(&root).agent_writable(&tool()));
    assert_eq!(modified(&side), old, "the same list in the next process");

    remove_database(&root);
    engine_with_projects(&root, &[]);
    assert!(!read(&root).agent_writable(&tool()));
    restart_unreadable(&root);
    assert!(
        !read(&root).agent_writable(&tool()),
        "the repository left the file with the list"
    );
}

/// The side file is written as a new 0600 file renamed over the name, so a link planted at
/// the name is replaced and the file it points to is left as it was.
#[test]
fn a_link_planted_at_the_side_file_name_is_replaced_and_its_target_kept() {
    let (_folder, root) = data_folder();
    engine_with_projects(&root, &[Path::new(REPO)]);
    let target = root.with_file_name("target");
    plant(&target, b"kept", 0o600);
    let side = root.join(SIDE_FILE);
    std::os::unix::fs::symlink(&target, &side)
        .unwrap_or_else(|error| panic!("{}: {error}", side.display()));
    assert!(read(&root).agent_writable(&tool()));
    let metadata = std::fs::symlink_metadata(&side)
        .unwrap_or_else(|error| panic!("{}: {error}", side.display()));
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o777, 0o600);
    let held =
        std::fs::read(&target).unwrap_or_else(|error| panic!("{}: {error}", target.display()));
    assert_eq!(held, b"kept");
}
