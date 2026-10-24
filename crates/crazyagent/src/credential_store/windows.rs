use crate::credentials::{Store, StoreError};
use std::sync::Arc;

/// Opens Windows Credential Manager for one engine namespace. Not built yet: always
/// `credential_store_unavailable` until the windows credential store track replaces it.
/// Input: the namespace. Output: the shared store. Not tested on this platform.
pub(super) fn open(_namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    Err(StoreError::code("credential_store_unavailable"))
}
