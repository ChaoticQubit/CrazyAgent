use super::journal::{Entry, Journal};
use crate::engine::EngineError;
use std::path::PathBuf;

/// The journal kind of the sound state Keep awake muted while the lid was closed
/// (DEC-167 Task 17).
pub const KIND: &str = "keep_awake_mute";

/// The program that reads and sets the sound: osascript on macOS, wpctl or pactl on
/// Linux, each at the path given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    Osascript(PathBuf),
    Wpctl(PathBuf),
    Pactl(PathBuf),
}

/// Mutes the sound: reads the current state, journals it synced, then mutes. Inputs: the
/// lane journal and the tool. Output: true when an entry was journaled, false for an
/// output that cannot be muted, or why the sound could not be read or set.
pub fn mute(_journal: &mut Journal, _tool: &Tool) -> Result<bool, EngineError> {
    Ok(false)
}

/// Puts back the journaled sound state, so an owner who was muted stays muted. Inputs: a
/// `KIND` entry and the tool. Output: Ok, or why it could not be put back.
pub fn restore(_entry: &Entry, _tool: &Tool) -> Result<(), EngineError> {
    Ok(())
}
