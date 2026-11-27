#![cfg(unix)]
use super::*;
use std::os::unix::fs::PermissionsExt;

/// The checks the retire rows pass: only the private-folder check is asked, as 2.4.0 made
/// the old folder, and any other hook call fails the row.
const HOOKS: Hooks = Hooks {
    reported_version: |root| panic!("retire asked the version at {}", root.display()),
    prepare_root: |root| panic!("retire prepared {}", root.display()),
    private_folder: |path| panic!("retire made {}", path.display()),
    is_private_folder: |path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.is_dir() && metadata.permissions().mode() & 0o777 == 0o700
        })
    },
};

/// A data root holding an old copy in `bin` and the kept copy in `setup/bin`, and an empty
/// command line link folder, all under one durable folder. Output: the folder, the data
/// root, the old copy, the kept copy and the link folder.
fn old_and_kept() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let temp = durable_folder();
    let root = temp.path().join("root");
    let private = |folder: &Path| {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    private(&root.join("bin"));
    private(&root.join("setup").join("bin"));
    let old = root.join("bin").join("crazyagent");
    let kept = root.join("setup").join("bin").join("crazyagent");
    for file in [&old, &kept] {
        write_mode(file, "#!/bin/sh\n", 0o755);
    }
    let cli = temp.path().join("home").join(".local").join("bin");
    std::fs::create_dir_all(&cli).unwrap();
    (temp, root, old, kept, cli)
}

/// A link that names the old copy relatively from its own folder is pointed at the kept
/// copy, and the old folder goes.
#[test]
fn a_relative_link_to_the_old_copy_is_pointed_at_the_kept_one() {
    let (_temp, root, _old, kept, cli) = old_and_kept();
    let link = cli.join("crazyagent");
    std::os::unix::fs::symlink("../../../root/bin/crazyagent", &link).unwrap();
    retire_old_copies_with(&root, &kept, Some(&cli), &HOOKS);
    assert_eq!(std::fs::read_link(&link).unwrap(), kept);
    assert!(!root.join("bin").exists());
}

/// A link that reaches the old copy through a second link is pointed at the kept copy, and
/// the old folder goes.
#[test]
fn a_link_reaching_the_old_copy_through_another_link_is_pointed_at_the_kept_one() {
    let (temp, root, old, kept, cli) = old_and_kept();
    let hop = temp.path().join("hop");
    std::os::unix::fs::symlink(&old, &hop).unwrap();
    let link = cli.join("crazyagent");
    std::os::unix::fs::symlink(&hop, &link).unwrap();
    retire_old_copies_with(&root, &kept, Some(&cli), &HOOKS);
    assert_eq!(std::fs::read_link(&link).unwrap(), kept);
    assert!(!root.join("bin").exists());
}

/// A link to another hard link of the old copy ends at the same file, so it is pointed at
/// the kept copy too.
#[test]
fn a_link_to_another_hard_link_of_the_old_copy_is_pointed_at_the_kept_one() {
    let (temp, root, old, kept, cli) = old_and_kept();
    let hard = temp.path().join("hard");
    std::fs::hard_link(&old, &hard).unwrap();
    let link = cli.join("crazyagent");
    std::os::unix::fs::symlink(&hard, &link).unwrap();
    retire_old_copies_with(&root, &kept, Some(&cli), &HOOKS);
    assert_eq!(std::fs::read_link(&link).unwrap(), kept);
    assert!(!root.join("bin").exists());
}

/// A link to an unrelated file is left as it was, and the old folder still goes.
#[test]
fn a_link_to_an_unrelated_file_is_left_as_it_was() {
    let (temp, root, _old, kept, cli) = old_and_kept();
    let other = temp.path().join("other");
    write_mode(&other, "#!/bin/sh\n", 0o755);
    let link = cli.join("crazyagent");
    std::os::unix::fs::symlink(&other, &link).unwrap();
    retire_old_copies_with(&root, &kept, Some(&cli), &HOOKS);
    assert_eq!(std::fs::read_link(&link).unwrap(), other);
    assert!(!root.join("bin").exists());
}

/// An old copy that is itself a link never counts, though the command line link ends at
/// the same file it names: the command line link is left as it was.
#[test]
fn an_old_copy_that_is_a_link_never_counts() {
    let (temp, root, old, kept, cli) = old_and_kept();
    let other = temp.path().join("other");
    write_mode(&other, "#!/bin/sh\n", 0o755);
    std::fs::remove_file(&old).unwrap();
    std::os::unix::fs::symlink(&other, &old).unwrap();
    let link = cli.join("crazyagent");
    std::os::unix::fs::symlink(&other, &link).unwrap();
    retire_old_copies_with(&root, &kept, Some(&cli), &HOOKS);
    assert_eq!(std::fs::read_link(&link).unwrap(), other);
}
