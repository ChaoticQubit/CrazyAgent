use super::*;
use crate::setup::process::{file_sha256, is_sha256};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

const TOP: &str = "node-v26.13.1-test";

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn script(path: &Path, line: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\necho \"{line}\"\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn tree(source: &Path, top: &str, version: &str) -> PathBuf {
    let folder = source.join(top);
    script(&folder.join("bin/node"), version);
    script(
        &folder.join("lib/node_modules/npm/bin/npm-cli.js"),
        "11.0.0",
    );
    script(
        &folder.join("lib/node_modules/npm/bin/npx-cli.js"),
        "11.0.0",
    );
    std::os::unix::fs::symlink(
        "../lib/node_modules/npm/bin/npm-cli.js",
        folder.join("bin/npm"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "../lib/node_modules/npm/bin/npx-cli.js",
        folder.join("bin/npx"),
    )
    .unwrap();
    folder
}

fn pack(work: &Path, source: &Path) -> (PathBuf, String) {
    let archive = work.join("node.tar.gz");
    let mut tar = Command::new("/usr/bin/tar");
    tar.arg("-czf").arg(&archive).arg("-C").arg(source);
    for entry in std::fs::read_dir(source).unwrap() {
        tar.arg(entry.unwrap().file_name());
    }
    assert!(crate::spawn_gate::status(&mut tar).unwrap().success());
    let sha256 = file_sha256(&archive).unwrap();
    (archive, sha256)
}

fn pin_for(archive: &Path, sha256: &str) -> NodePin {
    NodePin {
        url: format!("file://{}", archive.display()),
        sha256: sha256.to_owned(),
        top: TOP.to_owned(),
    }
}

fn nothing_left(root: &Path) {
    let setup = root.join("setup");
    assert!(std::fs::symlink_metadata(setup.join("node")).is_err());
    for name in ["node", "npm", "npx"] {
        assert!(std::fs::symlink_metadata(setup.join("bin").join(name)).is_err());
    }
    assert!(std::fs::symlink_metadata(crate::setup::harness_prefix(root)).is_err());
    let hidden: Vec<String> = std::fs::read_dir(&setup)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    assert!(hidden.is_empty(), "{hidden:?}");
    assert_eq!(runtime(root), None);
}

#[test]
fn a_node_archive_whose_checksum_or_version_does_not_match_is_refused_and_leaves_nothing() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    tree(&source, TOP, "v26.13.1");
    let (archive, sha256) = pack(work.path(), &source);
    let wrong = "0".repeat(64);
    assert_ne!(wrong, sha256);
    assert_eq!(
        install_pin(root.path(), &pin_for(&archive, &wrong)).unwrap_err(),
        InstallError("setup_checksum_mismatch")
    );
    nothing_left(root.path());

    let older = work.path().join("older");
    tree(&older, TOP, "v20.0.0");
    let (archive, sha256) = pack(work.path(), &older);
    assert_eq!(
        install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap_err(),
        InstallError("setup_version_mismatch")
    );
    nothing_left(root.path());
}

#[test]
fn the_pinned_node_unpacks_under_setup_with_its_links_and_an_engine_npm_prefix() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    tree(&source, TOP, "v26.13.1");
    let (archive, sha256) = pack(work.path(), &source);
    let installed = install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap();
    let setup = root.path().join("setup");
    let prefix = crate::setup::harness_prefix(root.path());
    assert_eq!(installed.node, setup.join("node/bin/node"));
    assert_eq!(installed.npm, setup.join("node/bin/npm"));
    assert_eq!(installed.prefix, prefix);
    assert_eq!(
        installed.search_folders(),
        vec![setup.join("node/bin"), prefix.join("bin")]
    );
    for folder in [
        prefix.clone(),
        prefix.join("bin"),
        prefix.join("lib/node_modules"),
    ] {
        let metadata = std::fs::symlink_metadata(&folder).unwrap();
        assert!(metadata.is_dir(), "{}", folder.display());
        assert_eq!(
            metadata.permissions().mode() & 0o077,
            0,
            "{}",
            folder.display()
        );
    }
    let real = setup.join("node").canonicalize().unwrap();
    for name in ["node", "npm", "npx"] {
        let linked = setup.join("bin").join(name);
        assert!(std::fs::symlink_metadata(&linked).unwrap().is_symlink());
        assert!(linked.canonicalize().unwrap().starts_with(&real), "{name}");
    }
    assert_eq!(runtime(root.path()), Some(installed.clone()));
    assert_eq!(
        install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap(),
        installed,
        "installing again replaces the folder"
    );
    assert!(std::fs::read_dir(&setup).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')
    }));
}

#[test]
fn an_archive_with_a_link_out_or_a_second_top_folder_is_refused_and_leaves_nothing() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    let folder = tree(&source, TOP, "v26.13.1");
    std::os::unix::fs::symlink("/bin/sh", folder.join("bin/corepack")).unwrap();
    let (archive, sha256) = pack(work.path(), &source);
    assert_eq!(
        install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap_err(),
        InstallError("setup_archive_unsafe")
    );
    nothing_left(root.path());

    let two = work.path().join("two");
    tree(&two, TOP, "v26.13.1");
    script(&two.join("extra/run"), "extra");
    let (archive, sha256) = pack(work.path(), &two);
    assert_eq!(
        install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap_err(),
        InstallError("setup_archive_unsafe")
    );
    nothing_left(root.path());
}

#[test]
fn an_unpacked_tree_is_kept_only_with_plain_files_folders_and_links_that_stay_inside_it() {
    let work = tempfile::tempdir().unwrap();
    let limits = Limits {
        entries: 64,
        bytes: 1024 * 1024,
        depth: 8,
    };
    let unsafe_tree = Err(InstallError("setup_archive_unsafe"));

    let good = tree(&work.path().join("good"), TOP, "v26.13.1");
    assert_eq!(check_tree(&good, limits), Ok(()));

    let absolute = tree(&work.path().join("absolute"), TOP, "v26.13.1");
    std::os::unix::fs::symlink("/etc/hosts", absolute.join("lib/hosts")).unwrap();
    assert_eq!(check_tree(&absolute, limits), unsafe_tree);

    let climbing = tree(&work.path().join("climbing"), TOP, "v26.13.1");
    std::os::unix::fs::symlink("../../../outside", climbing.join("bin/out")).unwrap();
    assert_eq!(check_tree(&climbing, limits), unsafe_tree);

    let special = tree(&work.path().join("special"), TOP, "v26.13.1");
    let fifo = crate::spawn_gate::status(Command::new("/usr/bin/mkfifo").arg(special.join("pipe")))
        .unwrap();
    assert!(fifo.success());
    assert_eq!(check_tree(&special, limits), unsafe_tree);

    let setuid = tree(&work.path().join("setuid"), TOP, "v26.13.1");
    std::fs::set_permissions(
        setuid.join("bin/node"),
        std::fs::Permissions::from_mode(0o4755),
    )
    .unwrap();
    assert_eq!(check_tree(&setuid, limits), unsafe_tree);

    let many = tree(&work.path().join("many"), TOP, "v26.13.1");
    for index in 0..64 {
        std::fs::write(many.join(format!("file-{index}")), "x").unwrap();
    }
    assert_eq!(check_tree(&many, limits), unsafe_tree);

    let large = tree(&work.path().join("large"), TOP, "v26.13.1");
    std::fs::write(large.join("big"), vec![0u8; 1024 * 1024 + 1]).unwrap();
    assert_eq!(check_tree(&large, limits), unsafe_tree);

    let deep = tree(&work.path().join("deep"), TOP, "v26.13.1");
    let mut folder = deep.clone();
    for _ in 0..9 {
        folder = folder.join("d");
    }
    std::fs::create_dir_all(&folder).unwrap();
    assert_eq!(check_tree(&deep, limits), unsafe_tree);
}

#[test]
fn a_late_failure_puts_the_earlier_node_back_and_drops_the_new_tree() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    tree(&source, TOP, "v26.13.1");
    let (archive, sha256) = pack(work.path(), &source);
    install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap();
    let kept = folder(root.path());
    std::fs::write(kept.join("earlier"), "the earlier Node").unwrap();
    let prefix = crate::setup::harness_prefix(root.path());
    std::fs::remove_dir_all(&prefix).unwrap();
    std::fs::write(&prefix, "not a folder").unwrap();
    assert_eq!(
        install_pin(root.path(), &pin_for(&archive, &sha256)),
        Err(InstallError("setup_store_unavailable"))
    );
    assert!(kept.join("earlier").is_file(), "the earlier Node is back");
    let setup_bin = root.path().join("setup/bin");
    for name in ["node", "npm", "npx"] {
        assert_eq!(
            std::fs::canonicalize(setup_bin.join(name)).unwrap(),
            std::fs::canonicalize(kept.join("bin").join(name)).unwrap(),
            "{name} still runs the earlier Node"
        );
    }
    let hidden: Vec<String> = std::fs::read_dir(root.path().join("setup"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(
        hidden.is_empty(),
        "the new tree goes with the staging folder: {hidden:?}"
    );

    let fresh = crate::setup::harness_prefix(&work.path().join("fresh"));
    std::fs::create_dir_all(fresh.parent().unwrap()).unwrap();
    std::fs::set_permissions(
        fresh.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(&fresh, "not a folder").unwrap();
    let fresh_root = work.path().join("fresh");
    assert_eq!(
        install_pin(&fresh_root, &pin_for(&archive, &sha256)),
        Err(InstallError("setup_store_unavailable"))
    );
    assert!(
        std::fs::symlink_metadata(folder(&fresh_root)).is_err(),
        "no new tree is kept"
    );
    for name in ["node", "npm", "npx"] {
        assert!(
            std::fs::symlink_metadata(fresh_root.join("setup/bin").join(name)).is_err(),
            "no {name} link is left pointing at nothing"
        );
    }
}

#[test]
fn a_link_that_climbs_out_through_another_link_is_refused_though_its_words_stay_inside() {
    let work = tempfile::tempdir().unwrap();
    let limits = Limits {
        entries: 64,
        bytes: 1024 * 1024,
        depth: 8,
    };
    let top = tree(&work.path().join("chain"), TOP, "v26.13.1");
    std::fs::create_dir_all(top.join("a/b")).unwrap();
    std::os::unix::fs::symlink("../..", top.join("a/b/s")).unwrap();
    assert_eq!(
        check_tree(&top, limits),
        Ok(()),
        "a link to the top itself stays inside"
    );
    std::os::unix::fs::symlink("s/../..", top.join("a/b/t")).unwrap();
    assert_eq!(
        check_tree(&top, limits),
        Err(InstallError("setup_archive_unsafe"))
    );
}

fn places(folders: Vec<PathBuf>) -> crate::tools::Places {
    crate::tools::Places::new(folders, None)
}

fn npm_launcher(folder: &Path) {
    std::fs::create_dir_all(folder).unwrap();
    let npm = folder.join("npm");
    std::fs::write(&npm, "#!/usr/bin/env node\n").unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn private(folder: &Path) {
    let metadata = std::fs::symlink_metadata(folder).unwrap();
    assert!(metadata.is_dir(), "{}", folder.display());
    assert_eq!(
        metadata.permissions().mode() & 0o077,
        0,
        "{}",
        folder.display()
    );
}

#[test]
fn a_usable_node_is_a_node_and_an_npm_the_resolver_finds_and_neither_alone_counts() {
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    script(&bin.join("node"), "v22.11.0");
    assert_eq!(usable(&places(vec![bin.clone()])), None, "node alone");
    npm_launcher(&bin);
    assert_eq!(
        usable(&places(vec![bin.clone()])),
        Some(Usable {
            node: bin.join("node"),
            npm: bin.join("npm"),
        })
    );
    std::fs::remove_file(bin.join("node")).unwrap();
    assert_eq!(usable(&places(vec![bin])), None, "npm alone");
}

#[test]
fn the_resolver_returns_the_engine_node_when_no_system_node_exists() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    tree(&source, TOP, "v26.13.1");
    let (archive, sha256) = pack(work.path(), &source);
    install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap();
    let setup_bin = root.path().join("setup/bin");
    let empty = work.path().join("no-system-node");
    std::fs::create_dir_all(&empty).unwrap();
    let only_engine = places(vec![empty, setup_bin.clone()]);
    let node = crate::tools::resolve("node", &only_engine).unwrap();
    assert_eq!(node.entry, setup_bin.join("node"));
    assert_eq!(
        node.program,
        folder(root.path()).join("bin/node").canonicalize().unwrap()
    );
    assert_eq!(
        usable(&only_engine),
        Some(Usable {
            node: setup_bin.join("node"),
            npm: setup_bin.join("npm"),
        })
    );
    let engine = crate::tools::Places::engine(Some(root.path()), None, None, None);
    let folders = engine.folders();
    assert_eq!(
        folders[folders.len() - 2..],
        [
            setup_bin,
            crate::setup::harness_prefix(root.path()).join("bin")
        ]
    );
}

#[test]
fn an_owner_node_and_the_app_node_both_present_picks_the_owners() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    tree(&source, TOP, "v26.13.1");
    let (archive, sha256) = pack(work.path(), &source);
    install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap();
    let home = work.path().join("home");
    let owner = home.join(".local/bin");
    script(&owner.join("node"), "v22.11.0");
    npm_launcher(&owner);
    let engine = crate::tools::Places::engine(Some(root.path()), None, Some(home), None);
    let setup_bin = root.path().join("setup/bin");
    let prefix_bin = crate::setup::harness_prefix(root.path()).join("bin");
    let both: Vec<PathBuf> = engine
        .folders()
        .iter()
        .filter(|folder| [&owner, &setup_bin, &prefix_bin].contains(folder))
        .cloned()
        .collect();
    assert_eq!(both, [owner.clone(), setup_bin, prefix_bin]);
    let found = usable(&places(both)).unwrap();
    assert_eq!(found.node, owner.join("node"));
    assert_eq!(found.npm, owner.join("npm"));
}

#[test]
fn an_owner_node_or_npm_alone_never_pairs_with_the_app_install_and_the_app_pair_is_used_whole() {
    use crate::setup::{Dependency, Installer, Version};
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    tree(&source, TOP, "v26.13.1");
    let (archive, sha256) = pack(work.path(), &source);
    install_pin(root.path(), &pin_for(&archive, &sha256)).unwrap();
    let owner = work.path().join("owner");
    let setup_bin = root.path().join("setup/bin");
    let both = places(vec![owner.clone(), setup_bin.clone()]);
    let engine_pair = Some(Usable {
        node: setup_bin.join("node"),
        npm: setup_bin.join("npm"),
    });
    script(&owner.join("node"), "v22.11.0");
    assert_eq!(usable(&both), engine_pair, "an owner node without npm");
    let installer = PinnedInstaller::with_places(root.path(), Vec::new(), both.clone());
    assert_eq!(
        installer.find(Dependency::Node),
        Ok(Some(Version::parse(NODE_VERSION).unwrap()))
    );
    std::fs::remove_file(owner.join("node")).unwrap();
    npm_launcher(&owner);
    assert_eq!(usable(&both), engine_pair, "an owner npm without node");
}

#[test]
fn the_engine_npm_prefix_sits_in_the_setup_folder_the_resolver_and_harness_finding_accept() {
    let root = Path::new("/engine");
    assert_eq!(
        crate::setup::harness_prefix(root),
        root.join("setup").join("harnesses")
    );
    assert!(crate::setup::harness_prefix(root).starts_with(root.join("setup")));
}

#[test]
fn the_engine_npm_prefix_links_the_node_its_install_used_so_a_node_script_harness_finds_it() {
    let root = root();
    let prefix = crate::setup::harness_prefix(root.path());
    let first = root.path().join("tools/one/bin/node");
    script(&first, "one");
    make_prefix(&prefix, &first).unwrap();
    assert_eq!(std::fs::read_link(prefix.join("bin/node")).unwrap(), first);
    let harness = prefix.join("lib/node_modules/@openai/codex/bin/codex.js");
    script(&harness, "codex");
    assert_eq!(
        crate::harnesses::node_prefix(&harness),
        Some(prefix.clone())
    );
    let second = root.path().join("tools/two/bin/node");
    script(&second, "two");
    make_prefix(&prefix, &second).unwrap();
    assert_eq!(
        std::fs::read_link(prefix.join("bin/node")).unwrap(),
        second,
        "a later install's node replaces the link"
    );
    private(&prefix.join("bin"));
}

#[test]
fn a_node_entry_that_is_the_prefix_link_itself_is_kept_and_never_linked_to_itself() {
    let root = root();
    let prefix = crate::setup::harness_prefix(root.path());
    let node = root.path().join("tools/bin/node");
    script(&node, "node");
    make_prefix(&prefix, &node).unwrap();
    npm_launcher(&prefix.join("bin"));
    let found = usable(&places(vec![prefix.join("bin")])).unwrap();
    assert_eq!(found.node, prefix.join("bin/node"));
    assert_eq!(make_prefix(&prefix, &found.node), Ok(()));
    assert_eq!(std::fs::read_link(prefix.join("bin/node")).unwrap(), node);
}

#[test]
fn the_engine_npm_prefix_is_made_private_even_with_no_engine_node() {
    let root = root();
    let made = crate::setup::harness_prefix(root.path());
    let node = root.path().join("tools/bin/node");
    script(&node, "node");
    make_prefix(&made, &node).unwrap();
    for folder in [
        made.clone(),
        made.join("bin"),
        made.join("lib"),
        made.join("lib/node_modules"),
    ] {
        private(&folder);
    }
    assert_eq!(runtime(root.path()), None);
    assert_eq!(
        make_prefix(&made, &node),
        Ok(()),
        "making it again keeps it"
    );
}

#[test]
fn setup_finds_a_usable_node_through_the_resolver_and_installs_the_pinned_archive_only_as_node() {
    use crate::setup::{Dependency, Installer, Pin, Version};
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let system = work.path().join("system");
    script(&system.join("node"), "v22.11.0");
    npm_launcher(&system);
    let installer = PinnedInstaller::with_places(
        root.path(),
        Vec::new(),
        places(vec![root.path().join("setup/bin"), system.clone()]),
    );
    assert_eq!(
        installer.find(Dependency::Node),
        Ok(Some(Version::parse("22.11.0").unwrap()))
    );
    std::fs::remove_file(system.join("npm")).unwrap();
    assert_eq!(installer.find(Dependency::Node), Ok(None));

    let source = work.path().join("source");
    tree(&source, TOP, "v26.13.1");
    let (archive, sha256) = pack(work.path(), &source);
    let pinned = pin_for(&archive, &sha256);
    let pin = Pin::Archive {
        version: Version::parse(NODE_VERSION).unwrap(),
        url: pinned.url,
        sha256: pinned.sha256,
        top: pinned.top,
    };
    assert_eq!(
        installer.install(Dependency::Uv, &pin),
        Err(InstallError("setup_pin_invalid"))
    );
    assert!(std::fs::symlink_metadata(folder(root.path())).is_err());
    let installed = Version::parse(NODE_VERSION).unwrap();
    assert_eq!(
        installer.install(Dependency::Node, &pin),
        Ok(installed.clone())
    );
    assert_eq!(installer.find(Dependency::Node), Ok(Some(installed)));
}

#[test]
fn every_mac_and_linux_target_pins_the_official_node_lts_archive_and_no_other_target_does() {
    let expected = [
        ("aarch64-apple-darwin", "darwin-arm64"),
        ("x86_64-apple-darwin", "darwin-x64"),
        ("aarch64-unknown-linux-gnu", "linux-arm64"),
        ("x86_64-unknown-linux-gnu", "linux-x64"),
    ];
    let mut hashes = Vec::new();
    for (target, platform) in expected {
        let pinned = pin(target).unwrap_or_else(|| panic!("{target} has no Node pin"));
        assert_eq!(
            pinned.url,
            format!("https://nodejs.org/dist/v26.13.1/node-v26.13.1-{platform}.tar.gz")
        );
        assert_eq!(pinned.top, format!("node-v26.13.1-{platform}"));
        assert!(is_sha256(&pinned.sha256), "{target}");
        hashes.push(pinned.sha256);
    }
    hashes.sort();
    hashes.dedup();
    assert_eq!(hashes.len(), expected.len());
    for target in [
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-msvc",
        "unknown",
    ] {
        assert_eq!(pin(target), None, "{target}");
    }
    assert_eq!(NODE_VERSION, "26.13.1");
    assert!(crate::setup::known_reason("setup_archive_unsafe").is_some());
}
