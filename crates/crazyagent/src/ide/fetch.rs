use super::manifest::{self, Build};
use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

const IDE_FOLDER: &str = "ide";
const STATE_FILE: &str = "build-state.json";
const STATE_BYTES_MAX: u64 = 64 * 1024;
const PROGRESS_BYTES: u64 = 1024 * 1024;
const ENTRIES_MAX: usize = 200_000;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Where the engine's IDE build stands, as the IDE tab shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BuildState {
    Missing,
    Downloading { bytes: u64, total: u64 },
    Verifying,
    Ready { version: String, root: PathBuf },
    Failed { code: String },
}

/// The engine roots with a fetch running in this process.
static ACTIVE: LazyLock<Mutex<BTreeSet<PathBuf>>> = LazyLock::new(Default::default);

/// Marks one engine root as fetching until dropped.
struct Active(PathBuf);

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Removes a file or folder when dropped, whatever the outcome.
struct Removed(PathBuf);

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes the downloaded bytes to the file, hashing them and reporting progress.
struct Progress<'a> {
    file: std::fs::File,
    hasher: Sha256,
    runtime: &'a Path,
    total: u64,
    written: u64,
    reported: u64,
}

impl Write for Progress<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let wrote = self.file.write(buffer)?;
        self.hasher.update(&buffer[..wrote]);
        self.written = self.written.saturating_add(wrote as u64);
        if self.written - self.reported >= PROGRESS_BYTES {
            self.reported = self.written;
            let _ = write_state(
                self.runtime,
                &BuildState::Downloading {
                    bytes: self.written,
                    total: self.total,
                },
            );
        }
        Ok(wrote)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    failure("ide_build_storage")
}

fn invalid(_: impl std::fmt::Display) -> EngineError {
    failure("ide_build_archive_invalid")
}

fn active(runtime: &Path) -> bool {
    ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(runtime)
}

fn read_state(runtime: &Path) -> Option<BuildState> {
    let file = std::fs::File::open(runtime.join(IDE_FOLDER).join(STATE_FILE)).ok()?;
    let mut text = String::new();
    file.take(STATE_BYTES_MAX).read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_state(runtime: &Path, state: &BuildState) -> Result<(), EngineError> {
    let folder = runtime.join(IDE_FOLDER);
    std::fs::create_dir_all(&folder).map_err(storage)?;
    let text = serde_json::to_vec(state).map_err(storage)?;
    let staged = folder.join(format!(".{STATE_FILE}.{}", std::process::id()));
    std::fs::write(&staged, text).map_err(storage)?;
    std::fs::rename(&staged, folder.join(STATE_FILE)).map_err(storage)
}

/// Reads where the IDE build stands. A download or check that no fetch in this process
/// is running, or a ready build whose folder is gone, reads Missing.
/// Input: the engine root. Output: the state.
pub fn state(runtime: &Path) -> BuildState {
    match read_state(runtime) {
        Some(BuildState::Downloading { .. } | BuildState::Verifying) if !active(runtime) => {
            BuildState::Missing
        }
        Some(BuildState::Ready { root, .. }) if !root.is_dir() => BuildState::Missing,
        Some(stored) => stored,
        None => BuildState::Missing,
    }
}

/// Starts fetching the pinned IDE build for this platform in the background unless it is
/// ready or already being fetched; a platform with no build records Failed.
/// Input: the engine root. Output: true when this call started a fetch.
pub fn ensure(runtime: &Path) -> bool {
    match crate::update::target()
        .ok_or_else(|| failure("ide_build_unsupported_platform"))
        .and_then(manifest::current)
    {
        Ok(build) => ensure_with(runtime, build),
        Err(error) => {
            let _ = write_state(runtime, &BuildState::Failed { code: error.0 });
            false
        }
    }
}

/// The first-launch trigger: fetches the IDE build only for the engine's default data
/// folder, so an engine run on any other root (a test, a scratch profile) never
/// downloads. Input: the engine root. Output: true when a fetch started.
pub fn on_engine_start(runtime: &Path) -> bool {
    let default = std::env::var_os("CRAZYAGENT_HOME").is_none()
        && crate::update::data_root().is_some_and(|root| {
            match (std::fs::canonicalize(root), std::fs::canonicalize(runtime)) {
                (Ok(home), Ok(given)) => home == given,
                _ => false,
            }
        });
    default && ensure(runtime)
}

fn ensure_with(runtime: &Path, build: Build) -> bool {
    assert!(manifest::version_valid(&build.version));
    if matches!(state(runtime), BuildState::Ready { ref version, .. } if *version == build.version)
    {
        return false;
    }
    if !ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(runtime.to_path_buf())
    {
        return false;
    }
    let marked = Active(runtime.to_path_buf());
    let _ = write_state(
        runtime,
        &BuildState::Downloading {
            bytes: 0,
            total: build.size_max,
        },
    );
    let owned = runtime.to_path_buf();
    std::thread::Builder::new()
        .name("ide-fetch".to_owned())
        .spawn(move || {
            let _marked = marked;
            fetch(&owned, &build);
        })
        .is_ok()
}

/// Downloads, verifies and extracts one build, then records and returns where it ended.
/// Inputs: the engine root and the build. Output: Ready or Failed with its code.
fn fetch(runtime: &Path, build: &Build) -> BuildState {
    let finished = match install(runtime, build) {
        Ok(root) => BuildState::Ready {
            version: build.version.clone(),
            root,
        },
        Err(error) => BuildState::Failed { code: error.0 },
    };
    match write_state(runtime, &finished) {
        Ok(()) => finished,
        Err(error) => BuildState::Failed { code: error.0 },
    }
}

fn install(runtime: &Path, build: &Build) -> Result<PathBuf, EngineError> {
    let ide = runtime.join(IDE_FOLDER);
    let (downloads, builds) = (ide.join("downloads"), ide.join("builds"));
    std::fs::create_dir_all(&downloads).map_err(storage)?;
    std::fs::create_dir_all(&builds).map_err(storage)?;
    let archive = downloads.join(format!("{}.tar.gz.part", build.version));
    let _archive = Removed(archive.clone());
    let digest = download(runtime, build, &archive)?;
    write_state(runtime, &BuildState::Verifying)?;
    if digest != build.sha256 {
        return Err(failure("ide_build_hash_mismatch"));
    }
    check_names(&archive)?;
    let staging = builds.join(format!(".staging-{}", build.version));
    let _staging = Removed(staging.clone());
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(storage)?;
    }
    extract(&archive, &staging)?;
    check_tree(&staging)?;
    let root = builds.join(&build.version);
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(storage)?;
    }
    std::fs::rename(&staging, &root).map_err(storage)?;
    Ok(root)
}

fn download(runtime: &Path, build: &Build, archive: &Path) -> Result<String, EngineError> {
    let mut progress = Progress {
        file: std::fs::File::create(archive).map_err(storage)?,
        hasher: Sha256::new(),
        runtime,
        total: build.size_max,
        written: 0,
        reported: 0,
    };
    crate::update::download(&build.url, &mut progress, build.size_max, DOWNLOAD_TIMEOUT).map_err(
        |error| {
            failure(match error.code {
                "update_too_large" => "ide_build_too_large",
                _ => "ide_build_download_failed",
            })
        },
    )?;
    progress.file.sync_all().map_err(storage)?;
    Ok(format!("{:x}", progress.hasher.finalize()))
}

fn tar(mode: &str, archive: &Path) -> Command {
    let mut command = Command::new(crate::update::system_tool("tar"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.arg(mode).arg("-z").arg("-f").arg(archive);
    command
}

/// Tells whether an archive entry name stays inside the folder it is extracted into:
/// relative, no `..` part, no backslash, drive letter or control character.
fn name_inside(name: &str) -> bool {
    let name = name.strip_prefix("./").unwrap_or(name);
    !name.starts_with('/')
        && !name.contains('\\')
        && name.as_bytes().get(1) != Some(&b':')
        && !name.chars().any(char::is_control)
        && name.split('/').all(|part| part != "..")
}

/// Lists the archive with the names exactly as stored and refuses any entry that would
/// land outside the target. Input: the verified archive. Output: Ok, or
/// `ide_build_archive_invalid`.
fn check_names(archive: &Path) -> Result<(), EngineError> {
    let output = crate::spawn_gate::output(tar("-t", archive).arg("-P")).map_err(invalid)?;
    if !output.status.success() {
        return Err(failure("ide_build_archive_invalid"));
    }
    let names = String::from_utf8(output.stdout).map_err(invalid)?;
    let mut count = 0_usize;
    for name in names.lines() {
        count += 1;
        if count > ENTRIES_MAX || !name_inside(name) {
            return Err(failure("ide_build_archive_invalid"));
        }
    }
    if count == 0 {
        return Err(failure("ide_build_archive_invalid"));
    }
    Ok(())
}

fn extract(archive: &Path, staging: &Path) -> Result<(), EngineError> {
    std::fs::create_dir(staging).map_err(storage)?;
    let output =
        crate::spawn_gate::output(tar("-x", archive).arg("-C").arg(staging)).map_err(invalid)?;
    if !output.status.success() {
        return Err(failure("ide_build_archive_invalid"));
    }
    Ok(())
}

/// Tells whether a link target, read from a link `depth` folders below the root, stays
/// inside the root. Inputs: the depth and the target. Output: the answer.
fn link_inside(depth: usize, target: &Path) -> bool {
    let mut depth = depth;
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Walks the extracted tree without following links and refuses a link that leaves it,
/// a hard link shared with a file outside it, or anything but files, folders and links.
/// Input: the staging folder. Output: Ok, or `ide_build_archive_invalid`.
fn check_tree(staging: &Path) -> Result<(), EngineError> {
    let mut folders = vec![(staging.to_path_buf(), 0_usize)];
    let mut shared: BTreeMap<(u64, u64), (u64, u64)> = BTreeMap::new();
    let mut count = 0_usize;
    while let Some((folder, depth)) = folders.pop() {
        for entry in std::fs::read_dir(&folder).map_err(invalid)? {
            count += 1;
            if count > ENTRIES_MAX {
                return Err(failure("ide_build_archive_invalid"));
            }
            let path = entry.map_err(invalid)?.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(invalid)?;
            let kind = metadata.file_type();
            if kind.is_symlink() {
                if !link_inside(depth, &std::fs::read_link(&path).map_err(invalid)?) {
                    return Err(failure("ide_build_archive_invalid"));
                }
            } else if kind.is_dir() {
                folders.push((path, depth + 1));
            } else if kind.is_file() {
                note_links(&metadata, &mut shared);
            } else {
                return Err(failure("ide_build_archive_invalid"));
            }
        }
    }
    if shared.values().any(|(links, seen)| seen != links) {
        return Err(failure("ide_build_archive_invalid"));
    }
    Ok(())
}

#[cfg(unix)]
fn note_links(metadata: &std::fs::Metadata, shared: &mut BTreeMap<(u64, u64), (u64, u64)>) {
    use std::os::unix::fs::MetadataExt;
    if metadata.nlink() > 1 {
        let noted = shared
            .entry((metadata.dev(), metadata.ino()))
            .or_insert((metadata.nlink(), 0));
        noted.1 += 1;
    }
}

#[cfg(not(unix))]
fn note_links(_: &std::fs::Metadata, _: &mut BTreeMap<(u64, u64), (u64, u64)>) {}

#[cfg(test)]
mod tests;
