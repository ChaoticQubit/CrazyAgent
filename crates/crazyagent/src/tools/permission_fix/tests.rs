use super::*;
use crate::tools::{Places, Refusal, Refused};
use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn set(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn file(path: &Path, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
    set(path, mode);
}

fn folder(path: &Path, mode: u32) {
    std::fs::create_dir_all(path).unwrap();
    set(path, mode);
}

fn mine() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A fixture HOME in its own temp folder, with a uv-like Python prefix five folders below
/// it whose interpreter and folders are group writable. Output: the temp folder, HOME and
/// the prefix.
fn uv_like() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let prefix = home.join(".local/share/uv/python/cpython-9.9.9-dummy");
    file(&prefix.join("bin/python3"), 0o775);
    folder(&prefix.join("bin"), 0o775);
    folder(&prefix, 0o775);
    (scratch, home, prefix)
}

fn whole(path: &Path) -> Target {
    Target {
        path: path.to_path_buf(),
        recursive: true,
    }
}

fn only(path: &Path) -> Target {
    Target {
        path: path.to_path_buf(),
        recursive: false,
    }
}

#[test]
fn the_fix_removes_only_group_and_other_write_in_the_tree() {
    let (_scratch, home, prefix) = uv_like();
    file(&prefix.join("lib/python9.9/os.py"), 0o664);
    file(&prefix.join("lib/python9.9/open.py"), 0o666);
    folder(&prefix.join("lib/python9.9/nested"), 0o777);
    file(&prefix.join("bin/setuid"), 0o4775);
    file(&prefix.join("bin/private"), 0o700);
    file(&prefix.join("lib/readonly.txt"), 0o444);
    folder(&prefix.join("lib"), 0o755);
    folder(&prefix.join("lib/python9.9"), 0o755);
    let places = Places::new(Vec::new(), Some(home.clone()));

    let report = fix(&whole(&prefix), &places).unwrap();

    assert_eq!(mode(&prefix), 0o755);
    assert_eq!(mode(&prefix.join("bin")), 0o755);
    assert_eq!(mode(&prefix.join("bin/python3")), 0o755);
    assert_eq!(mode(&prefix.join("lib/python9.9/os.py")), 0o644);
    assert_eq!(mode(&prefix.join("lib/python9.9/open.py")), 0o644);
    assert_eq!(mode(&prefix.join("lib/python9.9/nested")), 0o755);
    assert_eq!(mode(&prefix.join("bin/setuid")), 0o4755);
    assert_eq!(mode(&prefix.join("bin/private")), 0o700);
    assert_eq!(mode(&prefix.join("lib/readonly.txt")), 0o444);
    assert_eq!(report.changed, 7);
    assert_eq!(report.skipped_links, 0);
    assert_eq!(report.skipped_foreign, 0);
}

#[test]
fn the_fix_never_follows_a_link_out_of_the_tree() {
    let (_scratch, home, prefix) = uv_like();
    let outside_file = home.join("outside/open.txt");
    file(&outside_file, 0o666);
    let outside_folder = home.join("outside/open");
    folder(&outside_folder, 0o777);
    std::os::unix::fs::symlink(&outside_file, prefix.join("bin/linked")).unwrap();
    std::os::unix::fs::symlink(&outside_folder, prefix.join("linked-folder")).unwrap();
    let places = Places::new(Vec::new(), Some(home.clone()));

    let report = fix(&whole(&prefix), &places).unwrap();

    assert_eq!(mode(&outside_file), 0o666);
    assert_eq!(mode(&outside_folder), 0o777);
    assert_eq!(mode(&prefix.join("bin/python3")), 0o755);
    assert_eq!(report.skipped_links, 2);
}

#[test]
fn the_fix_skips_entries_another_user_owns_and_never_descends_into_them() {
    let (_scratch, home, prefix) = uv_like();
    let foreign_file = prefix.join("bin/theirs");
    file(&foreign_file, 0o775);
    let foreign_folder = prefix.join("share");
    folder(&foreign_folder, 0o777);
    let below_foreign = foreign_folder.join("inner.txt");
    file(&below_foreign, 0o666);
    let foreign = [
        std::fs::metadata(&foreign_file).unwrap().ino(),
        std::fs::metadata(&foreign_folder).unwrap().ino(),
    ];
    let me = mine();
    let owned = |uid: u32, inode: u64| uid == me && !foreign.contains(&inode);

    let report = walk(&home, &prefix, true, Bounds::default(), &owned).unwrap();

    assert_eq!(mode(&foreign_file), 0o775);
    assert_eq!(mode(&foreign_folder), 0o777);
    assert_eq!(mode(&below_foreign), 0o666);
    assert_eq!(mode(&prefix.join("bin/python3")), 0o755);
    assert_eq!(report.skipped_foreign, 2);
}

#[test]
fn the_fix_refuses_a_chain_from_home_with_a_link_or_a_part_another_user_owns() {
    let (_scratch, home, prefix) = uv_like();
    let real_share = home.join("real-share");
    folder(&real_share, 0o755);
    file(&real_share.join("tool/bin/python3"), 0o775);
    std::os::unix::fs::symlink(&real_share, home.join("linked-share")).unwrap();
    let through_link = home.join("linked-share/tool");
    let places = Places::new(Vec::new(), Some(home.clone()));

    assert_eq!(fix(&whole(&through_link), &places), Err(TOOL_FIX_REFUSED));
    assert_eq!(mode(&real_share.join("tool/bin/python3")), 0o775);

    let parent = home.join(".local").metadata().unwrap().ino();
    let me = mine();
    let owned = |uid: u32, inode: u64| uid == me && inode != parent;
    assert_eq!(
        walk(&home, &prefix, true, Bounds::default(), &owned),
        Err(TOOL_FIX_REFUSED)
    );
    assert_eq!(mode(&prefix.join("bin/python3")), 0o775);
}

#[test]
fn the_fix_refuses_home_itself_a_place_outside_home_and_engine_or_project_trees() {
    let (scratch, home, prefix) = uv_like();
    let outside = scratch.path().join("elsewhere");
    folder(&outside, 0o777);
    let project = home.join("code/app");
    folder(&project.join(".crazyagent"), 0o755);
    let project_bin = project.join("bin");
    folder(&project_bin, 0o777);
    let crazyagent = home.join("work/.crazyagent/tool");
    folder(&crazyagent, 0o777);
    let places = Places::new(Vec::new(), Some(home.clone()));

    for refused in [&home, &outside, &crazyagent, &project_bin] {
        assert_eq!(fix(&whole(refused), &places), Err(TOOL_FIX_REFUSED));
        assert_eq!(fix(&only(refused), &places), Err(TOOL_FIX_REFUSED));
    }
    assert_eq!(mode(&outside), 0o777);
    assert_eq!(mode(&crazyagent), 0o777);
    assert_eq!(mode(&project_bin), 0o777);
    assert!(!allowed(Path::new("/opt/homebrew/opt/python"), &places));
    assert!(!allowed(Path::new("/usr/local/Cellar/python"), &places));
    assert!(!allowed(Path::new("/"), &places));
    assert!(allowed(&prefix, &places));

    let root = home.join("engine");
    let engine = Places::engine(Some(&root), None, Some(home.clone()), Some(OsStr::new("")));
    let setup_python = root.join("setup/uv/python/cpython-9.9.9-dummy");
    folder(&setup_python, 0o775);
    assert!(!allowed(&setup_python, &engine));
    assert!(!allowed(&root.join("agents/one"), &engine));
    assert_eq!(fix(&whole(&setup_python), &engine), Err(TOOL_FIX_REFUSED));
    assert_eq!(mode(&setup_python), 0o775);
}

#[test]
fn the_fix_is_bounded_in_entries_and_depth() {
    let (_scratch, home, prefix) = uv_like();
    for index in 0..12 {
        file(&prefix.join(format!("lib/file-{index}")), 0o664);
    }
    let me = mine();
    let owned = |uid: u32, _inode: u64| uid == me;
    let small = Bounds {
        entries: 8,
        depth: DEPTH_MAX,
    };
    assert_eq!(
        walk(&home, &prefix, true, small, &owned),
        Err(TOOL_FIX_TOO_LARGE)
    );

    let mut deep = prefix.join("deep");
    for _ in 0..DEPTH_MAX + 2 {
        deep = deep.join("d");
    }
    folder(&deep, 0o755);
    assert_eq!(
        walk(&home, &prefix, true, Bounds::default(), &owned),
        Err(TOOL_FIX_TOO_LARGE)
    );
}

#[test]
fn a_fifo_in_the_tree_never_blocks_the_walk_and_is_left_as_it_is() {
    let (_scratch, home, prefix) = uv_like();
    let fifo = prefix.join("bin/pipe");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o666)).unwrap();
    set(&fifo, 0o666);
    let places = Places::new(Vec::new(), Some(home.clone()));

    let report = fix(&whole(&prefix), &places).unwrap();

    assert_eq!(mode(&fifo), 0o666);
    assert_eq!(mode(&prefix.join("bin/python3")), 0o755);
    assert_eq!(report.skipped_other, 1);
}

#[test]
fn a_hard_linked_file_is_tightened_and_counted_shared() {
    let (_scratch, home, prefix) = uv_like();
    let cached = home.join("cache/wheel.py");
    file(&cached, 0o664);
    std::fs::hard_link(&cached, prefix.join("lib.py")).unwrap();
    let places = Places::new(Vec::new(), Some(home.clone()));

    let report = fix(&whole(&prefix), &places).unwrap();

    assert_eq!(mode(&prefix.join("lib.py")), 0o644);
    assert_eq!(report.shared, 1);
}

#[test]
fn a_single_target_changes_only_that_folder() {
    let (_scratch, home, prefix) = uv_like();
    let python = home.join(".local/share/uv/python");
    set(&python, 0o775);
    let places = Places::new(Vec::new(), Some(home.clone()));

    let report = fix(&only(&python), &places).unwrap();

    assert_eq!(mode(&python), 0o755);
    assert_eq!(mode(&prefix), 0o775);
    assert_eq!(mode(&prefix.join("bin/python3")), 0o775);
    assert_eq!(report.changed, 1);
}

#[test]
fn a_refused_interpreter_names_its_prefix_tree_from_the_chain_its_part_is_on() {
    let (_scratch, home, prefix) = uv_like();
    let venv = home.join(".local/share/uv/tools/graphifyy");
    folder(&venv.join("bin"), 0o755);
    let linked = venv.join("bin/python3");
    std::os::unix::fs::symlink(prefix.join("bin/python3"), &linked).unwrap();
    let places = Places::new(Vec::new(), Some(home.clone()));
    let real_part = prefix.join("bin/python3").canonicalize().unwrap();
    let refused = Refused {
        file: linked.clone(),
        why: Refusal::OthersCanChange(real_part.clone()),
    };

    let named = target(&refused, &places).unwrap();

    assert!(named.recursive);
    assert_eq!(named.path, real_part.parent().unwrap().parent().unwrap());

    let above = home.join(".local/share/uv/python");
    let refused_above = Refused {
        file: prefix.join("bin/python3"),
        why: Refusal::OthersCanChange(above.clone()),
    };
    assert_eq!(target(&refused_above, &places), Some(only(&above)));
}

#[test]
fn a_tool_in_a_shallow_bin_folder_names_only_the_part_never_the_whole_folder_above_bin() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let entry = home.join(".local/bin/graphify");
    file(&entry, 0o775);
    let places = Places::new(Vec::new(), Some(home.clone()));
    let refused = Refused {
        file: entry.clone(),
        why: Refusal::OthersCanChange(entry.clone()),
    };

    assert_eq!(target(&refused, &places), Some(only(&entry)));

    let local_bin = home.join(".local/bin");
    let refused_folder = Refused {
        file: entry.clone(),
        why: Refusal::OthersCanChange(local_bin.clone()),
    };
    assert_eq!(target(&refused_folder, &places), Some(only(&local_bin)));
}

#[test]
fn no_target_for_refusals_a_permission_change_cannot_fix() {
    let (_scratch, home, prefix) = uv_like();
    let places = Places::new(Vec::new(), Some(home.clone()));
    for why in [Refusal::AgentsCanWrite, Refusal::Shim, Refusal::InTemp] {
        let refused = Refused {
            file: prefix.join("bin/python3"),
            why,
        };
        assert_eq!(target(&refused, &places), None);
    }
    let elsewhere = Refused {
        file: prefix.join("bin/python3"),
        why: Refusal::OthersCanChange(PathBuf::from("/usr/local/bin")),
    };
    assert_eq!(target(&elsewhere, &places), None);
}

#[test]
fn the_shown_command_quotes_the_path_for_the_shell() {
    let tree = whole(Path::new(
        "/Users/o'neil/.local/share/uv/python/cpython-3.13.1",
    ));
    assert_eq!(
        tree.command(),
        "chmod -R go-w '/Users/o'\\''neil/.local/share/uv/python/cpython-3.13.1'"
    );
    let folder = only(Path::new("/Users/me/.local/bin"));
    assert_eq!(folder.command(), "chmod go-w '/Users/me/.local/bin'");
}

#[test]
fn setup_tightens_its_own_tree_and_nothing_outside_its_setup_folder() {
    let scratch = tempfile::tempdir().unwrap();
    let setup = scratch.path().join("engine/setup");
    let python = setup.join("uv/python/cpython-9.9.9-dummy");
    file(&python.join("bin/python3"), 0o775);
    folder(&python, 0o775);
    let outside = scratch.path().join("engine/agents");
    folder(&outside, 0o777);

    let report = tighten_setup(&setup, &setup.join("uv/python")).unwrap();

    assert_eq!(mode(&python), 0o755);
    assert_eq!(mode(&python.join("bin/python3")), 0o755);
    assert!(report.changed >= 2);
    assert_eq!(tighten_setup(&setup, &outside), Err(TOOL_FIX_REFUSED));
    assert_eq!(mode(&outside), 0o777);
    assert_eq!(tighten_setup(&setup, &setup), Err(TOOL_FIX_REFUSED));
}
