use super::diff;
use super::git::failure;
use super::store::{self, Record};
use super::{ChangeSetId, ChangeSetView, ChangedFile, FileKind, FileMark, FileStatus};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use std::path::PathBuf;

/// The largest base file the IDE diff reads, in bytes; the bridge pages it into frames.
pub(crate) const BASE_BYTES_MAX: usize = 1024 * 1024;

fn of_project(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
) -> Result<Record, EngineError> {
    store::initialize(db)?;
    let record = store::load(db, id)?;
    if &record.project != project {
        return Err(failure("changes_not_found"));
    }
    Ok(record)
}

/// Finds a change set only when it belongs to the IDE's project; a set of another project
/// answers exactly as a missing one. Inputs: the store, the project and the change set.
/// Output: the agent worktree its files live in, or `changes_not_found`.
pub(crate) fn locate(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
) -> Result<PathBuf, EngineError> {
    Ok(of_project(db, project, id)?.worktree)
}

/// Reads a change set of the IDE's project as the Changes list shows it, however many sets
/// the project has. Inputs: the store, the project and the change set. Output: the view, or
/// `changes_not_found`.
pub(crate) fn view(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
) -> Result<ChangeSetView, EngineError> {
    Ok(of_project(db, project, id)?.view())
}

/// Lists every file a change set touches, across all pages of its diff, which the engine
/// computes once per base and head. Inputs: the store, the project and the change set.
/// Output: the files in path order.
pub(crate) fn files(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
) -> Result<Vec<ChangedFile>, EngineError> {
    let record = of_project(db, project, id)?;
    let mut files = Vec::new();
    let mut page = 0;
    loop {
        let shown = diff::page(&record.repository, &record.base, &record.head, page)?;
        files.extend(shown.files);
        match shown.next {
            Some(next) => {
                assert!(next > page, "diff pages only move forward");
                page = next;
            }
            None => return Ok(files),
        }
    }
}

/// Reads the text a changed file had at the change set's base, its old path for a rename,
/// with no git filter or text conversion. Only text files of this change set are readable,
/// so the IDE never reads other paths of the owner's history. Inputs: the store, the
/// project, the change set and the path. Output: the text, empty for an added file, or
/// `changes_not_found` for any other path, `changes_too_large` past `BASE_BYTES_MAX`.
pub(crate) fn base_text(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    path: &str,
) -> Result<String, EngineError> {
    let file = files(db, project, id)?
        .into_iter()
        .find(|file| file.path == path && file.kind == FileKind::Text)
        .ok_or_else(|| failure("changes_not_found"))?;
    if file.status == FileStatus::Added {
        return Ok(String::new());
    }
    let record = of_project(db, project, id)?;
    blob_text(
        &record,
        &format!(
            "{}:{}",
            record.base,
            file.old_path.as_deref().unwrap_or(path)
        ),
    )
}

/// Reads the text an IDE settings file has at the change set's head, the agent's side of
/// its diff, which the IDE server is not allowed to read from the agent's worktree. Only a
/// text file of this change set marked `IdeConfig` that the set did not delete is readable.
/// Inputs: the store, the project, the change set and the path. Output: the text, or
/// `changes_not_found` for any other path, `changes_too_large` past `BASE_BYTES_MAX`.
pub(crate) fn config_text(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    path: &str,
) -> Result<String, EngineError> {
    let found = files(db, project, id)?.into_iter().any(|file| {
        file.path == path
            && file.kind == FileKind::Text
            && file.status != FileStatus::Deleted
            && file.marks.contains(&FileMark::IdeConfig)
    });
    if !found {
        return Err(failure("changes_not_found"));
    }
    let record = of_project(db, project, id)?;
    blob_text(&record, &format!("{}:{path}", record.head))
}

/// Reads one blob of the change set's repository as UTF-8 text, its size checked before
/// it is read. Inputs: the record and the `<commit>:<path>` object name. Output: the text,
/// `changes_too_large` past `BASE_BYTES_MAX`, `changes_not_found` for non-UTF-8 bytes or
/// `changes_git_failed` when git answers otherwise.
fn blob_text(record: &Record, object: &str) -> Result<String, EngineError> {
    let repo = super::flow::shared(&record.repository)?;
    let size: usize = String::from_utf8(repo.run(&["cat-file", "-s", object])?)
        .ok()
        .and_then(|printed| printed.trim().parse().ok())
        .ok_or_else(|| failure("changes_git_failed"))?;
    if size > BASE_BYTES_MAX {
        return Err(failure("changes_too_large"));
    }
    let bytes = repo.capture(&["cat-file", "blob", object], BASE_BYTES_MAX)?;
    if bytes.len() != size {
        return Err(failure("changes_git_failed"));
    }
    String::from_utf8(bytes).map_err(|_| failure("changes_not_found"))
}
