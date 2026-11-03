use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use trackwright_core::model::{Refusal, codes};
use trackwright_core::runs::codes as run_codes;
use trackwright_core::tests_model::codes as test_codes;

const TEST_CODES: [&str; 26] = [
    test_codes::TEST_ID_INVALID,
    test_codes::CASE_UNKNOWN,
    test_codes::SET_UNKNOWN,
    test_codes::PLAN_UNKNOWN,
    test_codes::RUN_UNKNOWN,
    test_codes::CASES_FULL,
    test_codes::SETS_FULL,
    test_codes::PLANS_FULL,
    test_codes::STEPS_FULL,
    test_codes::SET_CASES_FULL,
    test_codes::PLAN_ITEMS_FULL,
    test_codes::CASE_LINKS_FULL,
    test_codes::STABLE_NAME_TAKEN,
    test_codes::CASE_LINK_EXISTS,
    test_codes::CASE_LINK_UNKNOWN,
    test_codes::SET_CASE_EXISTS,
    test_codes::SET_CASE_UNKNOWN,
    test_codes::PLAN_ITEM_EXISTS,
    test_codes::PLAN_ITEM_UNKNOWN,
    test_codes::PLAN_CLOSED,
    test_codes::CASE_PROTECTED,
    test_codes::CROSS_PROJECT,
    test_codes::OPTION_NEEDS_HOST,
    test_codes::RELEASE_UNKNOWN,
    test_codes::CYCLE_UNKNOWN,
    test_codes::ACTIVE_PLAN_EXISTS,
];

const RUN_CODES: [&str; 15] = [
    run_codes::PLAN_UNKNOWN,
    run_codes::RUN_UNKNOWN,
    run_codes::TEST_ID_INVALID,
    run_codes::RUN_CLOSED,
    run_codes::RUN_WRITER_REFUSED,
    run_codes::RUN_SOURCE_INVALID,
    run_codes::RUN_POLICY_INVALID,
    run_codes::RUN_INTEGRITY_INVALID,
    run_codes::RUN_FINISH_INVALID,
    run_codes::RUN_BATCH_INVALID,
    run_codes::RUN_RESULTS_FULL,
    run_codes::RUNS_FULL,
    run_codes::RESULTS_EMPTY,
    run_codes::RESULTS_TOO_MANY,
    run_codes::RESULT_NAME_REPEATED,
];

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

fn codes_block(text: &str) -> Option<&str> {
    let start = text.find("pub mod codes {")?;
    let end = text[start..].find("\n}").unwrap();
    Some(&text[start..start + end])
}

fn declared(block: &str) -> BTreeMap<&str, &str> {
    block
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub const "))
        .filter_map(|line| line.split_once(": &str = \""))
        .map(|(name, value)| (name, value.trim_end_matches("\";")))
        .collect()
}

fn reexported(block: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = block;
    while let Some(start) = rest.find("pub use crate::model::codes::{") {
        let after = &rest[start + "pub use crate::model::codes::{".len()..];
        let end = after.find('}').unwrap();
        names.extend(
            after[..end]
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty()),
        );
        rest = &after[end..];
    }
    names
}

#[test]
fn every_test_and_run_code_builds_a_core_refusal() {
    let mut seen = Vec::new();
    for code in TEST_CODES.iter().chain(RUN_CODES.iter()) {
        assert!(codes::ALL.contains(code), "{code} is in codes::ALL");
        let refusal = Refusal::new(code, format!("refused with {code}"));
        assert_eq!(refusal.code, *code);
        seen.push(*code);
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 38);
}

#[test]
fn every_codes_block_outside_model_reexports_the_core_codes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let model = fs::read_to_string(root.join("model.rs")).unwrap();
    let core = declared(codes_block(&model).unwrap());
    assert_eq!(core.len(), codes::ALL.len());
    let mut files = Vec::new();
    sources(&root, &mut files);
    let mut blocks = 0;
    for file in files.iter().filter(|file| !file.ends_with("model.rs")) {
        let text = fs::read_to_string(file).unwrap();
        let Some(block) = codes_block(&text) else {
            continue;
        };
        blocks += 1;
        assert!(
            declared(block).is_empty(),
            "{} declares codes of its own; declare them in model.rs",
            file.display()
        );
        let names = reexported(block);
        assert!(!names.is_empty(), "{}", file.display());
        for name in names {
            let value = core
                .get(name)
                .unwrap_or_else(|| panic!("{} re-exports {name}, not a core code", file.display()));
            assert!(codes::ALL.contains(value), "{name}");
        }
    }
    assert!(blocks >= 2, "tests_model.rs and runs.rs hold codes blocks");
}
