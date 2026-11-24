use serde::{Deserialize, Serialize};

use crate::problem::Problem;

/// What a plugin does for the app. One variant per kind, appended as each kind lands; an
/// embedding plugin turns text into vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Embedding,
}

/// Where a plugin comes from: shipped inside the app, or a file in the owner's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Bundled,
    User,
}

/// How a plugin runs, as its trust screen shows it: the app's own local runtime with the
/// size of its one-time download, or the HTTP API it calls, with the exact hosts it reaches,
/// whether that is plain http to this computer ("local, not encrypted") and the vector size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunsView {
    Local {
        runtime: String,
        download_bytes: u64,
    },
    Http {
        url: String,
        hosts: Vec<String>,
        local_unencrypted: bool,
        dimensions: u16,
    },
}

/// Where one declared secret's value comes from: nowhere yet, a saved API-key account's
/// key (by its credential reference, with the account's name), or a key the owner entered
/// for this plugin. The value itself is never shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    None,
    Account { account: String, name: String },
    Key,
}

/// One secret a plugin declares, by name, and where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretView {
    pub name: String,
    pub binding: Binding,
}

/// Whether a plugin can be used: ready, a user file the owner has not reviewed since it
/// changed, a plugin whose declared secrets are not all bound, or a file the engine refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginStatus {
    Ready,
    NeedsReview,
    NeedsKey,
    Refused { problem: Problem },
}

/// One plugin as the App plugins page lists it. A refused file has its file name as name
/// and title, no `runs`, and the kind its text declares, embedding when that can't be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginView {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub origin: Origin,
    pub status: PluginStatus,
    pub enabled: bool,
    pub runs: Option<RunsView>,
    pub secrets: Vec<SecretView>,
    pub sha256: String,
}

/// Every plugin the engine knows, bundled first then the owner's files in name order, and
/// whether the folder held more files than the engine reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    pub plugins: Vec<PluginView>,
    pub more: bool,
}

/// The trust screen of a manifest before it is added or trusted: what it is, how it runs,
/// the hosts it reaches, the secrets it asks for by name, the SHA-256 of its exact bytes
/// (the add or trust request must name it), whether it replaces a user plugin of the
/// same name, and on a re-review whether the text changed since it was trusted and which
/// hosts it reaches that the trusted version did not, false and empty otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustView {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub runs: RunsView,
    pub secrets: Vec<String>,
    pub sha256: String,
    pub replaces: bool,
    pub changed: bool,
    pub grew_hosts: Vec<String>,
}

/// A manifest read for its trust screen: ready to add, or refused with the line or field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Preview {
    Ready { trust: TrustView },
    Refused { problem: Problem },
}
