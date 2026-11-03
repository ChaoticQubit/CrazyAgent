use super::platform::open_append;
use super::{real_folder, refused};
use crate::engine::EngineError;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const CRAZYAGENT_DIRECTORY: &str = ".crazyagent";
pub const LAYOUT: [&str; 4] = ["setup", "research", "context", "worktrees"];
pub const EXCLUDES: [&str; 2] = [".crazyagent/", "graphify-out/"];
const EXCLUDE_BYTES_MAX: u64 = 1024 * 1024;

/// Creates a project's `.crazyagent/` folder with its setup, research, context
/// and agent worktree folders and an empty project `instructions.md`, reusing
/// any that already exist, and hides it and `graphify-out/` from git. Called
/// for a new project, when the owner starts onboarding and before an import.
/// Input: the project folder. Output: the `.crazyagent/` path, or a code when
/// the folder or an existing `.crazyagent/` entry is not a real folder or file.
pub fn prepare_layout(folder: &Path) -> Result<PathBuf, EngineError> {
    let folder = real_folder(folder, "project_folder_missing", "project_folder_missing")?;
    let root = folder.join(CRAZYAGENT_DIRECTORY);
    ensure_folder(&root)?;
    for name in LAYOUT {
        ensure_folder(&root.join(name))?;
    }
    ensure_file(&root.join(crate::agent_home::PROJECT_FILE))?;
    exclude_from_git(&folder)?;
    assert!(root.is_dir());
    Ok(root)
}

fn ensure_folder(path: &Path) -> Result<(), EngineError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| refused("crazyagent_folder_invalid"))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(refused("crazyagent_folder_invalid"))
            }
        }
        Err(_) => Err(refused("crazyagent_folder_unwritable")),
    }
}

/// Creates an empty owner-only file unless one is there, which must then be a
/// regular file, not a link. Input: the path. Output: unit, or a code.
fn ensure_file(path: &Path) -> Result<(), EngineError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| refused("crazyagent_folder_invalid"))?;
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(refused("crazyagent_folder_invalid"))
            }
        }
        Err(_) => Err(refused("crazyagent_folder_unwritable")),
    }
}

/// Adds `.crazyagent/` and `graphify-out/` to a repository's
/// `.git/info/exclude`, each line at most once however often it runs.
/// Input: the project folder. Output: true when the exclude file now holds
/// both lines, false when the folder has no plain `.git` folder (no repository,
/// or a linked worktree whose git folder lives elsewhere and is left alone);
/// `git_exclude_invalid` when `info` or `exclude` is a link or not plain.
pub fn exclude_from_git(folder: &Path) -> Result<bool, EngineError> {
    append_excludes(folder, &EXCLUDES)
}

/// Hides the project's agents root from git in the project folder, so git there never
/// lists or stages an agent's files and only Accept brings agent work in (IN1-R1):
/// `.crazyagent/` when the project keeps its state in that folder, `/worktrees/` for an
/// older project that keeps its agents at its top. The IDE runs it before each start.
/// Input: the project folder. Output: as [`exclude_from_git`].
pub fn exclude_agents(folder: &Path) -> Result<bool, EngineError> {
    assert_eq!(EXCLUDES[0], ".crazyagent/");
    let line = if super::state_folder(folder).as_path() == folder {
        "/worktrees/"
    } else {
        EXCLUDES[0]
    };
    append_excludes(folder, &[line])
}

/// Appends each of `lines` missing from the repository's `.git/info/exclude`, never
/// through a link. Inputs: the project folder and the lines. Output: as
/// [`exclude_from_git`].
fn append_excludes(folder: &Path, lines: &[&str]) -> Result<bool, EngineError> {
    let git = folder.join(".git");
    match fs::symlink_metadata(&git) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        _ => return Ok(false),
    }
    let info = git.join("info");
    match fs::symlink_metadata(&info) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(refused("git_exclude_invalid")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&info).map_err(|_| refused("git_exclude_invalid"))?;
        }
        Err(_) => return Err(refused("git_exclude_invalid")),
    }
    let path = info.join("exclude");
    let mut existing = String::new();
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if metadata.len() > EXCLUDE_BYTES_MAX {
                return Err(refused("git_exclude_invalid"));
            }
            fs::File::open(&path)
                .and_then(|file| file.take(EXCLUDE_BYTES_MAX).read_to_string(&mut existing))
                .map_err(|_| refused("git_exclude_invalid"))?;
        }
        Ok(_) => return Err(refused("git_exclude_invalid")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(refused("git_exclude_invalid")),
    }
    let missing: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !existing.lines().any(|present| present.trim_end() == *line))
        .collect();
    if missing.is_empty() {
        return Ok(true);
    }
    let mut text = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        text.push('\n');
    }
    for line in &missing {
        text.push_str(line);
        text.push('\n');
    }
    let mut file = open_append(&path).map_err(|_| refused("git_exclude_invalid"))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| refused("git_exclude_invalid"))?;
    assert!(!missing.is_empty() && missing.len() <= lines.len());
    Ok(true)
}
