use super::*;
use crate::master_session::{SESSION_DELETED, SessionDeleted};
use crate::state_log::fence::Fence;
use crate::state_log::replay::{Logged, redact_matching};
use crate::state_log::{Aggregate, Appender, Draft};

/// Deletes a project's master session that is not live, for the owner's Delete: first the
/// harness transcript it kept is removed from its master's private home, then a parked
/// session closes so it can no longer be resumed, and its stored turns, their owner
/// messages and its marks go, so it leaves the Sessions list; its kept conversation goes
/// with the event [`log_deleted`] logs once the request applies.
/// Takes the transaction, the project, the session and the revision read at.
/// Returns Applied, or a plain rejection before any write.
pub(super) fn delete_session(
    tx: &Transaction<'_>,
    project: &ProjectId,
    id: &SessionId,
    revision: u64,
) -> Result<Outcome, EngineError> {
    let Some(mut session) =
        load_session(tx, id, revision)?.filter(|session| &session.project_id == project)
    else {
        return Ok(reject("master_session_missing"));
    };
    if session.phase == MasterSessionPhase::Configured {
        return Ok(reject("master_session_live"));
    }
    if let Some(kept) = crate::team::master::session_conversation(tx, project, id.as_str())? {
        let Some(runtime) = tx.path().map(Path::new).and_then(Path::parent) else {
            return Ok(reject("master_session_files_kept"));
        };
        let home = crate::team::runtime::agent_home(&runtime.join("team"), kept.agent);
        if !remove_transcript(&home, &kept.resume) {
            return Ok(reject("master_session_files_kept"));
        }
    }
    if session.phase == MasterSessionPhase::Parked {
        end_session(tx, &mut session, revision, MasterSessionPhase::Closed)?;
        save_session(tx, &session)?;
    }
    crate::learning::latch(tx);
    crate::sessions_log::mark_session_entries(tx, id.as_str())?;
    crate::sessions_log::release_session_turns(tx, id.as_str())?;
    for statement in [
        "DELETE FROM master_preparations WHERE session=?1",
        "DELETE FROM conversation_entries WHERE kind='message' AND event_cursor IN (SELECT input FROM master_turns WHERE session=?1)",
        "DELETE FROM master_turns WHERE session=?1",
        "DELETE FROM session_owner_turns WHERE session=?1",
        "DELETE FROM session_messages WHERE session=?1",
        "DELETE FROM session_marks WHERE session=?1",
    ] {
        tx.execute(statement, [id.as_str()]).map_err(error)?;
    }
    assert!(!crate::master_sessions::has_owner_turn(tx, id)?);
    Ok(Outcome::Applied)
}

/// Logs an applied owner Delete at the request's revision: one [`SESSION_DELETED`] of the two
/// ids, applied by the team projection; the redaction of every earlier Sessions and Team
/// event naming the session (L1-N1); a close of the closed row, its Resolve and receipts, so
/// their final saves name it. Inputs: the transaction, revision, admission, project and
/// session. Output: how many events were redacted, or the refusal, which rolls all back.
pub(super) fn log_deleted(
    tx: &Transaction<'_>,
    revision: u64,
    admission: &Admission,
    project: &ProjectId,
    session: &SessionId,
) -> Result<usize, EngineError> {
    let fence = Fence::Owner(admission.clone());
    let mut log = Appender::owner(tx, revision, fence.clone())?;
    let body = serde_json::to_vec(&SessionDeleted {
        project: project.clone(),
        session: session.clone(),
    })
    .map_err(error)?;
    let at_ms = now_ms()?;
    let seq = log.append(&Draft {
        aggregate: Aggregate::Sessions,
        kind: SESSION_DELETED,
        schema: 1,
        at_ms,
        body: &body,
    })?;
    let event = Logged {
        seq,
        revision,
        aggregate: Aggregate::Sessions,
        kind: SESSION_DELETED.into(),
        schema: 1,
        at_ms,
        body,
    };
    log.project(|tx| crate::team::master::project_session_deleted(tx, &event))?;
    let redacted = redact_matching(
        tx,
        &[Aggregate::Sessions, Aggregate::Team],
        None,
        "session",
        session.as_str(),
        seq,
        &fence,
    )?;
    crate::sessions_log::mark_session(tx, project.as_str(), session.as_str())?;
    crate::sessions_log::mark_session_rest(tx, project.as_str(), session.as_str())?;
    crate::sessions_log::close(&mut log)?;
    Ok(redacted)
}

/// Moves the WAL into the database file and empties it after a Delete redacted earlier
/// events, so no old body stays in the WAL, as `redact` asks of its caller, trying again
/// while a reader keeps it busy, up to the bound of the shared checkpoint. A checkpoint
/// still busy is logged, not returned: the delete is already committed. Input: the
/// connection. Output: none.
pub(super) fn checkpoint(db: &Connection) {
    if !crate::state_log::replay::checkpoint(db) {
        eprintln!("state_log_redact_checkpoint_busy");
    }
}

/// How many folder levels a deleted session's transcript folder may hold before its removal
/// stops and keeps the files; Claude Code keeps two, the sub-agent and tool-result folders.
#[cfg(unix)]
const TRANSCRIPT_FOLDER_DEPTH_MAX: u32 = 16;

/// Removes a deleted session's harness transcript from its master's private home: the
/// record and the folder Claude Code keeps beside it under the conversation id. The
/// record's folder is opened once from the home, one plain name at a time with no link
/// followed, and both are removed through that open folder, so a folder swapped for a link
/// meanwhile is never removed through (RV-DS241-21). Takes the master's home and the kept
/// conversation. Returns true once neither is left, false when one lies outside the home
/// or could not be removed.
#[cfg(unix)]
pub(super) fn remove_transcript(
    home: &Path,
    kept: &crate::harnesses::terminal_session::Resume,
) -> bool {
    use rustix::fs::{AtFlags, FileType, OFlags, statat, unlinkat};
    use std::os::fd::AsFd;
    use std::path::Component;
    let Ok(below) = kept.record.strip_prefix(home) else {
        return false;
    };
    let (Some(folder), Some(name)) = (below.parent(), below.file_name()) else {
        return false;
    };
    let folders = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let Ok(parent) = crate::platform::open_beneath(home, folder, folders) else {
        return false;
    };
    let removed_record = match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
            unlinkat(&parent, name, AtFlags::empty()).is_ok()
        }
        Ok(_) => false,
        Err(missing) => missing == rustix::io::Errno::NOENT,
    };
    let mut parts = Path::new(&kept.id).components();
    let removed_folder = match (parts.next(), parts.next()) {
        (Some(Component::Normal(id)), None) => {
            match statat(&parent, id, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Directory => {
                    remove_tree_at(parent.as_fd(), id, TRANSCRIPT_FOLDER_DEPTH_MAX)
                }
                Ok(_) => true,
                Err(missing) => missing == rustix::io::Errno::NOENT,
            }
        }
        _ => true,
    };
    removed_record && removed_folder
}

/// Removes a folder and everything in it through the open folder that holds it, with no
/// link followed: an entry is read with its own type, never the listing's, a link inside
/// is removed as itself and a folder is entered only while it is still a folder. Takes the
/// open holding folder, the folder's name and how many levels it may still hold. Returns
/// true once the folder is gone, false when an entry stayed or the levels ran out.
#[cfg(unix)]
fn remove_tree_at<P: rustix::path::Arg + Copy>(
    holder: std::os::fd::BorrowedFd<'_>,
    name: P,
    depth: u32,
) -> bool {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat, unlinkat};
    use std::os::fd::AsFd;
    let Some(inner) = depth.checked_sub(1) else {
        return false;
    };
    let folders = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let Ok(folder) = openat(holder, name, folders, Mode::empty()) else {
        return false;
    };
    let Ok(entries) = Dir::read_from(&folder) else {
        return false;
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            names.push(entry.file_name().to_owned());
        }
    }
    let emptied = names.iter().all(|child| {
        match statat(&folder, child.as_c_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Directory => {
                remove_tree_at(folder.as_fd(), child.as_c_str(), inner)
            }
            Ok(_) => unlinkat(&folder, child.as_c_str(), AtFlags::empty()).is_ok(),
            Err(missing) => missing == rustix::io::Errno::NOENT,
        }
    });
    emptied && unlinkat(holder, name, AtFlags::REMOVEDIR).is_ok()
}

/// Removes a deleted session's harness transcript from its master's private home: the
/// record and the folder Claude Code keeps beside it under the conversation id. Nothing
/// outside the home, nothing that is a link and nothing reached through a link is removed.
/// Takes the master's home and the kept conversation. Returns true once neither is left,
/// false when one lies outside the home or could not be removed.
#[cfg(not(unix))]
pub(super) fn remove_transcript(
    home: &Path,
    kept: &crate::harnesses::terminal_session::Resume,
) -> bool {
    let record = kept.record.as_path();
    if !crate::harnesses::transcript::plain_below(home, record) {
        return false;
    }
    let removed_record = match std::fs::symlink_metadata(record) {
        Ok(metadata) if metadata.is_file() => std::fs::remove_file(record).is_ok(),
        Ok(_) => false,
        Err(missing) => missing.kind() == std::io::ErrorKind::NotFound,
    };
    let Some(folder) = record.parent().map(|parent| parent.join(&kept.id)) else {
        return false;
    };
    let removed_folder = !crate::harnesses::transcript::plain_below(home, &folder)
        || match std::fs::symlink_metadata(&folder) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&folder).is_ok(),
            Ok(_) => true,
            Err(missing) => missing.kind() == std::io::ErrorKind::NotFound,
        };
    removed_record && removed_folder
}
