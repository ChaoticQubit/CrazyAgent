use super::error;
use crate::engine::EngineError;
use crate::engine::owner::{Admission, HostStamp};
use rusqlite::{Transaction, params};

#[cfg(all(test, unix))]
mod tests;

pub(crate) fn check_owner_host(tx: &Transaction<'_>, stamp: &HostStamp) -> Result<(), EngineError> {
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_meta WHERE singleton=1 AND schema_version=1 AND db_instance=?1 AND engine_epoch=?2 AND runtime_state='active')", params![stamp.db_instance, stamp.engine_epoch], |r|r.get(0)).map_err(error)?;
    if !valid {
        return Err(error("owner_runtime_stale"));
    }
    Ok(())
}

pub(crate) fn check_owner_admission(
    tx: &Transaction<'_>,
    a: &Admission,
) -> Result<(), EngineError> {
    check_owner_host(
        tx,
        &HostStamp {
            db_instance: a.db_instance.clone(),
            engine_epoch: a.engine_epoch.clone(),
        },
    )?;
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_identities WHERE owner_id=?1 AND owner_epoch=?2 AND status='active')", params![a.owner_id,a.owner_epoch], |r|r.get(0)).map_err(error)?;
    if !valid {
        return Err(error("owner_grant_stale_or_revoked"));
    }
    Ok(())
}
