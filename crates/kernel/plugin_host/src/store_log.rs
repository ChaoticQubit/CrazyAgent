use crate::Refused;
use rusqlite::Connection;
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

/// The refusal for an event body that is not one of the closed shapes, or a field outside
/// its bounds.
pub const REFUSED: &str = "app_plugin_event_refused";

/// Where a bound secret comes from: an API-key account, or a key entered for the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Account,
    Key,
}

/// One change to the plugin store as the log keeps it: plugin names, hashes, hosts and key
/// references only, never a secret value. A user plugin and a bundled code plugin switch
/// under separate kinds, so a user plugin's removal never touches a code plugin's switch.
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

impl Event {
    /// The event's kind, one of [`KINDS`]. Output: the kind.
    pub fn kind(&self) -> &'static str {
        let _ = self;
        todo!()
    }

    /// The plugin the event is about, the field delete redaction matches. Output: the name.
    pub fn plugin(&self) -> &str {
        let _ = self;
        todo!()
    }

    /// The event's closed JSON body, after every field is checked against its bounds.
    /// Output: the body, or [`REFUSED`].
    pub fn body(&self) -> Result<Vec<u8>, Refused> {
        let _ = self;
        todo!()
    }

    /// Reads one logged plugin store event, refusing an unknown kind or schema, an unknown
    /// field and any field outside its bounds. Inputs: the kind, the schema and the body.
    /// Output: the event, or [`REFUSED`].
    pub fn decode(kind: &str, schema: u16, body: &[u8]) -> Result<Self, Refused> {
        let _ = (kind, schema, body);
        todo!()
    }
}

/// Writes one event into the plugin store tables, as the statement it replaces did; a
/// code plugin's switch is kept under `code:<id>`. Inputs: the connection, inside the
/// caller's transaction, and the event. Output: Ok, or the database's refusal.
pub fn apply(db: &Connection, event: &Event) -> Result<(), Refused> {
    let _ = (db, event);
    todo!()
}
