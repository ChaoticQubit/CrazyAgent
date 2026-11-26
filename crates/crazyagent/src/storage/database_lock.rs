use super::*;

/// Releases the engine database lock and reports whether the unlock succeeded.
/// Takes the hold. Returns once the lock is released, or `database_lock_release_failed`.
pub(super) fn release_database_lock(lock: EngineLock) -> Result<(), EngineError> {
    unlock_database(&lock.file)
}
