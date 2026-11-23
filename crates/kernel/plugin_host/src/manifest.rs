use std::path::Path;

use kernel_common::names::{is_dotted, is_snake};
use toml::{Table, Value};

use crate::Refused;

/// The most bytes a plugin's `plugin.toml` may hold, and the bound plugin_check keeps for
/// the crate's `Cargo.toml` and `build.rs`.
pub const MANIFEST_BYTES_MAX: usize = 65_536;
/// The most permissions one `plugin.toml` may ask for.
pub const PERMISSIONS_MAX: usize = 64;
/// The most bytes a plugin id may hold.
pub const PLUGIN_ID_MAX: usize = 64;

/// The most bytes a kind word or one permission may hold.
pub const NAME_BYTES_MAX: usize = 64;

const PLUGIN_KEYS: [&str; 5] = ["plugin", "tier", "kind", "version", "permissions"];
/// The keys only an embedding plugin's `plugin.toml` holds; the embedding reader judges
/// their values.
pub const EMBEDDING_KEYS: [&str; 4] = ["title", "release", "description", "http"];

/// A plugin's `plugin.toml`: its id, tier word, kind, manifest version and the
/// permissions it asks the kernel for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub plugin: String,
    pub tier: String,
    pub kind: String,
    pub version: i64,
    pub permissions: Vec<String>,
}

/// What a bounded read of one file found: its UTF-8 text, more bytes than the bound, or a
/// file that cannot be opened, read or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bounded {
    Text(String),
    TooLarge,
    Unreadable,
}

/// The Windows open flag that opens a link as itself, the twin of Unix `O_NOFOLLOW` on the
/// last path component.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// The Windows file attribute of a reparse point, such as a symbolic link or a junction.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Opens one file without following a link at its last component and without waiting on a
/// FIFO, then keeps it only when the handle is a regular file (Windows: and no reparse
/// point). Input: the path. Output: the file, or none.
fn open_plain(path: &Path) -> Option<std::fs::File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)
            .ok()?
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .ok()?
    };
    #[cfg(not(any(unix, windows)))]
    let file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }
    }
    Some(file)
}

/// Reads one file through a guarded open ([`open_plain`]: no link, no FIFO, a regular file
/// only), taking at most `max` + 1 bytes, so a large file is refused without holding it
/// whole in memory. Inputs: the path and the bound. Output: the text, `TooLarge` past the
/// bound, or `Unreadable`.
pub fn read_bounded(path: &Path, max: usize) -> Bounded {
    use std::io::Read;
    let Some(file) = open_plain(path) else {
        return Bounded::Unreadable;
    };
    let limit = u64::try_from(max)
        .expect("a bound fits u64")
        .saturating_add(1);
    let mut bytes = Vec::new();
    if file.take(limit).read_to_end(&mut bytes).is_err() {
        return Bounded::Unreadable;
    }
    assert!(bytes.len() <= max.saturating_add(1));
    if bytes.len() > max {
        return Bounded::TooLarge;
    }
    String::from_utf8(bytes).map_or(Bounded::Unreadable, Bounded::Text)
}

/// Whether a plugin id is 1 to `PLUGIN_ID_MAX` bytes of lowercase ASCII letters, digits
/// and hyphens, starting with a letter or digit and never a Windows device name, so it is
/// safe in a log line, an event body and a folder name on every system. Input: the id.
/// Output: true when it is.
pub fn is_plugin_id(id: &str) -> bool {
    (1..=PLUGIN_ID_MAX).contains(&id.len())
        && id
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !kernel_common::windows_device(id)
}

/// The 1-based line of the first occurrence of a needle, 1 when absent or empty.
/// Inputs: the text and the needle. Output: the line.
pub fn line_of(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 1;
    }
    text.lines()
        .position(|line| line.contains(needle))
        .map_or(1, |index| index + 1)
}

/// Reads and checks a plugin folder's `plugin.toml` with [`parse`]. Input: the folder.
/// Output: the manifest, or `plugin_manifest_invalid` (missing, over the bound, or at the
/// first refused key) or `plugin_id_invalid`, each with its line.
pub fn read(dir: &Path) -> Result<Manifest, Refused> {
    let Bounded::Text(text) = read_bounded(&dir.join("plugin.toml"), MANIFEST_BYTES_MAX) else {
        return Err(Refused {
            code: "plugin_manifest_invalid",
            line: 1,
        });
    };
    parse(&text)
}

/// Checks one `plugin.toml` text: exactly its five keys, plus [`EMBEDDING_KEYS`] when the
/// kind is `embedding`, a tier word, a snake kind, a positive version and at most
/// `PERMISSIONS_MAX` distinct dotted permissions. Input: the text. Output: the manifest, or
/// `plugin_manifest_invalid` or `plugin_id_invalid` with the line of the refused key.
pub fn parse(text: &str) -> Result<Manifest, Refused> {
    let invalid = |needle: &str| Refused {
        code: "plugin_manifest_invalid",
        line: line_of(text, needle),
    };
    if text.len() > MANIFEST_BYTES_MAX {
        return Err(invalid(""));
    }
    let table = text.parse::<Table>().map_err(|_| invalid(""))?;
    let embedding = table.get("kind").and_then(Value::as_str) == Some("embedding");
    if let Some(key) = table.keys().find(|key| {
        !PLUGIN_KEYS.contains(&key.as_str())
            && !(embedding && EMBEDDING_KEYS.contains(&key.as_str()))
    }) {
        return Err(invalid(key));
    }
    let string = |key: &str| {
        table
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid(key))
    };
    let (plugin, tier, kind) = (string("plugin")?, string("tier")?, string("kind")?);
    if !is_plugin_id(&plugin) {
        return Err(Refused {
            code: "plugin_id_invalid",
            ..invalid("plugin")
        });
    }
    if !matches!(tier.as_str(), "t1" | "t2" | "t3") {
        return Err(invalid("tier"));
    }
    if kind.len() > NAME_BYTES_MAX || !is_snake(&kind) {
        return Err(invalid("kind"));
    }
    let version = table
        .get("version")
        .and_then(Value::as_integer)
        .filter(|version| *version >= 1)
        .ok_or_else(|| invalid("version"))?;
    let permissions = table
        .get("permissions")
        .and_then(Value::as_array)
        .filter(|list| list.len() <= PERMISSIONS_MAX)
        .and_then(|list| {
            list.iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<String>>>()
        })
        .filter(|list| {
            list.iter().enumerate().all(|(index, permission)| {
                permission.len() <= NAME_BYTES_MAX
                    && is_dotted(permission)
                    && !list[..index].contains(permission)
            })
        })
        .ok_or_else(|| invalid("permissions"))?;
    Ok(Manifest {
        plugin,
        tier,
        kind,
        version,
        permissions,
    })
}
