use super::*;

/// Deletes a project's master session that is not live, for the owner's Delete: first the
/// harness transcript it kept is removed from its master's private home, then a parked
/// session closes so it can no longer be resumed, and its stored turns, their owner
/// messages, its marks and its kept conversation go, so it leaves the Sessions list.
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
    for statement in [
        "DELETE FROM master_preparations WHERE session=?1",
        "DELETE FROM conversation_entries WHERE kind='message' AND event_cursor IN (SELECT input FROM master_turns WHERE session=?1)",
        "DELETE FROM master_turns WHERE session=?1",
        "DELETE FROM team_master_conversations WHERE session=?1",
        "DELETE FROM session_owner_turns WHERE session=?1",
        "DELETE FROM session_messages WHERE session=?1",
        "DELETE FROM session_marks WHERE session=?1",
    ] {
        tx.execute(statement, [id.as_str()]).map_err(error)?;
    }
    assert!(!crate::master_sessions::has_owner_turn(tx, id)?);
    Ok(Outcome::Applied)
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
