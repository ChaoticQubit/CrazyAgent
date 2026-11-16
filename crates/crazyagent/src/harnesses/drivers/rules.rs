use super::super::launch::rate_limited;
use super::super::manifest::{self, Fact, Manifest, Refused, Rule};
use crate::adapters::clip_text;
use crate::app_plugins::yaml::Value as Yaml;
use crate::usage::capture::Tokens;
use serde_json::Value;

/// The most rules one rule set holds.
pub const SET_RULES_MAX: usize = 32;
/// The most conditions one rule's `when` holds.
pub const WHEN_MAX: usize = 8;
/// The most bytes of one dotted path a rule reads.
pub const PATH_BYTES_MAX: usize = 128;
/// The most parts of one dotted path, such as `usage.input` with two.
pub const PATH_PARTS_MAX: usize = 8;
/// The most token count paths of one rule: input, output, cache read and cache write.
pub const TOKEN_PATHS_MAX: usize = 4;

const RULE_KEYS: [&str; 8] = [
    "when",
    "emit",
    "text",
    "name",
    "session",
    "success",
    "tokens",
    "rate_limited",
];
const FACTS: [(&str, Fact); 10] = [
    ("ready", Fact::Ready),
    ("turn_started", Fact::TurnStarted),
    ("text", Fact::Text),
    ("tool_use", Fact::ToolUse),
    ("waiting", Fact::Waiting),
    ("interrupted", Fact::Interrupted),
    ("turn_finished", Fact::TurnFinished),
    ("turn_error", Fact::TurnError),
    ("usage", Fact::Usage),
    ("exited", Fact::Exited),
];

/// What one rule found in one JSON object: its fact and the details its paths read. A
/// detail is none when the rule names no path for it or the path holds no value of the
/// right type. Text and tool names are clipped to `FIELD_BYTES_MAX`; the session id is kept
/// as written for its driver to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    pub fact: Fact,
    pub text: Option<String>,
    pub name: Option<String>,
    pub session: Option<String>,
    pub success: Option<bool>,
    pub tokens: Option<Tokens>,
    pub rate_limited: Option<bool>,
}

/// Reads one named rule set from a manifest's top-level `rules` block, in file order.
/// Inputs: the manifest and the set's name, as its `piped.rules` or `record.rules` names it.
/// Output: the rules, or a refusal that names the setting: a missing set, a key a rule does
/// not have, a fact the engine does not report, a path that is not 1 to `PATH_PARTS_MAX`
/// plain parts, or a `text`, `name` or `tokens` path missing where its fact needs one.
pub fn read(manifest: &Manifest, set: &str) -> Result<Vec<Rule>, Refused> {
    let place = format!("rules.{set}");
    let entries = match manifest.block("rules").and_then(|rules| rules.get(set)) {
        None | Some(Yaml::Null) => {
            return refuse(
                manifest::MISSING,
                format!("{place}: this setting is needed."),
            );
        }
        Some(Yaml::Map(entries)) => entries,
        Some(_) => {
            return refuse(
                manifest::TYPE,
                format!("{place}: this must be a map of rules."),
            );
        }
    };
    if entries.is_empty() || entries.len() > SET_RULES_MAX {
        return refuse(
            manifest::VALUE,
            format!("{place}: a rule set holds 1 to {SET_RULES_MAX} rules."),
        );
    }
    let rules = entries
        .iter()
        .map(|(name, rule)| one(rule, &format!("{place}.{name}")))
        .collect::<Result<Vec<Rule>, Refused>>()?;
    assert!(!rules.is_empty() && rules.len() <= SET_RULES_MAX);
    Ok(rules)
}

/// Finds the first rule, in file order, whose conditions all hold for one JSON object, and
/// reads its details. A condition holds when its path reaches a text equal to the condition,
/// or a number or flag whose JSON text equals it; a part of only digits indexes a list.
/// Inputs: the rules and the object. Output: what the rule found, or none when no rule
/// matches.
pub fn matched(rules: &[Rule], object: &Value) -> Option<Matched> {
    let rule = rules.iter().find(|rule| {
        assert!(!rule.when.is_empty());
        rule.when
            .iter()
            .all(|(path, want)| holds(at(object, path), want))
    })?;
    let text = |path: &Option<String>| {
        path.as_deref()
            .and_then(|path| at(object, path))
            .and_then(Value::as_str)
    };
    let clipped =
        |path: &Option<String>| text(path).filter(|found| !found.is_empty()).map(clip_text);
    let counts: Vec<Option<u64>> = rule
        .tokens
        .iter()
        .map(|path| at(object, path).and_then(Value::as_u64))
        .collect();
    assert!(counts.len() <= TOKEN_PATHS_MAX);
    let count = |index: usize| counts.get(index).copied().flatten().unwrap_or(0);
    let tokens = counts.iter().any(Option::is_some).then(|| Tokens {
        input: count(0),
        output: count(1),
        cache_read: count(2),
        cache_write: count(3),
    });
    let limited = rule
        .rate_limited
        .as_deref()
        .and_then(|path| at(object, path))
        .and_then(|found| match found {
            Value::Bool(flag) => Some(*flag),
            Value::String(message) => Some(rate_limited(message)),
            _ => None,
        });
    Some(Matched {
        fact: rule.emit,
        text: clipped(&rule.text),
        name: clipped(&rule.name),
        session: text(&rule.session).map(str::to_owned),
        success: rule
            .success
            .as_deref()
            .and_then(|path| at(object, path))
            .and_then(Value::as_bool),
        tokens,
        rate_limited: limited,
    })
}

fn one(rule: &Yaml, place: &str) -> Result<Rule, Refused> {
    let Yaml::Map(keys) = rule else {
        return refuse(manifest::TYPE, format!("{place}: this must be a map."));
    };
    if let Some((key, _)) = keys
        .iter()
        .find(|(key, _)| !RULE_KEYS.contains(&key.as_str()))
    {
        return refuse(
            manifest::UNKNOWN_KEY,
            format!("{place}.{key}: this key is not one a rule has."),
        );
    }
    let when = match rule.get("when") {
        None | Some(Yaml::Null) => {
            return refuse(
                manifest::MISSING,
                format!("{place}.when: this setting is needed."),
            );
        }
        Some(Yaml::Map(conditions)) => conditions,
        Some(_) => {
            return refuse(manifest::TYPE, format!("{place}.when: this must be a map."));
        }
    };
    if when.is_empty() || when.len() > WHEN_MAX {
        return refuse(
            manifest::VALUE,
            format!("{place}.when: a rule holds 1 to {WHEN_MAX} conditions."),
        );
    }
    let mut conditions = Vec::with_capacity(when.len());
    for (path, want) in when {
        let at = format!("{place}.when.{path}");
        checked_path(path, &at)?;
        let Yaml::Str(want) = want else {
            return refuse(
                manifest::TYPE,
                format!("{at}: this must be text in quotes."),
            );
        };
        conditions.push((path.clone(), want.clone()));
    }
    let emit = match rule.get("emit") {
        None | Some(Yaml::Null) => {
            return refuse(
                manifest::MISSING,
                format!("{place}.emit: this setting is needed."),
            );
        }
        Some(Yaml::Str(name)) => FACTS
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, fact)| *fact)
            .map_or_else(
                || {
                    refuse(
                        manifest::KIND,
                        format!("{place}.emit: CrazyAgent has no fact named {name}."),
                    )
                },
                Ok,
            )?,
        Some(_) => {
            return refuse(
                manifest::TYPE,
                format!("{place}.emit: this must be text in quotes."),
            );
        }
    };
    let path = |key: &str| path_of(rule, key, &format!("{place}.{key}"));
    let tokens = match rule.get("tokens") {
        None | Some(Yaml::Null) => Vec::new(),
        Some(Yaml::List(paths)) if !paths.is_empty() && paths.len() <= TOKEN_PATHS_MAX => paths
            .iter()
            .map(|found| match found {
                Yaml::Str(path) => {
                    checked_path(path, &format!("{place}.tokens")).map(|()| path.clone())
                }
                _ => refuse(
                    manifest::TYPE,
                    format!("{place}.tokens: this must be a list of texts."),
                ),
            })
            .collect::<Result<Vec<String>, Refused>>()?,
        Some(Yaml::List(_)) => {
            return refuse(
                manifest::VALUE,
                format!("{place}.tokens: a rule reads 1 to {TOKEN_PATHS_MAX} token counts."),
            );
        }
        Some(_) => {
            return refuse(
                manifest::TYPE,
                format!("{place}.tokens: this must be a list of texts."),
            );
        }
    };
    let found = Rule {
        when: conditions,
        emit,
        text: path("text")?,
        name: path("name")?,
        session: path("session")?,
        success: path("success")?,
        tokens,
        rate_limited: path("rate_limited")?,
    };
    let needs = match found.emit {
        Fact::Text if found.text.is_none() => Some("text"),
        Fact::ToolUse if found.name.is_none() => Some("name"),
        Fact::Usage if found.tokens.is_empty() => Some("tokens"),
        _ => None,
    };
    if let Some(key) = needs {
        return refuse(
            manifest::MISSING,
            format!("{place}.{key}: this setting is needed for this fact."),
        );
    }
    Ok(found)
}

pub(super) fn path_of(rule: &Yaml, key: &str, place: &str) -> Result<Option<String>, Refused> {
    match rule.get(key) {
        None | Some(Yaml::Null) => Ok(None),
        Some(Yaml::Str(path)) => checked_path(path, place).map(|()| Some(path.clone())),
        Some(_) => refuse(
            manifest::TYPE,
            format!("{place}: this must be text in quotes."),
        ),
    }
}

fn checked_path(path: &str, place: &str) -> Result<(), Refused> {
    let plain = !path.is_empty()
        && path.len() <= PATH_BYTES_MAX
        && path.split('.').count() <= PATH_PARTS_MAX
        && path.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
    if plain {
        Ok(())
    } else {
        refuse(
            manifest::VALUE,
            format!(
                "{place}: a path is 1 to {PATH_PARTS_MAX} parts of letters, digits, _ or -, joined by dots."
            ),
        )
    }
}

pub(super) fn at<'a>(object: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(object, |found, part| match found {
        Value::Object(members) => members.get(part),
        Value::Array(items) if part.bytes().all(|byte| byte.is_ascii_digit()) => part
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get(index)),
        _ => None,
    })
}

fn holds(found: Option<&Value>, want: &str) -> bool {
    match found {
        Some(Value::String(text)) => text == want,
        Some(Value::Bool(flag)) => want == if *flag { "true" } else { "false" },
        Some(Value::Number(number)) => number.to_string() == want,
        _ => false,
    }
}

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
