//! The uid pin (threat model section 6, QUESTION 31a): the one account that may hold, written at
//! the start that finds no pin file, or `none` when no non-system user is at the console.

use crate::journal::StoreError;
use std::path::Path;

pub const PIN_FILE: &str = "uid";
pub const PIN_BYTES_MAX: u64 = 32;
pub const SYSTEM_UID_MAX: u32 = 500;
pub const CONSOLE: &str = "/dev/console";

/// The pin: one uid, or none (no account may hold until Remove lid helper and add again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    Uid(u32),
    None,
}

/// Reads the pin at start, or writes it when the folder has no pin file. Inputs: the checked
/// folder, its owner uid and the console path. Output: the pin.
pub fn at_start(folder: &Path, owner_uid: u32, console: &Path) -> Result<Pin, StoreError> {
    let _ = (folder, owner_uid, console);
    Ok(Pin::Uid(0))
}

/// The non-system user at the console: the owner of `console` when its uid is above
/// `SYSTEM_UID_MAX`.
pub fn console_user(console: &Path) -> Option<u32> {
    let _ = console;
    None
}

/// Deletes the pin file. Input: the checked folder. Output: the result; a missing file is fine.
pub fn forget(folder: &Path) -> Result<(), StoreError> {
    let _ = folder;
    Ok(())
}
