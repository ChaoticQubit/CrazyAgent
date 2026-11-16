use crate::app_plugins::yaml::Value;
use crate::engine::EngineError;
use crate::harnesses::registry::{HARNESSES_MAX, Registry};
use crate::tools::Places;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// One harness's own instruction file in the owner's home that the owner may import once
/// into the app-wide instructions (DEC-106): the harness's catalog id and name, the file as
/// its manifest's `import.instructions` names it under the home, and its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub harness: String,
    pub name: String,
    pub path: String,
    pub bytes: u64,
}

/// Lists, in catalog order, each harness whose manifest names an `import.instructions` file
/// that is there in the owner's home; reads no file, and lists none off Unix, where [`read`]
/// refuses every one. Inputs: the owner's home and the registry. Output: the sources, at
/// most one per harness.
pub fn sources(home: &Path, registry: &Registry) -> Vec<Source> {
    if !cfg!(unix) {
        return Vec::new();
    }
    let found: Vec<Source> = registry
        .all()
        .iter()
        .filter_map(|harness| {
            let manifest = harness.manifest();
            let path = file_of(registry, &manifest.id)?;
            let held = fs::metadata(home.join(path)).ok().filter(fs::Metadata::is_file)?;
            Some(Source {
                harness: manifest.id.clone(),
                name: manifest.name.clone(),
                path: path.to_owned(),
                bytes: held.len(),
            })
        })
        .collect();
    assert!(found.len() <= HARNESSES_MAX);
    found
}

/// The instruction file a harness's manifest names in `import.instructions`, under the
/// owner's home, as the registry checked it (inside one of its `home.owner_folders`, no `~`,
/// `.` or `..` part). Inputs: the registry and the harness id. Output: the path, or none for
/// an unknown harness or one that names no instruction file.
pub fn file_of<'a>(registry: &'a Registry, harness: &str) -> Option<&'a str> {
    let manifest = registry
        .get(harness)
        .map(|found| found.manifest())
        .filter(|manifest| manifest.id == harness)?;
    match manifest.block("import")?.get("instructions")? {
        Value::Str(path) => Some(path.as_str()),
        _ => None,
    }
}

/// Reads one harness's instruction file for the owner's import preview, on the owner's click
/// only: the path comes from the harness's manifest, never from a client. The file is found
/// by one walk that opens it ([`walk`]), refused when it, a link on its way or a folder a
/// `..` left lies where agents can write, and read from the descriptor that walk opened, as
/// a picked file is checked. Inputs: the owner's home, the registry, the places agents can
/// write and the harness id. Output: the text, or a `PROCESS_CODES` refusal.
pub fn read(
    home: &Path,
    registry: &Registry,
    places: &Places,
    harness: &str,
) -> Result<String, EngineError> {
    read_between(home, registry, places, harness, || {})
}

/// [`read`], with a step run after the file is found and before it is checked and read, so
/// a row can swap a link in that window. Inputs: as [`read`] and the step. Output: as
/// [`read`].
#[cfg(unix)]
fn read_between(
    home: &Path,
    registry: &Registry,
    places: &Places,
    harness: &str,
    between: impl FnOnce(),
) -> Result<String, EngineError> {
    let refused = |code: &str| EngineError(code.into());
    let Some(relative) = file_of(registry, harness) else {
        return Err(refused("process_request_invalid"));
    };
    let path = home.join(relative);
    if !path.is_absolute() {
        return Err(refused("process_request_invalid"));
    }
    let found = walk(&path).map_err(refused)?;
    between();
    let writable = places.agent_writable(&found.real)
        || found.hops.iter().any(|hop| places.agent_writable(hop));
    if writable {
        return Err(refused("process_import_agents_can_write"));
    }
    super::text_of(found.file).map_err(refused)
}

/// Refuses a harness's instruction file where no walk by descriptors is built, after the
/// same harness check as on Unix. Inputs: as [`read_between`] on Unix. Output:
/// `process_request_invalid` or `process_import_unsafe`.
#[cfg(not(unix))]
fn read_between(
    home: &Path,
    registry: &Registry,
    places: &Places,
    harness: &str,
    between: impl FnOnce(),
) -> Result<String, EngineError> {
    let _ = (home, places, between);
    match file_of(registry, harness) {
        Some(_) => Err(EngineError("process_import_unsafe".into())),
        None => Err(EngineError("process_request_invalid".into())),
    }
}

/// The most parts one [`walk`] takes, links and `..` included, past which a chain is refused
/// as unsafe; a real file in a home is a few dozen parts.
#[cfg(unix)]
const WALK_PARTS_MAX: usize = 4096;

/// What one [`walk`] found: every link it read and every real folder a `..` left, each where
/// it really sits; the real path of the file it opened; and that file, open to read.
#[cfg(unix)]
struct Found {
    hops: Vec<std::path::PathBuf>,
    real: std::path::PathBuf,
    file: fs::File,
}

/// Opens a file by one walk from `/`, one part at a time through the folder descriptors it
/// holds, following no link on its own: a link is read from the folder that holds it and its
/// text walked on (from `/` again when it is absolute), and a `..` returns to the folder held
/// before, so the real path it reports is the one the descriptor was opened at, and nothing
/// is looked up twice. At most `LINK_HOPS_MAX` links and [`WALK_PARTS_MAX`] parts. Input: an
/// absolute path. Output: what was found, or `process_import_missing` for a part that is not
/// there, `process_import_unsafe` past a bound or for a path that ends on a folder part, or
/// `process_import_unreadable`.
#[cfg(unix)]
fn walk(path: &Path) -> Result<Found, &'static str> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Component, PathBuf};
    const FOLDER: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const LAST: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);
    assert!(path.is_absolute());
    let queue = |pending: &mut Vec<OsString>, from: &Path| {
        for part in from.components().rev() {
            match part {
                Component::Normal(name) => pending.push(name.to_owned()),
                Component::ParentDir => pending.push(OsString::from("..")),
                Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            }
        }
    };
    let failed = |problem: rustix::io::Errno| match problem {
        rustix::io::Errno::NOENT => "process_import_missing",
        _ => "process_import_unreadable",
    };
    let root = rustix::fs::open("/", FOLDER, Mode::empty()).map_err(failed)?;
    let mut folders = vec![(root, PathBuf::from("/"))];
    let mut pending = Vec::new();
    queue(&mut pending, path);
    let mut hops = Vec::new();
    let mut links = 0;
    let mut parts = 0;
    while let Some(part) = pending.pop() {
        parts += 1;
        if parts > WALK_PARTS_MAX {
            return Err("process_import_unsafe");
        }
        assert!(!folders.is_empty());
        let (folder, at) = &folders[folders.len() - 1];
        if part.as_os_str() == OsStr::new("..") {
            if folders.len() > 1 {
                hops.push(at.clone());
                folders.pop();
            }
            continue;
        }
        let next = at.join(&part);
        let kind = rustix::fs::statat(folder, &part, AtFlags::SYMLINK_NOFOLLOW).map_err(failed)?;
        if FileType::from_raw_mode(kind.st_mode) == FileType::Symlink {
            if links == crate::harnesses::finding::LINK_HOPS_MAX {
                return Err("process_import_unsafe");
            }
            links += 1;
            let text = rustix::fs::readlinkat(folder, &part, Vec::new()).map_err(failed)?;
            let target = PathBuf::from(OsString::from_vec(text.into_bytes()));
            hops.push(next);
            if target.is_absolute() {
                folders.truncate(1);
            }
            queue(&mut pending, &target);
            continue;
        }
        if pending.is_empty() {
            let file = rustix::fs::openat(folder, &part, LAST, Mode::empty()).map_err(failed)?;
            assert!(links <= crate::harnesses::finding::LINK_HOPS_MAX);
            return Ok(Found {
                hops,
                real: next,
                file: fs::File::from(file),
            });
        }
        let opened = rustix::fs::openat(folder, &part, FOLDER, Mode::empty()).map_err(failed)?;
        folders.push((opened, next));
    }
    Err("process_import_unsafe")
}

#[cfg(all(test, unix))]
#[path = "sources_tests.rs"]
mod tests;
