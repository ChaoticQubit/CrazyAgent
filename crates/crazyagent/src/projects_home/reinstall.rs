use super::icloud::{JOB_SKIPPED_LISTED_MAX, SKIPPED_LISTED_MAX, SKIPPED_PATH_MAX, SkippedFolders};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[cfg(all(test, unix))]
mod tests;

/// The most reinstall offers the engine keeps; past it the oldest goes first.
pub const OFFERS_MAX: usize = 64;

/// The most bytes one drafted message takes: a fixed head and tail, and per named folder its
/// path, the folder to run its command in, the command and the file that chose it.
pub const DRAFT_BYTES_MAX: usize = 1024 + SKIPPED_LISTED_MAX * (2 * SKIPPED_PATH_MAX + 200);
const _: () = assert!(DRAFT_BYTES_MAX <= crate::agents::PROMPT_BYTES_MAX);

/// The most named folders an offer's reply carries, as a job's line names three.
pub const OFFER_FOLDERS_MAX: usize = JOB_SKIPPED_LISTED_MAX;
const _: () = assert!(
    2 * (DRAFT_BYTES_MAX + OFFER_FOLDERS_MAX * SKIPPED_PATH_MAX) + 1024
        <= crate::protocol::FRAME_BYTES_MAX
);

/// The most bytes a kept record takes as stored JSON: each named path with every byte
/// escaped as JSON escapes a control character, six bytes each, and the fields around it.
pub const KEPT_BYTES_MAX: usize = SKIPPED_LISTED_MAX * (6 * SKIPPED_PATH_MAX + 64) + 64;
const _: () = assert!(KEPT_BYTES_MAX <= 65536);

/// The command that rebuilds a left folder, chosen by a file beside it: a lockfile or
/// manifest for `node_modules` and `.venv`, and the project's build for `dist` or `build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hint {
    Pnpm,
    Yarn,
    Bun,
    BunBinary,
    NpmCi,
    Npm,
    Uv,
    Poetry,
    Pip,
    Python,
    Build,
}

/// One folder a job left out, by its path below the project's folder, with the command its
/// neighbours name, or None when none does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Left {
    pub path: String,
    pub hint: Option<Hint>,
}

/// What the engine keeps for one folder until the owner dismisses it: the named left folders,
/// at most `SKIPPED_LISTED_MAX`, and how many more the listing only counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kept {
    pub left: Vec<Left>,
    pub more: u64,
}

/// A project's reinstall offer as a client gets it: the first named folders, how many more
/// there are, and the message the master gets when the owner says yes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub folders: Vec<String>,
    pub more: u64,
    pub draft: String,
}

/// What an offer keeps from a job's left folders: each named one but `__pycache__`, which
/// rebuilds itself, with the command the files beside it name, and the counted rest.
/// Inputs: the folder the job landed in, the folders its download left and the metadata
/// read, links not followed. Output: the record, with no named folder when none is left.
pub fn kept(
    landed: &Path,
    skipped: &SkippedFolders,
    look: &dyn Fn(&Path) -> std::io::Result<fs::Metadata>,
) -> Kept {
    let _ = (landed, skipped, look);
    Kept::default()
}

/// The message the master gets when the owner says yes: each named folder with where and how
/// to rebuild it, then the counted rest. Input: the record. Output: the text, within
/// `DRAFT_BYTES_MAX`.
pub fn draft(kept: &Kept) -> String {
    let _ = kept;
    String::new()
}

/// The offer a client gets for a record: its first `OFFER_FOLDERS_MAX` paths shown plain,
/// the rest counted, and the draft. Input: the record. Output: the offer.
pub fn offer(kept: &Kept) -> Offer {
    let _ = kept;
    Offer {
        folders: Vec::new(),
        more: 0,
        draft: String::new(),
    }
}

/// Whether a stored record keeps its bounds: at most `SKIPPED_LISTED_MAX` named folders,
/// each a path of at most `SKIPPED_PATH_MAX` bytes. Input: the record. Output: the answer.
pub fn bounded(kept: &Kept) -> bool {
    kept.left.len() <= SKIPPED_LISTED_MAX
        && kept
            .left
            .iter()
            .all(|left| !left.path.is_empty() && left.path.len() <= SKIPPED_PATH_MAX)
}
