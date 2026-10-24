use serde::{Deserialize, Serialize};

/// What a default-pack entry installs: a skill folder agents read, or an MCP server
/// agents call. Track C's `PackEntry` status view reuses this kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackKind {
    Skill,
    McpServer,
}

/// Where a pack entry belongs on the setup screen. `Always` is required and cannot be
/// switched off; `Optional` entries start off; every other group starts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackGroup {
    Always,
    Workflow,
    Design,
    ReplyStyle,
    Context,
    Optional,
}

/// Where setup fetches an entry from: a public git repository pinned to one commit and
/// an optional folder inside it, or a folder the app bundles from the owner's own
/// skills. Paths are relative and never leave their root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackSource {
    Git {
        repo: String,
        commit: String,
        path: Option<String>,
    },
    Bundled {
        path: String,
    },
}

/// One pinned entry of the default pack as shipped in `pack.toml`: a stable id, its
/// kind and group, the pinned source, the SPDX license, whether it starts on, and a
/// short install hint for setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifestEntry {
    pub id: String,
    pub kind: PackKind,
    pub group: PackGroup,
    pub source: PackSource,
    pub license: String,
    pub on_by_default: bool,
    pub install: String,
}

/// The whole default-pack manifest: its format version and every entry in setup order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub version: u32,
    pub entries: Vec<PackManifestEntry>,
}
