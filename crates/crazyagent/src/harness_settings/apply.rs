use super::{Apply, Catalog, Resolved, Source, catalog, installed, store};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::path::Path;

/// The owner's settings one harness start applies: only the unlocked keys the owner set,
/// as the app value or the project override, in catalog order. A locked key never appears,
/// and each launch builder writes its own values over these, last. The default applies
/// nothing, so the harness starts with the engine's values only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    harness: Option<String>,
    values: Vec<Resolved>,
}

impl Applied {
    /// Keeps the owner's values of one resolution and drops the locked keys it ends with.
    /// Inputs: the catalog and its `resolve` output. Output: the settings.
    pub fn new(catalog: &Catalog, resolved: Vec<Resolved>) -> Self {
        let values: Vec<Resolved> = resolved
            .into_iter()
            .filter(|value| value.source != Source::Engine)
            .collect();
        assert!(values.iter().all(|value| {
            matches!(value.source, Source::App | Source::Project)
                && catalog
                    .key(&value.key)
                    .is_some_and(|key| key.locked.is_none() && key.apply == value.apply)
        }));
        Self {
            harness: Some(catalog.harness.clone()),
            values,
        }
    }

    /// Names the harness these settings were resolved for, none for the default.
    pub fn harness(&self) -> Option<&str> {
        self.harness.as_deref()
    }

    /// Lists the owner's values in apply order.
    pub fn values(&self) -> &[Resolved] {
        &self.values
    }

    fn of(&self, apply: Apply) -> impl Iterator<Item = &Resolved> {
        self.values.iter().filter(move |value| value.apply == apply)
    }
}

/// Reads the settings one harness start applies: the shipped catalog of the harness, the
/// version its executable reports and the owner's app and project values. A harness
/// without a catalog applies nothing. Inputs: the engine database, the harness id or
/// connection name, its executable and the project, none for an app-level start. Output:
/// the settings, or a `HARNESS_SETTINGS_CODES` refusal.
pub fn for_launch(
    db: &Connection,
    harness: &str,
    executable: &Path,
    project: Option<&ProjectId>,
) -> Result<Applied, EngineError> {
    launch(db, harness, project, || {
        installed::version_of(executable, installed::LIMIT)
    })
}

/// Reads the settings one master turn applies, as `for_launch` does, asking the executable
/// for its version once per content the engine verified. Inputs: the engine database, the
/// harness id, the executable, its SHA-256 and the project. Output: the settings, or a
/// `HARNESS_SETTINGS_CODES` refusal.
pub fn for_master(
    db: &Connection,
    harness: &str,
    executable: &Path,
    digest: &str,
    project: &ProjectId,
) -> Result<Applied, EngineError> {
    launch(db, harness, Some(project), || {
        installed::version_of_digest(executable, digest, installed::LIMIT)
    })
}

fn launch(
    db: &Connection,
    harness: &str,
    project: Option<&ProjectId>,
    version: impl FnOnce() -> Option<String>,
) -> Result<Applied, EngineError> {
    let Some(id) = crate::harnesses::spec(harness).map(|spec| spec.id) else {
        return Ok(Applied::default());
    };
    let Some(catalog) = catalog::builtin(id)? else {
        return Ok(Applied::default());
    };
    resolved(db, &catalog, version().as_deref(), project)
}

/// Reads the settings one start of a known catalog and version applies. Inputs: the
/// engine database, the catalog, the installed version, none when unknown, and the
/// project. Output: the settings, or a `HARNESS_SETTINGS_CODES` refusal.
pub fn resolved(
    db: &Connection,
    catalog: &Catalog,
    installed: Option<&str>,
    project: Option<&ProjectId>,
) -> Result<Applied, EngineError> {
    let values = store::resolved(db, catalog, installed, project)?;
    Ok(Applied::new(catalog, values))
}

/// Merges the owner's values into a settings object the engine built, such as Claude
/// Code's `--settings` JSON: each dotted key becomes nested objects, a key whose first part
/// the engine sets or reserves is dropped, and every engine key is written last, so it keeps
/// the engine's value. Inputs: the engine's object, the settings, which keys apply here and
/// the top-level keys the engine sets on other launches. Output: the merged object.
pub fn json_settings(engine: &Value, applied: &Applied, apply: Apply, reserved: &[&str]) -> Value {
    assert!(matches!(apply, Apply::ClaudeSettings | Apply::RcliConfig));
    assert!(engine.is_object());
    let empty = Map::new();
    let fixed = engine.as_object().unwrap_or(&empty);
    let mut merged = Map::new();
    for value in applied.of(apply) {
        let top = value.key.split('.').next().unwrap_or_default();
        if !fixed.contains_key(top) && !reserved.contains(&top) {
            insert(&mut merged, &value.key, value.value.clone());
        }
    }
    for (key, value) in fixed {
        merged.insert(key.clone(), value.clone());
    }
    assert!(
        fixed
            .iter()
            .all(|(key, value)| merged.get(key) == Some(value))
    );
    assert!(
        reserved
            .iter()
            .all(|key| merged.get(*key) == fixed.get(*key))
    );
    Value::Object(merged)
}

fn insert(map: &mut Map<String, Value>, key: &str, value: Value) {
    match key.split_once('.') {
        None => {
            let previous = map.insert(key.to_owned(), value);
            assert!(previous.is_none());
        }
        Some((head, rest)) => {
            let child = map
                .entry(head.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            assert!(child.is_object());
            if let Value::Object(child) = child {
                insert(child, rest, value);
            }
        }
    }
}

/// Reads back the owner's values a settings object carries, for a command verifier: every
/// value must sit at an unlocked key of the catalog with this apply and pass its checks,
/// and merging them over the engine's object must give the same object. Inputs: the
/// shipped catalog, none when the harness has none, the engine's object, the apply, the
/// reserved keys and the JSON text. Output: the settings, or none for any other text.
pub fn read_back(
    catalog: Option<&Catalog>,
    engine: &Value,
    apply: Apply,
    reserved: &[&str],
    text: &str,
) -> Option<Applied> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let applied = match catalog {
        None => Applied::default(),
        Some(catalog) => {
            let values = catalog
                .keys
                .iter()
                .filter(|key| key.locked.is_none() && key.apply == apply)
                .filter_map(|key| {
                    let value = lookup(&parsed, &key.key)?;
                    super::check_value(key, value).ok()?;
                    Some(Resolved {
                        key: key.key.clone(),
                        value: value.clone(),
                        source: Source::App,
                        apply,
                    })
                })
                .collect();
            Applied::new(catalog, values)
        }
    };
    (json_settings(engine, &applied, apply, reserved) == parsed).then_some(applied)
}

fn lookup<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.')
        .try_fold(value, |value, part| value.get(part))
}

/// Renders the owner's Codex values as a `config.toml` for a home whose launch passes the
/// engine's settings as `-c` lines, which Codex reads over the file: the lines
/// `codex_lines` gives, one per line. Inputs: as `codex_lines`. Output: the file text,
/// none when no owner value applies.
pub fn codex_config(engine: &[String], reserved: &[&str], applied: &Applied) -> Option<String> {
    let lines = codex_lines(engine, reserved, applied);
    if lines.is_empty() {
        return None;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    assert_eq!(text.lines().count(), lines.len());
    Some(text)
}

/// Renders the owner's Codex values as `-c` settings, one `key=value` each in TOML, for
/// the launch to pass before the engine's own: a key that equals, contains or sits inside
/// a key the engine sets or reserves is dropped, so no engine setting is ever overridden.
/// Inputs: the engine's settings, the keys the engine sets on other launches and the
/// owner's settings. Output: the owner's settings.
pub fn codex_lines(engine: &[String], reserved: &[&str], applied: &Applied) -> Vec<String> {
    let taken: Vec<&str> = engine
        .iter()
        .map(|line| line_key(line))
        .chain(reserved.iter().copied())
        .collect();
    let lines: Vec<String> = applied
        .of(Apply::CodexConfig)
        .filter(|value| !taken.iter().any(|key| overlaps(key, &value.key)))
        .map(|value| {
            assert!(value.value.is_boolean() || value.value.is_i64() || value.value.is_string());
            format!("{}={}", value.key, value.value)
        })
        .collect();
    assert!(lines.iter().all(|line| {
        let key = line_key(line);
        !taken.iter().any(|taken| overlaps(taken, key))
    }));
    lines
}

/// Names the key one `-c` setting sets: the text before its first `=`, trimmed.
fn line_key(line: &str) -> &str {
    line.split_once('=').map_or(line, |(key, _)| key).trim()
}

/// Tells whether two dotted keys name the same setting or one sits inside the other.
fn overlaps(one: &str, other: &str) -> bool {
    let inside = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('.'))
    };
    one == other || inside(one, other) || inside(other, one)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "apply_tests.rs"]
mod tests;
