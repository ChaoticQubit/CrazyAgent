use crate::dacl::{
    ADMINISTRATORS, SYSTEM, allowing, inherited_grantee, inherited_label, label, labelled,
    other_writer, trusted_owner,
};

#[cfg(windows)]
pub use crate::win::private::{
    check, dir, facts, file, free, identity, lock, read_only, repair, same, secret_file, store_dir,
    sync_folder, unlock, user_sid,
};

const FULL_ACCESS: u32 = 0x001f_01ff;
const FOLDER_INHERIT: u8 = 0x03;
const FILES_IN_FOLDER: u8 = 0x01 | 0x04 | 0x08;
const ANY_RIGHT: u32 = u32::MAX;
const MEDIUM_LEVEL: u32 = 0x2000;
const NO_WRITE_UP: u32 = 0x1;
const NO_READ_UP: u32 = 0x2;
const NO_EXECUTE_UP: u32 = 0x4;
const FULL_POLICY: u32 = NO_WRITE_UP | NO_READ_UP | NO_EXECUTE_UP;

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
/// lets a lower integrity process write, read or hold it; an object with no label passes.
/// Inputs: the facts and the user's SID. Output: Ok, or the reason it is not private.
pub fn judge(facts: &Facts, user: &[u8]) -> Result<(), &'static str> {
    judged(facts, user, FULL_POLICY)
}

/// Judges a private folder: [`judge`] on the folder itself, and, since on Windows a child is
/// opened by its own path whatever its folder allows, no inheritable allow entry for another
/// account and no inheritable label below medium or without the full policy, so what is made
/// in it, SQLite's side files included, starts private. Inputs: the facts and the user's
/// SID. Output: Ok, or the reason it is not private.
pub fn judge_folder(facts: &Facts, user: &[u8]) -> Result<(), &'static str> {
    folder_judged(facts, user, FULL_POLICY)
}

/// Judges a store folder before it takes its store's label: [`judge_folder`], save that a
/// medium label an earlier build gave it, own or handed down, passes with no-write-up alone,
/// as the full label then replaces it; a label below medium or without no-write-up, or a
/// second label entry, is refused. Inputs: the facts and the user's SID. Output: Ok, or the
/// reason it is not private.
pub fn judge_store_before_label(facts: &Facts, user: &[u8]) -> Result<(), &'static str> {
    folder_judged(facts, user, NO_WRITE_UP)
}

/// The work of [`judge`]: an own label must be medium or above and carry every bit of
/// `wanted`, which always holds no-write-up. Inputs: the facts, the user's SID and the
/// policy bits wanted. Output: Ok, or the reason it is not private.
fn judged(facts: &Facts, user: &[u8], wanted: u32) -> Result<(), &'static str> {
    assert!(wanted & NO_WRITE_UP != 0 && wanted & !FULL_POLICY == 0);
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
    {
        if level < MEDIUM_LEVEL || policy & NO_WRITE_UP == 0 {
            return Err("a lower integrity process may change a private path");
        }
        if policy & wanted != wanted {
            return Err("a lower integrity process may read or hold a private path");
        }
    }
    Ok(())
}

/// The work of [`judge_folder`] and [`judge_store_before_label`]: [`judged`] on the folder
/// itself, no inheritable allow entry for another account, and an inheritable label medium
/// or above carrying every bit of `wanted`. Inputs: the facts, the user's SID and the policy
/// bits wanted. Output: Ok, or the reason it is not private.
fn folder_judged(facts: &Facts, user: &[u8], wanted: u32) -> Result<(), &'static str> {
    judged(facts, user, wanted)?;
    let list = facts
        .list
        .as_deref()
        .expect("judge refuses an object with no access list");
    if inherited_grantee(list, user)?.is_some() {
        return Err("a private folder hands another account access to what is made in it");
    }
    if let Some(sacl) = facts.label.as_deref()
        && let Some((level, policy)) = inherited_label(sacl)?
        && (level < MEDIUM_LEVEL || policy & wanted != wanted)
    {
        return Err("a private folder hands a lower integrity label to what is made in it");
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

/// The engine stores that carry the full medium label (#524 r3, V244-WIN-STORE-LABEL-NX): a
/// secret store (account keys, MCP secrets) and the database folder, the data root, whose
/// files engine.db, its SQLite side files and the GitHub sign-in are. Every other private
/// folder carries no label, as it may hold a root granted to an AppContainer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    Secret,
    Database,
}

/// The system list a store folder is given (rv-sec2 N1, #524 r3, V244-WIN-STORE-LABEL-NX): a
/// medium label with no-write-up, no-read-up and no-execute-up, so a lower integrity process
/// of the user can neither change, read nor hold what it covers. A secret folder labels
/// itself and all made in it; the database folder only the files made directly in it.
/// Input: the store. Output: the bytes.
pub fn label_list(store: Store) -> Vec<u8> {
    let flags = match store {
        Store::Secret => FOLDER_INHERIT,
        Store::Database => FILES_IN_FOLDER,
    };
    labelled(MEDIUM_LEVEL, FULL_POLICY, flags)
}

/// The system list a secret file is created with (rv-sec3 #555 N1, rv-code #562 F1): a medium
/// label with no-write-up, no-read-up and no-execute-up, since a lower integrity process may
/// otherwise open it for execute only and, sharing nothing, block its rename. It sits on the
/// file itself with nothing to pass on, so it holds in an unlabelled folder. Output: the bytes.
pub fn secret_file_label() -> Vec<u8> {
    labelled(MEDIUM_LEVEL, FULL_POLICY, 0)
}
