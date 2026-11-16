use super::super::manifest::{Manifest, Refused};
use super::super::transcript::Speaker;
use crate::adapters::LINE_BYTES_MAX;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The most past conversation records one import reads.
pub const RECORDS_MAX: usize = 4096;
/// The most folder entries, and names looked up, one import's search of the owner's folders
/// takes.
pub const ENTRIES_SCANNED_MAX: usize = 65_536;
/// The most bytes one import reads of one record; a longer record is read up to here.
pub const RECORD_BYTES_MAX: u64 = 64 * 1024 * 1024;
/// The most messages one imported conversation keeps.
pub const MESSAGES_MAX: usize = 4096;
/// The most bytes one import reads in all.
pub const IMPORT_BYTES_MAX: u64 = 256 * 1024 * 1024;
/// A record that could not be opened as a plain file below the owner's folders.
pub const UNREADABLE: &str = "import_record_unreadable";
/// A whole-document record larger than one import reads of a record.
pub const TOO_LARGE: &str = "import_record_too_large";
/// A record with no message whose lines or document could not be read as JSON or text.
pub const BROKEN: &str = "import_record_broken";
/// A record that could be read and holds no message.
pub const EMPTY: &str = "import_record_empty";
/// What stands in imported text for a relay token, `crazyagent-relay-` and 64 lowercase hex
/// digits, which a tool the harness ran may have printed into a conversation.
pub const RELAY_REDACTED: &str = "crazyagent-relay-[redacted]";

/// How much one import reads: records, folder entries scanned, bytes of one record, bytes of
/// one line, messages of one conversation and bytes in all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    records: usize,
    entries: usize,
    record_bytes: u64,
    line_bytes: usize,
    messages: usize,
    total_bytes: u64,
}

impl Bounds {
    const DEFAULT: Self = Self {
        records: RECORDS_MAX,
        entries: ENTRIES_SCANNED_MAX,
        record_bytes: RECORD_BYTES_MAX,
        line_bytes: LINE_BYTES_MAX,
        messages: MESSAGES_MAX,
        total_bytes: IMPORT_BYTES_MAX,
    };
}

/// One message of an imported conversation: its place in the conversation from 0, who said
/// it and its text, clipped to `FIELD_BYTES_MAX`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub ordinal: usize,
    pub speaker: Speaker,
    pub text: String,
}

/// One past conversation read from one record: the record's place below the owner's home,
/// the working folder the record names, its last change, its messages in order, and whether
/// a bound clipped it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub record: PathBuf,
    pub cwd: Option<String>,
    pub modified: SystemTime,
    pub messages: Vec<Message>,
    pub clipped: bool,
}

/// One record an import passed over: its place below the owner's home and the reason code,
/// which [`sentence`] says in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub record: PathBuf,
    pub reason: &'static str,
}

/// What one import read: the conversations and the skipped records, each in record order,
/// and whether a bound on records, folder entries or bytes in all stopped it early.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub conversations: Vec<Conversation>,
    pub skipped: Vec<Skipped>,
    pub cut: bool,
}

/// Reads the past conversations of one harness from the owner's own harness folders, only
/// reading them, by the records its manifest's `import.conversations` pattern matches.
/// Inputs: the manifest and the absolute owner home. Output: what was read, nothing without
/// the key, or a refusal that names the setting.
pub fn import(manifest: &Manifest, owner_home: &Path) -> Result<Imported, Refused> {
    import_within(manifest, owner_home, &Bounds::DEFAULT)
}

/// [`import`] within the given bounds. Inputs: the manifest, the absolute owner home and the
/// bounds. Output: as [`import`].
fn import_within(
    manifest: &Manifest,
    owner_home: &Path,
    bounds: &Bounds,
) -> Result<Imported, Refused> {
    let _ = (manifest, owner_home, bounds);
    Err(Refused {
        code: "import_stub",
        message: "import.conversations: not read yet.".into(),
    })
}

/// The plain sentence of one skip reason. Input: the code. Output: the sentence, or none for
/// a code that is not a skip reason.
pub fn sentence(code: &str) -> Option<&'static str> {
    let _ = code;
    None
}

#[cfg(all(test, unix))]
#[path = "past_tests.rs"]
mod tests;
