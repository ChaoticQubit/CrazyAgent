use crate::dacl::{ADMINISTRATORS, SYSTEM, allowing, label, other_writer, trusted_owner};

#[cfg(windows)]
pub use crate::win::private::{
    check, dir, facts, file, free, identity, lock, read_only, repair, same, sync_folder, unlock,
    user_sid,
};

const FULL_ACCESS: u32 = 0x001f_01ff;
const FOLDER_INHERIT: u8 = 0x03;
const ANY_RIGHT: u32 = u32::MAX;
const MEDIUM_LEVEL: u32 = 0x2000;
const NO_WRITE_UP: u32 = 0x1;

/// What W1's check reads from one opened private folder or file (DESIGN-0140 D1): its owner
/// SID, its access list (None when it has none), its system list holding the mandatory label
/// (None when it has none), whether it is a reparse point, its hard link count and whether
/// it sits on a network volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub owner: Vec<u8>,
    pub list: Option<Vec<u8>>,
    pub label: Option<Vec<u8>>,
    pub reparse: bool,
    pub links: u32,
    pub remote: bool,
}

/// Judges whether an object is private to the user, the Windows twin of the Unix owner,
/// mode 0600/0700, `nlink == 1` and no-follow checks: no link or junction, one name, a local
/// volume, a trusted owner, no other SID with any right that applies to it, and no label that
/// lets a lower integrity process write it. Inputs: the facts and the user's SID. Output: Ok,
/// or the reason it is not private.
pub fn judge(facts: &Facts, user: &[u8]) -> Result<(), &'static str> {
    if facts.reparse {
        return Err("a private path is a link or a junction");
    }
    if facts.links != 1 {
        return Err("a private file has more than one name");
    }
    if facts.remote {
        return Err("a private path is on a network volume");
    }
    if !trusted_owner(&facts.owner, user) {
        return Err("another account owns a private path");
    }
    let list = facts
        .list
        .as_deref()
        .ok_or("a private path has no access list, so everyone may open it")?;
    if other_writer(list, user, ANY_RIGHT)?.is_some() {
        return Err("another account may open a private path");
    }
    if let Some(sacl) = facts.label.as_deref()
        && let Some((level, policy)) = label(sacl)?
        && (level < MEDIUM_LEVEL || policy & NO_WRITE_UP == 0)
    {
        return Err("a lower integrity process may change a private path");
    }
    Ok(())
}

/// The access list a new private folder or file is created with (DESIGN-0140 D1): full
/// access for the user, SYSTEM and the administrators only, inherited by everything made in
/// a folder, so SQLite's side files get it too (D2). Inputs: the user's SID and whether the
/// object is a folder. Output: the list bytes.
pub fn list(user: &[u8], folder: bool) -> Vec<u8> {
    let flags = if folder { FOLDER_INHERIT } else { 0 };
    let planned = allowing(
        &[user, &SYSTEM[..], &ADMINISTRATORS[..]],
        flags,
        FULL_ACCESS,
    );
    assert!(planned.len() > 8);
    planned
}
