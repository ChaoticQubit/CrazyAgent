use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use crate::storage::versioned::{ITEM_BYTES_MAX, Item};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;

pub mod apply;
pub mod catalog;
pub mod installed;
pub mod store;
#[cfg(test)]
pub(crate) mod tests;

pub use catalog::{Apply, Catalog, Key, KeyScope, Kind, Locked};

/// The longest harness id a catalog names, in bytes.
pub const HARNESS_BYTES_MAX: usize = 32;
/// The longest setting key, in bytes: ASCII letters, digits, `_` and `-`, in dot-separated parts.
pub const KEY_BYTES_MAX: usize = 64;
/// The most keys one catalog lists.
pub const KEYS_MAX: usize = 256;
/// The longest label of one key, a single line.
pub const LABEL_BYTES_MAX: usize = 80;
/// The longest help of one key, a single paragraph.
pub const HELP_BYTES_MAX: usize = 1024;
/// The most options one `enum` key offers.
pub const OPTIONS_MAX: usize = 32;
/// The longest option of an `enum` key.
pub const OPTION_BYTES_MAX: usize = 64;
/// The largest `max_bytes` a `string` key may name.
pub const STRING_BYTES_MAX: usize = 1024;
/// The longest reason a locked key shows.
pub const REASON_BYTES_MAX: usize = 240;
/// The most harness versions one catalog is qualified for.
pub const VERSIONS_MAX: usize = 32;
/// The longest harness version text.
pub const VERSION_BYTES_MAX: usize = 64;
/// The most source links one catalog names.
pub const SOURCES_MAX: usize = 16;
/// The longest source link.
pub const SOURCE_BYTES_MAX: usize = 512;
/// The largest catalog file the engine reads.
pub const CATALOG_BYTES_MAX: usize = 1024 * 1024;
/// The most bytes one catalog or values page holds as JSON, so it fits one owner frame.
pub const PAGE_BYTES_MAX: usize = 48 * 1024;
/// The reason a key set once for the whole app shows on a project's page.
pub const APP_ONLY: &str = "Set once for the whole app in Settings > Harnesses";
/// The reason a key whose apply no CrazyAgent launch writes yet, such as an rcli or
/// command-line key, shows on a page.
pub const NOT_APPLIED: &str = "CrazyAgent does not apply this setting yet";

const ITEM_PREFIX: &str = "harness-setting";
const KEY_JSON_MAX: usize = 2
    * (KEY_BYTES_MAX
        + LABEL_BYTES_MAX
        + HELP_BYTES_MAX
        + OPTIONS_MAX * (OPTION_BYTES_MAX + 3)
        + 2 * STRING_BYTES_MAX
        + REASON_BYTES_MAX)
    + 512;
const HEADER_JSON_MAX: usize = 2
    * (VERSIONS_MAX * (VERSION_BYTES_MAX + 3)
        + SOURCES_MAX * (SOURCE_BYTES_MAX + 3)
        + VERSION_BYTES_MAX
        + 2 * HARNESS_BYTES_MAX)
    + 1024;
/// The longest stored value as JSON: a `string` value at its bound, every byte escaped
/// once, in quotes.
pub(crate) const VALUE_JSON_MAX: usize = 2 * STRING_BYTES_MAX + 2;

const _: () = assert!(
    ITEM_PREFIX.len() + HARNESS_BYTES_MAX + ":project:".len() + 128 + 1 + KEY_BYTES_MAX
        <= ITEM_BYTES_MAX
);
const _: () = assert!(HEADER_JSON_MAX + 2 * KEY_JSON_MAX <= PAGE_BYTES_MAX);
const _: () = assert!(PAGE_BYTES_MAX + 1024 <= crate::protocol::FRAME_BYTES_MAX);

/// Where one value is set: app-wide for every project, or as one project's override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    App,
    Project(ProjectId),
}

/// Whether the owner may change one key here: editable, locked by the engine with the
/// reason, or shown read-only with the reason (not qualified, or set for the whole app only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Access {
    Editable,
    Locked { reason: String },
    ReadOnly { reason: String },
}

/// Where a key's effective value comes from: the catalog default, the app value, the
/// project override, the engine's locked value, or nowhere because the key is not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Default,
    App,
    Project,
    Engine,
    NotApplied,
}

/// One key on a harness settings page: the value set at this page's scope, if any, the
/// value the next start uses and where it comes from, the version a set or reset names,
/// and whether the owner may change it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub key: String,
    pub here: Option<Value>,
    pub effective: Value,
    pub source: Source,
    pub version: u64,
    pub access: Access,
}

/// One page of a harness's settings at one scope: the catalog's keys in order, then keys
/// stored here that the catalog no longer lists. `next` starts the following page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub harness: String,
    pub installed: Option<String>,
    pub qualified: bool,
    pub settings: Vec<Setting>,
    pub next: Option<u32>,
}

/// One page of a harness's catalog: its name, the installed version and whether it is
/// qualified, the versions and sources the catalog was built from, and its keys in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPage {
    pub harness: String,
    pub name: String,
    pub installed: Option<String>,
    pub qualified: bool,
    pub qualified_versions: Vec<String>,
    pub sources: Vec<String>,
    pub keys: Vec<Key>,
    pub next: Option<u32>,
}

/// What a set or reset did: saved as the next version, refused as stale with the current
/// row, or refused with a code and the row whose access shows the reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Written {
    Saved { setting: Setting },
    Stale { setting: Setting },
    Refused { code: String, setting: Setting },
}

/// One value a harness start applies, with where it comes from and how it is applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub key: String,
    pub value: Value,
    pub source: Source,
    pub apply: Apply,
}

/// Values stored at one scope for one harness, by key.
pub type Stored = BTreeMap<String, Value>;

/// Why one key may or may not be changed at one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    Editable,
    Locked,
    NotQualified,
    AppOnly,
    NotApplied,
}

/// Tells whether a CrazyAgent launch writes keys of one apply: Claude Code's settings JSON
/// and Codex's `-c` lines. rcli's profile and command-line keys are not written yet.
/// Input: the apply. Output: true when a launch writes it.
pub fn applied_at_launch(apply: Apply) -> bool {
    matches!(apply, Apply::ClaudeSettings | Apply::CodexConfig)
}

/// Tells whether the installed harness version is one the catalog was qualified for.
/// Inputs: the catalog and the installed version, none when unknown. Output: true on an
/// exact match.
pub fn qualified(catalog: &Catalog, installed: Option<&str>) -> bool {
    installed.is_some_and(|version| catalog.qualified_versions.iter().any(|q| q == version))
}

/// Checks one value against its key's type and bounds. Inputs: the key and the value.
/// Output: Ok, or `harness_settings_value_invalid` for another type, an option the key
/// does not offer, a number outside its range, or text that is too long, holds a line
/// break or control character, or hides characters.
pub fn check_value(key: &Key, value: &Value) -> Result<(), EngineError> {
    let valid = match key.kind {
        Kind::Bool => value.is_boolean(),
        Kind::Integer => value
            .as_i64()
            .zip(key.min.zip(key.max))
            .is_some_and(|(number, (min, max))| (min..=max).contains(&number)),
        Kind::Enum => value
            .as_str()
            .is_some_and(|text| key.options.iter().any(|option| option == text)),
        Kind::String => value.as_str().is_some_and(|text| {
            key.max_bytes
                .and_then(|max| usize::try_from(max).ok())
                .is_some_and(|max| text.len() <= max)
                && !text.chars().any(char::is_control)
                && !crate::memory::hidden_text(text)
        }),
    };
    if valid {
        Ok(())
    } else {
        Err(failure("harness_settings_value_invalid"))
    }
}

/// Checks one setting key name: 1 to `KEY_BYTES_MAX` bytes of dot-separated parts, each
/// starting with an ASCII letter or digit and holding only letters, digits, `_` and `-`.
pub fn key_valid(name: &str) -> bool {
    name.len() <= KEY_BYTES_MAX
        && name.split('.').all(|part| {
            part.bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// Names the versioned item of one key at one scope. Inputs: a catalog harness id, the
/// scope and a valid key. Output: the item, or `versioned_item_invalid`.
pub(crate) fn item(harness: &str, scope: &Scope, key: &str) -> Result<Item, EngineError> {
    assert!(!harness.is_empty() && harness.len() <= HARNESS_BYTES_MAX);
    assert!(key_valid(key));
    Item::new(&match scope {
        Scope::App => format!("{ITEM_PREFIX}:{harness}:app:{key}"),
        Scope::Project(project) => {
            format!("{ITEM_PREFIX}:{harness}:project:{}:{key}", project.as_str())
        }
    })
}

fn harness_name(harness: &str) -> &str {
    crate::harnesses::spec(harness)
        .filter(|spec| spec.id == harness)
        .map_or(harness, |spec| spec.name)
}

fn not_qualified(catalog: &Catalog, installed: Option<&str>) -> String {
    let name = harness_name(&catalog.harness);
    match installed {
        Some(version) => format!("Not qualified for {name} {version}"),
        None => format!("Not qualified for {name} (version unknown)"),
    }
}

fn standing(catalog: &Catalog, installed: Option<&str>, scope: &Scope, name: &str) -> Standing {
    match catalog.key(name) {
        None => Standing::NotQualified,
        Some(key) if key.locked.is_some() => Standing::Locked,
        Some(key) if !applied_at_launch(key.apply) => Standing::NotApplied,
        Some(_) if !qualified(catalog, installed) => Standing::NotQualified,
        Some(key) if matches!(scope, Scope::Project(_)) && key.scope == KeyScope::App => {
            Standing::AppOnly
        }
        Some(_) => Standing::Editable,
    }
}

/// The refusal code of a set or reset on a key the owner may not change here, none when
/// the key is editable. Inputs: as `setting`. Output: the code.
pub(crate) fn refusal(
    catalog: &Catalog,
    installed: Option<&str>,
    scope: &Scope,
    name: &str,
) -> Option<&'static str> {
    match standing(catalog, installed, scope, name) {
        Standing::Editable => None,
        Standing::Locked => Some("harness_settings_key_locked"),
        Standing::NotQualified | Standing::NotApplied => Some("harness_settings_key_not_qualified"),
        Standing::AppOnly => Some("harness_settings_key_app_only"),
    }
}

/// Picks one unlocked key's value: the project override, then the app value, then the
/// catalog default, null when the harness decides. A stored value the catalog in force
/// refuses, such as one saved under an older catalog, is passed over. Inputs: the key, the
/// app values and the project's, none at app scope. Output: the value and where it comes
/// from.
fn layered(key: &Key, app: &Stored, project: Option<&Stored>) -> (Value, Source) {
    let allowed = |value: &&Value| check_value(key, value).is_ok();
    let overridable = key.scope == KeyScope::AppAndProject;
    if let Some(value) = project
        .and_then(|values| values.get(&key.key))
        .filter(|_| overridable)
        .filter(allowed)
    {
        return (value.clone(), Source::Project);
    }
    if let Some(value) = app.get(&key.key).filter(allowed) {
        return (value.clone(), Source::App);
    }
    assert!(key.locked.is_none());
    assert!(
        check_value(key, &key.default).is_ok()
            || (key.default.is_null() && key.harness_default.is_none())
    );
    (key.default.clone(), Source::Default)
}

/// Builds one key's row at one scope: catalog default, then the app value, then the
/// project override, with the engine's value for a locked key; a key the catalog does not
/// list, an unlocked key no launch writes yet, or any unlocked key of an unqualified
/// version, shows read-only. A stored value the catalog refuses stays shown here, with set
/// and reset open, but is not in force.
/// Inputs: the catalog, the installed version, the scope, the key, the app and project
/// values and the key's version at this scope. Output: the row.
pub(crate) fn setting(
    catalog: &Catalog,
    installed: Option<&str>,
    scope: &Scope,
    name: &str,
    app: &Stored,
    project: &Stored,
    version: u64,
) -> Setting {
    let stored_here = match scope {
        Scope::App => app.get(name),
        Scope::Project(_) => project.get(name),
    };
    let row = |here: Option<&Value>, effective: Value, source: Source, access: Access| Setting {
        key: name.to_owned(),
        here: here.cloned(),
        effective,
        source,
        version,
        access,
    };
    let Some(key) = catalog.key(name) else {
        let access = Access::ReadOnly {
            reason: not_qualified(catalog, installed),
        };
        return row(stored_here, Value::Null, Source::NotApplied, access);
    };
    if let Some(locked) = &key.locked {
        let access = Access::Locked {
            reason: locked.reason.clone(),
        };
        return row(None, locked.engine_value.clone(), Source::Engine, access);
    }
    let here = match key.scope {
        KeyScope::App if matches!(scope, Scope::Project(_)) => None,
        _ => stored_here,
    };
    let standing = standing(catalog, installed, scope, name);
    assert_ne!(standing, Standing::Locked);
    if standing == Standing::NotApplied {
        let access = Access::ReadOnly {
            reason: NOT_APPLIED.into(),
        };
        return row(here, Value::Null, Source::NotApplied, access);
    }
    if standing == Standing::NotQualified {
        let access = Access::ReadOnly {
            reason: not_qualified(catalog, installed),
        };
        return row(here, key.default.clone(), Source::Default, access);
    }
    let overrides = match scope {
        Scope::App => None,
        Scope::Project(_) => Some(project),
    };
    let (effective, source) = layered(key, app, overrides);
    let access = if standing == Standing::AppOnly {
        Access::ReadOnly {
            reason: APP_ONLY.into(),
        }
    } else {
        Access::Editable
    };
    row(here, effective, source, access)
}

/// Resolves what one harness start applies: for a qualified version, every unlocked key a
/// launch writes that the owner set, or whose default the engine picks over the harness's
/// own (catalog default, then app, then project), then every locked key with the engine's
/// value, last; an unqualified version gets only the locked keys. A key left to the
/// harness default applies nothing until the owner sets a value. Inputs: the catalog, the
/// installed version, the app values and the project's, none for an app-level start.
/// Output: the values in apply order.
pub fn resolve(
    catalog: &Catalog,
    installed: Option<&str>,
    app: &Stored,
    project: Option<&Stored>,
) -> Vec<Resolved> {
    let qualified = qualified(catalog, installed);
    let mut owner = Vec::new();
    let mut locked = Vec::new();
    for key in &catalog.keys {
        let resolved = |value: Value, source: Source| Resolved {
            key: key.key.clone(),
            value,
            source,
            apply: key.apply,
        };
        if let Some(lock) = &key.locked {
            locked.push(resolved(lock.engine_value.clone(), Source::Engine));
            continue;
        }
        if !qualified || !applied_at_launch(key.apply) {
            continue;
        }
        let (value, source) = layered(key, app, project);
        if source != Source::Default || key.harness_default.is_some() {
            assert!(!value.is_null(), "{}", key.key);
            owner.push(resolved(value, source));
        }
    }
    owner.extend(locked);
    assert!(owner.len() <= catalog.keys.len());
    owner
}

/// Builds one page of a harness's catalog. Inputs: the catalog, the installed version and
/// the index of the page's first key, none for the first page. Output: the page, or
/// `harness_settings_page_invalid` for an index past the last key.
pub fn catalog_page(
    catalog: &Catalog,
    installed: Option<&str>,
    after: Option<u32>,
) -> Result<CatalogPage, EngineError> {
    let mut page = CatalogPage {
        harness: catalog.harness.clone(),
        name: harness_name(&catalog.harness).to_owned(),
        installed: installed.map(str::to_owned),
        qualified: qualified(catalog, installed),
        qualified_versions: catalog.qualified_versions.clone(),
        sources: catalog.sources.clone(),
        keys: Vec::new(),
        next: None,
    };
    let (range, next) = window(&catalog.keys, after, budget(&page)?)?;
    page.keys = catalog.keys[range].to_vec();
    page.next = next;
    Ok(page)
}

/// Builds one page of rows from all of a scope's rows. Inputs: the harness, the installed
/// version and qualification, every row in order and the index of the page's first row.
/// Output: the page, or `harness_settings_page_invalid`.
pub(crate) fn values_page(
    catalog: &Catalog,
    installed: Option<&str>,
    rows: Vec<Setting>,
    after: Option<u32>,
) -> Result<Page, EngineError> {
    let mut page = Page {
        harness: catalog.harness.clone(),
        installed: installed.map(str::to_owned),
        qualified: qualified(catalog, installed),
        settings: Vec::new(),
        next: None,
    };
    let mut rows = rows;
    let (range, next) = window(&rows, after, budget(&page)?)?;
    page.settings = rows.drain(range).collect();
    page.next = next;
    Ok(page)
}

fn budget<T: Serialize>(header: &T) -> Result<usize, EngineError> {
    let bytes = serde_json::to_vec(header)
        .map_err(|_| failure("harness_settings_reply_bounds"))?
        .len();
    assert!(bytes <= HEADER_JSON_MAX);
    Ok(PAGE_BYTES_MAX - bytes)
}

fn window<T: Serialize>(
    items: &[T],
    after: Option<u32>,
    budget: usize,
) -> Result<(Range<usize>, Option<u32>), EngineError> {
    let start = usize::try_from(after.unwrap_or(0)).unwrap_or(usize::MAX);
    if start > items.len() {
        return Err(failure("harness_settings_page_invalid"));
    }
    let mut used = 0;
    let mut end = start;
    while end < items.len() {
        let size = serde_json::to_vec(&items[end])
            .map_err(|_| failure("harness_settings_reply_bounds"))?
            .len()
            + 1;
        assert!(size <= KEY_JSON_MAX);
        if used + size > budget {
            break;
        }
        used += size;
        end += 1;
    }
    assert!(end > start || start == items.len());
    let next = if end < items.len() {
        Some(u32::try_from(end).map_err(|_| failure("harness_settings_page_invalid"))?)
    } else {
        None
    };
    Ok((start..end, next))
}
