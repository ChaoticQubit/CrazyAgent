use super::super::launch::{LaunchError, failure};
use super::super::transcript::{EVENTS_PER_READ_MAX, FileKey, LINE_BYTES_MAX};
use super::json_files::Pattern;
use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::SystemTime;

/// The most bytes of a rewritten record one read takes; a larger record is not read.
pub const DOCUMENT_BYTES_MAX: usize = LINE_BYTES_MAX;

/// Follows a harness record written anew as one JSON list each time, often as a new file
/// moved over the old: the newest file its record path matches, read whole after each
/// change and diffed by index. Each read returns the objects at indexes past the position,
/// in list order.
pub struct JsonRewrite {
    pattern: Pattern,
    earlier: Vec<FileKey>,
    below: Option<PathBuf>,
    position: usize,
    skipping: bool,
    seen: Option<Seen>,
}

/// What a reader last read whole with nothing left waiting: which file, how long and when
/// it last changed. The same three again are no news, and the file is not read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    key: FileKey,
    length: u64,
    modified: SystemTime,
}

impl JsonRewrite {
    /// Starts following the record of a harness about to start. Every file the record path
    /// matches now is remembered and never taken; since each rewrite is a new file, that
    /// is judged only until a record is first found. Input: the record path. Output: the
    /// reader, before its record exists.
    pub fn new(pattern: Pattern) -> Self {
        let earlier = pattern.keys();
        Self::starting(pattern, earlier, 0)
    }

    /// Follows the record of a session that already ran from the position an earlier
    /// reader reached ([`Self::position`]): the newest file the record path matches,
    /// whatever its age. Inputs: the record path and the position. Output: the reader.
    pub fn resumed(pattern: Pattern, position: usize) -> Self {
        Self::starting(pattern, Vec::new(), position)
    }

    fn starting(pattern: Pattern, earlier: Vec<FileKey>, position: usize) -> Self {
        Self {
            pattern,
            earlier,
            below: None,
            position,
            skipping: false,
            seen: None,
        }
    }

    /// Names the record this reader follows once it found one. Output: the full path, or
    /// none before the record exists.
    pub fn record(&self) -> Option<PathBuf> {
        let below = self.below.as_ref()?;
        Some(self.pattern.full(below))
    }

    /// How many entries of the record's list this reader is done with, for a later
    /// [`Self::resumed`]. Output: the position.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Looks for the record now without reading it, as a resumed run checks the record it
    /// was handed. Output: the record's full path, or none while there is none.
    pub fn find(&mut self) -> Option<PathBuf> {
        if self.below.is_none() {
            self.first();
        }
        self.record()
    }

    /// Moves past every entry the record holds now, so the next read returns only entries
    /// written after; a record caught half-written is skipped once it is whole. Output:
    /// nothing, or a plain code when the record cannot be read or is no list.
    pub fn skip_to_end(&mut self) -> Result<(), LaunchError> {
        self.skipping = true;
        self.seen = None;
        let skipped = self.read()?;
        assert!(skipped.is_empty());
        Ok(())
    }

    /// Reads the record again when it changed and returns the entries past the position. A
    /// record that does not parse yet is no news; an entry already returned and changed
    /// since is not returned again; a shorter list moves the position back with a note;
    /// entries that are no JSON object are passed over; past `EVENTS_PER_READ_MAX` the
    /// rest wait. Output: the objects, or a plain code for a record that is no JSON list.
    pub fn read(&mut self) -> Result<Vec<Value>, LaunchError> {
        let opened = match self.below.clone() {
            Some(below) => self.pattern.open(&below),
            None => self.first(),
        };
        let Some((file, key)) = opened else {
            return Ok(Vec::new());
        };
        let metadata = file
            .metadata()
            .map_err(|_| failure("launch_transcript_unavailable"))?;
        let seen = Seen {
            key,
            length: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        };
        if self.seen.as_ref() == Some(&seen) {
            return Ok(Vec::new());
        }
        let mut bytes = Vec::new();
        let taken = file
            .take(DOCUMENT_BYTES_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure("launch_transcript_unavailable"))?;
        assert_eq!(taken, bytes.len());
        if taken > DOCUMENT_BYTES_MAX {
            eprintln!("launch_record_document_skipped over={DOCUMENT_BYTES_MAX}");
            self.seen = Some(seen);
            return Ok(Vec::new());
        }
        let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
            self.seen = Some(seen);
            return Ok(Vec::new());
        };
        let Value::Array(entries) = document else {
            eprintln!("launch_record_not_a_list");
            return Err(failure("launch_transcript_unavailable"));
        };
        Ok(self.diffed(entries, seen))
    }

    /// Opens the newest record not present at the start and keeps its place. Output: the
    /// file and its identity, or none while there is none.
    fn first(&mut self) -> Option<(File, FileKey)> {
        let (file, below, key) = self.pattern.newest(&self.earlier)?;
        self.below = Some(below);
        Some((file, key))
    }

    /// Takes the entries of one whole read past the position, at most
    /// `EVENTS_PER_READ_MAX` of them, and remembers the read as seen once none wait.
    /// Inputs: the list and what was read. Output: the objects among the entries taken.
    fn diffed(&mut self, entries: Vec<Value>, seen: Seen) -> Vec<Value> {
        let count = entries.len();
        if count < self.position {
            eprintln!("launch_record_shrunk from={} to={count}", self.position);
            self.position = count;
        }
        if self.skipping {
            self.position = count;
            self.skipping = false;
        }
        let end = count.min(self.position + EVENTS_PER_READ_MAX);
        let objects: Vec<Value> = entries
            .into_iter()
            .skip(self.position)
            .take(end - self.position)
            .filter(Value::is_object)
            .collect();
        self.position = end;
        self.seen = (end == count).then_some(seen);
        assert!(self.position <= count);
        assert!(objects.len() <= EVENTS_PER_READ_MAX);
        objects
    }
}

#[cfg(all(test, unix))]
#[path = "json_rewrite_tests.rs"]
mod tests;
