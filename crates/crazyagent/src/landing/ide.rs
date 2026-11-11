use super::diff::{self, Recent, keep, recall};
use super::git::{failure, is_object_id};
use super::store::{self, Record};
use super::{ChangeSetId, ChangeSetView, ChangedFile, FileKind, FileMark, FileStatus};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// The largest file the IDE diff reads on either side, in bytes; the bridge pages it into
/// frames.
pub(crate) const BASE_BYTES_MAX: usize = 1024 * 1024;
const TEXTS_MAX: usize = 16;
const TEXTS_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The file texts the IDE diff read last, by repository, commit and path, at most
/// `TEXTS_MAX` and `TEXTS_BYTES_MAX` apart from the newest, so paging one text reads git
/// once.
static TEXTS: LazyLock<Mutex<Recent<str>>> = LazyLock::new(Default::default);

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

/// Reads a change set of the IDE's project only at the head the IDE listed, which Accept
/// and Discard name too, so a page never shows a head the owner did not list. Inputs: the
/// store, the project, the change set and the listed head. Output: the record, or
/// `changes_not_found`, or `changes_stale` once the set holds another head.
fn at_head(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
) -> Result<Record, EngineError> {
    let record = of_project(db, project, id)?;
    assert!(!record.head.is_empty(), "a change set always has a head");
    if record.head != head {
        return Err(failure("changes_stale"));
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
    let record = of_project(db, project, id)?;
    Ok(record.view(store::number_of(db, &record.id)?))
}

/// Lists every file a change set touches, which the engine computes once per base and
/// head and keeps apart from the diff text. Inputs: the store, the project and the change
/// set. Output: the files in path order.
pub(crate) fn files(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
) -> Result<Vec<ChangedFile>, EngineError> {
    let record = of_project(db, project, id)?;
    Ok(diff::listed(&record.repository, &record.base, &record.head)?.to_vec())
}

/// Reads the text a changed file had at the change set's base, its old path for a rename,
/// the left side of its IDE diff. Only text files of this change set are readable, so the
/// IDE never reads other paths of the owner's history. Inputs: the store, the project, the
/// change set, the head the IDE listed and the path. Output: the text, empty for an added
/// file, or `changes_not_found` for any other path, `changes_stale` once the set holds
/// another head, `changes_too_large` past `BASE_BYTES_MAX`.
pub(crate) fn base_text(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    path: &str,
) -> Result<Arc<str>, EngineError> {
    let record = at_head(db, project, id, head)?;
    let file = text_file(&record, path)?;
    if file.status == FileStatus::Added {
        return Ok(Arc::from(""));
    }
    text_at(
        &record.repository,
        &record.base,
        file.old_path.as_deref().unwrap_or(path),
    )
}

/// Reads the text a changed file has at the head the change set recorded, the right side
/// of its IDE diff, which is what Accept lands, never the agent worktree it may still be
/// changing. Inputs: the store, the project, the change set, the head the IDE listed and
/// the path. Output: the text, or `changes_not_found` for a deleted file or any path the
/// set does not change as text, `changes_stale` once the set holds another head,
/// `changes_too_large` past `BASE_BYTES_MAX`.
pub(crate) fn head_text(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    path: &str,
) -> Result<Arc<str>, EngineError> {
    let record = at_head(db, project, id, head)?;
    let file = text_file(&record, path)?;
    if file.status == FileStatus::Deleted {
        return Err(failure("changes_not_found"));
    }
    text_at(&record.repository, &record.head, path)
}

/// Reads the text an IDE settings file has at the change set's head, the agent's side of
/// its diff, which the IDE server is not allowed to read from the agent's worktree. Only a
/// text file of this change set marked `IdeConfig` that the set did not delete is readable.
/// Inputs: the store, the project, the change set, the head the IDE listed and the path.
/// Output: the text, or `changes_not_found` for any other path, `changes_stale` once the
/// set holds another head, `changes_too_large` past `BASE_BYTES_MAX`.
pub(crate) fn config_text(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    path: &str,
) -> Result<Arc<str>, EngineError> {
    let record = at_head(db, project, id, head)?;
    let file = text_file(&record, path)?;
    if file.status == FileStatus::Deleted || !file.marks.contains(&FileMark::IdeConfig) {
        return Err(failure("changes_not_found"));
    }
    text_at(&record.repository, &record.head, path)
}

/// The file a change set changes at a path as text. Inputs: the change set and the path.
/// Output: the file, or `changes_not_found`.
fn text_file(record: &Record, path: &str) -> Result<ChangedFile, EngineError> {
    diff::listed(&record.repository, &record.base, &record.head)?
        .iter()
        .find(|file| file.path == path && file.kind == FileKind::Text)
        .cloned()
        .ok_or_else(|| failure("changes_not_found"))
}

/// Reads a file at a commit with no git filter or text conversion, as UTF-8 with each
/// invalid byte shown as U+FFFD, as the patch text shows it, so a file in a legacy encoding
/// still opens. A text read at a commit id stays among the recent texts, so its later
/// pages never read git again. Inputs: the repository, the commit and the path in it.
/// Output: the text, or `changes_too_large` past `BASE_BYTES_MAX`.
fn text_at(repository: &Path, commit: &str, path: &str) -> Result<Arc<str>, EngineError> {
    let key = (repository.to_path_buf(), commit.to_owned(), path.to_owned());
    let lasting = is_object_id(commit);
    if lasting && let Some(found) = recall(&TEXTS, &key) {
        return Ok(found);
    }
    let object = format!("{commit}:{path}");
    let repo = super::flow::shared(repository)?;
    let size: usize = String::from_utf8(repo.run(&["cat-file", "-s", &object])?)
        .ok()
        .and_then(|printed| printed.trim().parse().ok())
        .ok_or_else(|| failure("changes_git_failed"))?;
    if size > BASE_BYTES_MAX {
        return Err(failure("changes_too_large"));
    }
    let bytes = repo.capture(&["cat-file", "blob", &object], BASE_BYTES_MAX)?;
    if bytes.len() != size {
        return Err(failure("changes_git_failed"));
    }
    let text: Arc<str> = Arc::from(String::from_utf8_lossy(&bytes).into_owned());
    if lasting {
        keep(&TEXTS, key, &text, TEXTS_MAX, TEXTS_BYTES_MAX, str::len);
    }
    Ok(text)
}
