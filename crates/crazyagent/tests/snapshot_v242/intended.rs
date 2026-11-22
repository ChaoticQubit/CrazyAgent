use super::{KINDS, compare, snapshots_root};
use serde::Deserialize;
use std::sync::OnceLock;

/// One reviewed difference against a 2.4.2 golden (v250 plan 9.13, H7, P6): the golden lines
/// from line `at` that the capture shows as `capture`, with the commit and why. An entry on
/// sandbox policy, grants, secret-carrying env or agent-home files is `security`, says in
/// `change` whether agents' reach widens, narrows or stays the same, and keeps the owner
/// sign-off row red until the owner sets `owner_signed`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub(super) scenario: String,
    pub(super) at: usize,
    pub(super) golden: Vec<String>,
    pub(super) capture: Vec<String>,
    pub(super) commit: String,
    pub(super) reason: String,
    pub(super) security: bool,
    #[serde(default)]
    pub(super) change: Option<String>,
    #[serde(default)]
    pub(super) owner_signed: bool,
}

/// Reads every kind's `intended.yaml` once (JSON, which is YAML, so no new parser is needed),
/// checking each entry's shape. Output: all entries; panics naming the file and entry when one
/// is malformed, since a bad list must never pass a difference.
pub(super) fn entries() -> &'static [Entry] {
    static ALL: OnceLock<Vec<Entry>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut all = Vec::new();
        for kind in KINDS {
            let Ok(text) =
                std::fs::read_to_string(snapshots_root().join(kind).join("intended.yaml"))
            else {
                continue;
            };
            let listed: Vec<Entry> = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{kind}/intended.yaml: {error}"));
            for entry in &listed {
                if let Err(why) = shape(kind, entry) {
                    panic!(
                        "{kind}/intended.yaml: {} at {}: {why}",
                        entry.scenario, entry.at
                    );
                }
            }
            all.extend(listed);
        }
        all
    })
}

/// How a security entry's `change` starts: what agents reach grows, shrinks, or is spelled
/// another way with the same reach.
const CHANGES: [&str; 3] = ["widens: ", "narrows: ", "same: "];

/// Checks one entry against the rules of the list. Inputs: the kind whose file holds it and
/// the entry. Output: Ok, or the rule it breaks.
fn shape(kind: &str, entry: &Entry) -> Result<(), String> {
    let (listed_kind, name) = entry
        .scenario
        .split_once('/')
        .ok_or("the scenario is not kind/name")?;
    if listed_kind != kind || name.is_empty() {
        return Err(format!("the scenario is not one of {kind}"));
    }
    if entry.at == 0 {
        return Err("golden lines count from 1".to_owned());
    }
    if entry.golden.is_empty() && entry.capture.is_empty() {
        return Err("the entry changes nothing".to_owned());
    }
    if entry.golden == entry.capture {
        return Err("the golden and capture lines are equal".to_owned());
    }
    let short = entry.commit.len() >= 9 && entry.commit.len() <= 40;
    if !short || !entry.commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("the commit is not a sha of 9 to 40 hex digits".to_owned());
    }
    if entry.reason.trim().is_empty() {
        return Err("the reason is empty".to_owned());
    }
    let says = entry
        .change
        .as_deref()
        .is_some_and(|change| CHANGES.iter().any(|said| change.starts_with(said)));
    if entry.security && !says {
        return Err(
            "a security entry says whether it widens, narrows or keeps the same".to_owned(),
        );
    }
    if !entry.security && (entry.change.is_some() || entry.owner_signed) {
        return Err("only a security entry carries a change or the owner's sign-off".to_owned());
    }
    Ok(())
}

/// Applies a scenario's listed differences to its golden, as a patch anchored at golden
/// lines, so the byte-equal compare then fails on any difference the list does not hold,
/// on a stale entry and on one that no longer sits where it says. Inputs: the scenario's
/// `kind/name`, its entries and the golden's bytes. Output: the expected bytes, or why an
/// entry does not apply.
pub(super) fn apply(scenario: &str, listed: &[&Entry], golden: &[u8]) -> Result<Vec<u8>, String> {
    let lines: Vec<&[u8]> = golden.split(|byte| *byte == b'\n').collect();
    let mut ordered = listed.to_vec();
    ordered.sort_by_key(|entry| entry.at);
    let mut out: Vec<&[u8]> = Vec::with_capacity(lines.len());
    let mut cursor = 0;
    for entry in ordered {
        let start = entry.at - 1;
        if start < cursor {
            return Err(format!(
                "intended entry at line {} overlaps the one before it",
                entry.at
            ));
        }
        let end = start + entry.golden.len();
        let held = lines.get(start..end).ok_or_else(|| {
            format!(
                "intended entry at line {} runs past the golden's end",
                entry.at
            )
        })?;
        if !held
            .iter()
            .copied()
            .eq(entry.golden.iter().map(String::as_bytes))
        {
            return Err(format!(
                "intended entry at line {} no longer matches the golden ({scenario})",
                entry.at
            ));
        }
        out.extend_from_slice(&lines[cursor..start]);
        out.extend(entry.capture.iter().map(String::as_bytes));
        cursor = end;
    }
    out.extend_from_slice(&lines[cursor..]);
    assert!(cursor <= lines.len());
    Ok(out.join(&b'\n'))
}

/// The golden a scenario's capture must equal: the 2.4.2 golden with the scenario's listed
/// differences applied. Inputs: the scenario's `kind/name` and the golden's bytes. Output:
/// the expected bytes, or why an entry does not apply.
pub(super) fn expected(scenario: &str, golden: &[u8]) -> Result<Vec<u8>, String> {
    let listed: Vec<&Entry> = entries()
        .iter()
        .filter(|entry| entry.scenario == scenario)
        .collect();
    apply(scenario, &listed, golden)
}

/// Names each security entry the owner has not yet signed, so the sign-off row stays red,
/// with what the entry changes. Input: the entries. Output: one line per held entry, sorted.
pub(super) fn held(listed: &[Entry]) -> Vec<String> {
    let mut held: Vec<String> = listed
        .iter()
        .filter(|entry| entry.security && !entry.owner_signed)
        .map(|entry| {
            let change = entry.change.as_deref().unwrap_or("says no change");
            format!(
                "{} at line {} ({}): {change}",
                entry.scenario, entry.at, entry.commit
            )
        })
        .collect();
    held.sort();
    held
}

#[test]
fn every_security_difference_is_owner_signed() {
    let held = held(entries());
    assert!(
        held.is_empty(),
        "held red for owner sign-off (v250 plan 9.13):\n{}",
        held.join("\n")
    );
}

/// An entry of the rows below, in scenario `runner/sample`. Inputs: the golden line it starts
/// at, the golden and capture lines and whether it is a security entry. Output: the entry.
fn entry(at: usize, golden: &[&str], capture: &[&str], security: bool) -> Entry {
    Entry {
        scenario: "runner/sample".to_owned(),
        at,
        golden: golden.iter().map(|line| (*line).to_owned()).collect(),
        capture: capture.iter().map(|line| (*line).to_owned()).collect(),
        commit: "992947bb6".to_owned(),
        reason: "a sample difference".to_owned(),
        security,
        change: security.then(|| "narrows: a sample deny".to_owned()),
        owner_signed: false,
    }
}

#[test]
fn an_unlisted_difference_fails_the_compare() {
    let golden = b"one\ntwo\nthree\n";
    let expected = apply("runner/sample", &[], golden).unwrap();
    assert!(compare(&expected, "one\n2\nthree\n").is_err());
    assert_eq!(compare(&expected, "one\ntwo\nthree\n"), Ok(()));
}

#[test]
fn a_listed_difference_passes_and_only_where_it_is_listed() {
    let golden = b"one\ntwo\nthree\n";
    let swap = entry(2, &["two"], &["2", "2b"], false);
    let removed = entry(3, &["three"], &[], false);
    let expected = apply("runner/sample", &[&swap, &removed], golden).unwrap();
    assert_eq!(compare(&expected, "one\n2\n2b\n"), Ok(()));
    let added = entry(1, &[], &["zero"], false);
    let expected = apply("runner/sample", &[&added], golden).unwrap();
    assert_eq!(compare(&expected, "zero\none\ntwo\nthree\n"), Ok(()));
    assert!(compare(&expected, "one\nzero\ntwo\nthree\n").is_err());
}

#[test]
fn a_stale_or_misplaced_entry_fails() {
    let golden = b"one\ntwo\nthree\n";
    let swap = entry(2, &["two"], &["2"], false);
    let expected = apply("runner/sample", &[&swap], golden).unwrap();
    assert!(compare(&expected, "one\ntwo\nthree\n").is_err());
    let misplaced = entry(3, &["two"], &["2"], false);
    assert!(apply("runner/sample", &[&misplaced], golden).is_err());
    let overlapping = entry(2, &["two", "three"], &["x"], false);
    assert!(apply("runner/sample", &[&swap, &overlapping], golden).is_err());
    let beyond = entry(4, &["four"], &[], false);
    assert!(apply("runner/sample", &[&beyond], golden).is_err());
}

#[test]
fn an_unsigned_security_entry_holds_its_scenario_and_a_signed_one_does_not() {
    let unsigned = entry(2, &["two"], &["2"], true);
    let mut signed = unsigned.clone();
    signed.owner_signed = true;
    let plain = entry(2, &["two"], &["2"], false);
    assert_eq!(
        held(&[unsigned, signed, plain]),
        ["runner/sample at line 2 (992947bb6): narrows: a sample deny"]
    );
    assert!(shape("runner", &entry(2, &["two"], &["2"], true)).is_ok());
    let mut respelled = entry(2, &["two"], &["2"], true);
    respelled.change = Some("same: one regex for two".to_owned());
    assert!(shape("runner", &respelled).is_ok());
    respelled.change = Some("a regex for two".to_owned());
    assert!(shape("runner", &respelled).is_err());
    let mut silent = entry(2, &["two"], &["2"], true);
    silent.change = None;
    assert!(shape("runner", &silent).is_err());
    let mut self_signed = entry(2, &["two"], &["2"], false);
    self_signed.owner_signed = true;
    assert!(shape("runner", &self_signed).is_err());
    assert!(shape("storage", &entry(2, &["two"], &["2"], false)).is_err());
    assert!(shape("runner", &entry(2, &[], &[], false)).is_err());
}
