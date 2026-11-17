use super::{Fixed, Roots};
use std::path::PathBuf;

/// The folders a command writes or runs from, which no other job may change while it runs.
/// Inputs: the command and the roots. Output: the folders.
pub(super) fn folders(_fixed: &Fixed, _roots: &Roots) -> Vec<PathBuf> {
    Vec::new()
}
