use std::path::Path;

use kernel_common::names::{is_dotted, is_snake, is_store_name};
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
/// The one optional key of every kind's `plugin.toml`: the hosts `net.request` may reach.
const NET_KEY: &str = "net";
/// The permission that requires a `[net]` section, and that a `[net]` section requires.
const NET_PERMISSION: &str = "net.request";
/// The most hosts one `[net]` section may list.
pub const HOSTS_MAX: usize = 16;
/// The most bytes of one `[net]` host name, its port not counted.
pub const HOST_BYTES_MAX: usize = 253;
/// The most capture fields one `[net.capture]` host may name.
pub const NET_CAPTURES_MAX: usize = 4;
/// The most bytes of one capture field name.
pub const CAPTURE_FIELD_BYTES_MAX: usize = 64;
/// The most hosts one `[net.send]` row names.
pub const SEND_HOSTS_MAX: usize = 16;
/// The most bytes of one `keep_as` name, the secret store's name bound.
pub const KEEP_AS_BYTES_MAX: usize = kernel_common::names::STORE_NAME_BYTES_MAX;
/// The keys only an embedding plugin's `plugin.toml` holds; the embedding reader judges
/// their values.
pub const EMBEDDING_KEYS: [&str; 4] = ["title", "release", "description", "http"];
/// The optional key marking a code plugin the app relies on, so its switch stays locked
/// on; an embedding plugin, the only kind a user adds, never holds it.
const ESSENTIAL_KEY: &str = "essential";

/// A plugin's `plugin.toml`: its id, tier word, kind, manifest version, the permissions it
/// asks the kernel for, its `[net]` hosts and whether it is essential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub plugin: String,
    pub tier: String,
    pub kind: String,
    pub version: i64,
    pub permissions: Vec<String>,
    pub net: Option<Net>,
    pub essential: bool,
}

/// A plugin's `[net]` section: the hosts `net.request` may reach, each a lowercase DNS
/// name or loopback address with an optional port, and per host the reply fields the
/// kernel always captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Net {
    pub hosts: Vec<String>,
    pub capture: Vec<(String, Vec<String>)>,
    pub send: Vec<NetSend>,
}

/// One `[net.send]` row: a captured field, the listed hosts its value may also be sent
/// to, and the name it is kept under, which `secrets.find` binds to the same hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetSend {
    pub field: String,
    pub hosts: Vec<String>,
    pub keep_as: Option<String>,
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
/// kind is `embedding` and an optional bool `essential` when it is not, a tier word, a snake kind, a positive version and at most
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
            && key.as_str() != NET_KEY
            && !(embedding && EMBEDDING_KEYS.contains(&key.as_str()))
            && !(!embedding && key.as_str() == ESSENTIAL_KEY)
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
    let net = match (
        table.get(NET_KEY),
        permissions.iter().any(|name| name == NET_PERMISSION),
    ) {
        (None, false) => None,
        (Some(section), true) => Some(net(section).ok_or_else(|| Refused {
            code: "plugin_manifest_not_allowed",
            line: net_line(text),
        })?),
        (None, true) => {
            return Err(Refused {
                code: "plugin_net_without_permission",
                ..invalid("permissions")
            });
        }
        (Some(_), false) => {
            return Err(Refused {
                code: "plugin_net_without_permission",
                line: net_line(text),
            });
        }
    };
    let essential = match table.get(ESSENTIAL_KEY) {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| invalid(ESSENTIAL_KEY))?,
    };
    Ok(Manifest {
        plugin,
        tier,
        kind,
        version,
        permissions,
        net,
        essential,
    })
}

/// The line of a manifest's `[net]` section, or of a top-level `net` key. Input: the
/// text. Output: the 1-based line, 1 when there is none.
fn net_line(text: &str) -> usize {
    text.lines()
        .position(|line| {
            let line = line.trim_start();
            line.starts_with("[net]")
                || line.starts_with("[net.")
                || line
                    .strip_prefix(NET_KEY)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .map_or(1, |index| index + 1)
}

/// Reads one `[net]` section: only `hosts`, 1 to `HOSTS_MAX` distinct [`net_host`]s, an
/// optional `[net.capture]` table whose keys are listed hosts, each naming 1 to
/// `NET_CAPTURES_MAX` distinct [`capture_field`]s, and an optional `[net.send]` table read
/// by [`net_send`]. Input: the section's value. Output: the section, or none for any other
/// key, type or value.
fn net(section: &Value) -> Option<Net> {
    let section = section.as_table()?;
    if section
        .keys()
        .any(|key| !matches!(key.as_str(), "hosts" | "capture" | "send"))
    {
        return None;
    }
    let hosts = section
        .get("hosts")?
        .as_array()?
        .iter()
        .map(|host| {
            host.as_str()
                .filter(|host| net_host(host))
                .map(str::to_owned)
        })
        .collect::<Option<Vec<String>>>()?;
    let distinct = hosts
        .iter()
        .enumerate()
        .all(|(index, host)| !hosts[..index].contains(host));
    if hosts.is_empty() || hosts.len() > HOSTS_MAX || !distinct {
        return None;
    }
    let mut capture = Vec::new();
    if let Some(table) = section.get("capture") {
        for (host, fields) in table.as_table()? {
            let fields = fields
                .as_array()?
                .iter()
                .map(|field| {
                    field
                        .as_str()
                        .filter(|field| capture_field(field))
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<String>>>()?;
            let distinct = fields
                .iter()
                .enumerate()
                .all(|(index, field)| !fields[..index].contains(field));
            if !hosts.contains(host)
                || fields.is_empty()
                || fields.len() > NET_CAPTURES_MAX
                || !distinct
            {
                return None;
            }
            capture.push((host.clone(), fields));
        }
    }
    let send = match section.get("send") {
        Some(table) => net_send(table, &hosts, &capture)?,
        None => Vec::new(),
    };
    assert!(hosts.len() <= HOSTS_MAX && capture.len() <= hosts.len());
    Some(Net {
        hosts,
        capture,
        send,
    })
}

/// Reads a `[net.send]` table: each key a field some `[net.capture]` row names, each
/// value a table of `hosts`, 1 to `SEND_HOSTS_MAX` distinct listed hosts, and an optional
/// `keep_as` store name ([`is_store_name`]) used by no other row. Inputs: the table, the listed
/// hosts and the capture rows. Output: the rows, or none for any other key, type or value.
fn net_send(
    table: &Value,
    hosts: &[String],
    capture: &[(String, Vec<String>)],
) -> Option<Vec<NetSend>> {
    let mut send: Vec<NetSend> = Vec::new();
    for (field, row) in table.as_table()? {
        let row = row.as_table()?;
        if row.keys().any(|key| key != "hosts" && key != "keep_as") {
            return None;
        }
        let to = row
            .get("hosts")?
            .as_array()?
            .iter()
            .map(|host| {
                host.as_str()
                    .filter(|host| hosts.iter().any(|listed| listed == host))
                    .map(str::to_owned)
            })
            .collect::<Option<Vec<String>>>()?;
        let keep_as = match row.get("keep_as") {
            Some(name) => Some(name.as_str().filter(|name| is_store_name(name))?.to_owned()),
            None => None,
        };
        let distinct = to
            .iter()
            .enumerate()
            .all(|(index, host)| !to[..index].contains(host));
        let captured = capture.iter().any(|(_, fields)| fields.contains(field));
        let name_taken = keep_as
            .as_ref()
            .is_some_and(|name| send.iter().any(|row| row.keep_as.as_ref() == Some(name)));
        if !captured || to.is_empty() || !distinct || name_taken {
            return None;
        }
        assert!(to.len() <= hosts.len() && hosts.len() <= HOSTS_MAX && HOSTS_MAX <= SEND_HOSTS_MAX);
        send.push(NetSend {
            field: field.clone(),
            hosts: to,
            keep_as,
        });
    }
    Some(send)
}

/// Whether one `[net]` host fits: a lowercase DNS name of at most `HOST_BYTES_MAX` bytes
/// whose labels are 1 to 63 letters, digits or inner hyphens, or the loopback address
/// `127.0.0.1` or `[::1]`, then an optional `:port` from 1 to 65535 without a leading zero.
/// No scheme, path, query, wildcard, userinfo or other IP literal: a name whose last label
/// is all digits or starts with `0x` is an IPv4 form a URL parser reads as an address, so it
/// is refused. Input: the host. Output: the answer.
fn net_host(host: &str) -> bool {
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port))
            if !name.ends_with(':') && (!name.contains(':') || name.ends_with(']')) =>
        {
            (name, Some(port))
        }
        _ => (host, None),
    };
    let port_fits = port.is_none_or(|port| {
        !port.is_empty()
            && !port.starts_with('0')
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok_and(|port| port >= 1)
    });
    let loopback = matches!(name, "127.0.0.1" | "[::1]");
    let last = name.rsplit('.').next().unwrap_or(name);
    let numeric = last.starts_with("0x")
        || (!last.is_empty() && last.bytes().all(|byte| byte.is_ascii_digit()));
    let dns = !numeric
        && name.len() <= HOST_BYTES_MAX
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    port_fits && (loopback || dns)
}

/// Whether one capture field name fits: 1 to `CAPTURE_FIELD_BYTES_MAX` bytes of ASCII
/// letters, digits, `_` or `-`. Input: the name. Output: the answer.
fn capture_field(field: &str) -> bool {
    !field.is_empty()
        && field.len() <= CAPTURE_FIELD_BYTES_MAX
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
