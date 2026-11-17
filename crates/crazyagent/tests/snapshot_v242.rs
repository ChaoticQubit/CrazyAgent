use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The golden folders below tests/snapshots/v242, one per surface kind of the 2.5 plan's
/// section 2.0, plus `runner` for the runner's own example. A scenario names one of these.
const KINDS: [&str; 10] = [
    "runner",
    "harness",
    "embedding",
    "provider",
    "check",
    "role",
    "workflow",
    "landing",
    "packs",
    "storage",
];

/// The longest scenario name a golden file stem may have.
const NAME_MAX: usize = 64;

/// One 2.4.2 surface capture, stored at tests/snapshots/v242/<kind>/<name>.json. `capture`
/// gets a fresh scratch folder under the system temp folder and returns the surface as JSON
/// before normalisation. A driver that needs the engine starts the built binary through
/// e2e_support's `Engine` with a fixture HOME and seeds it only through owner requests, never
/// rows written into engine.db; it reads only public crate items, never src cfg(test) code.
#[derive(Clone, Copy)]
struct Scenario {
    kind: &'static str,
    name: &'static str,
    capture: fn(&Path) -> Result<Value, String>,
}

/// Lists every scenario: the runner's own example, then each kind's drivers.
/// Output: the scenarios in the order the snapshot row runs them.
fn scenarios() -> Vec<Scenario> {
    vec![Scenario {
        kind: "runner",
        name: "masking-example",
        capture: masking_example,
    }]
}

/// Captures a fixed surface that holds one of every value the normalisation list masks: an
/// RFC 3339 time, `*_at` and `*_ms` integers, one id twice and another once, a pid, a port, a
/// CRLF line and sandbox rules under the scratch folder. Input: the scratch folder.
/// Output: the surface as JSON; its golden shows what each value becomes.
fn masking_example(scratch: &Path) -> Result<Value, String> {
    let agent = "3f2a9c1e5b7d4a608e1f2c3b4a5d6e7f";
    let rule = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "scratch path is not UTF-8".to_owned())
    };
    Ok(json!({
        "zebra": true,
        "apple": false,
        "agent": agent,
        "lead": agent,
        "session": "123e4567-e89b-12d3-a456-426614174000",
        "started_at": 1_763_322_000_000_u64,
        "elapsed_ms": 1520,
        "pid": 4242,
        "relay_port": 50123,
        "log": format!("2026-11-16T20:31:07.123Z agent {agent} started on 127.0.0.1:50123\r\n"),
        "sandbox": {
            "write": [rule(&scratch.join("projects").join("app"))?],
            "read": [rule(scratch)?],
        },
    }))
}

/// Names where a scenario's golden lives. Input: the scenario.
/// Output: tests/snapshots/v242/<kind>/<name>.json below this crate.
fn golden_path(scenario: &Scenario) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots/v242")
        .join(scenario.kind)
        .join(format!("{}.json", scenario.name))
}

/// Says whether a scenario name is a safe golden file stem.
/// Input: the name. Output: true for 1 to `NAME_MAX` of a-z, 0-9 and -, not starting with -.
fn safe_name(name: &str) -> bool {
    let safe = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
    !name.is_empty() && name.len() <= NAME_MAX && !name.starts_with('-') && name.bytes().all(safe)
}

/// Checks every scenario names a known kind and a safe name, once.
/// Input: the scenarios. Output: the first scenario that breaks a rule, as text.
fn check_names(scenarios: &[Scenario]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for scenario in scenarios {
        let at = format!("{}/{}", scenario.kind, scenario.name);
        if !KINDS.contains(&scenario.kind) {
            return Err(format!("{at}: unknown kind"));
        }
        if !safe_name(scenario.name) {
            return Err(format!("{at}: unsafe name"));
        }
        if !seen.insert(at.clone()) {
            return Err(format!("{at}: named twice"));
        }
    }
    Ok(())
}

#[test]
fn every_scenario_names_a_known_kind_and_a_safe_name_once() {
    let all = scenarios();
    assert!(!all.is_empty());
    assert_eq!(check_names(&all), Ok(()));
    assert!(golden_path(&all[0]).ends_with("tests/snapshots/v242/runner/masking-example.json"));
    assert_eq!(
        check_names(&[all[0], all[0]]),
        Err("runner/masking-example: named twice".to_owned())
    );
    let unknown = Scenario {
        kind: "kinds",
        ..all[0]
    };
    assert_eq!(
        check_names(&[unknown]),
        Err("kinds/masking-example: unknown kind".to_owned())
    );
    let long = "a".repeat(NAME_MAX + 1);
    for name in ["", "Upper", "a/b", "a.json", "-lead", "a b", long.as_str()] {
        assert!(!safe_name(name), "{name:?}");
    }
    assert!(safe_name(&"a".repeat(NAME_MAX)));
    assert!(safe_name("claude-code-worker-2"));
}

#[test]
fn the_comparer_fails_on_one_changed_byte_after_normalisation() {
    let text = normalise(&json!({"rule": "allow", "paths": ["projects/app"]})).unwrap();
    assert_eq!(compare(text.as_bytes(), &text), Ok(()));
    let changed = text.replacen("allow", "allox", 1);
    assert_eq!(changed.len(), text.len());
    assert_eq!(
        compare(changed.as_bytes(), &text),
        Err("line 5: golden \"  \\\"rule\\\": \\\"allox\\\"\", capture \"  \\\"rule\\\": \\\"allow\\\"\"".to_owned())
    );
    let unterminated = &text.as_bytes()[..text.len() - 1];
    assert!(compare(unterminated, &text).is_err());
    let crlf = text.replace('\n', "\r\n");
    assert!(compare(crlf.as_bytes(), &text).is_err());
}

#[test]
fn masking_keeps_two_equal_ids_equal_and_two_different_ids_apart() {
    let one = "0123456789abcdef0123456789abcdef";
    let two = "fedcba9876543210fedcba9876543210";
    let capture = json!({
        "third": one,
        "uuid_lower": "123e4567-e89b-12d3-a456-426614174000",
        "second": two,
        "first": one,
        "short": "abcdef012345678",
        "uuid": "123E4567-E89B-12D3-A456-426614174000",
        "in_text": format!("job {two} for {one}"),
    });
    let text = normalise(&capture).unwrap();
    assert_eq!(
        text,
        "{\n  \"first\": \"<id-1>\",\n  \"in_text\": \"job <id-2> for <id-1>\",\n  \"second\": \"<id-2>\",\n  \"short\": \"abcdef012345678\",\n  \"third\": \"<id-1>\",\n  \"uuid\": \"<id-3>\",\n  \"uuid_lower\": \"<id-3>\"\n}\n"
    );
    let reordered = json!({
        "in_text": format!("job {two} for {one}"),
        "first": one,
        "uuid": "123E4567-E89B-12D3-A456-426614174000",
        "short": "abcdef012345678",
        "second": two,
        "uuid_lower": "123e4567-e89b-12d3-a456-426614174000",
        "third": one,
    });
    assert_eq!(normalise(&reordered).unwrap(), text);
    assert!(normalise(&json!({ one: 1 })).is_err());
}

#[test]
fn a_sandbox_rule_widened_to_its_parent_fails_after_normalisation() {
    let scratch = tempfile::tempdir().unwrap();
    let rule = |path: &Path| json!({"sandbox": {"write": [path.to_str().unwrap()]}});
    let narrow = scratch.path().join("projects").join("app");
    let golden = normalise(&rule(&narrow)).unwrap();
    assert!(golden.contains("\"<path-1>/projects/app\""), "{golden}");
    assert_eq!(compare(golden.as_bytes(), &normalise(&rule(&narrow)).unwrap()), Ok(()));
    let real = scratch.path().canonicalize().unwrap().join("projects").join("app");
    assert_eq!(compare(golden.as_bytes(), &normalise(&rule(&real)).unwrap()), Ok(()));
    for wider in [
        scratch.path().join("projects"),
        scratch.path().to_path_buf(),
        std::env::temp_dir(),
    ] {
        let text = normalise(&rule(&wider)).unwrap();
        assert!(compare(golden.as_bytes(), &text).is_err(), "{text}");
    }
}

#[test]
fn the_golden_writer_writes_only_a_missing_file_and_refuses_private_text() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("harness").join("claude-code.json");
    assert_eq!(write_missing(&path, "{}\n"), Ok(true));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    assert_eq!(write_missing(&path, "{\"x\": 1}\n"), Ok(false));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    let names = (Some("/Users/owner"), Some("owner"));
    assert_eq!(refuse_private("\"<path-1>/projects/app\"", names.0, names.1), Ok(()));
    assert_eq!(refuse_private("\"owners and co-owners\"", names.0, names.1), Ok(()));
    assert_eq!(refuse_private("\"task-sk-1\"", names.0, names.1), Ok(()));
    for leak in [
        "\"/Users/someone/projects\"",
        "\"/home/someone/projects\"",
        "\"/Users/owner\"",
        "\"built by Owner on the mac\"",
        "\"key sk-ant-api03-abc\"",
        "\"ghp_abc\"",
        "\"github_pat_11A\"",
        "\"AKIAABC\"",
    ] {
        assert!(refuse_private(leak, names.0, names.1).is_err(), "{leak}");
    }
}
