use super::*;
use serde_json::{Value, json};
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

fn fixture_json() -> Value {
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
    assert_eq!(locked.engine_value, json!("workspace-write"));
    assert!(qualified(&catalog, Some("0.51.0")));
    assert!(!qualified(&catalog, Some("0.50")));
    assert!(!qualified(&catalog, None));
}

#[test]
fn a_catalog_that_breaks_the_contract_is_refused() {
    let edits: Vec<(&str, Box<dyn Fn(&mut Value)>)> = vec![
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
            "a key inside another key",
            Box::new(|c| c["keys"][3]["key"] = json!("hide_agent_reasoning.label")),
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
    }
    assert!(catalog::builtin("codex-not-a-harness").unwrap().is_none());
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
fn a_verified_executable_is_asked_for_its_version_once_per_content() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let count = base.join("asked");
    let executable = base.join("codex");
    let write = |body: &str| {
        std::fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    let asked = || {
        std::fs::read_to_string(&count)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };
    let digest = |name: &str| format!("{}-{name}", base.display());
    let limit = Duration::from_secs(20);
    write(&format!(
        "echo x >> '{}'; echo \"codex-cli 0.50.0\"",
        count.display()
    ));
    for _ in 0..3 {
        assert_eq!(
            installed::version_of_digest(&executable, &digest("one"), limit).as_deref(),
            Some("0.50.0")
        );
    }
    assert_eq!(asked(), 1);
    assert_eq!(
        installed::version_of_digest(&executable, &digest("two"), limit).as_deref(),
        Some("0.50.0")
    );
    assert_eq!(asked(), 2);
    write(&format!("echo x >> '{}'; exit 3", count.display()));
    assert_eq!(
        installed::version_of_digest(&executable, &digest("three"), limit),
        None
    );
    write(&format!(
        "echo x >> '{}'; echo \"codex-cli 0.51.0\"",
        count.display()
    ));
    assert_eq!(
        installed::version_of_digest(&executable, &digest("three"), limit).as_deref(),
        Some("0.51.0")
    );
    assert_eq!(asked(), 4);
}
