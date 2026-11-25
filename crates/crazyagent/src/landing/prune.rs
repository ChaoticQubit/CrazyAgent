use super::flow::{discarded_ref, set_engine_ref, shared};
use super::hold::hold;
use super::store;
use super::{ChangeSetId, ChangeState};
use crate::engine::EngineError;
use rusqlite::Connection;

/// Deletes the discarded refs whose restore window ended by `now`, once each. A change set
/// whose ref cannot be deleted now is logged by its plain code and tried again at the next
/// run, so it never stops the others. Inputs: the store and the time now. Output: how many
/// were deleted.
pub(super) fn prune(db: &Connection, now: u64) -> Result<usize, EngineError> {
    store::initialize(db)?;
    let mut pruned = 0;
    for found in store::discarded_due(db, now)? {
        let _held = hold(&found.repository);
        match prune_one(db, &found.id) {
            Ok(true) => pruned += 1,
            Ok(false) => {}
            Err(error) => eprintln!(
                "landing_prune_failed cause={}",
                crate::protocol::plain_code(&error.0)
            ),
        }
    }
    Ok(pruned)
}

/// Deletes one discarded change set's ref, marks it pruned and forgets the owner edits
/// settled on it. The caller holds [`hold`] on its repository. Inputs: the store and the change set. Output: whether it was pruned
/// now, false when it is no longer discarded or already pruned.
fn prune_one(db: &Connection, id: &ChangeSetId) -> Result<bool, EngineError> {
    let mut record = store::load(db, id)?;
    if record.state != ChangeState::Discarded || record.pruned {
        return Ok(false);
    }
    if std::fs::symlink_metadata(record.repository.join(".git")).is_ok_and(|found| found.is_dir()) {
        set_engine_ref(
            &shared(&record.repository)?,
            &record.repository,
            &discarded_ref(&record.id),
            None,
        )?;
    }
    record.pruned = true;
    store::save(db, &record)?;
    super::owner_edit::logged("forget", super::owner_edit::forget(db, &record.id));
    Ok(true)
}
