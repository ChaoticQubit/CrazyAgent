//! The uid pin (threat model section 6, QUESTION 31a): the one account that may hold, written at
//! the start that finds no pin file, or `none` when no non-system user is at the console.

use crate::journal::{StoreError, checked_folder, read_small, sync_folder, write_atomic};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
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
    let folder = checked_folder(folder, owner_uid)?;
    let path = folder.join(PIN_FILE);
    if let Some(bytes) = read_small(&path, owner_uid, PIN_BYTES_MAX)? {
        return parse(&bytes);
    }
    let pin = console_user(console).map_or(Pin::None, Pin::Uid);
    let text = text(pin);
    write_atomic(&folder, PIN_FILE, text.as_bytes())?;
    assert_eq!(parse(text.as_bytes()).ok(), Some(pin));
    Ok(pin)
}

/// The pin file's text: the uid or `none`, then a newline.
fn text(pin: Pin) -> String {
    match pin {
        Pin::Uid(uid) => {
            assert!(uid > SYSTEM_UID_MAX);
            format!("{uid}\n")
        }
        Pin::None => "none\n".to_string(),
    }
}

/// Parses the pin file: `none` or decimal digits for a uid above `SYSTEM_UID_MAX`, then one
/// newline. Output: the pin, or `Unreadable`.
fn parse(bytes: &[u8]) -> Result<Pin, StoreError> {
    let unreadable = || StoreError::Unreadable("pin file".to_string());
    let text = std::str::from_utf8(bytes).map_err(|_| unreadable())?;
    let line = text.strip_suffix('\n').ok_or_else(unreadable)?;
    if line == "none" {
        return Ok(Pin::None);
    }
    if line.is_empty() || !line.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(unreadable());
    }
    let uid: u32 = line.parse().map_err(|_| unreadable())?;
    if uid <= SYSTEM_UID_MAX {
        return Err(unreadable());
    }
    Ok(Pin::Uid(uid))
}

/// The non-system user at the console: the owner of `console` when its uid is above
/// `SYSTEM_UID_MAX`.
pub fn console_user(console: &Path) -> Option<u32> {
    let uid = fs::metadata(console).ok()?.uid();
    (uid > SYSTEM_UID_MAX).then_some(uid)
}

/// Deletes the pin file. Input: the checked folder. Output: the result; a missing file is fine.
pub fn forget(folder: &Path) -> Result<(), StoreError> {
    match fs::remove_file(folder.join(PIN_FILE)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StoreError::Write(error)),
    }
    assert!(fs::symlink_metadata(folder.join(PIN_FILE)).is_err());
    sync_folder(folder)
}
