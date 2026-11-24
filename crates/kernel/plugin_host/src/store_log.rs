use std::cell::RefCell;

use crate::Refused;
use crate::embedding::{HOSTS_MAX, valid_host, valid_name, valid_secret};
use crate::manifest::HOST_BYTES_MAX;
use crate::registry::{FolderStep, code_key, is_bundled, is_code_plugin, set_flag_row};
use crate::store::{KeyRef, is_entered_key, refused, sqlite};
use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The schema every plugin store event is written with.
pub const SCHEMA: u16 = 1;

/// Every plugin store event kind, in the order [`Event`] lists them.
pub const KINDS: [&str; 6] = [
    "AppPluginSwitched",
    "AppPluginCodeSwitched",
    "AppPluginTrusted",
    "AppPluginBound",
    "AppPluginUnbound",
    "AppPluginForgotten",
];

/// The kinds a plugin's removal tombstones: everything logged about it except the removal
/// itself and a bundled code plugin's switch.
pub const REDACTED_ON_REMOVE: [&str; 4] = [
    "AppPluginSwitched",
    "AppPluginTrusted",
    "AppPluginBound",
    "AppPluginUnbound",
];

/// The refusal for an event body that is not one of the closed shapes, or a field outside
/// its bounds.
pub const REFUSED: &str = "app_plugin_event_refused";

/// The refusal for a store change made outside a capture: nothing is written, so no store
/// row ever misses the engine's log.
pub const UNLOGGED: &str = "app_plugin_store_unlogged";

/// The most events one captured run records, far over what one request writes: a start
/// pass forgets at most every reserved name, and one plugin holds at most its bindings.
pub const CAPTURED_EVENTS_MAX: usize = 4096;

/// Where a bound secret comes from: an API-key account, or a key entered for the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Account,
    Key,
}

impl Source {
    /// The word the secrets table keeps for the source. Output: `account` or `key`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Key => "key",
        }
    }

    /// Reads the word the secrets table keeps. Input: the word. Output: the source, none
    /// for any other word.
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "account" => Some(Self::Account),
            "key" => Some(Self::Key),
            _ => None,
        }
    }
}

/// One change to the plugin store as the log keeps it: plugin names, hashes, hosts and key
/// references only, never a secret value. A user or bundled plugin and a bundled code plugin
/// switch under separate kinds, so a plugin's removal never touches a code plugin's switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Switched {
        plugin: String,
        enabled: bool,
    },
    CodeSwitched {
        plugin: String,
        enabled: bool,
    },
    Trusted {
        plugin: String,
        sha256: String,
        hosts: Vec<String>,
        trusted_ms: u64,
    },
    Bound {
        plugin: String,
        secret: String,
        source: Source,
        reference: String,
    },
    Unbound {
        plugin: String,
        secret: String,
    },
    Forgotten {
        plugin: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SwitchBody {
    plugin: String,
    enabled: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustBody {
    plugin: String,
    sha256: String,
    hosts: Vec<String>,
    trusted_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindBody {
    plugin: String,
    secret: String,
    source: Source,
    reference: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnbindBody {
    plugin: String,
    secret: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginBody {
    plugin: String,
}

/// Whether a sha256 is 64 lowercase hex digits. Input: the text. Output: the answer.
fn valid_sha(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether a reference is one the secrets table can keep for the source: an identity of at
/// most 128 bytes, and for an entered key the store's own key name. Inputs: the source and
/// the reference. Output: the answer.
fn valid_reference(source: Source, reference: &str) -> bool {
    KeyRef::new(reference).is_some() && (source == Source::Account || is_entered_key(reference))
}

/// Whether trusted hosts are as the trust record keeps them: at most an embedding
/// plugin's count, each a host a plugin may name, in ascending order. Input: the hosts. Output: the answer.
fn valid_hosts(hosts: &[String]) -> bool {
    hosts.len() <= HOSTS_MAX
        && hosts
            .iter()
            .all(|host| host.len() <= HOST_BYTES_MAX && valid_host(host))
        && hosts.windows(2).all(|pair| pair[0] <= pair[1])
}

impl Event {
    /// The event's kind, one of [`KINDS`]. Output: the kind.
    pub fn kind(&self) -> &'static str {
        let at = match self {
            Self::Switched { .. } => 0,
            Self::CodeSwitched { .. } => 1,
            Self::Trusted { .. } => 2,
            Self::Bound { .. } => 3,
            Self::Unbound { .. } => 4,
            Self::Forgotten { .. } => 5,
        };
        KINDS[at]
    }

    /// The plugin the event is about, the field delete redaction matches. Output: the name.
    pub fn plugin(&self) -> &str {
        match self {
            Self::Switched { plugin, .. }
            | Self::CodeSwitched { plugin, .. }
            | Self::Trusted { plugin, .. }
            | Self::Bound { plugin, .. }
            | Self::Unbound { plugin, .. }
            | Self::Forgotten { plugin } => plugin,
        }
    }

    /// Whether every field is within its bounds: a switched plugin is never a code plugin,
    /// a code switch names one, a trusted plugin is the owner's, a secret and a reference
    /// have their store shapes. Output: the answer.
    fn valid(&self) -> bool {
        let plugin = self.plugin();
        if !valid_name(plugin) {
            return false;
        }
        match self {
            Self::Switched { .. } => !is_code_plugin(plugin),
            Self::CodeSwitched { .. } => is_code_plugin(plugin),
            Self::Trusted {
                sha256,
                hosts,
                trusted_ms,
                ..
            } => {
                !is_bundled(plugin)
                    && !is_code_plugin(plugin)
                    && valid_sha(sha256)
                    && valid_hosts(hosts)
                    && i64::try_from(*trusted_ms).is_ok()
            }
            Self::Bound {
                secret,
                source,
                reference,
                ..
            } => {
                !is_code_plugin(plugin)
                    && valid_secret(secret)
                    && valid_reference(*source, reference)
            }
            Self::Unbound { secret, .. } => !is_code_plugin(plugin) && valid_secret(secret),
            Self::Forgotten { .. } => true,
        }
    }

    /// The event's closed JSON body, after every field is checked against its bounds.
    /// Output: the body, or [`REFUSED`].
    pub fn body(&self) -> Result<Vec<u8>, Refused> {
        if !self.valid() {
            return Err(refused(REFUSED));
        }
        let plugin = self.plugin().to_owned();
        let body = match self.clone() {
            Self::Switched { enabled, .. } | Self::CodeSwitched { enabled, .. } => {
                serde_json::to_vec(&SwitchBody { plugin, enabled })
            }
            Self::Trusted {
                sha256,
                hosts,
                trusted_ms,
                ..
            } => serde_json::to_vec(&TrustBody {
                plugin,
                sha256,
                hosts,
                trusted_ms,
            }),
            Self::Bound {
                secret,
                source,
                reference,
                ..
            } => serde_json::to_vec(&BindBody {
                plugin,
                secret,
                source,
                reference,
            }),
            Self::Unbound { secret, .. } => serde_json::to_vec(&UnbindBody { plugin, secret }),
            Self::Forgotten { .. } => serde_json::to_vec(&PluginBody { plugin }),
        }
        .map_err(|_| refused(REFUSED))?;
        assert!(
            body.len() <= u16::MAX as usize,
            "a store event body is small"
        );
        Ok(body)
    }

    /// Reads one logged plugin store event, refusing an unknown kind or schema, an unknown
    /// field and any field outside its bounds. Inputs: the kind, the schema and the body.
    /// Output: the event, or [`REFUSED`].
    pub fn decode(kind: &str, schema: u16, body: &[u8]) -> Result<Self, Refused> {
        fn read<T: DeserializeOwned>(body: &[u8]) -> Result<T, Refused> {
            serde_json::from_slice(body).map_err(|_| refused(REFUSED))
        }
        if schema != SCHEMA {
            return Err(refused(REFUSED));
        }
        let event = match kind {
            "AppPluginSwitched" => {
                let SwitchBody { plugin, enabled } = read(body)?;
                Self::Switched { plugin, enabled }
            }
            "AppPluginCodeSwitched" => {
                let SwitchBody { plugin, enabled } = read(body)?;
                Self::CodeSwitched { plugin, enabled }
            }
            "AppPluginTrusted" => {
                let TrustBody {
                    plugin,
                    sha256,
                    hosts,
                    trusted_ms,
                } = read(body)?;
                Self::Trusted {
                    plugin,
                    sha256,
                    hosts,
                    trusted_ms,
                }
            }
            "AppPluginBound" => {
                let BindBody {
                    plugin,
                    secret,
                    source,
                    reference,
                } = read(body)?;
                Self::Bound {
                    plugin,
                    secret,
                    source,
                    reference,
                }
            }
            "AppPluginUnbound" => {
                let UnbindBody { plugin, secret } = read(body)?;
                Self::Unbound { plugin, secret }
            }
            "AppPluginForgotten" => {
                let PluginBody { plugin } = read(body)?;
                Self::Forgotten { plugin }
            }
            _ => return Err(refused(REFUSED)),
        };
        if !event.valid() {
            return Err(refused(REFUSED));
        }
        Ok(event)
    }
}

/// Writes one event into the plugin store tables, as the statement it replaces did; a
/// code plugin's switch is kept under `code:<id>`. Inputs: the connection, inside the
/// caller's transaction, and the event. Output: Ok, or the database's refusal.
pub fn apply(db: &Connection, event: &Event) -> Result<(), Refused> {
    match event {
        Event::Switched { plugin, enabled } => set_flag_row(db, plugin, *enabled),
        Event::CodeSwitched { plugin, enabled } => set_flag_row(db, &code_key(plugin), *enabled),
        Event::Trusted {
            plugin,
            sha256,
            hosts,
            trusted_ms,
        } => {
            let trusted_ms = i64::try_from(*trusted_ms).map_err(|_| refused(REFUSED))?;
            db.execute(
                "INSERT INTO app_plugin_trust (name, sha256, hosts, trusted_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(name) DO UPDATE SET sha256=excluded.sha256, hosts=excluded.hosts,
                 trusted_ms=excluded.trusted_ms",
                rusqlite::params![plugin, sha256, hosts.join("\n"), trusted_ms],
            )
            .map(drop)
            .map_err(sqlite)
        }
        Event::Bound {
            plugin,
            secret,
            source,
            reference,
        } => db
            .execute(
                "INSERT INTO app_plugin_secrets (plugin, secret, source, reference) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(plugin, secret) DO UPDATE SET source=excluded.source,
                 reference=excluded.reference",
                [plugin.as_str(), secret, source.word(), reference],
            )
            .map(drop)
            .map_err(sqlite),
        Event::Unbound { plugin, secret } => db
            .execute(
                "DELETE FROM app_plugin_secrets WHERE plugin=?1 AND secret=?2",
                [plugin, secret],
            )
            .map(drop)
            .map_err(sqlite),
        Event::Forgotten { plugin } => [
            "DELETE FROM app_plugin_secrets WHERE plugin=?1",
            "DELETE FROM app_plugin_state WHERE name=?1",
            "DELETE FROM app_plugin_trust WHERE name=?1",
        ]
        .iter()
        .try_for_each(|sql| db.execute(sql, [plugin]).map(drop))
        .map_err(sqlite),
    }
}

/// What one captured run recorded: the events in the order they were applied, the
/// entered keys whose binding went, to delete once the change commits, and the plugin
/// folder steps, to end once the transaction does. It also keeps the address of the
/// connection the capture was opened on.
#[derive(Debug, Default)]
pub struct Captured {
    pub events: Vec<Event>,
    pub forgotten_keys: Vec<KeyRef>,
    pub folders: Vec<FolderStep>,
    connection: usize,
}

/// The address of a connection, which names it while it is borrowed. Input: the
/// connection. Output: the address.
fn address(db: &Connection) -> usize {
    std::ptr::from_ref(db).addr()
}

/// What this thread's plugin store work runs under: a capture keeping each change for the
/// log, or, in plugin host rows only, a hold that writes without keeping.
enum Held {
    Capture(Captured),
    #[cfg(test)]
    Rows,
}

thread_local! {
    static CAPTURE: RefCell<Option<Held>> = const { RefCell::new(None) };
}

/// Ends the capture this thread holds when the captured run returns or unwinds.
struct Ending;

impl Drop for Ending {
    fn drop(&mut self) {
        CAPTURE.with(|capture| capture.borrow_mut().take());
    }
}

/// Runs plugin store work on one connection and collects what it recorded, for the engine
/// to append to its log in the transaction it holds open on that connection. Captures
/// never nest, and a change made on another thread holds no capture, so it is refused.
/// Inputs: the connection and the work. Output: its result and what it recorded.
pub fn capture<T>(db: &Connection, work: impl FnOnce() -> T) -> (T, Captured) {
    let opened = Captured {
        connection: address(db),
        ..Captured::default()
    };
    let outer = CAPTURE.with(|capture| capture.replace(Some(Held::Capture(opened))));
    assert!(outer.is_none(), "plugin store captures never nest");
    let ending = Ending;
    let result = work();
    let held = CAPTURE.with(|capture| capture.borrow_mut().take());
    drop(ending);
    let Some(Held::Capture(captured)) = held else {
        unreachable!("the capture is held until the work returns");
    };
    assert!(captured.events.len() <= CAPTURED_EVENTS_MAX);
    (result, captured)
}

/// Lets a plugin host row write the store with no log for as long as the returned guard
/// lives on this thread. Output: the guard.
#[cfg(test)]
pub(crate) fn rows_hold() -> RowsHold {
    let outer = CAPTURE.with(|capture| capture.replace(Some(Held::Rows)));
    assert!(outer.is_none(), "a row holds the store once");
    RowsHold
}

/// Ends a row's hold when the row's case is dropped.
#[cfg(test)]
pub(crate) struct RowsHold;

#[cfg(test)]
impl Drop for RowsHold {
    fn drop(&mut self) {
        CAPTURE.with(|capture| capture.borrow_mut().take());
    }
}

/// Checks one change and, inside a capture, writes it into the store tables and keeps it
/// for the log. A change outside any capture is refused before anything is written, so no
/// store row ever misses the log. Inputs: the connection and the event. Output: Ok,
/// [`REFUSED`] for an event outside its bounds, [`UNLOGGED`] outside a capture, or the
/// database's refusal.
pub(crate) fn record(db: &Connection, event: Event) -> Result<(), Refused> {
    if !event.valid() {
        return Err(refused(REFUSED));
    }
    if CAPTURE.with(|capture| capture.borrow().is_none()) {
        return Err(refused(UNLOGGED));
    }
    apply(db, &event)?;
    CAPTURE.with(|capture| {
        if let Some(Held::Capture(captured)) = capture.borrow_mut().as_mut() {
            assert!(captured.events.len() < CAPTURED_EVENTS_MAX);
            captured.events.push(event);
        }
    });
    Ok(())
}

/// Refuses plugin store work that touches files unless it runs in the capture opened on
/// this very connection while a transaction is open on it: the capture keeps the address
/// of the connection it was opened on, and the engine opens it only inside the transaction
/// it began itself, after refusing one the caller held. A call on another connection, or
/// after that transaction ended, is refused before anything is written. In plugin host
/// rows a hold stands in, outside any transaction. Input: the connection. Output: whether
/// a capture holds the call, or `app_plugins_unavailable`.
pub(crate) fn in_capture(db: &Connection) -> Result<bool, Refused> {
    CAPTURE.with(|capture| match capture.borrow().as_ref() {
        Some(Held::Capture(captured))
            if captured.connection == address(db) && !db.is_autocommit() =>
        {
            Ok(true)
        }
        #[cfg(test)]
        Some(Held::Rows) if db.is_autocommit() => Ok(false),
        _ => Err(refused("app_plugins_unavailable")),
    })
}

/// Keeps a plugin folder step for the capture's owner to end with its transaction. With no
/// capture, as in a plugin host row's hold, the step is handed back to end at once. Input:
/// the step. Output: none once the capture took it, else the step.
pub(crate) fn after_commit(step: FolderStep) -> Option<FolderStep> {
    CAPTURE.with(|capture| match capture.borrow_mut().as_mut() {
        Some(Held::Capture(captured)) => {
            assert!(captured.folders.len() < CAPTURED_EVENTS_MAX);
            captured.folders.push(step);
            None
        }
        _ => Some(step),
    })
}

/// Keeps an entered key whose binding went for deletion after the capture's commit.
/// Outside a capture it is left to the start sweep. Input: the key. Output: whether a
/// capture took it.
pub(crate) fn forget_after_commit(key: KeyRef) -> bool {
    CAPTURE.with(|capture| match capture.borrow_mut().as_mut() {
        Some(Held::Capture(captured)) => {
            assert!(captured.forgotten_keys.len() < CAPTURED_EVENTS_MAX);
            captured.forgotten_keys.push(key);
            true
        }
        _ => false,
    })
}
