use super::draft::{self, Drafted};
use super::*;
use crate::project_setup::Platform;
use std::os::unix::fs::symlink;

fn pool() -> CredentialRef {
    CredentialRef::new("main").unwrap()
}

fn rust_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        root.join("Makefile"),
        "build:\n\tcargo build\ntest:\n\tcargo test\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "# Demo\n\nA small greeting tool.\n").unwrap();
    dir
}

#[test]
fn project_ids_are_the_folder_name_as_a_slug() {
    let id = |path: &str| project_id_for(Path::new(path)).unwrap().as_str().to_owned();
    assert_eq!(id("/work/repos/My Cool_App!"), "my-cool-app");
    assert_eq!(id("/work/repos/---"), "project");
    assert_eq!(id("/work/repos/Café"), "caf");
    assert_eq!(id(&format!("/work/repos/{}", "a".repeat(100))).len(), 64);
    assert!(project_id_for(Path::new("/")).is_err());
}

#[test]
fn steps_round_trip_through_their_columns_and_wire_tag() {
    for step in [
        Step::Waiting,
        Step::Running,
        Step::Done,
        Step::failed("graphify not installed"),
    ] {
        let (state, reason) = step.columns();
        assert_eq!(
            Step::from_columns(state, reason.map(str::to_owned)).unwrap(),
            step
        );
    }
    assert!(Step::from_columns("lost", None).is_err());
    assert_eq!(
        serde_json::to_value(Step::failed("graphify not installed")).unwrap(),
        serde_json::json!({"kind": "failed", "reason": "graphify not installed"})
    );
}

#[test]
fn failed_steps_keep_a_bounded_plain_reason() {
    let Step::Failed { reason } = Step::failed(&format!("bad\u{7}{}", "é".repeat(300))) else {
        panic!("a failed step is failed");
    };
    assert!(reason.len() <= 256);
    assert!(reason.starts_with("badé"));
    assert!(!reason.contains('\u{7}'));
}

#[test]
fn manifests_alone_give_the_rust_facts() {
    let dir = rust_repo();
    let evidence = draft::gather(dir.path());
    let (facts, stack) = draft::found_facts(&evidence, dir.path());
    assert_eq!(facts.languages, ["Rust"]);
    assert_eq!(facts.build_commands, ["cargo build", "make build"]);
    assert_eq!(facts.test_commands, ["cargo test", "make test"]);
    assert_eq!(facts.entry_points, ["src/main.rs"]);
    assert_eq!(stack, ["Rust"]);
    assert!(evidence.readme.is_some());
    assert!(evidence.entries.contains(&"src/".to_owned()));
}

#[test]
fn evidence_never_reads_env_files_or_follows_links() {
    let dir = rust_repo();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("token"), "SECRET_VALUE_7731").unwrap();
    std::fs::write(dir.path().join(".env"), "TOKEN=SECRET_VALUE_7731").unwrap();
    std::fs::write(dir.path().join(".env.local"), "TOKEN=SECRET_VALUE_7731").unwrap();
    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    symlink(outside.path().join("token"), dir.path().join("README.md")).unwrap();
    symlink(
        outside.path().join("token"),
        dir.path().join("package.json"),
    )
    .unwrap();
    let evidence = draft::gather(dir.path());
    let (found, _) = draft::found_facts(&evidence, dir.path());
    let prompt = draft::prompt(&evidence, &found);
    assert!(evidence.readme.is_none());
    assert!(
        evidence
            .manifests
            .iter()
            .all(|(name, _)| name != "package.json")
    );
    assert!(
        !evidence
            .entries
            .iter()
            .any(|entry| entry.starts_with(".env"))
    );
    assert!(!prompt.contains("SECRET_VALUE_7731"));
    assert!(!found.languages.contains(&"JavaScript".to_owned()));
}

#[test]
fn the_last_json_block_fills_the_answer() {
    let text = concat!(
        "## Structure\n\nA CLI that prints greetings.\n\n",
        "```json\n{\"about\": \"draft\"}\n```\n\n",
        "```json\n{\"about\": \"Greets people.\", \"tech_stack\": [\"Rust\", \" rust \", \"\"], ",
        "\"platforms\": [\"cli\", \"Desktop app\", \"moon\"], \"languages\": [\"Rust\"]}\n```\n",
    );
    let answer = draft::parse_answer(text);
    assert_eq!(answer.about, "Greets people.");
    assert_eq!(answer.tech_stack, ["Rust"]);
    assert_eq!(answer.platforms, [Platform::Cli, Platform::DesktopApp]);
    assert_eq!(answer.facts.languages, ["Rust"]);
    assert!(answer.overview.starts_with("## Structure"));
    assert!(!answer.overview.contains("Greets people."));
}

#[test]
fn an_answer_without_json_takes_about_from_its_first_paragraph() {
    let answer = draft::parse_answer("# Overview\n\nA web shop\nfor books.\n\n- item\n");
    assert_eq!(answer.about, "A web shop for books.");
    assert!(answer.tech_stack.is_empty());
    assert!(answer.platforms.is_empty());
}

#[test]
fn merging_fills_only_the_gaps_and_keeps_research_off() {
    let mut base = Drafted::new("demo".into(), &pool());
    assert_eq!(base.missing(), ["about", "tech_stack", "platforms"]);
    base.draft.about = "Owner words.".into();
    base.draft.tech_stack = vec!["Postgres".into()];
    let answer = draft::parse_answer(concat!(
        "```json\n{\"about\": \"Agent words.\", \"tech_stack\": [\"rust\", \"Tokio\"], ",
        "\"platforms\": [\"cli\"]}\n```\n",
    ));
    let found = MappingFacts {
        languages: vec!["Rust".into()],
        ..MappingFacts::default()
    };
    let merged = draft::merge(&base, &answer, &found, &["Rust".to_owned()]);
    assert_eq!(merged.draft.about, "Owner words.");
    assert_eq!(merged.draft.tech_stack, ["Postgres", "Rust", "Tokio"]);
    assert_eq!(merged.draft.platforms, [Platform::Cli]);
    assert!(merged.draft.research.is_empty());
    assert_eq!(merged.facts.languages, ["Rust"]);
    assert!(merged.missing().is_empty());
}

#[test]
fn a_saved_draft_and_overview_are_reused_not_overwritten() {
    let dir = rust_repo();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir(&layout).unwrap();
    assert!(draft::reused(&layout).is_none());
    assert!(draft::overview(&layout).is_none());
    let mut saved = Drafted::new("Demo tool".into(), &pool());
    saved.draft.about = "Kept by the owner.".into();
    draft::save(&layout, &saved).unwrap();
    assert_eq!(draft::reused(&layout), Some(saved));
    assert!(draft::write_overview(&layout, "First overview.").unwrap());
    assert!(!draft::write_overview(&layout, "Second overview.").unwrap());
    let overview = draft::overview(&layout).unwrap();
    assert!(overview.contains("First overview."));
    assert!(!overview.contains("Second overview."));
}

#[test]
fn a_linked_draft_is_ignored() {
    let dir = rust_repo();
    let outside = tempfile::tempdir().unwrap();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir_all(layout.join("setup")).unwrap();
    let mut planted = Drafted::new("Planted".into(), &pool());
    planted.draft.about = "From outside.".into();
    std::fs::write(
        outside.path().join("draft.json"),
        serde_json::to_vec(&planted).unwrap(),
    )
    .unwrap();
    symlink(
        outside.path().join("draft.json"),
        layout.join("setup/draft.json"),
    )
    .unwrap();
    assert!(draft::reused(&layout).is_none());
}
