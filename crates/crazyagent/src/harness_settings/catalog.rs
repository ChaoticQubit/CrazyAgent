use super::{
    CATALOG_BYTES_MAX, HARNESS_BYTES_MAX, HELP_BYTES_MAX, KEYS_MAX, LABEL_BYTES_MAX,
    OPTION_BYTES_MAX, OPTIONS_MAX, REASON_BYTES_MAX, SOURCE_BYTES_MAX, SOURCES_MAX,
    STRING_BYTES_MAX, VERSIONS_MAX, check_value, installed, key_valid,
};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// The catalogs the engine ships, by harness id, each a version-qualified JSON document.
pub const BUILTIN: &[(&str, &str)] = &[];

/// One harness's settings catalog: the harness, the exact versions it was checked
/// against, the published references it was built from, and every key in page order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub harness: String,
    pub qualified_versions: Vec<String>,
    pub sources: Vec<String>,
    pub keys: Vec<Key>,
}

impl Catalog {
    /// Finds one key by name. Input: the key. Output: its definition, or none.
    pub fn key(&self, name: &str) -> Option<&Key> {
        self.keys.iter().find(|key| key.key == name)
    }
}

/// One setting of a harness: its name, label and help, its type with options or bounds,
/// the harness's own default, how a start applies it, whether a project may override it,
/// and, for a key the engine enforces, the reason and the value the engine sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub key: String,
    pub label: String,
    pub help: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    pub default: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u32>,
    pub apply: Apply,
    pub scope: KeyScope,
    pub locked: Option<Locked>,
}

/// The type of one key's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Bool,
    Integer,
    Enum,
    String,
}

/// How a start applies one key: into Claude Code's `--settings` JSON, as a Codex `-c`
/// line, into the rcli agent config file, or as a command-line argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    ClaudeSettings,
    CodexConfig,
    RcliConfig,
    Arg,
}

/// Where one key may be set: once for the whole app, or app-wide with a project override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyScope {
    App,
    AppAndProject,
}

/// Why the engine enforces one key and the value it sets; null names a key the engine
/// keeps unset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Locked {
    pub reason: String,
    pub engine_value: Value,
}

/// Reads one catalog and checks it against the contract. Input: the JSON bytes, at most
/// `CATALOG_BYTES_MAX`. Output: the catalog, or `harness_settings_catalog_invalid`.
pub fn parse(bytes: &[u8]) -> Result<Catalog, EngineError> {
    let invalid = || failure("harness_settings_catalog_invalid");
    if bytes.len() > CATALOG_BYTES_MAX {
        return Err(invalid());
    }
    let catalog: Catalog = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if !valid(&catalog) {
        return Err(invalid());
    }
    Ok(catalog)
}

/// Reads the shipped catalog of one harness. Input: the harness id. Output: the catalog,
/// none for a harness without one, or `harness_settings_catalog_invalid`.
pub fn builtin(harness: &str) -> Result<Option<Catalog>, EngineError> {
    let Some((_, text)) = BUILTIN.iter().find(|(id, _)| *id == harness) else {
        return Ok(None);
    };
    let catalog = parse(text.as_bytes())?;
    assert_eq!(catalog.harness, harness);
    Ok(Some(catalog))
}

fn valid(catalog: &Catalog) -> bool {
    let harness = catalog.harness.as_str();
    let known = harness.len() <= HARNESS_BYTES_MAX
        && crate::harnesses::spec(harness).is_some_and(|spec| spec.id == harness);
    let versions = (1..=VERSIONS_MAX).contains(&catalog.qualified_versions.len())
        && catalog
            .qualified_versions
            .iter()
            .all(|version| installed::version_valid(version))
        && distinct(catalog.qualified_versions.iter());
    let sources = (1..=SOURCES_MAX).contains(&catalog.sources.len())
        && catalog.sources.iter().all(|source| {
            source.len() <= SOURCE_BYTES_MAX
                && source.starts_with("https://")
                && source.bytes().all(|b| b.is_ascii_graphic())
        });
    let keys = (1..=KEYS_MAX).contains(&catalog.keys.len())
        && distinct(catalog.keys.iter().map(|key| &key.key))
        && catalog.keys.iter().all(|key| key_ok(harness, key));
    known && versions && sources && keys
}

fn distinct<'a>(mut items: impl Iterator<Item = &'a String>) -> bool {
    let mut seen = BTreeSet::new();
    items.all(|item| seen.insert(item))
}

fn line(text: &str, max: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= max
        && !text.chars().any(char::is_control)
        && !crate::memory::hidden_text(text)
}

fn key_ok(harness: &str, key: &Key) -> bool {
    let shape = match key.kind {
        Kind::Bool => key.options.is_empty() && key.min.is_none() && key.max.is_none(),
        Kind::Integer => {
            key.options.is_empty() && key.min.zip(key.max).is_some_and(|(min, max)| min <= max)
        }
        Kind::Enum => {
            (1..=OPTIONS_MAX).contains(&key.options.len())
                && key
                    .options
                    .iter()
                    .all(|option| line(option, OPTION_BYTES_MAX))
                && distinct(key.options.iter())
                && key.min.is_none()
                && key.max.is_none()
        }
        Kind::String => {
            key.options.is_empty()
                && key.min.is_none()
                && key.max.is_none()
                && key
                    .max_bytes
                    .and_then(|max| usize::try_from(max).ok())
                    .is_some_and(|max| (1..=STRING_BYTES_MAX).contains(&max))
        }
    };
    let bytes = key.max_bytes.is_none() || key.kind == Kind::String;
    let applies = match key.apply {
        Apply::ClaudeSettings => harness == "claude-code",
        Apply::CodexConfig => harness == "codex",
        Apply::RcliConfig => harness == "rcli",
        Apply::Arg => true,
    };
    let locked = key.locked.as_ref().is_none_or(|locked| {
        line(&locked.reason, REASON_BYTES_MAX)
            && (locked.engine_value.is_null() || check_value(key, &locked.engine_value).is_ok())
    });
    key_valid(&key.key)
        && line(&key.label, LABEL_BYTES_MAX)
        && line(&key.help, HELP_BYTES_MAX)
        && shape
        && bytes
        && applies
        && check_value(key, &key.default).is_ok()
        && locked
}
