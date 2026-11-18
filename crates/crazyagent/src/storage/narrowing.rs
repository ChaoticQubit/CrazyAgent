use crate::engine::EngineError;
use crate::engine::owner::OwnerClientKind;
use crate::engine::owner::native_auth::NativeAuthLogout;
use crate::protocol::RequestId;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[cfg(unix)]
pub(crate) const FOLDER: &str = "narrowing-intents";
#[cfg(unix)]
pub(crate) const MAX_BYTES: usize = 4096;
pub(crate) const MAX_INTENTS: usize = 1024;
#[cfg(unix)]
const VERSION: u8 = 1;
#[cfg(unix)]
const NAME_LEN: usize = 16;
#[cfg(unix)]
const FOLDERS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
    .union(rustix::fs::OFlags::DIRECTORY)
    .union(rustix::fs::OFlags::NOFOLLOW)
    .union(rustix::fs::OFlags::CLOEXEC);

/// One access-narrowing change the owner made, kept on disk from before its storage job is
/// submitted until storage commits it or finds it no longer applies, so a lost job is applied
/// at the next open. Only narrowing kinds exist; `db_instance` names the database it is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    pub(crate) db_instance: String,
    pub(crate) change: Change,
}

/// The narrowing change an intent keeps: a client revoked at the epoch the owner saw, or a
/// sign-out of one managed profile. No kind that widens access can be written or read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Change {
    Revoke {
        operation_id: RequestId,
        target: OwnerClientKind,
        expected_epoch: i64,
    },
    Logout(NativeAuthLogout),
}

#[cfg(unix)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    created_ns: u64,
    intent: Intent,
}

/// The name of one intent file: sixteen lowercase hex digits from the system's entropy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct IntentId(String);

impl IntentId {
    /// A fresh name from the system's entropy. Output: the name, or
    /// `owner_entropy_unavailable`.
    #[cfg(unix)]
    fn fresh() -> Result<Self, EngineError> {
        let mut random = [0; NAME_LEN / 2];
        getrandom::fill(&mut random).map_err(|_| super::error("owner_entropy_unavailable"))?;
        let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let id = Self::parse(name.as_bytes()).expect("sixteen lowercase hex digits");
        Ok(id)
    }

    /// Reads a folder entry name as an intent name. Input: the raw name. Output: the name, or
    /// none for anything but sixteen lowercase hex digits.
    #[cfg(unix)]
    fn parse(name: &[u8]) -> Option<Self> {
        (name.len() == NAME_LEN && name.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(String::from_utf8(name.to_vec()).expect("ascii hex")))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// What one listing read: the intents in the order they were written, and how many entries
/// it refused, each logged by cause and left in place.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Listing {
    pub(crate) intents: Vec<(IntentId, Intent)>,
    pub(crate) refused: usize,
}

/// Whether an intent's fields are in bounds: a database id of 64 lowercase hex digits, and a
/// sign-out scoped to that same database. Input: the intent. Output: the verdict.
#[cfg(unix)]
fn valid(intent: &Intent) -> bool {
    crate::engine::owner::is_hex_id(&intent.db_instance)
        && match &intent.change {
            Change::Revoke { expected_epoch, .. } => *expected_epoch >= 1,
            Change::Logout(logout) => logout.scope.expected_database_instance == intent.db_instance,
        }
}

/// A creation stamp later than every earlier one of this process, so intents list in the
/// order they were written. Output: nanoseconds since the Unix epoch, strictly increasing.
#[cfg(unix)]
fn created_ns() -> Result<u64, EngineError> {
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_nanos()).ok())
        .ok_or_else(|| super::error("narrowing_intent_clock"))?;
    let mut last = LAST.load(std::sync::atomic::Ordering::SeqCst);
    loop {
        let next = now.max(last.saturating_add(1));
        match LAST.compare_exchange(
            last,
            next,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        ) {
            Ok(_) => {
                assert!(next > last);
                return Ok(next);
            }
            Err(seen) => last = seen,
        }
    }
}

/// The intent store of one engine root: the root judged once at open as a private folder of
/// this user, and its intent folder, made on the first write, judged at every use. Every
/// file is opened relative to the folder without following links.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct Intents {
    root: std::fs::File,
}

#[cfg(unix)]
impl Intents {
    /// Opens the intent store of the engine root that holds `engine.db`, without making its
    /// folder. Input: that root. Output: the store, or `narrowing_intents_unsafe` when the
    /// root is not a private folder of this user or is a link.
    pub(crate) fn open(root: &Path) -> Result<Self, EngineError> {
        use rustix::fs::{CWD, Mode, openat};
        let root = std::fs::File::from(
            openat(CWD, root, FOLDERS, Mode::empty())
                .map_err(|_| super::error("narrowing_intents_unsafe"))?,
        );
        if !private_folder(&root, 0o077, 0)? {
            return Err(super::error("narrowing_intents_unsafe"));
        }
        Ok(Self { root })
    }

    /// Opens the intent folder and judges it: this user's, mode 0700, no link. With `make`, a
    /// missing folder is made and the root synced first. Input: whether to make it. Output:
    /// the folder, none when missing and not made, or the unsafe or I/O code.
    fn folder(&self, make: bool) -> Result<Option<std::fs::File>, EngineError> {
        use rustix::fs::{Mode, mkdirat, openat};
        if make {
            match mkdirat(&self.root, FOLDER, Mode::from_raw_mode(0o700)) {
                Ok(()) => rustix::fs::fsync(&self.root).map_err(super::error)?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(super::error(format!(
                        "narrowing_intents_unavailable: {error}"
                    )));
                }
            }
        }
        let folder = match openat(&self.root, FOLDER, FOLDERS, Mode::empty()) {
            Ok(folder) => std::fs::File::from(folder),
            Err(rustix::io::Errno::NOENT) if !make => return Ok(None),
            Err(_) => return Err(super::error("narrowing_intents_unsafe")),
        };
        if !private_folder(&folder, 0o7777, 0o700)? {
            return Err(super::error("narrowing_intents_unsafe"));
        }
        Ok(Some(folder))
    }

    /// Writes one intent as a new private file, durable before this returns: the file, then
    /// the folder, are synced and the file is judged again through its own handle. Input: the
    /// intent. Output: its name, or the bounds, invalid, unsafe or I/O code with no file left.
    pub(crate) fn write(&self, intent: &Intent) -> Result<IntentId, EngineError> {
        use rustix::fs::{Mode, OFlags, openat};
        use std::io::Write;
        let bytes = serde_json::to_vec(&Wire {
            version: VERSION,
            created_ns: created_ns()?,
            intent: intent.clone(),
        })
        .map_err(super::error)?;
        if bytes.len() > MAX_BYTES {
            return Err(super::error("narrowing_intent_bounds"));
        }
        if !valid(intent) {
            return Err(super::error("narrowing_intent_invalid"));
        }
        let folder = self.folder(true)?.expect("a made intent folder");
        let id = IntentId::fresh()?;
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut file = std::fs::File::from(
            openat(&folder, id.as_str(), flags, Mode::from_raw_mode(0o600))
                .map_err(|error| super::error(format!("narrowing_intent_unwritten: {error}")))?,
        );
        let written = file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| super::error(format!("narrowing_intent_unwritten: {error}")))
            .and_then(|()| match private_file(&file)? {
                Some(len) if len == bytes.len() as u64 => Ok(()),
                _ => Err(super::error("narrowing_intent_unsafe")),
            })
            .and_then(|()| rustix::fs::fsync(&folder).map_err(super::error));
        if let Err(error) = written {
            drop(file);
            if let Err(left) = self.remove(&id) {
                eprintln!(
                    "narrowing_intent_unwritten_left id={} cause={}",
                    id.as_str(),
                    left.0
                );
            }
            return Err(error);
        }
        Ok(id)
    }

    /// Removes one intent once its change committed or no longer applies, durable before this
    /// returns. Removing one already gone is no error. Input: its name. Output: none, or the
    /// I/O code.
    pub(crate) fn remove(&self, id: &IntentId) -> Result<(), EngineError> {
        let Some(folder) = self.folder(false)? else {
            return Ok(());
        };
        match rustix::fs::unlinkat(&folder, id.as_str(), rustix::fs::AtFlags::empty()) {
            Ok(()) => {}
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => {
                return Err(super::error(format!("narrowing_intent_unremoved: {error}")));
            }
        }
        rustix::fs::fsync(&folder).map_err(super::error)
    }

    /// Reads every intent the folder holds, at most `MAX_INTENTS`, in the order they were
    /// written. An entry with another name, a link, a folder, a file that is not this user's
    /// private single-named file, too large, or unparsed is refused, logged and left in place.
    /// Output: the listing, empty with no folder, or the unsafe or I/O code.
    pub(crate) fn list(&self) -> Result<Listing, EngineError> {
        let Some(folder) = self.folder(false)? else {
            return Ok(Listing::default());
        };
        let mut found = Vec::new();
        let mut refused = 0;
        let entries = rustix::fs::Dir::read_from(&folder).map_err(super::error)?;
        for entry in entries {
            let entry = entry.map_err(super::error)?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            refused += 1;
            let Some(id) = IntentId::parse(name) else {
                eprintln!("narrowing_intent_refused cause=name");
                continue;
            };
            if found.len() >= MAX_INTENTS {
                eprintln!("narrowing_intent_refused id={} cause=bounded", id.as_str());
                continue;
            }
            match read(&folder, &id) {
                Ok((created, intent)) => {
                    refused -= 1;
                    found.push((created, id, intent));
                }
                Err(cause) => {
                    eprintln!("narrowing_intent_refused id={} cause={cause}", id.as_str())
                }
            }
        }
        found.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
        assert!(found.len() <= MAX_INTENTS);
        Ok(Listing {
            intents: found
                .into_iter()
                .map(|(_, id, intent)| (id, intent))
                .collect(),
            refused,
        })
    }
}

/// Reads and judges one intent file through its own handle. Inputs: the intent folder and
/// the name. Output: its creation stamp and intent, or the cause it is refused.
#[cfg(unix)]
fn read(folder: &std::fs::File, id: &IntentId) -> Result<(u64, Intent), &'static str> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::io::Read;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = std::fs::File::from(
        openat(folder, id.as_str(), flags, Mode::empty()).map_err(|_| "unopened")?,
    );
    let Some(len) = private_file(&file).map_err(|_| "unjudged")? else {
        return Err("unsafe");
    };
    if len > MAX_BYTES as u64 {
        return Err("bounds");
    }
    let mut bytes = Vec::with_capacity(MAX_BYTES);
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unread")?;
    if bytes.len() > MAX_BYTES {
        return Err("bounds");
    }
    let wire: Wire = serde_json::from_slice(&bytes).map_err(|_| "unparsed")?;
    if wire.version != VERSION {
        return Err("version");
    }
    if !valid(&wire.intent) {
        return Err("invalid");
    }
    Ok((wire.created_ns, wire.intent))
}

/// Whether an open folder is this user's and its mode bits under `mask` equal `mode`.
/// Inputs: the folder, the mask and the mode. Output: the verdict, or the I/O code.
#[cfg(unix)]
fn private_folder(folder: &std::fs::File, mask: u32, mode: u32) -> Result<bool, EngineError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = folder.metadata().map_err(super::error)?;
    Ok(metadata.is_dir()
        && metadata.uid() == rustix::process::getuid().as_raw()
        && metadata.mode() & mask == mode)
}

/// The length of an open file that is this user's private file with one name, else none.
/// Input: the file. Output: its length or none, or the I/O code.
#[cfg(unix)]
fn private_file(file: &std::fs::File) -> Result<Option<u64>, EngineError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(super::error)?;
    Ok((metadata.is_file()
        && metadata.uid() == rustix::process::getuid().as_raw()
        && metadata.mode() & 0o7777 == 0o600
        && metadata.nlink() == 1)
        .then_some(metadata.len()))
}

/// The intent store where storage has no owner-only folder check: it never opens, so no
/// narrowing change is ever kept where it could not be judged.
#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct Intents;

#[cfg(not(unix))]
impl Intents {
    /// Refuses the intent store until this platform has its private folder judge.
    /// Input: the engine root, unused. Output: `narrowing_intents_unavailable`.
    pub(crate) fn open(_root: &Path) -> Result<Self, EngineError> {
        Err(super::error("narrowing_intents_unavailable"))
    }

    /// Never reached: no store opens here. Input: the intent, unused. Output: the refusal.
    pub(crate) fn write(&self, _intent: &Intent) -> Result<IntentId, EngineError> {
        Err(super::error("narrowing_intents_unavailable"))
    }

    /// Never reached: no store opens here. Input: the name, unused. Output: the refusal.
    pub(crate) fn remove(&self, _id: &IntentId) -> Result<(), EngineError> {
        Err(super::error("narrowing_intents_unavailable"))
    }

    /// Never reached: no store opens here. Output: the refusal.
    pub(crate) fn list(&self) -> Result<Listing, EngineError> {
        Err(super::error("narrowing_intents_unavailable"))
    }
}

#[cfg(all(test, unix))]
#[path = "narrowing_tests.rs"]
mod tests;
