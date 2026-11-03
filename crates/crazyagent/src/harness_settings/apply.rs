use super::{Apply, Catalog, Resolved, Source, catalog, installed, store};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::path::Path;

/// The owner's settings one harness start applies: only the unlocked keys the owner set,
/// as the app value or the project override, and those whose default the engine picks over
/// the harness's own, in catalog order, and the locked keys whose engine value the catalog
/// names, which every launch must already use. A locked key is never among the owner's
/// values, nor is a key left to the harness default, and each launch builder writes its
/// own values over these, last. The default applies nothing, so the harness starts with
/// the engine's values only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    harness: Option<String>,
    values: Vec<Resolved>,
    locked: Vec<Resolved>,
}

impl Applied {
    /// Splits one resolution into the owner's values and the locked keys it ends with,
    /// keeping only the locked keys that name an engine value. Inputs: the catalog and its
    /// `resolve` output. Output: the settings.
    pub fn new(catalog: &Catalog, resolved: Vec<Resolved>) -> Self {
        let (locked, values): (Vec<Resolved>, Vec<Resolved>) = resolved
            .into_iter()
            .partition(|value| value.source == Source::Engine);
        assert!(values.iter().all(|value| !value.value.is_null()));
        assert!(values.iter().all(|value| {
            catalog.key(&value.key).is_some_and(|key| {
                (matches!(value.source, Source::App | Source::Project)
                    || (value.source == Source::Default && key.harness_default.is_some()))
                    && key.locked.is_none()
                    && key.apply == value.apply
                    && super::applied_at_launch(key.apply)
            })
        }));
        assert!(locked.iter().all(|value| {
            catalog.key(&value.key).is_some_and(|key| {
                key.apply == value.apply
                    && key
                        .locked
                        .as_ref()
                        .is_some_and(|lock| lock.engine_value == value.value)
            })
        }));
        let locked: Vec<Resolved> = locked
            .into_iter()
            .filter(|value| !value.value.is_null())
            .collect();
        assert!(
            locked
                .iter()
                .all(|value| super::applied_at_launch(value.apply))
        );
        Self {
            harness: Some(catalog.harness.clone()),
            values,
            locked,
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

    fn locked(&self, apply: Apply) -> impl Iterator<Item = &Resolved> {
        self.locked.iter().filter(move |value| value.apply == apply)
    }
}

/// Reads the settings one harness start applies, for an agent or a master turn: the
/// shipped catalog of the harness, the version its executable reports and the owner's app
/// and project values. A harness without a catalog applies nothing. Inputs: the engine
/// database, the harness id or connection name, its executable and the project, none for
/// an app-level start. Output: the settings, or a `HARNESS_SETTINGS_CODES` refusal.
pub fn for_launch(
    db: &Connection,
    harness: &str,
    executable: &Path,
    project: Option<&ProjectId>,
) -> Result<Applied, EngineError> {
    let Some(id) = crate::harnesses::spec(harness).map(|spec| spec.id) else {
        return Ok(Applied::default());
    };
    let Some(catalog) = catalog::builtin(id)? else {
        return Ok(Applied::default());
    };
    for_catalog(db, &catalog, executable, project)
}

/// Reads the settings one start of a known catalog applies, asking the executable for its
/// version once per file (`installed::version_cached`). Inputs: the engine database, the
/// catalog, the executable and the project. Output: the settings, or a
/// `HARNESS_SETTINGS_CODES` refusal.
pub fn for_catalog(
    db: &Connection,
    catalog: &Catalog,
    executable: &Path,
    project: Option<&ProjectId>,
) -> Result<Applied, EngineError> {
    let version = installed::version_cached(executable, installed::LIMIT);
    resolved(db, catalog, version.as_deref(), project)
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

/// Merges the owner's values into Claude Code's `--settings` JSON the engine built: each
/// dotted key becomes nested objects, a key whose first part the engine sets or reserves
/// is dropped, and every engine key is written last, so it keeps the engine's value. Every
/// locked key that names an engine value must then hold it. Inputs: the engine's object,
/// the settings, which keys apply here and the top-level keys the engine sets on other
/// launches. Output: the merged object.
pub fn json_settings(engine: &Value, applied: &Applied, apply: Apply, reserved: &[&str]) -> Value {
    assert_eq!(apply, Apply::ClaudeSettings);
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
    let merged = Value::Object(merged);
    for lock in applied.locked(apply) {
        assert_eq!(at(&merged, &lock.key), Some(&lock.value), "{}", lock.key);
    }
    merged
}

/// Finds the value at one dotted key of a JSON object. Inputs: the object and the key.
/// Output: the value, none when a part is missing.
pub fn at<'a>(document: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.')
        .try_fold(document, |value, part| value.get(part))
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
/// and merging them over the engine's object must give the same object. A value equal to
/// a default the engine picks over the harness's own reads back as that default. The
/// catalog's locked keys come with them, as a launch resolves them. Inputs: the shipped
/// catalog, none when the harness has none, the engine's object, the apply, the reserved
/// keys and the JSON text. Output: the settings, or none for any other text.
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
            let owner = catalog
                .keys
                .iter()
                .filter(|key| key.locked.is_none() && key.apply == apply)
                .filter_map(|key| {
                    let value = at(&parsed, &key.key)?;
                    super::check_value(key, value).ok()?;
                    let chosen = key.harness_default.is_some() && *value == key.default;
                    Some(Resolved {
                        key: key.key.clone(),
                        value: value.clone(),
                        source: if chosen { Source::Default } else { Source::App },
                        apply,
                    })
                });
            let locked = catalog.keys.iter().filter_map(|key| {
                let lock = key.locked.as_ref().filter(|_| key.apply == apply)?;
                Some(Resolved {
                    key: key.key.clone(),
                    value: lock.engine_value.clone(),
                    source: Source::Engine,
                    apply,
                })
            });
            Applied::new(catalog, owner.chain(locked).collect())
        }
    };
    (json_settings(engine, &applied, apply, reserved) == parsed).then_some(applied)
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
/// Every locked key that names an engine value must be one of the engine's settings with
/// that value. Inputs: the engine's settings, the keys the engine sets on other launches
/// and the owner's settings. Output: the owner's settings.
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
    for lock in applied.locked(Apply::CodexConfig) {
        let set = engine
            .iter()
            .rev()
            .find(|line| line_key(line) == lock.key)
            .and_then(|line| line.split_once('='))
            .map(|(_, value)| value.trim());
        assert_eq!(set, Some(lock.value.to_string().as_str()), "{}", lock.key);
    }
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
