use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{HARNESS_SETTINGS_CODES, OwnerClient};
use crazyagent::harness_settings::{
    Access, CatalogPage, HARNESS_BYTES_MAX, KEYS_MAX, Key, Kind, Page, STRING_BYTES_MAX, Setting,
    Source, Written, check_value, key_valid,
};
use crazyagent::protocol::ProjectId;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most rows one window page holds, as the engine reads them at one scope: every catalog
/// key, then at most `UNLISTED_MAX` keys stored there that the catalog no longer lists.
pub const ROWS_MAX: usize = KEYS_MAX + crazyagent::harness_settings::store::UNLISTED_MAX;
const _: () = assert!(ROWS_MAX >= KEYS_MAX);
const _: () = assert!(ROWS_MAX == 512);
/// The most engine pages one window read follows; every engine page holds at least one row.
const PAGES_MAX: usize = ROWS_MAX + 1;
const ROW_BYTES_MAX: usize = 16 * 1024;
const PAGE_BYTES_MAX: usize = ROWS_MAX * ROW_BYTES_MAX;

/// One harness setting as the window shows it: the catalog's label, help, type, options,
/// default and bounds, then the value set at the page's scope (null when none), whether the
/// catalog in force allows that value, whether the catalog lists the key at all, the value
/// the next start uses with its source, the version a change names and the engine's access.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingRow {
    pub key: String,
    pub label: String,
    pub help: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub options: Vec<String>,
    pub default: Value,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub max_bytes: Option<u32>,
    pub here: Value,
    pub here_valid: bool,
    pub listed: bool,
    pub value: Value,
    pub source: &'static str,
    pub version: u64,
    pub access: Access,
}

/// One harness's settings page as the window shows it: the harness and its name, the
/// installed version and whether it is qualified, and every row in the engine's order.
/// `project_id` is None on the app page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HarnessSettingsPage {
    pub harness: String,
    pub name: String,
    pub installed: Option<String>,
    pub qualified: bool,
    pub qualified_versions: Vec<String>,
    pub project_id: Option<String>,
    pub keys: Vec<SettingRow>,
}

/// The bridge failure for an engine harness settings error: a known code reaches the window
/// as itself, anything else is an unknown outcome with the engine code behind it. Input: the
/// engine error. Output: the bridge failure.
pub fn harness_settings_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        HARNESS_SETTINGS_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("harness_settings_outcome_unknown"),
        &error.0,
    )
}

/// Reads what a set or reset did: Ok once saved, `harness_settings_stale` when the key
/// changed since the owner read it, or the engine's refusal code. Input: the engine reply.
/// Output: the result.
pub fn written(written: Written) -> Result<(), BridgeError> {
    match written {
        Written::Saved { .. } => Ok(()),
        Written::Stale { .. } => Err(failure("harness_settings_stale")),
        Written::Refused { code, .. } => Err(harness_settings_error(EngineError(code))),
    }
}

/// A value as the window sends it, before the engine checks it against the key: a boolean,
/// a whole number that fits in i64, or text of at most `STRING_BYTES_MAX` bytes. Input: the
/// value. Output: the value, or `harness_settings_value_invalid`.
pub fn window_value(value: Value) -> Result<Value, BridgeError> {
    let fits = match &value {
        Value::Bool(_) => true,
        Value::Number(number) => number.is_i64(),
        Value::String(text) => text.len() <= STRING_BYTES_MAX,
        Value::Null | Value::Array(_) | Value::Object(_) => false,
    };
    if fits {
        Ok(value)
    } else {
        Err(failure("harness_settings_value_invalid"))
    }
}

/// A harness id as the window sends it: 1 to `HARNESS_BYTES_MAX` bytes of lowercase ASCII
/// letters, digits and `-`. Input: the id. Output: the id, or
/// `harness_settings_request_invalid`.
pub fn window_harness(harness: String) -> Result<String, BridgeError> {
    let shaped = (1..=HARNESS_BYTES_MAX).contains(&harness.len())
        && harness
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if shaped {
        Ok(harness)
    } else {
        Err(failure("harness_settings_request_invalid"))
    }
}

fn window_key(key: String) -> Result<String, BridgeError> {
    if key_valid(&key) {
        Ok(key)
    } else {
        Err(failure("harness_settings_key_invalid"))
    }
}

fn window_project(project_id: Option<String>) -> Result<Option<ProjectId>, BridgeError> {
    project_id
        .map(|id| ProjectId::new(id).map_err(|_| failure("harness_settings_project_unknown")))
        .transpose()
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Bool => "bool",
        Kind::Integer => "integer",
        Kind::Enum => "enum",
        Kind::String => "string",
    }
}

fn source_word(source: Source) -> &'static str {
    match source {
        Source::Default => "default",
        Source::App => "app",
        Source::Project => "project",
        Source::Engine => "engine",
        Source::NotApplied => "not_applied",
    }
}

fn bound_fits(max_bytes: Option<u32>) -> bool {
    max_bytes
        .is_none_or(|bytes| usize::try_from(bytes).is_ok_and(|bytes| bytes <= STRING_BYTES_MAX))
}

fn unreadable() -> BridgeError {
    failure("owner_read_failed")
}

fn here_valid(key: Option<&Key>, here: Option<&Value>) -> bool {
    match (key, here) {
        (_, None) => true,
        (Some(key), Some(here)) => check_value(key, here).is_ok(),
        (None, Some(_)) => false,
    }
}

/// One window row from the engine's row and the catalog key it names, none for a key stored
/// here that the catalog no longer lists, which the engine must show read-only, not applied
/// and with its stored value. A key is locked exactly when its catalog key is, with the
/// engine's value; an editable key's value must pass the catalog's own check, or be none
/// from a null catalog default the harness decides, while a stored value the catalog
/// refuses stays on the row as not valid; a text key's bound must fit the engine's. Output:
/// the row, or `owner_read_failed`.
fn row(key: Option<&Key>, setting: Setting) -> Result<SettingRow, BridgeError> {
    let editable = matches!(setting.access, Access::Editable);
    let locked = matches!(setting.access, Access::Locked { .. });
    let consistent = match key {
        Some(key) => {
            let unset = key.default.is_null()
                && setting.effective.is_null()
                && setting.source == Source::Default;
            (!editable || unset || check_value(key, &setting.effective).is_ok())
                && locked == key.locked.is_some()
                && (!locked || setting.source == Source::Engine)
                && bound_fits(key.max_bytes)
        }
        None => {
            matches!(setting.access, Access::ReadOnly { .. })
                && setting.source == Source::NotApplied
                && setting.here.is_some()
        }
    };
    if !consistent || setting.key.is_empty() {
        return Err(unreadable());
    }
    let valid = here_valid(key, setting.here.as_ref());
    let here = setting.here.unwrap_or(Value::Null);
    let row = match key {
        Some(key) => SettingRow {
            key: setting.key,
            label: key.label.clone(),
            help: key.help.clone(),
            kind: kind_word(key.kind),
            options: key.options.clone(),
            default: key.default.clone(),
            min: key.min,
            max: key.max,
            max_bytes: key.max_bytes,
            here,
            here_valid: valid,
            listed: true,
            value: setting.effective,
            source: source_word(setting.source),
            version: setting.version,
            access: setting.access,
        },
        None => SettingRow {
            label: setting.key.clone(),
            key: setting.key,
            help: String::new(),
            kind: "string",
            options: Vec::new(),
            default: Value::Null,
            min: None,
            max: None,
            max_bytes: None,
            here,
            here_valid: valid,
            listed: false,
            value: setting.effective,
            source: source_word(setting.source),
            version: setting.version,
            access: setting.access,
        },
    };
    assert!(!row.label.is_empty());
    Ok(row)
}

/// Merges every catalog page and every values page of one harness at one scope into the
/// window's page: each row takes its catalog key's label, help, type and bounds. Every page
/// must name the harness, every catalog key must have exactly one row, and rows stay in the
/// engine's order. Inputs: the pages, at least one of each, and the project id, None for the
/// app. Output: the page, or `owner_read_failed` for pages that do not agree.
pub fn page_of(
    catalog: Vec<CatalogPage>,
    values: Vec<Page>,
    project_id: Option<String>,
) -> Result<HarnessSettingsPage, BridgeError> {
    assert!(!catalog.is_empty());
    assert!(!values.is_empty());
    let head = &catalog[0];
    let (harness, name) = (head.harness.clone(), head.name.clone());
    let qualified_versions = head.qualified_versions.clone();
    let (installed, qualified) = (values[0].installed.clone(), values[0].qualified);
    let same = catalog.iter().all(|page| page.harness == harness)
        && values.iter().all(|page| page.harness == harness);
    let keys: Vec<Key> = catalog.into_iter().flat_map(|page| page.keys).collect();
    let settings: Vec<Setting> = values.into_iter().flat_map(|page| page.settings).collect();
    let by_name: BTreeMap<&str, &Key> = keys.iter().map(|key| (key.key.as_str(), key)).collect();
    if !same || keys.len() > KEYS_MAX || settings.len() > ROWS_MAX || by_name.len() != keys.len() {
        return Err(unreadable());
    }
    let mut seen = BTreeSet::new();
    let mut rows = Vec::with_capacity(settings.len());
    for setting in settings {
        if !seen.insert(setting.key.clone()) {
            return Err(unreadable());
        }
        rows.push(row(by_name.get(setting.key.as_str()).copied(), setting)?);
    }
    if keys.iter().any(|key| !seen.contains(&key.key)) {
        return Err(unreadable());
    }
    assert!(rows.len() >= keys.len());
    assert!(rows.len() <= ROWS_MAX);
    bounded_by(
        HarnessSettingsPage {
            harness,
            name,
            installed,
            qualified,
            qualified_versions,
            project_id,
            keys: rows,
        },
        PAGE_BYTES_MAX,
    )
}

/// Checks one engine page's `next` against the index the page was read from: each must
/// move past the one before, the first page starting at 0, so paging always ends. Inputs:
/// the index read from, None for the first page, and the page's `next`. Output: the index
/// to read next, None when this was the last page, or `owner_read_failed`.
pub fn following(after: Option<u32>, next: Option<u32>) -> Result<Option<u32>, BridgeError> {
    match next {
        None => Ok(None),
        Some(index) if index > after.unwrap_or(0) => Ok(Some(index)),
        Some(_) => Err(unreadable()),
    }
}

async fn catalog_pages(
    client: &OwnerClient,
    harness: &str,
) -> Result<Vec<CatalogPage>, BridgeError> {
    let mut pages = Vec::new();
    let mut after = None;
    while pages.len() < PAGES_MAX {
        let page = client
            .harness_settings_catalog(harness, after)
            .await
            .map_err(harness_settings_error)?;
        let next = following(after, page.next)?;
        pages.push(page);
        let Some(index) = next else {
            return Ok(pages);
        };
        after = Some(index);
    }
    Err(unreadable())
}

async fn value_pages(
    client: &OwnerClient,
    harness: &str,
    project: Option<ProjectId>,
) -> Result<Vec<Page>, BridgeError> {
    let mut pages = Vec::new();
    let mut after = None;
    while pages.len() < PAGES_MAX {
        let page = client
            .harness_settings_values(harness, project.clone(), after)
            .await
            .map_err(harness_settings_error)?;
        let next = following(after, page.next)?;
        pages.push(page);
        let Some(index) = next else {
            return Ok(pages);
        };
        after = Some(index);
    }
    Err(unreadable())
}

impl NativeBridge {
    fn harness_settings_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(harness_settings_error)
    }

    /// Reads one harness's settings page app-wide or for one project, run without the
    /// bridge lock: every catalog page, then every values page at the scope, merged.
    /// Inputs: the harness id and the project id, None for the app. Output: the page, or
    /// the engine's refusal, such as `harness_settings_no_catalog`.
    pub fn harness_settings(
        &self,
        harness: String,
        project_id: Option<String>,
    ) -> Result<Unlocked<HarnessSettingsPage>, BridgeError> {
        let harness = window_harness(harness)?;
        let project = window_project(project_id.clone())?;
        let client = self.harness_settings_client()?;
        Ok(Box::pin(async move {
            let catalog = catalog_pages(&client, &harness).await?;
            let values = value_pages(&client, &harness, project).await?;
            page_of(catalog, values, project_id)
        }))
    }

    /// Sets one harness setting app-wide or as a project override, naming the version the
    /// owner read, run without the bridge lock. Inputs: the harness, the project id, None
    /// for the app, the key, the value and the version. Output: Ok once saved;
    /// `harness_settings_stale` when it changed since, or the engine's refusal.
    pub fn set_harness_setting(
        &self,
        harness: String,
        project_id: Option<String>,
        key: String,
        value: Value,
        version: u64,
    ) -> Result<Unlocked<()>, BridgeError> {
        let harness = window_harness(harness)?;
        let project = window_project(project_id)?;
        let key = window_key(key)?;
        let value = window_value(value)?;
        let client = self.harness_settings_client()?;
        Ok(Box::pin(async move {
            written(
                client
                    .harness_settings_set(&harness, project, &key, value, version)
                    .await
                    .map_err(harness_settings_error)?,
            )
        }))
    }

    /// Resets one harness setting at one scope, naming the version the owner read, run
    /// without the bridge lock. Inputs: as `set_harness_setting`, without a value. Output:
    /// as `set_harness_setting`.
    pub fn reset_harness_setting(
        &self,
        harness: String,
        project_id: Option<String>,
        key: String,
        version: u64,
    ) -> Result<Unlocked<()>, BridgeError> {
        let harness = window_harness(harness)?;
        let project = window_project(project_id)?;
        let key = window_key(key)?;
        let client = self.harness_settings_client()?;
        Ok(Box::pin(async move {
            written(
                client
                    .harness_settings_reset(&harness, project, &key, version)
                    .await
                    .map_err(harness_settings_error)?,
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every harness settings code the engine answers reaches the window as itself, so the
    /// owner reads its sentence; anything else is an unknown outcome with the engine code
    /// behind it.
    #[test]
    fn engine_harness_settings_codes_pass_through_unchanged() {
        assert!(!HARNESS_SETTINGS_CODES.is_empty());
        for code in HARNESS_SETTINGS_CODES {
            assert_eq!(
                harness_settings_error(EngineError((*code).into())).code,
                *code
            );
        }
        assert_eq!(
            harness_settings_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = harness_settings_error(EngineError("harness_settings_torn".into()));
        assert_eq!(unlisted.code, "harness_settings_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("harness_settings_torn"));
        let raw = harness_settings_error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(raw.code, "harness_settings_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
    }

    /// The window sends a boolean, a whole number or bounded text, a harness id of the
    /// engine's shape and a valid key; anything else is refused before the engine is asked.
    #[test]
    fn the_window_sends_a_bounded_value_a_shaped_harness_and_a_valid_key() {
        for value in [
            Value::Bool(false),
            Value::from(-3),
            Value::from(i64::MAX),
            Value::from("medium"),
            Value::from("a".repeat(STRING_BYTES_MAX)),
        ] {
            assert_eq!(window_value(value.clone()).unwrap(), value);
        }
        for value in [
            Value::Null,
            Value::from(1.5),
            Value::from(u64::MAX),
            Value::from("a".repeat(STRING_BYTES_MAX + 1)),
            serde_json::json!(["a"]),
            serde_json::json!({"a": 1}),
        ] {
            assert_eq!(
                window_value(value.clone()).unwrap_err().code,
                "harness_settings_value_invalid",
                "{value}"
            );
        }
        assert_eq!(window_harness("claude-code".into()).unwrap(), "claude-code");
        for harness in ["", "Claude", "../codex", "codex\n"] {
            assert_eq!(
                window_harness(harness.into()).unwrap_err().code,
                "harness_settings_request_invalid"
            );
        }
        assert_eq!(
            window_harness("a".repeat(HARNESS_BYTES_MAX + 1))
                .unwrap_err()
                .code,
            "harness_settings_request_invalid"
        );
        assert_eq!(
            window_key("tui.status_label".into()).unwrap(),
            "tui.status_label"
        );
        assert_eq!(
            window_key("../x".into()).unwrap_err().code,
            "harness_settings_key_invalid"
        );
        assert_eq!(
            window_project(Some(String::new())).unwrap_err().code,
            "harness_settings_project_unknown"
        );
        assert!(window_project(None).unwrap().is_none());
    }

    /// Paging ends: each `next` must move past the index read, so an engine that repeats or
    /// goes back is refused instead of read forever.
    #[test]
    fn every_next_moves_past_the_page_before() {
        assert_eq!(following(None, None).unwrap(), None);
        assert_eq!(
            following(None, Some(0)).unwrap_err().code,
            "owner_read_failed"
        );
        assert_eq!(following(None, Some(4)).unwrap(), Some(4));
        assert_eq!(following(Some(4), Some(9)).unwrap(), Some(9));
        assert_eq!(following(Some(9), None).unwrap(), None);
        assert_eq!(
            following(Some(4), Some(4)).unwrap_err().code,
            "owner_read_failed"
        );
        assert_eq!(
            following(Some(9), Some(4)).unwrap_err().code,
            "owner_read_failed"
        );
        const { assert!(PAGES_MAX > ROWS_MAX) };
    }
}
