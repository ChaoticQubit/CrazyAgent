use crate::engine::EngineError;
use crate::plugins::validate_server_name;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const VERSION: u32 = 1;
pub const TEXT_BYTES_MAX: usize = 64 * 1024;
pub const ENTRIES_MAX: usize = 64;
pub const LICENSE_BYTES_MAX: usize = 64;
pub const INSTALL_BYTES_MAX: usize = 512;
pub const PATH_BYTES_MAX: usize = 256;
pub const NAME_PART_BYTES_MAX: usize = 100;
const GITHUB: &str = "https://github.com/";
const SHIPPED: &str = include_str!("pack.toml");

/// What a default-pack entry installs: a skill folder agents read, or an MCP server
/// agents call. Track C's `PackEntry` status view reuses this kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackKind {
    Skill,
    McpServer,
}

/// The id of the code graph entry. While it is not installed and on, onboarding builds no
/// code graph and setup installs no graphify command.
pub const GRAPH_ENTRY: &str = "graphify";

/// The name the setup screens show for a pack entry, kept apart from the tool of the
/// same name. Input: the entry id. Output: `Graphify skill for agents` for the code
/// graph entry, otherwise the id.
pub fn entry_title(id: &str) -> &str {
    if id == GRAPH_ENTRY {
        "Graphify skill for agents"
    } else {
        id
    }
}

/// Where a pack entry belongs on the setup screen. `Graph` holds the code graph entry;
/// `Optional` entries start off; every other group starts on. Every entry, the code
/// graph included, can be unticked, switched off or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackGroup {
    Graph,
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

fn error(code: &str) -> EngineError {
    EngineError(code.into())
}

/// Loads the default pack compiled into the engine from `pack.toml`, with no file or
/// network access at load. Input: none. Output: the validated manifest, or the code
/// of the first problem.
pub fn default_pack() -> Result<PackManifest, EngineError> {
    parse(SHIPPED)
}

/// Parses and validates a pack manifest. Input: untrusted TOML text. Output: the
/// manifest, or a code naming the first problem: size, unreadable TOML or an unknown
/// field, version, or the first entry check that fails.
pub fn parse(text: &str) -> Result<PackManifest, EngineError> {
    if text.len() > TEXT_BYTES_MAX {
        return Err(error("pack_manifest_size"));
    }
    let manifest: PackManifest =
        toml::from_str(text).map_err(|_| error("pack_manifest_unreadable"))?;
    manifest.validate()?;
    Ok(manifest)
}

impl PackManifest {
    /// Checks the version, the entry count and every entry, and that ids are unique.
    /// Input: a parsed manifest. Output: Ok, or the first problem's code.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.version != VERSION {
            return Err(error("pack_manifest_version"));
        }
        if self.entries.is_empty() || self.entries.len() > ENTRIES_MAX {
            return Err(error("pack_manifest_size"));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.entries {
            entry.validate()?;
            if !seen.insert(entry.id.as_str()) {
                return Err(error("pack_entry_id_duplicate"));
            }
        }
        Ok(())
    }
}

impl PackManifestEntry {
    /// Checks one entry: an id usable as an MCP server key, a present SPDX-style
    /// license, a printable install hint, the default that matches its group, and a
    /// GitHub repo pinned to a full lowercase commit or a relative bundled path.
    /// Input: a parsed entry. Output: Ok, or the first problem's code.
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_server_name(&self.id).map_err(|_| error("pack_entry_id_invalid"))?;
        if self.license.is_empty() {
            return Err(error("pack_entry_license_missing"));
        }
        if !license_text(&self.license) {
            return Err(error("pack_entry_license_invalid"));
        }
        if self.install.is_empty()
            || self.install.len() > INSTALL_BYTES_MAX
            || self.install.chars().any(char::is_control)
        {
            return Err(error("pack_entry_install_invalid"));
        }
        if self.on_by_default == (self.group == PackGroup::Optional) {
            return Err(error("pack_entry_default_invalid"));
        }
        match &self.source {
            PackSource::Git { repo, commit, path } => {
                if !github_repo(repo) {
                    return Err(error("pack_entry_repo_invalid"));
                }
                if !full_commit(commit) {
                    return Err(error("pack_entry_commit_invalid"));
                }
                if path.as_deref().is_some_and(|path| !relative_path(path)) {
                    return Err(error("pack_entry_path_invalid"));
                }
            }
            PackSource::Bundled { path } => {
                if !relative_path(path) {
                    return Err(error("pack_entry_path_invalid"));
                }
            }
        }
        Ok(())
    }
}

fn license_text(license: &str) -> bool {
    license.len() <= LICENSE_BYTES_MAX
        && !license.starts_with(' ')
        && !license.ends_with(' ')
        && license
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+() ".contains(&byte))
}

fn name_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= NAME_PART_BYTES_MAX
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn github_repo(repo: &str) -> bool {
    let Some(rest) = repo.strip_prefix(GITHUB) else {
        return false;
    };
    let mut parts = rest.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [owner, name].into_iter().all(name_part) && !name.ends_with(".git")
}

fn full_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn relative_path(path: &str) -> bool {
    path.len() <= PATH_BYTES_MAX && path.split('/').all(name_part)
}
