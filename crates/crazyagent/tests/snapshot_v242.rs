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
