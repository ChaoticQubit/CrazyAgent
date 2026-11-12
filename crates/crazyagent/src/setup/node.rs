use super::InstallError;
use super::pinned::{PinnedInstaller, link, runnable};
use super::process::{is_sha256, private_folder, run, staging_folder};
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// The Node.js LTS release setup installs when no usable Node is found (DEC-157).
pub const NODE_VERSION: &str = "26.13.1";

const TAR: &str = "/usr/bin/tar";
const UNPACK_TIME: Duration = Duration::from_secs(300);
const VERSION_TIME: Duration = Duration::from_secs(10);
const TOP_BYTES_MAX: usize = 64;
const LINKED: [&str; 3] = ["node", "npm", "npx"];
const LIMITS: Limits = Limits {
    entries: 20_000,
    bytes: 512 * 1024 * 1024,
    depth: 32,
};
const NODE_RELEASES: [(&str, &str, &str); 4] = [
    (
        "aarch64-apple-darwin",
        "darwin-arm64",
        "aec2514ea1ceb2ba52455e9ec65986f6749a06dd759e5db2a72e6889668c9735",
    ),
    (
        "x86_64-apple-darwin",
        "darwin-x64",
        "82bb96078a24ca50b9c9a38f2379e6be6d87171fcd400fc14c8934b3bce96e2f",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "linux-arm64",
        "80a330b35415382878df18d41afbd106ea89bfb3be0087d6608e289a14c5cc7d",
    ),
    (
        "x86_64-unknown-linux-gnu",
        "linux-x64",
        "518674de2b621728a5e4486f5fb3b7d2900bb3cc4aa97969edaa463db99f9778",
    ),
];

/// One pinned Node release archive: where it is downloaded from, the SHA-256 it must have,
/// and the one top folder every file in it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePin {
    pub url: String,
    pub sha256: String,
    pub top: String,
}

/// The engine's own Node: the `node` and `npm` programs under `<engine root>/setup/node` and
/// the engine-owned npm prefix `<engine root>/harnesses` npm harnesses install into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRuntime {
    pub node: PathBuf,
    pub npm: PathBuf,
    pub prefix: PathBuf,
}

impl NodeRuntime {
    /// The folders a harness run or an npm install puts first on its search path: Node's own
    /// bin folder, then the npm prefix's bin folder. Output: the two folders in that order.
    pub fn search_folders(&self) -> Vec<PathBuf> {
        let node_bin = self.node.parent().map(Path::to_path_buf);
        let folders: Vec<PathBuf> = node_bin
            .into_iter()
            .chain([self.prefix.join("bin")])
            .collect();
        assert_eq!(folders.len(), 2);
        folders
    }
}

/// The bounds an unpacked archive must stay within: how many entries, how many file bytes
/// and how many folders deep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    entries: usize,
    bytes: u64,
    depth: usize,
}

/// The folder the engine's Node archive tree is kept in, `<engine root>/setup/node`.
/// Input: the engine root. Output: the folder.
pub fn folder(engine_root: &Path) -> PathBuf {
    engine_root.join("setup").join("node")
}

/// The engine-owned npm prefix npm harnesses install into, `<engine root>/harnesses`.
/// Input: the engine root. Output: the folder.
pub fn prefix(engine_root: &Path) -> PathBuf {
    engine_root.join("harnesses")
}

/// The pinned Node archive for one target triple, from nodejs.org's own dist folder.
/// Input: the target, such as `aarch64-apple-darwin`. Output: its pin, or none for a
/// target with no Node pin.
pub fn pin(target: &str) -> Option<NodePin> {
    let (_, platform, sha256) = NODE_RELEASES
        .iter()
        .find(|(known, _, _)| *known == target)?;
    let top = format!("node-v{NODE_VERSION}-{platform}");
    Some(NodePin {
        url: format!("https://nodejs.org/dist/v{NODE_VERSION}/{top}.tar.gz"),
        sha256: (*sha256).to_owned(),
        top,
    })
}

/// Reads the engine's own Node without running anything: a plain `node` file this user owns
/// that no one else can change, an `npm` that resolves inside the Node folder, and a private
/// npm prefix with its `lib/node_modules` and `bin` folders. Input: the engine root.
/// Output: the runtime when all of it is in place, else none.
pub fn runtime(engine_root: &Path) -> Option<NodeRuntime> {
    let home = folder(engine_root);
    let node = home.join("bin").join("node");
    let npm = home.join("bin").join("npm");
    let prefix = prefix(engine_root);
    let uid = rustix::process::getuid().as_raw();
    let own_file = std::fs::symlink_metadata(&node).is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.uid() == uid
            && metadata.permissions().mode() & 0o022 == 0
            && metadata.permissions().mode() & 0o100 != 0
    });
    let inside = home.canonicalize().ok().is_some_and(|real| {
        npm.canonicalize()
            .is_ok_and(|target| target.starts_with(&real))
            && runnable(&npm)
    });
    let private = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.is_dir() && metadata.uid() == uid && metadata.permissions().mode() & 0o077 == 0
        })
    };
    let prefix_ready = private(&prefix)
        && private(&prefix.join("bin"))
        && private(&prefix.join("lib"))
        && private(&prefix.join("lib").join("node_modules"));
    (own_file && inside && prefix_ready).then_some(NodeRuntime { node, npm, prefix })
}

/// Installs this target's pinned Node into the engine root, as `install_pin` does. Input:
/// the engine root. Output: the runtime, `setup_platform_unsupported` on a target with no
/// Node pin, or the setup code of why it was not installed.
pub fn install(engine_root: &Path) -> Result<NodeRuntime, InstallError> {
    let pinned = pin(super::TARGET).ok_or(InstallError(super::PLATFORM_UNSUPPORTED))?;
    install_pin(engine_root, &pinned)
}

/// Installs one Node archive pin: downloads it into a private staging folder, keeps it only
/// with the pinned SHA-256, unpacks it there, checks the tree (`check_tree`) and that its
/// node reports the pinned version, then moves it to `<engine root>/setup/node`, links
/// node, npm and npx in `setup/bin` and makes the private npm prefix. A failure removes the
/// staging folder and leaves any earlier Node as it was. Nothing runs with sudo. Inputs:
/// the engine root and the pin. Output: the runtime, or the setup code.
pub(crate) fn install_pin(engine_root: &Path, pin: &NodePin) -> Result<NodeRuntime, InstallError> {
    let valid = (pin.url.starts_with("https://") || pin.url.starts_with("file:///"))
        && pin.url.len() <= 2048
        && !pin.url.chars().any(|c| c.is_control() || c.is_whitespace())
        && is_sha256(&pin.sha256)
        && plain_top(&pin.top);
    if !valid {
        return Err(InstallError("setup_pin_invalid"));
    }
    let installer = PinnedInstaller::with_paths(engine_root, Vec::new());
    let setup = installer.setup_folder().to_path_buf();
    private_folder(&setup)?;
    let staging = staging_folder(&setup, "node")?;
    let result = (|| {
        let archive = staging.join("node.tar.gz");
        installer.download(&pin.url, &pin.sha256, &archive, &staging)?;
        let out = staging.join("out");
        private_folder(&out)?;
        let (unpacked, _) = run(
            Path::new(TAR),
            &[
                "-xzf".into(),
                archive.into_os_string(),
                "-C".into(),
                out.clone().into_os_string(),
            ],
            &installer.env(),
            &staging,
            UNPACK_TIME,
        )?;
        if !unpacked {
            return Err(InstallError("setup_install_failed"));
        }
        let top = only_top(&out, &pin.top)?;
        check_tree(&top, LIMITS)?;
        let (ok, output) = run(
            &top.join("bin").join("node"),
            &[OsString::from("--version")],
            &installer.env(),
            &staging,
            VERSION_TIME,
        )?;
        let reported = String::from_utf8_lossy(&output);
        if !ok || reported.lines().next().map(str::trim) != Some(&format!("v{NODE_VERSION}")) {
            return Err(InstallError("setup_version_mismatch"));
        }
        put_in_place(&top, &folder(engine_root), &staging)?;
        let bin = setup.join("bin");
        private_folder(&bin)?;
        for name in LINKED {
            link(&folder(engine_root).join("bin").join(name), &bin.join(name))?;
        }
        let prefix = prefix(engine_root);
        for made in [prefix.join("bin"), prefix.join("lib").join("node_modules")] {
            private_folder(&made)?;
        }
        runtime(engine_root).ok_or(InstallError("setup_install_failed"))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Checks an unpacked tree before it is kept: only plain folders, files and links, no file
/// with a set-id or sticky bit or that others can write, every link relative and resolving
/// inside the tree, and no more entries, file bytes or depth than the bounds allow. Nothing
/// is followed. Inputs: the tree's top folder and the bounds. Output: Ok, or
/// `setup_archive_unsafe`.
pub(crate) fn check_tree(top: &Path, limits: Limits) -> Result<(), InstallError> {
    let unsafe_tree = InstallError("setup_archive_unsafe");
    let top_ok = std::fs::symlink_metadata(top).is_ok_and(|metadata| metadata.is_dir());
    if !top_ok {
        return Err(unsafe_tree);
    }
    let mut pending: Vec<(PathBuf, usize)> = vec![(top.to_path_buf(), 0)];
    let mut entries: usize = 0;
    let mut bytes: u64 = 0;
    while let Some((dir, depth)) = pending.pop() {
        let listed = std::fs::read_dir(&dir).map_err(|_| unsafe_tree)?;
        for entry in listed {
            let path = entry.map_err(|_| unsafe_tree)?.path();
            entries += 1;
            if entries > limits.entries {
                return Err(unsafe_tree);
            }
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| unsafe_tree)?;
            let kind = metadata.file_type();
            let mode = metadata.permissions().mode();
            if mode & 0o7000 != 0 || (!kind.is_symlink() && mode & 0o002 != 0) {
                return Err(unsafe_tree);
            }
            if kind.is_dir() {
                if depth + 1 > limits.depth {
                    return Err(unsafe_tree);
                }
                pending.push((path, depth + 1));
            } else if kind.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > limits.bytes {
                    return Err(unsafe_tree);
                }
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&path).map_err(|_| unsafe_tree)?;
                if !stays_inside(top, &dir, &target) {
                    return Err(unsafe_tree);
                }
            } else {
                return Err(unsafe_tree);
            }
        }
    }
    assert!(entries <= limits.entries && bytes <= limits.bytes);
    Ok(())
}

/// Tells whether a link's target, read relative to the folder holding the link, names a
/// place inside the tree, judged on the words alone: an absolute target or one that climbs
/// above the top is outside. Inputs: the top, the link's folder and its target. Output:
/// the answer.
fn stays_inside(top: &Path, dir: &Path, target: &Path) -> bool {
    let Ok(below) = dir.strip_prefix(top) else {
        return false;
    };
    let mut depth: usize = below.components().count();
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                let Some(up) = depth.checked_sub(1) else {
                    return false;
                };
                depth = up;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    !target.as_os_str().is_empty()
}

/// Finds the archive's one top folder: the unpack folder must hold exactly one entry, a
/// plain folder with the pinned name. Inputs: the unpack folder and the pinned name.
/// Output: the top folder, or `setup_archive_unsafe`.
fn only_top(out: &Path, top: &str) -> Result<PathBuf, InstallError> {
    let unsafe_tree = InstallError("setup_archive_unsafe");
    let names: Vec<OsString> = std::fs::read_dir(out)
        .map_err(|_| unsafe_tree)?
        .take(2)
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()
        .map_err(|_| unsafe_tree)?;
    if names.len() != 1 || names[0] != top {
        return Err(unsafe_tree);
    }
    let found = out.join(top);
    let plain = std::fs::symlink_metadata(&found).is_ok_and(|metadata| metadata.is_dir());
    plain.then_some(found).ok_or(unsafe_tree)
}

/// Moves a checked tree to its kept folder, first moving any earlier one into the staging
/// folder, which is removed later, and putting it back when the move fails. Inputs: the
/// tree, the kept folder and the staging folder. Output: Ok, or `setup_store_unavailable`.
fn put_in_place(tree: &Path, kept: &Path, staging: &Path) -> Result<(), InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    let earlier = staging.join("earlier");
    let had = std::fs::symlink_metadata(kept).is_ok();
    if had {
        std::fs::rename(kept, &earlier).map_err(|_| unavailable)?;
    }
    if std::fs::rename(tree, kept).is_err() {
        if had {
            let _ = std::fs::rename(&earlier, kept);
        }
        return Err(unavailable);
    }
    Ok(())
}

/// Tells whether a pinned top folder name is one plain path part: letters, digits, `.`,
/// `-` and `_`, not starting with a dot. Input: the name. Output: the answer.
fn plain_top(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= TOP_BYTES_MAX
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
