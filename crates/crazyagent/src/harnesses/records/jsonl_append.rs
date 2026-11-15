use super::super::launch::{LaunchError, failure};
use super::super::transcript::{EVENTS_PER_READ_MAX, FileKey, LINE_BYTES_MAX, READ_BYTES_MAX};
use super::json_files::Pattern;
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

/// Follows a harness record that grows by whole JSON lines, such as pi's session file: the
/// newest file its record path matches, read as the harness appends. Each read returns the
/// JSON objects of the lines finished since the last, in record order, within the bounds
/// `transcript.rs` keeps.
pub struct JsonlAppend {
    pattern: Pattern,
    earlier: Vec<FileKey>,
    start: u64,
    held: Option<Held>,
    pending: Vec<u8>,
    scanned: usize,
    skipping: bool,
}

/// The record a reader holds open: its place below the home, which file it is and how many
/// of its bytes the reader has taken.
struct Held {
    file: File,
    below: PathBuf,
    key: FileKey,
    taken: u64,
}

impl JsonlAppend {
    /// Starts following the record of a harness about to start. Every file the record path
    /// matches now is remembered and never taken, since an earlier session may have left
    /// it. Input: the record path. Output: the reader, before its record exists.
    pub fn new(pattern: Pattern) -> Self {
        let earlier = pattern.keys();
        Self::starting(pattern, earlier, 0)
    }

    /// Follows the record of a session that already ran from a position an earlier reader
    /// reached ([`Self::position`]): the newest file the record path matches, whatever its
    /// age, and from its end when it is now shorter than the position. Inputs: the record
    /// path and the position. Output: the reader.
    pub fn resumed(pattern: Pattern, position: u64) -> Self {
        Self::starting(pattern, Vec::new(), position)
    }

    fn starting(pattern: Pattern, earlier: Vec<FileKey>, start: u64) -> Self {
        Self {
            pattern,
            earlier,
            start,
            held: None,
            pending: Vec::new(),
            scanned: 0,
            skipping: false,
        }
    }

    /// Names the record this reader follows once it found one. Output: the full path, or
    /// none before the record exists.
    pub fn record(&self) -> Option<PathBuf> {
        let held = self.held.as_ref()?;
        Some(self.pattern.full(&held.below))
    }

    /// The byte just past the last line this reader is done with, for a later
    /// [`Self::resumed`]. Output: the position; before the record is found, the one this
    /// reader starts from.
    pub fn position(&self) -> u64 {
        let Some(held) = self.held.as_ref() else {
            return self.start;
        };
        let pending = self.pending.len() as u64;
        assert!(pending <= held.taken);
        held.taken - pending
    }

    /// Looks for the record now without reading it, as a resumed run checks the record it
    /// was handed. Output: the record's full path, or none while there is none.
    pub fn find(&mut self) -> Option<PathBuf> {
        self.follow();
        self.record()
    }

    /// Moves past everything the record holds now, looking for the record first when none
    /// is held, so the next read returns only lines written after this call. Output:
    /// nothing, or a plain code when the record cannot be read.
    pub fn skip_to_end(&mut self) -> Result<(), LaunchError> {
        self.follow();
        let Some(held) = self.held.as_mut() else {
            return Ok(());
        };
        held.taken = held
            .file
            .seek(SeekFrom::End(0))
            .map_err(|_| failure("launch_transcript_unavailable"))?;
        self.pending.clear();
        self.scanned = 0;
        self.skipping = false;
        Ok(())
    }

    /// Reads the lines the harness finished since the last read. A half-written last line
    /// waits for its newline, a line past `LINE_BYTES_MAX` is passed over to its newline,
    /// a line that is no JSON object is passed over, and past `EVENTS_PER_READ_MAX` objects
    /// the rest wait for the next read. Output: the objects in record order, none before
    /// the record exists, or a plain code when it cannot be read.
    pub fn read(&mut self) -> Result<Vec<Value>, LaunchError> {
        self.follow();
        let Some(held) = self.held.as_mut() else {
            return Ok(Vec::new());
        };
        let room = (LINE_BYTES_MAX + READ_BYTES_MAX).saturating_sub(self.pending.len());
        let count = Read::by_ref(&mut held.file)
            .take(READ_BYTES_MAX.min(room) as u64)
            .read_to_end(&mut self.pending)
            .map_err(|_| failure("launch_transcript_unavailable"))?;
        held.taken += count as u64;
        if self.skipping {
            assert_eq!(self.scanned, 0);
            match self.pending.iter().position(|byte| *byte == b'\n') {
                Some(end) => {
                    self.pending.drain(..=end);
                    self.skipping = false;
                }
                None => self.pending.clear(),
            }
        }
        let mut objects = Vec::new();
        let mut used = 0;
        let mut scanned = self.scanned;
        while objects.len() < EVENTS_PER_READ_MAX {
            let Some(length) = self.pending[scanned..]
                .iter()
                .position(|byte| *byte == b'\n')
            else {
                scanned = self.pending.len();
                break;
            };
            let line = &self.pending[used..=scanned + length];
            used = scanned + length + 1;
            scanned = used;
            if let Ok(object @ Value::Object(_)) = serde_json::from_slice::<Value>(line) {
                objects.push(object);
            }
        }
        self.pending.drain(..used);
        self.scanned = scanned - used;
        if self.pending.len() > LINE_BYTES_MAX && self.scanned == self.pending.len() {
            self.pending.clear();
            self.scanned = 0;
            self.skipping = true;
            eprintln!("launch_record_line_skipped over={LINE_BYTES_MAX}");
        }
        assert!(self.pending.len() <= LINE_BYTES_MAX + READ_BYTES_MAX);
        assert!(self.scanned <= self.pending.len());
        assert!(objects.len() >= EVENTS_PER_READ_MAX || self.scanned == self.pending.len());
        Ok(objects)
    }

    /// Finds the record once it exists, and notices a record replaced at its place or cut
    /// shorter than what was read. The reader then follows the file at that place from its
    /// end, with a note, so a rewritten record repeats no turn. Output: nothing; a record
    /// that cannot be opened or moved in is looked at again on the next call.
    fn follow(&mut self) {
        if self.held.is_none() {
            self.held = self.opened();
            return;
        }
        let Some(held) = self.held.as_mut() else {
            return;
        };
        let replaced = self
            .pattern
            .key_at(&held.below)
            .is_some_and(|key| key != held.key);
        let cut = held
            .file
            .metadata()
            .is_ok_and(|metadata| metadata.len() < held.taken);
        if !replaced && !cut {
            return;
        }
        if replaced {
            let Some((file, key)) = self.pattern.open(&held.below) else {
                return;
            };
            held.file = file;
            held.key = key;
        }
        let Ok(end) = held.file.seek(SeekFrom::End(0)) else {
            return;
        };
        held.taken = end;
        self.pending.clear();
        self.scanned = 0;
        self.skipping = false;
        eprintln!("launch_record_replaced replaced={replaced} cut={cut}");
    }

    /// Opens the newest record not present at the start, at the start position, or at its
    /// end when it is shorter. Output: the held record, or none while there is none.
    fn opened(&self) -> Option<Held> {
        let (mut file, below, key) = self.pattern.newest(&self.earlier)?;
        let length = file.metadata().ok()?.len();
        let taken = self.start.min(length);
        file.seek(SeekFrom::Start(taken)).ok()?;
        Some(Held {
            file,
            below,
            key,
            taken,
        })
    }
}

#[cfg(all(test, unix))]
#[path = "jsonl_append_tests.rs"]
mod tests;
