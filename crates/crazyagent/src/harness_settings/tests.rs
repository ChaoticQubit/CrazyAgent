use super::*;
use serde_json::{Value, json};
#[cfg(unix)]
use std::time::Duration;

const CODEX: &str = include_str!("../../tests/fixtures/harness-settings/codex.json");
const CLAUDE_CODE: &str = include_str!("../../tests/fixtures/harness-settings/claude-code.json");
const RCLI: &str = include_str!("../../tests/fixtures/harness-settings/rcli.json");

/// Reads one fixture catalog the HS-2 and HS-3 tests share, as its JSON text. Input: the
/// harness id, `codex`, `claude-code` or `rcli`. Output: the text.
pub(crate) fn fixture_text(harness: &str) -> &'static str {
    match harness {
        "codex" => CODEX,
        "claude-code" => CLAUDE_CODE,
        "rcli" => RCLI,
        other => panic!("no fixture catalog for {other}"),
    }
}

/// Parses one fixture catalog. Input: the harness id, as for `fixture_text`. Output: the
/// catalog; a fixture that breaks the contract fails the calling test.
pub(crate) fn fixture(harness: &str) -> Catalog {
    catalog::parse(fixture_text(harness).as_bytes()).unwrap()
}

/// Reads the fixture Codex catalog as JSON, so a test can build a later catalog from it.
/// Output: the document.
pub(crate) fn fixture_json() -> Value {
    serde_json::from_str(CODEX).unwrap()
}

fn refused(document: &Value) -> String {
    let bytes = serde_json::to_vec(document).unwrap();
    catalog::parse(&bytes).unwrap_err().0
}

#[test]
fn the_fixture_catalog_parses_in_its_own_order() {
    let catalog = fixture("codex");
    assert_eq!(catalog.harness, "codex");
    assert_eq!(catalog.qualified_versions, ["0.50.0", "0.51.0"]);
    let keys: Vec<&str> = catalog.keys.iter().map(|key| key.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "hide_agent_reasoning",
            "model_reasoning_effort",
            "project_doc_max_bytes",
            "tui.status_label",
            "sandbox_mode",
            "approval_policy"
        ]
    );
    let sandbox = catalog.key("sandbox_mode").unwrap();
    let locked = sandbox.locked.as_ref().unwrap();
    assert_eq!(
        locked.reason,
        "CrazyAgent runs every agent in its own sandbox."
    );
    assert_eq!(locked.engine_value, Value::Null);
    let docs = catalog.key("project_doc_max_bytes").unwrap();
    assert_eq!(docs.locked.as_ref().unwrap().engine_value, json!(0));
    assert!(qualified(&catalog, Some("0.51.0")));
    assert!(!qualified(&catalog, Some("0.50")));
    assert!(!qualified(&catalog, None));
}

/// One way to break the fixture Codex catalog: an edit made in place to its JSON document.
type CatalogEdit = Box<dyn Fn(&mut Value)>;

#[test]
fn a_catalog_that_breaks_the_contract_is_refused() {
    let edits: Vec<(&str, CatalogEdit)> = vec![
        (
            "no versions",
            Box::new(|c| c["qualified_versions"] = json!([])),
        ),
        ("no sources", Box::new(|c| c["sources"] = json!([]))),
        (
            "unknown harness",
            Box::new(|c| c["harness"] = json!("nope")),
        ),
        ("unknown field", Box::new(|c| c["extra"] = json!(1))),
        (
            "unknown key field",
            Box::new(|c| c["keys"][0]["secret"] = json!("x")),
        ),
        (
            "duplicate key",
            Box::new(|c| c["keys"][1]["key"] = json!("hide_agent_reasoning")),
        ),
        (
            "spaced key",
            Box::new(|c| c["keys"][0]["key"] = json!("hide agent")),
        ),
        ("empty key", Box::new(|c| c["keys"][0]["key"] = json!(""))),
        (
            "long key",
            Box::new(|c| c["keys"][0]["key"] = json!("k".repeat(KEY_BYTES_MAX + 1))),
        ),
        (
            "bool default is text",
            Box::new(|c| c["keys"][0]["default"] = json!("false")),
        ),
        (
            "enum default not an option",
            Box::new(|c| c["keys"][1]["default"] = json!("extreme")),
        ),
        (
            "enum without options",
            Box::new(|c| c["keys"][1]["options"] = json!([])),
        ),
        (
            "integer without max",
            Box::new(|c| {
                c["keys"][2].as_object_mut().unwrap().remove("max");
            }),
        ),
        (
            "integer min over max",
            Box::new(|c| c["keys"][2]["min"] = json!(2_000_000)),
        ),
        (
            "integer default out of range",
            Box::new(|c| c["keys"][2]["default"] = json!(-1)),
        ),
        (
            "string without max_bytes",
            Box::new(|c| {
                c["keys"][3].as_object_mut().unwrap().remove("max_bytes");
            }),
        ),
        (
            "string max_bytes over the bound",
            Box::new(|c| c["keys"][3]["max_bytes"] = json!(STRING_BYTES_MAX + 1)),
        ),
        (
            "bool with options",
            Box::new(|c| c["keys"][0]["options"] = json!(["a"])),
        ),
        (
            "hidden help",
            Box::new(|c| c["keys"][0]["help"] = json!("help\u{202e}text")),
        ),
        (
            "empty label",
            Box::new(|c| c["keys"][0]["label"] = json!("")),
        ),
        (
            "locked without a reason",
            Box::new(|c| c["keys"][4]["locked"]["reason"] = json!("  ")),
        ),
        (
            "locked engine value of the wrong type",
            Box::new(|c| c["keys"][4]["locked"]["engine_value"] = json!(7)),
        ),
        (
            "apply of another harness",
            Box::new(|c| c["keys"][0]["apply"] = json!("claude_settings")),
        ),
        (
            "an engine value on a key no launch writes",
            Box::new(|c| {
                c["keys"][0]["apply"] = json!("arg");
                c["keys"][0]["locked"] = json!({"reason": "Fixture.", "engine_value": false});
            }),
        ),
        (
            "a key inside another key",
            Box::new(|c| c["keys"][3]["key"] = json!("hide_agent_reasoning.label")),
        ),
        (
            "a harness default equal to the default",
            Box::new(|c| c["keys"][0]["harness_default"] = json!(false)),
        ),
        (
            "a harness default of the wrong type",
            Box::new(|c| c["keys"][0]["harness_default"] = json!("true")),
        ),
        (
            "a harness default outside the options",
            Box::new(|c| c["keys"][1]["harness_default"] = json!("extreme")),
        ),
        (
            "a harness default on a locked key",
            Box::new(|c| c["keys"][4]["harness_default"] = json!("workspace-write")),
        ),
        (
            "a harness default on a key no launch writes",
            Box::new(|c| {
                c["keys"][0]["apply"] = json!("arg");
                c["keys"][0]["harness_default"] = json!(true);
            }),
        ),
        (
            "a null default on a locked key",
            Box::new(|c| c["keys"][4]["default"] = Value::Null),
        ),
        (
            "a null default beside a harness default",
            Box::new(|c| {
                c["keys"][1]["default"] = Value::Null;
                c["keys"][1]["harness_default"] = json!("high");
            }),
        ),
        (
            "too many keys",
            Box::new(|c| {
                let key = c["keys"][0].clone();
                let keys: Vec<Value> = (0..=KEYS_MAX)
                    .map(|n| {
                        let mut key = key.clone();
                        key["key"] = json!(format!("k{n}"));
                        key
                    })
                    .collect();
                c["keys"] = Value::Array(keys);
            }),
        ),
    ];
    for (case, edit) in edits {
        let mut document = fixture_json();
        edit(&mut document);
        assert_eq!(
            refused(&document),
            "harness_settings_catalog_invalid",
            "{case}"
        );
    }
    let mut unlocked = fixture_json();
    unlocked["keys"][4]["locked"]["engine_value"] = Value::Null;
    assert!(catalog::parse(&serde_json::to_vec(&unlocked).unwrap()).is_ok());
    let mut chosen = fixture_json();
    chosen["keys"][0]["harness_default"] = json!(true);
    let parsed = catalog::parse(&serde_json::to_vec(&chosen).unwrap()).unwrap();
    assert_eq!(parsed.keys[0].harness_default, Some(json!(true)));
    assert_eq!(parsed.keys[1].harness_default, None);
    let written = serde_json::to_value(&parsed.keys[1]).unwrap();
    assert!(written.get("harness_default").is_none());
    assert_eq!(
        catalog::parse(&vec![b' '; CATALOG_BYTES_MAX + 1])
            .unwrap_err()
            .0,
        "harness_settings_catalog_invalid"
    );
}

#[test]
fn every_built_in_catalog_parses_for_its_own_harness() {
    for (harness, _) in catalog::BUILTIN {
        let parsed = catalog::builtin(harness).unwrap().unwrap();
        assert_eq!(parsed.harness, *harness);
        let reserved: &[&str] = match *harness {
            "codex" => &crate::harnesses::launch::CODEX_ENGINE_KEYS,
            "claude-code" => &crate::harnesses::launch::CLAUDE_ENGINE_KEYS,
            _ => &[],
        };
        for key in parsed.keys.iter().filter(|key| key.locked.is_none()) {
            let taken = reserved.iter().any(|name| {
                key.key == *name
                    || key.key.starts_with(&format!("{name}."))
                    || name.starts_with(&format!("{}.", key.key))
            });
            assert!(!taken, "{harness} lists {} unlocked", key.key);
        }
    }
    assert!(catalog::builtin("codex-not-a-harness").unwrap().is_none());
}

/// Words in a key name that mean it can hold a credential: a key, token, secret, password,
/// sign-in or auth value, a cookie or header, or a helper command that prints one.
const CREDENTIAL_WORDS: [&str; 18] = [
    "apikey",
    "auth",
    "bearer",
    "cookie",
    "cookies",
    "credential",
    "credentials",
    "header",
    "headers",
    "helper",
    "key",
    "login",
    "oauth",
    "passwd",
    "password",
    "secret",
    "secrets",
    "token",
];

/// Words in a key name that mean it shapes the agent's environment variables.
const ENVIRONMENT_WORDS: [&str; 2] = ["env", "environment"];

/// Splits a dotted key into its lowercase words, at `.`, `_`, `-` and each camelCase hump.
/// Input: the key. Output: the words in order.
fn name_words(key: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in key.split(['.', '_', '-']) {
        let mut word = String::new();
        let mut previous: Option<char> = None;
        for ch in part.chars() {
            let hump = ch.is_ascii_uppercase()
                && previous
                    .is_some_and(|before| before.is_ascii_lowercase() || before.is_ascii_digit());
            if hump && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(ch.to_ascii_lowercase());
            previous = Some(ch);
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

fn credential_named(key: &str) -> bool {
    name_words(key)
        .iter()
        .any(|word| CREDENTIAL_WORDS.contains(&word.as_str()))
}

fn environment_named(key: &str) -> bool {
    name_words(key)
        .iter()
        .any(|word| ENVIRONMENT_WORDS.contains(&word.as_str()))
}

/// Tells whether two dotted keys name the same setting or one sits inside the other.
fn overlap(one: &str, other: &str) -> bool {
    let inside = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('.'))
    };
    one == other || inside(one, other) || inside(other, one)
}

/// Lists the settings one Claude Code `--settings` object sets, as dotted key and value:
/// each object is walked to its scalar and list values. Input: the object. Output: the
/// settings, in key order.
pub(crate) fn json_leaves(document: &Value) -> Vec<(String, Value)> {
    fn walk(prefix: &str, value: &Value, out: &mut Vec<(String, Value)>) {
        match value.as_object() {
            Some(fields) if !fields.is_empty() => {
                for (name, inner) in fields {
                    let key = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}.{name}")
                    };
                    walk(&key, inner, out);
                }
            }
            _ => out.push((prefix.to_owned(), value.clone())),
        }
    }
    assert!(document.is_object());
    let mut out = Vec::new();
    walk("", document, &mut out);
    assert!(out.iter().all(|(key, _)| !key.is_empty()));
    out
}

/// Lists the settings Codex `-c` lines set, as dotted key and value: a line's key is the
/// text before its first `=`, and its value is read as TOML. Input: the lines. Output: the
/// settings, in line order.
pub(crate) fn codex_settings<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for line in lines {
        let (key, _) = line.split_once('=').expect("a Codex setting is key=value");
        let key = key.trim();
        let document = serde_json::to_value(toml::from_str::<toml::Table>(line).unwrap()).unwrap();
        let value = apply::at(&document, key)
            .cloned()
            .unwrap_or_else(|| panic!("{line} sets {key}"));
        out.push((key.to_owned(), value));
    }
    out
}

/// Lists the top-level settings of one `config.toml` the engine wrote. Input: the file
/// text. Output: the settings, in key order.
#[cfg(target_os = "macos")]
pub(crate) fn codex_file_settings(text: &str) -> Vec<(String, Value)> {
    let table = toml::from_str::<toml::Table>(text).unwrap();
    table
        .into_iter()
        .map(|(key, value)| (key, serde_json::to_value(value).unwrap()))
        .collect()
}

/// Asserts that every setting one engine launch passes is locked in the shipped catalog of
/// its harness, or is an open key whose default the engine picks over the harness's own,
/// set to that default: every catalog key on, inside or around it is locked with a reason; a
/// scalar value has at least one such key, and a lock that names an engine value names this
/// one; a table or list sits under a key the engine reserves, since no catalog type can hold
/// one. Inputs: the harness and the launch's settings as dotted key and value, with no owner
/// value among them. A failed check panics naming the key.
pub(crate) fn assert_locked_in_shipped(harness: &str, settings: &[(String, Value)]) {
    let shipped = catalog::builtin(harness)
        .unwrap()
        .unwrap_or_else(|| panic!("{harness} ships no catalog"));
    let reserved: &[&str] = match harness {
        "codex" => &crate::harnesses::launch::CODEX_ENGINE_KEYS,
        "claude-code" => &crate::harnesses::launch::CLAUDE_ENGINE_KEYS,
        other => panic!("no reserved keys for {other}"),
    };
    assert!(!settings.is_empty());
    for (name, value) in settings {
        if let Some(key) = shipped
            .key(name)
            .filter(|key| key.locked.is_none() && key.harness_default.is_some())
        {
            assert_eq!(
                *value, key.default,
                "{harness}: the engine starts {name} at another value than its catalog default"
            );
            continue;
        }
        let near: Vec<&Key> = shipped
            .keys
            .iter()
            .filter(|key| overlap(&key.key, name))
            .collect();
        for key in &near {
            let lock = key.locked.as_ref();
            assert!(
                lock.is_some_and(|lock| !lock.reason.trim().is_empty()),
                "{harness}: the engine sets {name}, and {} is not locked with a reason",
                key.key
            );
        }
        if value.is_object() || value.is_array() {
            let kept = reserved
                .iter()
                .any(|key| name == key || name.starts_with(&format!("{key}.")));
            assert!(
                kept,
                "{harness}: the engine sets the table {name} without reserving it"
            );
            continue;
        }
        assert!(
            !near.is_empty(),
            "{harness}: the engine sets {name} and the catalog does not lock it"
        );
        let named = shipped.key(name).and_then(|key| key.locked.as_ref());
        assert!(
            named.is_none_or(|lock| lock.engine_value.is_null() || lock.engine_value == *value),
            "{harness}: {name} is locked to another value than the launch sets"
        );
    }
}

#[test]
fn the_launch_lock_check_trips_on_an_unlocked_or_missing_key() {
    let codex = codex_settings([
        "approval_policy=\"never\"",
        "projects={\"/x\"={trust_level=\"trusted\"}}",
    ]);
    assert_eq!(codex[0], ("approval_policy".to_owned(), json!("never")));
    assert!(codex[1].1.is_object());
    let claude =
        json_leaves(&json!({"attribution": {"commit": "", "pr": ""}, "disableAllHooks": true}));
    let keys: Vec<&str> = claude.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        ["attribution.commit", "attribution.pr", "disableAllHooks"]
    );
    assert_locked_in_shipped("codex", &codex);
    assert_locked_in_shipped("claude-code", &claude);
    assert_locked_in_shipped(
        "claude-code",
        &[("autoMemoryEnabled".to_owned(), json!(false))],
    );
    for (harness, settings) in [
        (
            "claude-code",
            vec![("autoMemoryEnabled".to_owned(), json!(true))],
        ),
        (
            "codex",
            vec![("not_a_codex_setting".to_owned(), json!(true))],
        ),
        (
            "codex",
            vec![("approval_policy".to_owned(), json!("on-request"))],
        ),
        ("codex", vec![("not_reserved".to_owned(), json!({}))]),
        (
            "claude-code",
            vec![("includeCoAuthoredBy".to_owned(), json!(true))],
        ),
        (
            "claude-code",
            vec![("spinnerTipsEnabled".to_owned(), json!(false))],
        ),
    ] {
        let tripped = std::panic::catch_unwind(|| assert_locked_in_shipped(harness, &settings));
        assert!(tripped.is_err(), "{harness} {settings:?}");
    }
}

#[test]
fn every_catalog_parses_and_names_its_sources_and_versions() {
    let shipped: Vec<&str> = catalog::BUILTIN
        .iter()
        .map(|(harness, _)| *harness)
        .collect();
    assert_eq!(shipped, ["claude-code", "codex", "rcli"]);
    let hosts = [
        "https://code.claude.com/docs/en/",
        "https://developers.openai.com/codex/",
        "https://github.com/openai/codex/blob/rust-v0.164.0/",
    ];
    for (harness, version, references) in [
        (
            "claude-code",
            "2.1.309",
            &["https://code.claude.com/docs/en/settings-reference"][..],
        ),
        (
            "codex",
            "0.164.0",
            &[
                "https://developers.openai.com/codex/config-reference",
                "https://github.com/openai/codex/blob/rust-v0.164.0/",
            ][..],
        ),
    ] {
        let parsed = catalog::builtin(harness).unwrap().unwrap();
        assert_eq!(parsed.harness, harness);
        assert_eq!(parsed.qualified_versions, [version]);
        assert!(qualified(&parsed, Some(version)));
        assert!(installed::version_valid(version));
        for reference in references {
            assert!(
                parsed
                    .sources
                    .iter()
                    .any(|source| source.starts_with(reference)),
                "{harness} names no {reference} source"
            );
        }
        assert!(
            parsed
                .sources
                .iter()
                .all(|source| hosts.iter().any(|host| source.starts_with(host))),
            "{harness} names a source outside the published references"
        );
        let open = parsed
            .keys
            .iter()
            .filter(|key| key.locked.is_none())
            .count();
        assert!(
            open > 0 && open < parsed.keys.len(),
            "{harness}: {open} open keys"
        );
        for key in &parsed.keys {
            assert!(applied_at_launch(key.apply), "{harness} {}", key.key);
            let unset =
                key.default.is_null() && key.locked.is_none() && key.harness_default.is_none();
            assert!(
                unset || check_value(key, &key.default).is_ok(),
                "{harness} {}",
                key.key
            );
            assert!(
                key.locked
                    .as_ref()
                    .is_none_or(|lock| !lock.reason.trim().is_empty()),
                "{harness} {}",
                key.key
            );
        }
    }
    let rcli = catalog::builtin("rcli").unwrap().unwrap();
    assert_eq!(rcli.qualified_versions, ["3.0.4"]);
    assert!(installed::version_valid("3.0.4"));
    assert!(
        rcli.sources
            .iter()
            .all(|source| source.starts_with(RCLI_SOURCE))
    );
    let defaults: Vec<(&str, Value)> = rcli
        .keys
        .iter()
        .map(|key| (key.key.as_str(), key.default.clone()))
        .collect();
    assert_eq!(
        defaults,
        [
            ("model.provider", json!("")),
            ("model.model", json!("")),
            ("thinking.effort", Value::Null),
            ("memory", json!("auto")),
            ("memoryProfile", json!("code")),
            ("notify", json!(false)),
            ("verbose", json!(true)),
            ("timerSeconds", Value::Null),
        ]
    );
    let empty = Stored::new();
    for key in &rcli.keys {
        assert_eq!(key.apply, Apply::RcliConfig, "{}", key.key);
        assert!(!applied_at_launch(key.apply));
        let locked = key.key.starts_with("model.");
        assert_eq!(
            key.locked.as_ref().map(|lock| &lock.engine_value),
            locked.then_some(&Value::Null),
            "{}",
            key.key
        );
        let row = setting(&rcli, None, &Scope::App, &key.key, &empty, &empty, 0);
        let access = if locked {
            assert_eq!(row.source, Source::Engine);
            Access::Locked {
                reason: key.locked.as_ref().unwrap().reason.clone(),
            }
        } else {
            assert_eq!(row.source, Source::NotApplied);
            Access::ReadOnly {
                reason: NOT_APPLIED.into(),
            }
        };
        assert_eq!(row.access, access, "{}", key.key);
    }
    for absent in ["apiKey", "apiUrl", "providers", "expert.enabled"] {
        assert!(rcli.key(absent).is_none(), "rcli lists {absent}");
    }
    let applied = resolve(&rcli, Some("3.0.4"), &empty, Some(&empty));
    assert_eq!(applied.len(), 2);
    assert!(
        applied
            .iter()
            .all(|value| value.source == Source::Engine && value.value.is_null())
    );
}

/// Where the rcli catalog's keys were read: the config sources of the upstream revision
/// `scripts/rcli/source-lock.json` pins, which the installed host is built from.
const RCLI_SOURCE: &str = "https://github.com/Backboard-io/Backboard-R-CLI/blob/51bee17e02344de5fffd89523ce2b73d5042f216/src/config/";

/// The keys of the shipped catalogs whose harness default is only "unset": each is listed
/// unlocked with a null default, so the harness decides until the owner sets a value.
const UNSET: [(&str, &[&str]); 2] = [
    (
        "codex",
        &[
            "model_context_window",
            "model_auto_compact_token_limit",
            "model_post_turn_compact_threshold_percent",
            "analytics.enabled",
            "personality",
            "service_tier",
            "skills.max_context_tokens",
            "features.context_management.experimental_mode",
            "features.rollout_budget.limit_tokens",
            "memories.version",
            "memories.dual_write",
            "goals.max_goal_token_budget",
            "model_reasoning_effort",
            "plan_mode_reasoning_effort",
            "model_reasoning_summary",
            "model_verbosity",
            "tool_output_token_limit",
            "history.max_bytes",
            "tui.resume_cwd",
        ],
    ),
    (
        "claude-code",
        &[
            "effortLevel",
            "maxEffortLevel",
            "promptCacheTtl",
            "subagentPromptCacheTtl",
            "bashEditDiffEnabled",
            "keybindingFlavor",
            "tui",
            "viewMode",
            "voiceEnabled",
            "disableArtifact",
            "disableDeepLinkRegistration",
            "enableArtifact",
        ],
    ),
];

#[test]
fn keys_whose_harness_default_is_unset_are_listed_with_a_null_default() {
    for (harness, names) in UNSET {
        let shipped = catalog::builtin(harness).unwrap().unwrap();
        for name in names {
            assert!(
                shipped.key(name).is_some_and(|key| key.default.is_null()
                    && key.locked.is_none()
                    && key.harness_default.is_none()),
                "{harness} {name}"
            );
        }
        let unset = shipped
            .keys
            .iter()
            .filter(|key| key.default.is_null())
            .count();
        assert_eq!(unset, names.len(), "{harness}");
        let version = shipped.qualified_versions.first().map(String::as_str);
        let empty = Stored::new();
        let applied = resolve(&shipped, version, &empty, Some(&empty));
        assert!(
            applied
                .iter()
                .all(|value| !names.contains(&value.key.as_str()))
        );
        for name in names {
            let row = setting(&shipped, version, &Scope::App, name, &empty, &empty, 0);
            assert_eq!(
                (&row.effective, row.source, &row.access),
                (&Value::Null, Source::Default, &Access::Editable),
                "{harness} {name}"
            );
        }
    }
    for (harness, name) in [
        ("codex", "compact_prompt"),
        ("codex", "tui.theme"),
        ("codex", "features.rollout_budget.sampling_token_weight"),
        ("codex", "features.rollout_budget.prefill_token_weight"),
        ("claude-code", "language"),
        ("claude-code", "outputStyle"),
        ("claude-code", "timeZone"),
        ("claude-code", "prUrlTemplate"),
    ] {
        let shipped = catalog::builtin(harness).unwrap().unwrap();
        assert!(
            shipped.key(name).is_none(),
            "{harness} {name} takes free text or a fraction"
        );
    }
}

#[test]
fn an_unset_default_applies_nothing_until_the_owner_sets_a_value() {
    let mut document = fixture_json();
    document["keys"][1]["default"] = Value::Null;
    let parsed = catalog::parse(&serde_json::to_vec(&document).unwrap());
    assert!(
        parsed.is_ok(),
        "an unlocked key may leave its default to the harness"
    );
    let catalog = parsed.unwrap();
    let key = catalog.key("model_reasoning_effort").unwrap();
    assert_eq!(key.default, Value::Null);
    assert_eq!(
        check_value(key, &Value::Null).unwrap_err().0,
        "harness_settings_value_invalid",
        "null is no value the owner sets; a reset returns to the harness default"
    );
    let version = Some("0.50.0");
    let project = Scope::Project(crate::protocol::ProjectId::new("alpha").unwrap());
    let empty = Stored::new();
    let row = setting(&catalog, version, &Scope::App, &key.key, &empty, &empty, 0);
    assert_eq!(
        (&row.effective, row.source, &row.access),
        (&Value::Null, Source::Default, &Access::Editable)
    );
    let unqualified = setting(&catalog, None, &Scope::App, &key.key, &empty, &empty, 0);
    assert_eq!(
        (&unqualified.effective, unqualified.source),
        (&Value::Null, Source::Default)
    );
    let found = |values: &[Resolved]| {
        values
            .iter()
            .find(|value| value.key == key.key)
            .map(|value| (value.value.clone(), value.source))
    };
    assert_eq!(
        found(&resolve(&catalog, version, &empty, Some(&empty))),
        None
    );
    assert!(
        resolve(&catalog, version, &empty, Some(&empty))
            .iter()
            .all(|value| !value.value.is_null() || value.source == Source::Engine)
    );
    let app = Stored::from([(key.key.clone(), json!("high"))]);
    let here = Stored::from([(key.key.clone(), json!("low"))]);
    assert_eq!(
        found(&resolve(&catalog, version, &app, None)),
        Some((json!("high"), Source::App))
    );
    assert_eq!(
        found(&resolve(&catalog, version, &app, Some(&here))),
        Some((json!("low"), Source::Project))
    );
    assert_eq!(
        found(&resolve(&catalog, version, &empty, Some(&here))),
        Some((json!("low"), Source::Project))
    );
    let overridden = setting(&catalog, version, &project, &key.key, &app, &here, 1);
    assert_eq!(
        (&overridden.effective, overridden.source),
        (&json!("low"), Source::Project)
    );
    let refused = Stored::from([(key.key.clone(), json!("extreme"))]);
    assert_eq!(found(&resolve(&catalog, version, &refused, None)), None);
    let stale = setting(
        &catalog,
        version,
        &Scope::App,
        &key.key,
        &refused,
        &empty,
        1,
    );
    assert_eq!(
        (&stale.here, &stale.effective, stale.source),
        (&Some(json!("extreme")), &Value::Null, Source::Default)
    );
}

#[test]
fn no_catalog_key_can_carry_a_credential_or_environment_secret() {
    assert!(credential_named("apiKeyHelper"));
    assert!(credential_named("cli_auth_credentials_store"));
    assert!(credential_named("otelHeadersHelper"));
    assert!(!credential_named("keybindingFlavor"));
    assert!(!credential_named("spinnerTipsEnabled"));
    assert!(environment_named("shell_environment_policy.inherit"));
    assert!(environment_named(
        "sandbox_workspace_write.exclude_tmpdir_env_var"
    ));
    assert!(!environment_named("tui.alternate_screen"));
    let secret_like = |text: &str| {
        text.contains(crate::model_gateway::relay::LOCAL_PLACEHOLDER)
            || ["sk-", "ghp_", "xox", "Bearer ", "-----BEGIN", "AKIA"]
                .iter()
                .any(|mark| text.contains(mark))
    };
    let mut credential_keys = 0;
    for (harness, _) in catalog::BUILTIN {
        let parsed = catalog::builtin(harness).unwrap().unwrap();
        for key in &parsed.keys {
            let lock = key.locked.as_ref();
            let text = matches!(key.kind, Kind::String);
            if environment_named(&key.key) {
                assert!(
                    lock.is_some_and(|lock| lock.engine_value.is_null()),
                    "{harness}: {} shapes the environment and is not locked unset",
                    key.key
                );
            }
            if credential_named(&key.key) {
                credential_keys += 1;
                assert!(
                    lock.is_some() || !text,
                    "{harness}: {} can hold a credential and is not locked",
                    key.key
                );
                assert!(
                    lock.is_none_or(|lock| lock.engine_value.is_null() || !text),
                    "{harness}: {} is locked to a credential text",
                    key.key
                );
            }
            assert!(
                lock.is_some() || !text,
                "{harness}: {} takes free text, which could carry a secret into argv",
                key.key
            );
            let texts = key
                .options
                .iter()
                .map(String::as_str)
                .chain(key.default.as_str())
                .chain(key.harness_default.as_ref().and_then(Value::as_str))
                .chain(lock.and_then(|lock| lock.engine_value.as_str()));
            for text in texts {
                assert!(!secret_like(text), "{harness}: {} carries {text}", key.key);
            }
        }
    }
    assert!(credential_keys > 0);
}

#[test]
fn a_value_is_checked_against_its_key() {
    let catalog = fixture("codex");
    let key = |name| catalog.key(name).unwrap();
    for (name, value) in [
        ("hide_agent_reasoning", json!(true)),
        ("model_reasoning_effort", json!("minimal")),
        ("project_doc_max_bytes", json!(0)),
        ("project_doc_max_bytes", json!(1_048_576)),
        ("tui.status_label", json!("a".repeat(32))),
        ("tui.status_label", json!("")),
    ] {
        assert!(check_value(key(name), &value).is_ok(), "{name} {value}");
    }
    for (name, value) in [
        ("hide_agent_reasoning", json!("true")),
        ("hide_agent_reasoning", json!(1)),
        ("hide_agent_reasoning", Value::Null),
        ("model_reasoning_effort", json!("Medium")),
        ("model_reasoning_effort", json!(["low"])),
        ("project_doc_max_bytes", json!(-1)),
        ("project_doc_max_bytes", json!(1_048_577)),
        ("project_doc_max_bytes", json!(1.5)),
        ("project_doc_max_bytes", json!("10")),
        ("tui.status_label", json!("a".repeat(33))),
        ("tui.status_label", json!("two\nlines")),
        ("tui.status_label", json!("right\u{202e}left")),
        ("tui.status_label", json!({"text": "x"})),
    ] {
        assert_eq!(
            check_value(key(name), &value).unwrap_err().0,
            "harness_settings_value_invalid",
            "{name} {value}"
        );
    }
}

#[test]
fn catalog_pages_fit_one_frame_and_list_every_key_once() {
    let mut document = fixture_json();
    let template = document["keys"][1].clone();
    let keys: Vec<Value> = (0..KEYS_MAX)
        .map(|n| {
            let mut key = template.clone();
            key["key"] = json!(format!("key_{n:03}"));
            key["help"] = json!("h".repeat(HELP_BYTES_MAX));
            key["label"] = json!("l".repeat(LABEL_BYTES_MAX));
            key["options"] = json!(
                (0..OPTIONS_MAX)
                    .map(|o| format!("{o:0>width$}", width = OPTION_BYTES_MAX))
                    .collect::<Vec<_>>()
            );
            key["default"] = key["options"][0].clone();
            key
        })
        .collect();
    document["keys"] = Value::Array(keys);
    let catalog = catalog::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = catalog_page(&catalog, Some("0.50.0"), after).unwrap();
        pages += 1;
        assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_BYTES_MAX);
        assert!(!page.keys.is_empty());
        assert!(page.qualified);
        seen.extend(page.keys.iter().map(|key| key.key.clone()));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert!(pages > 1);
    let expected: Vec<String> = catalog.keys.iter().map(|key| key.key.clone()).collect();
    assert_eq!(seen, expected);
    let last = u32::try_from(catalog.keys.len()).unwrap();
    assert!(
        catalog_page(&catalog, None, Some(last))
            .unwrap()
            .keys
            .is_empty()
    );
    assert_eq!(
        catalog_page(&catalog, None, Some(last + 1)).unwrap_err().0,
        "harness_settings_page_invalid"
    );
}

#[test]
fn the_version_is_the_first_digit_led_word_of_the_first_line() {
    for (output, version) in [
        ("2.0.31 (Claude Code)\n", Some("2.0.31")),
        ("codex-cli 0.50.0\n", Some("0.50.0")),
        (
            "tool 1.2.3-beta.1+build\nignored 9.9.9\n",
            Some("1.2.3-beta.1+build"),
        ),
        ("codex-cli\n0.50.0\n", None),
        ("v1.2.3\n", None),
        ("", None),
        ("1.2.3/../x\n", None),
    ] {
        assert_eq!(
            installed::version_from(output.as_bytes()).as_deref(),
            version,
            "{output:?}"
        );
    }
    let long = format!("tool {}\n", "1".repeat(VERSION_BYTES_MAX + 1));
    assert_eq!(installed::version_from(long.as_bytes()), None);
}

#[cfg(unix)]
#[test]
fn a_real_executable_reports_its_version_within_the_limit() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let script = |name: &str, body: &str| {
        let path = tmp.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    };
    let reports = script("reports", "[ \"$1\" = --version ] && echo \"tool 3.4.5\"");
    let slow = script("slow", "sleep 5; echo \"tool 3.4.5\"");
    let fails = script("fails", "echo \"tool 3.4.5\"; exit 3");
    let environment = script(
        "environment",
        "[ -z \"$HOME$CRAZYAGENT_PROBE_SECRET\" ] && echo \"tool 1.0.0\"",
    );
    let limit = Duration::from_secs(20);
    assert_eq!(
        installed::version_of(&reports, limit).as_deref(),
        Some("3.4.5")
    );
    assert_eq!(
        installed::version_of(&environment, limit).as_deref(),
        Some("1.0.0")
    );
    let started = std::time::Instant::now();
    assert_eq!(
        installed::version_of(&slow, Duration::from_millis(300)),
        None
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(installed::version_of(&fails, limit), None);
    assert_eq!(
        installed::version_of(&tmp.path().join("missing"), limit),
        None
    );
}

#[cfg(unix)]
#[test]
fn an_executable_file_is_asked_for_its_version_once() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tool");
    let count = tmp.path().join("tool.count");
    let write = |body: &str| {
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho asked >> \"$0.count\"\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    let asked = || std::fs::read_to_string(&count).unwrap().lines().count();
    let limit = Duration::from_secs(20);
    write("echo \"tool 3.4.5\"");
    for _ in 0..3 {
        assert_eq!(
            installed::version_cached(&path, limit).as_deref(),
            Some("3.4.5")
        );
    }
    assert_eq!(asked(), 1);
    write("echo \"tool 3.4.10\"");
    assert_eq!(
        installed::version_cached(&path, limit).as_deref(),
        Some("3.4.10")
    );
    assert_eq!(
        installed::version_cached(&path, limit).as_deref(),
        Some("3.4.10")
    );
    assert_eq!(asked(), 2);
    write("echo \"tool 3.4.11\"; exit 3");
    assert_eq!(installed::version_cached(&path, limit), None);
    assert_eq!(installed::version_cached(&path, limit), None);
    assert_eq!(asked(), 4);
    assert_eq!(
        installed::version_cached(&tmp.path().join("missing"), limit),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_failed_version_ask_for_models_is_kept_for_the_backoff_and_a_launch_asks_again() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tool");
    let count = tmp.path().join("tool.count");
    let write = |body: &str| {
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho asked >> \"$0.count\"\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    let asked = || std::fs::read_to_string(&count).unwrap().lines().count();
    let limit = Duration::from_secs(20);
    let backoff = Duration::from_secs(600);
    let start = std::time::Instant::now();
    let models = |at: Duration| installed::version_for_models(&path, limit, backoff, start + at);
    write("exit 3");
    assert_eq!(models(Duration::ZERO), None);
    assert_eq!(asked(), 1);
    assert_eq!(models(Duration::from_secs(1)), None);
    assert_eq!(models(backoff - Duration::from_millis(1)), None);
    assert_eq!(asked(), 1, "a Models read inside the backoff asks nothing");
    let again = start + Duration::from_secs(2);
    assert_eq!(
        installed::version_for_models(&path, limit, Duration::ZERO, again),
        None
    );
    assert_eq!(asked(), 2, "the owner asking again asks with no backoff");
    assert_eq!(installed::version_cached(&path, limit), None);
    assert_eq!(asked(), 3, "a launch asks again");
    assert_eq!(models(backoff), None);
    assert_eq!(asked(), 3, "the backoff counts from the last failed ask");
    assert_eq!(models(backoff + Duration::from_secs(2)), None);
    assert_eq!(asked(), 4, "the backoff ends");
    write("echo \"tool 3.4.5\"");
    assert_eq!(
        models(backoff + Duration::from_secs(3)).as_deref(),
        Some("3.4.5")
    );
    assert_eq!(asked(), 5, "a changed file is a new identity");
    assert_eq!(
        models(backoff + Duration::from_secs(4)).as_deref(),
        Some("3.4.5")
    );
    assert_eq!(asked(), 5);
    assert_eq!(
        installed::version_for_models(&tmp.path().join("missing"), limit, backoff, start),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_probe_past_its_limit_leaves_no_process_behind() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("hangs");
    std::fs::write(
        &path,
        "#!/bin/sh\n/bin/sleep 30 &\necho $! > \"$0.child\"\nexec /bin/sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(installed::version_of(&path, Duration::from_secs(2)), None);
    assert!(started.elapsed() < Duration::from_secs(10));
    let child: i32 = std::fs::read_to_string(tmp.path().join("hangs.child"))
        .expect("the probe was stopped before it started its own child")
        .trim()
        .parse()
        .unwrap();
    let child = rustix::process::Pid::from_raw(child).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while rustix::process::test_kill_process(child).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the probe's own child still runs after the probe was stopped"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
