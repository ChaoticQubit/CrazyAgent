use crate::Failure;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The environment variable naming the database file when `--db` is absent.
pub const DB_ENV: &str = "TRACKWRIGHT_DB";
/// The folder the upward search looks for and `init` creates.
pub const FOLDER: &str = ".trackwright";
/// The database file inside [`FOLDER`].
pub const FILE: &str = "tracker.db";
/// The most ancestor folders the upward search visits, so a search always ends.
pub const ANCESTORS_MAX: usize = 256;

/// The store-location codes the CLI replies itself, before the core runs.
pub mod codes {
    pub const STORE_NOT_FOUND: &str = "store_not_found";
    pub const STORE_PATH_REFUSED: &str = "store_path_refused";
    pub const STORE_UNREADABLE: &str = "store_unreadable";
    pub const STORE_EXISTS_ABOVE: &str = "store_exists_above";
}

/// Finds the database an operation runs on: `--db`, then `TRACKWRIGHT_DB`, then
/// `.trackwright/tracker.db` in `cwd` or its nearest ancestor holding one. A symlink as the
/// file or as a `.trackwright` folder met on the way is refused, never followed or skipped.
/// Inputs: the flag, the variable, the absolute working folder. Output: the file path.
pub fn existing(flag: Option<&Path>, env: Option<&OsStr>, cwd: &Path) -> Result<PathBuf, Failure> {
    assert!(cwd.is_absolute(), "the working folder is absolute");
    let found = match chosen(flag, env) {
        Some(path) => named(path)?,
        None => upward(cwd)?,
    };
    assert!(found.file_name().is_some());
    Ok(found)
}

/// Picks where `init` creates or reopens the database: `--db`, then `TRACKWRIGHT_DB`, then
/// `cwd/.trackwright/tracker.db`, refused when an ancestor already holds one it would hide.
/// Inputs: as for [`existing`]. Output: the path and whether a file is already there; a
/// symlink in either place is refused.
pub fn for_init(
    flag: Option<&Path>,
    env: Option<&OsStr>,
    cwd: &Path,
) -> Result<(PathBuf, bool), Failure> {
    assert!(cwd.is_absolute(), "the working folder is absolute");
    let path = match chosen(flag, env) {
        Some(path) => path,
        None => {
            let folder = cwd.join(FOLDER);
            if let Some(kind) = kind_of(&folder)?
                && kind != Kind::Folder
            {
                return Err(refused(&folder, "is not a plain folder"));
            }
            let file = folder.join(FILE);
            if kind_of(&file)?.is_none() {
                none_above(cwd)?;
            }
            file
        }
    };
    let exists = match kind_of(&path)? {
        None => false,
        Some(Kind::File) => true,
        Some(_) => return Err(refused(&path, "is not a plain file")),
    };
    Ok((path, exists))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Folder,
    Link,
    Other,
}

fn chosen(flag: Option<&Path>, env: Option<&OsStr>) -> Option<PathBuf> {
    flag.map(Path::to_path_buf)
        .or_else(|| env.filter(|value| !value.is_empty()).map(PathBuf::from))
}

fn named(path: PathBuf) -> Result<PathBuf, Failure> {
    match kind_of(&path)? {
        Some(Kind::File) => Ok(path),
        Some(_) => Err(refused(&path, "is not a plain file")),
        None => Err(Failure::new(
            codes::STORE_NOT_FOUND,
            format!(
                "no tracker database at {}; run trackwright init",
                path.display()
            ),
        )),
    }
}

fn upward(cwd: &Path) -> Result<PathBuf, Failure> {
    let mut visited = 0;
    for folder in cwd.ancestors().take(ANCESTORS_MAX) {
        visited += 1;
        let candidate = folder.join(FOLDER);
        match kind_of(&candidate)? {
            Some(Kind::Folder) => {}
            Some(Kind::Link) => return Err(refused(&candidate, "is a symlink")),
            Some(Kind::File | Kind::Other) | None => continue,
        }
        let file = candidate.join(FILE);
        match kind_of(&file)? {
            Some(Kind::File) => return Ok(file),
            Some(_) => return Err(refused(&file, "is not a plain file")),
            None => continue,
        }
    }
    assert!(visited >= 1, "a search visits the working folder itself");
    assert!(visited <= ANCESTORS_MAX);
    Err(Failure::new(
        codes::STORE_NOT_FOUND,
        format!(
            "no {FOLDER}/{FILE} in {} or above; run trackwright init or pass --db",
            cwd.display()
        ),
    ))
}

/// Refuses a new database in `cwd` while an ancestor holds one: the upward search would find
/// the new file first and hide every project above it. Input: the absolute working folder.
/// Output: `Ok` when no ancestor holds a database, else `store_exists_above` naming it.
fn none_above(cwd: &Path) -> Result<(), Failure> {
    assert!(cwd.is_absolute(), "the working folder is absolute");
    let Some(parent) = cwd.parent() else {
        return Ok(());
    };
    match upward(parent) {
        Ok(found) => {
            assert!(found.ends_with(Path::new(FOLDER).join(FILE)));
            let holder = found.parent().and_then(Path::parent);
            assert!(holder.is_some_and(|holder| parent.starts_with(holder)));
            Err(Failure::new(
                codes::STORE_EXISTS_ABOVE,
                format!(
                    "{} already serves this folder; pass --db {} to add the project there, \
                     or --db ./{FOLDER}/{FILE} for a separate tracker that hides it here",
                    found.display(),
                    found.display()
                ),
            ))
        }
        Err(failure) if failure.code == codes::STORE_NOT_FOUND => Ok(()),
        Err(failure) => Err(failure),
    }
}

fn kind_of(path: &Path) -> Result<Option<Kind>, Failure> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(Some(Kind::Link)),
        Ok(meta) if meta.is_file() => Ok(Some(Kind::File)),
        Ok(meta) if meta.is_dir() => Ok(Some(Kind::Folder)),
        Ok(_) => Ok(Some(Kind::Other)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Failure::new(
            codes::STORE_UNREADABLE,
            format!("cannot read {}: {}", path.display(), error.kind()),
        )),
    }
}

fn refused(path: &Path, why: &str) -> Failure {
    Failure::new(
        codes::STORE_PATH_REFUSED,
        format!(
            "{} {why}; trackwright never follows a link to its database",
            path.display()
        ),
    )
}
