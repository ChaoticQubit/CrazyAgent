use super::draft::{self, Drafted};
use super::*;
use crate::project_setup::{Platform, ResearchKind, StoredSetup, TeamPlan};
use std::os::unix::fs::symlink;

const OTHER_GRAPH_MARKER: &str = "# Other project graph 6620";
const GATHER_RACE_TIME: std::time::Duration = std::time::Duration::from_secs(20);
const GATHER_RACE_MAX: u64 = 2_000_000;
const SAVE_RACE_TIME: std::time::Duration = std::time::Duration::from_secs(20);
const SAVE_RACE_MAX: u64 = 20_000;

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
        Step::WAITING,
        Step::Waiting {
            until_ms: Some(1_792_729_920_000),
        },
        Step::Running,
        Step::Done,
        Step::failed("graphify not installed"),
    ] {
        let (state, reason) = step.columns();
        assert_eq!(Step::from_columns(state, reason).unwrap(), step);
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
    git_init(dir.path());
    let evidence = draft::gather(dir.path(), &[]);
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
    let evidence = draft::gather(dir.path(), &[]);
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

fn write_file(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn documents_with_unrelated_names_are_read_from_their_text_and_mapped_to_research() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("token"), "SECRET_VALUE_7731").unwrap();
    write_file(
        root,
        "research/landscape-2026.md",
        "# Landscape\n\nRival note apps: Notion and Obsidian lead. COMPETITOR_TEXT_5521\n",
    );
    write_file(
        root,
        "notes/q3-sizing.txt",
        "Serviceable market 2.1B dollars, growing 9% a year. MARKET_TEXT_8830\n",
    );
    write_file(
        root,
        "docs/plan.md",
        "Release plan for sync. PLAN_TEXT_4410\n",
    );
    write_file(root, "docs/spec.pdf", "%PDF-1.7 PDF_BYTES_3390");
    write_file(root, "private/salaries.md", "PRIVATE_TEXT_6602");
    write_file(root, "docs/Private/board.md", "PRIVATE_TEXT_6602");
    write_file(root, ".notes/hidden.md", "HIDDEN_TEXT_1207");
    write_file(root, "docs/.draft.md", "HIDDEN_TEXT_1207");
    write_file(root, "node_modules/pkg/README.md", "VENDOR_TEXT_7781");
    write_file(root, "docs/a/b/c/deep.md", "DEPTH_FOUR_TEXT_2290");
    write_file(root, "docs/a/b/c/d/deeper.md", "DEPTH_FIVE_TEXT_8121");
    std::fs::write(root.join("docs/huge.md"), "x".repeat(1024 * 1024 + 1)).unwrap();
    symlink(outside.path().join("token"), root.join("notes/linked.md")).unwrap();
    symlink(outside.path(), root.join("docs/linked-folder")).unwrap();

    let evidence = draft::gather(root, &[]);
    let (found, stack) = draft::found_facts(&evidence, root);
    let prompt = draft::prompt(&evidence, &found);
    for text in [
        "COMPETITOR_TEXT_5521",
        "MARKET_TEXT_8830",
        "PLAN_TEXT_4410",
        "DEPTH_FOUR_TEXT_2290",
        "### research/landscape-2026.md (",
        "docs/spec.pdf (",
        "\"research\"",
    ] {
        assert!(prompt.contains(text), "{text} is missing from the prompt");
    }
    for text in [
        "SECRET_VALUE_7731",
        "PRIVATE_TEXT_6602",
        "HIDDEN_TEXT_1207",
        "VENDOR_TEXT_7781",
        "DEPTH_FIVE_TEXT_8121",
        "PDF_BYTES_3390",
        "docs/huge.md",
        "salaries",
        "linked.md",
        "linked-folder",
    ] {
        assert!(!prompt.contains(text), "{text} reached the prompt");
    }
    assert_eq!(prompt.matches("A small greeting tool").count(), 1);

    let answer = draft::parse_answer(concat!(
        "## Structure\n\nA note app.\n\n## Project documents\n\n",
        "research/landscape-2026.md holds the competitor analysis.\n\n",
        "```json\n{\"about\": \"A note app.\", \"research\": [",
        "{\"kind\": \"competitor_research\", \"files\": [\"research/landscape-2026.md\"]}, ",
        "{\"kind\": \"market_research\", \"files\": [\"./notes/q3-sizing.txt\", \"research/made-up.md\"]}, ",
        "{\"kind\": \"sales_research\", \"files\": [\"docs/plan.md\"]}], ",
        "\"documents\": [\"docs/plan.md\", \"docs/spec.pdf\", \"private/salaries.md\"]}\n```\n",
    ));
    let base = Drafted::new("notes".into(), &pool());
    let merged = draft::merge(&base, &answer, &found, &stack);
    assert_eq!(
        merged.facts.research,
        [
            FoundResearch {
                kind: ResearchKind::MarketResearch,
                files: vec!["notes/q3-sizing.txt".into()],
            },
            FoundResearch {
                kind: ResearchKind::CompetitorResearch,
                files: vec!["research/landscape-2026.md".into()],
            },
        ]
    );
    assert_eq!(merged.facts.documents, ["docs/plan.md"]);
    assert_eq!(merged.draft.about, "A note app.");
    assert!(
        merged.draft.research.is_empty(),
        "mapping never turns research on"
    );
    assert!(merged.facts.shown.is_empty());
    let again = draft::merge(&merged, &draft::Answer::default(), &found, &stack);
    assert_eq!(again.facts, merged.facts);
}

#[test]
fn document_reading_stops_at_its_bounds_and_lists_the_rest() {
    let dir = rust_repo();
    git_init(dir.path());
    for index in 0..60 {
        write_file(
            dir.path(),
            &format!("docs/note-{index:02}.md"),
            &format!("NOTE_{index:02} {}\n", "y".repeat(7 * 1024)),
        );
    }
    let evidence = draft::gather(dir.path(), &[]);
    let read: Vec<&str> = evidence
        .documents
        .iter()
        .map(|document| document.path.as_str())
        .collect();
    assert_eq!(read.len(), 8, "{read:?}");
    assert_eq!(read[0], "docs/note-00.md");
    assert!(
        evidence
            .documents
            .iter()
            .all(|document| document.text.len() == 6 * 1024 && document.size == 7177)
    );
    assert_eq!(evidence.unread.len(), 52);
    assert_eq!(evidence.unread[0], ("docs/note-08.md".to_owned(), 7177));
    let (found, _) = draft::found_facts(&evidence, dir.path());
    assert_eq!(found.shown.len(), 8);
    let prompt = draft::prompt(&evidence, &found);
    assert!(prompt.contains("### docs/note-00.md (7177 bytes, the first 6144 shown)"));
    assert!(prompt.contains("docs/note-59.md (7177 bytes)"));

    let many = rust_repo();
    git_init(many.path());
    for index in 0..45 {
        write_file(
            many.path(),
            &format!("notes/n-{index:02}.txt"),
            &format!("Short note {index}.\n"),
        );
    }
    let evidence = draft::gather(many.path(), &[]);
    assert_eq!(evidence.documents.len(), 40);
    assert_eq!(evidence.unread.len(), 5);
    assert_eq!(evidence.unread[0].0, "notes/n-40.txt");
}

#[test]
fn facts_without_documents_serialize_as_before() {
    assert_eq!(
        serde_json::to_value(MappingFacts::default()).unwrap(),
        serde_json::json!({"languages": [], "build_commands": [], "test_commands": [], "entry_points": []})
    );
    let old: MappingFacts = serde_json::from_str(
        r#"{"languages":["Rust"],"build_commands":[],"test_commands":[],"entry_points":[]}"#,
    )
    .unwrap();
    assert!(old.research.is_empty() && old.documents.is_empty() && old.shown.is_empty());
}

fn git_init(root: &Path) {
    let mut command = std::process::Command::new(crate::research::runner::setup_git());
    command
        .args(["init", "-q", "--template="])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(root);
    let status = crate::spawn_gate::status(&mut command);
    assert!(
        status.as_ref().is_ok_and(std::process::ExitStatus::success),
        "git init: {status:?}"
    );
}

#[cfg(target_os = "macos")]
fn git_in(root: &Path, args: &[&str]) {
    let mut command = std::process::Command::new(crate::research::runner::setup_git());
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Mapping Test")
        .env("GIT_AUTHOR_EMAIL", "mapping@example.invalid")
        .env("GIT_COMMITTER_NAME", "Mapping Test")
        .env("GIT_COMMITTER_EMAIL", "mapping@example.invalid")
        .current_dir(root);
    let status = crate::spawn_gate::status(&mut command);
    assert!(
        status.as_ref().is_ok_and(std::process::ExitStatus::success),
        "git {args:?}: {status:?}"
    );
}

fn broken_git(root: &Path) {
    let missing = root.join("missing.git");
    std::fs::write(
        root.join(".git"),
        format!("gitdir: {}\n", missing.display()),
    )
    .unwrap();
}

fn mapping_prompt(root: &Path) -> String {
    let evidence = draft::gather(root, &[]);
    let (found, _) = draft::found_facts(&evidence, root);
    draft::prompt(&evidence, &found)
}

#[test]
fn git_ignored_documents_are_neither_read_nor_listed() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    write_file(
        root,
        ".gitignore",
        "notes/private-*.md\n!notes/private-ok.md\ndocs/scratch/\n*.log.md\n",
    );
    write_file(root, ".git/info/exclude", "docs/excluded.md\n");
    write_file(
        root,
        "docs/plan.md",
        "Release plan for sync. PLAN_TEXT_4410\n",
    );
    write_file(
        root,
        "docs/meeting notes.md",
        "Weekly sync. SPACED_TEXT_6161\n",
    );
    write_file(
        root,
        "notes/private-ok.md",
        "Kept by a rule. NEGATED_TEXT_1414\n",
    );
    write_file(root, "docs/spec.pdf", "%PDF-1.7");
    let ignored = [
        "notes/private-plan.md",
        "docs/scratch/idea.md",
        "docs/scratch/spec.pdf",
        "docs/run.log.md",
        "docs/excluded.md",
    ];
    for path in ignored {
        write_file(root, path, "IGNORED_TEXT_3141 api_key=sk-test-0000\n");
    }

    let prompt = mapping_prompt(root);
    for text in [
        "PLAN_TEXT_4410",
        "SPACED_TEXT_6161",
        "NEGATED_TEXT_1414",
        "docs/spec.pdf (",
    ] {
        assert!(prompt.contains(text), "{text} is missing from the prompt");
    }
    assert!(
        !prompt.contains("IGNORED_TEXT_3141"),
        "git-ignored text reached the prompt"
    );
    for path in ignored {
        assert!(!prompt.contains(path), "the git-ignored {path} is listed");
    }
}

#[test]
fn secret_named_documents_are_neither_read_nor_listed() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    write_file(
        root,
        "docs/plan.md",
        "Release plan for sync. PLAN_TEXT_4410\n",
    );
    let kept = [
        "docs/monkey-business.md",
        "docs/tokenizer.md",
        "docs/keynote.md",
        "docs/environment.md",
    ];
    for path in kept {
        write_file(root, path, &format!("KEPT_TEXT_5150 {path}\n"));
    }
    let secret = [
        "docs/credentials.txt",
        "docs/api-keys.md",
        "notes/Passwords.md",
        "docs/server.pem.txt",
        "docs/prod.env.md",
        "research/aws_secret_notes.md",
        "docs/privateKey.txt",
        "docs/tokens.md",
        "docs/Api_Key.txt",
        "docs/client.p12.txt",
        "docs/cert.PFX.md",
        "notes/passwd.txt",
        "docs/deploy.key.md",
        "secrets/notes.md",
        "docs/Credential.pdf",
    ];
    for path in secret {
        write_file(root, path, "SECRET_TEXT_9001 password=hunter2\n");
    }

    let prompt = mapping_prompt(root);
    assert!(prompt.contains("PLAN_TEXT_4410"));
    for path in kept {
        assert!(
            prompt.contains(&format!("KEPT_TEXT_5150 {path}")),
            "{path} is missing from the prompt"
        );
    }
    assert!(
        !prompt.contains("SECRET_TEXT_9001"),
        "a secret file's text reached the prompt"
    );
    for path in secret {
        assert!(!prompt.contains(path), "the secret-named {path} is listed");
    }
}

#[test]
fn no_secret_like_or_git_ignored_name_reaches_the_mapping_prompt() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    write_file(root, ".gitignore", "private/\nlocal-notes.md\n");
    write_file(
        root,
        "notes/api-keys.md",
        "SECRET_TEXT_9001 key=sk-test-0000\n",
    );
    write_file(
        root,
        "notes/ui-direction.md",
        "Dark theme first. UI_TEXT_2718\nThe keys for it sit in notes/api-keys.md.\n",
    );
    write_file(root, "private/plan.md", "PRIVATE_TEXT_1618\n");
    write_file(root, "secrets.txt", "SECRET_TEXT_9001\n");
    write_file(root, "local-notes.md", "PRIVATE_TEXT_1618\n");
    write_file(
        root,
        "README.md",
        concat!(
            "# Demo\n\nA small greeting tool. README_KEPT_1111\n",
            "See notes/ui-direction.md for the look.\n",
            "The keys sit in `notes/api-keys.md`.\n",
            "Drafts go to [the plan](private/plan.md).\n",
        ),
    );
    write_file(
        root,
        "Makefile",
        "build:\n\tcargo build\ntest:\n\tcargo test\nrelease:\n\tcp config/secrets.toml dist/\n",
    );
    write_file(
        root,
        "graphify-out/GRAPH_REPORT.md",
        concat!(
            "# Graph Report\n\n## God Nodes\n",
            "- api-keys.md\n- notes/api-keys.md\n- private/plan.md\n- ApiKeys\n",
            "- ui-direction.md GRAPH_KEPT_4242\n",
        ),
    );
    write_file(
        root,
        &format!("graphify-out/{}", graph::KEPT_OUT_MARK),
        &graph::current_mark(root, &[]).unwrap(),
    );
    write_file(
        root,
        ".crazyagent/overview.md",
        concat!(
            "# Project overview\n\nGreets people. OVERVIEW_KEPT_3030\n",
            "The code graph mentions an `api-keys.md` node, probably here. It was not read.\n",
        ),
    );

    let evidence = draft::gather(root, &[]);
    for entry in ["Cargo.toml", "Makefile", "README.md", "notes/", "src/"] {
        assert!(
            evidence.entries.contains(&entry.to_owned()),
            "the entry {entry} is missing"
        );
    }
    for entry in ["secrets.txt", "local-notes.md", "private/"] {
        assert!(
            !evidence.entries.contains(&entry.to_owned()),
            "the entry {entry} is listed"
        );
    }
    let prompt = mapping_prompt(root);
    for text in [
        "README_KEPT_1111",
        "See notes/ui-direction.md for the look.",
        "UI_TEXT_2718",
        "GRAPH_KEPT_4242",
        "OVERVIEW_KEPT_3030",
        "make build",
    ] {
        assert!(prompt.contains(text), "{text} is missing from the prompt");
    }
    let lower = prompt.to_lowercase();
    for name in [
        "api-keys",
        "apikeys",
        "secret_text_9001",
        "private/plan.md",
        "private_text_1618",
        "secrets.txt",
        "secrets.toml",
        "local-notes.md",
    ] {
        assert!(!lower.contains(name), "{name} reached the mapping prompt");
    }
}

#[test]
fn hard_linked_documents_are_neither_read_nor_listed() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("notes.md"), "HARDLINK_TEXT_4242\n").unwrap();
    write_file(
        root,
        "docs/plan.md",
        "Release plan for sync. PLAN_TEXT_4410\n",
    );
    std::fs::hard_link(
        outside.path().join("notes.md"),
        root.join("docs/linked-copy.md"),
    )
    .unwrap();
    write_file(root, "docs/twin-a.md", "TWIN_TEXT_4243\n");
    std::fs::hard_link(root.join("docs/twin-a.md"), root.join("docs/twin-b.md")).unwrap();

    let prompt = mapping_prompt(root);
    assert!(prompt.contains("PLAN_TEXT_4410"));
    for text in [
        "HARDLINK_TEXT_4242",
        "docs/linked-copy.md",
        "TWIN_TEXT_4243",
        "docs/twin-a.md",
        "docs/twin-b.md",
    ] {
        assert!(
            !prompt.contains(text),
            "the hard-linked {text} reached the prompt"
        );
    }
}

#[test]
fn without_an_answer_from_git_no_document_is_read_and_the_prompt_says_so() {
    let dir = rust_repo();
    let root = dir.path();
    broken_git(root);
    write_file(
        root,
        "docs/plan.md",
        "Release plan for sync. PLAN_TEXT_4410\n",
    );
    write_file(root, "docs/spec.pdf", "%PDF-1.7");

    let evidence = draft::gather(root, &[]);
    assert!(
        evidence.documents.is_empty() && evidence.unread.is_empty(),
        "documents were read without git saying which it ignores"
    );
    assert!(
        evidence.entries.is_empty(),
        "entries were listed without git saying which it ignores: {:?}",
        evidence.entries
    );
    let prompt = mapping_prompt(root);
    assert!(!prompt.contains("PLAN_TEXT_4410") && !prompt.contains("docs/spec.pdf"));
    assert!(
        prompt.contains(
            "No project document was read: the project's git could not tell which files it ignores."
        ),
        "the prompt does not say why no document was read"
    );
}

#[test]
fn a_secret_like_package_main_is_never_an_entry_point() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    write_file(
        root,
        "package.json",
        "{\"main\": \"lib/apikeys.js\", \"scripts\": {\"build\": \"tsc\"}}\n",
    );

    let evidence = draft::gather(root, &[]);
    let (found, _) = draft::found_facts(&evidence, root);
    assert_eq!(found.entry_points, ["src/main.rs"]);
    assert!(!mapping_prompt(root).to_lowercase().contains("apikeys"));
}

#[test]
fn without_an_answer_from_git_no_entry_or_path_name_reaches_the_prompt() {
    let dir = rust_repo();
    let root = dir.path();
    broken_git(root);
    write_file(root, ".gitignore", "private/\n");
    write_file(root, "private/plan.md", "PRIVATE_TEXT_1618\n");
    write_file(
        root,
        "README.md",
        "# Demo\n\nA small greeting tool. README_KEPT_1111\nDrafts go to private/plan.md.\n",
    );

    let evidence = draft::gather(root, &[]);
    assert!(
        evidence.entries.is_empty(),
        "entries were listed without git saying which it ignores: {:?}",
        evidence.entries
    );
    let prompt = mapping_prompt(root);
    assert!(prompt.contains("README_KEPT_1111"));
    assert!(!prompt.contains("private/plan.md"));
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
    let first = draft::write_overview(&layout, "First overview.").unwrap();
    assert_eq!(first, draft::overview(&layout));
    assert_eq!(
        draft::write_overview(&layout, "Second overview.").unwrap(),
        None
    );
    let overview = draft::overview(&layout).unwrap();
    assert!(overview.contains("First overview."));
    assert!(!overview.contains("Second overview."));
}

#[test]
fn only_an_overview_this_engine_wrote_is_shown_as_the_mapping_agents() {
    let dir = rust_repo();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir(&layout).unwrap();
    let mut database = Connection::open_in_memory().unwrap();
    crate::storage::plugins::initialize(&database).unwrap();
    crate::storage::setup::initialize(&database).unwrap();
    crate::project_setup::initialize(&database).unwrap();
    initialize(&database).unwrap();
    database
        .execute(
            "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
             VALUES ('demo',?1,'main','done','done',?2,1,1)",
            params![
                dir.path().to_str().unwrap(),
                serde_json::to_string(&Drafted::new("demo".into(), &pool())).unwrap()
            ],
        )
        .unwrap();
    let shown = |database: &mut Connection| {
        let tx = database.transaction().unwrap();
        let view = status(&tx, &ProjectId::new("demo").unwrap())
            .unwrap()
            .unwrap();
        (view.overview, view.folder_overview)
    };

    let written = draft::write_overview(&layout, "Greets people.")
        .unwrap()
        .unwrap();
    assert_eq!(shown(&mut database), (None, Some(written.clone())));
    seal_overview(&database, dir.path(), &written).unwrap();
    assert_eq!(shown(&mut database), (Some(written), None));
    std::fs::write(layout.join("overview.md"), "# Changed by hand\n").unwrap();
    assert_eq!(
        shown(&mut database),
        (None, Some("# Changed by hand".to_owned()))
    );
}

#[test]
fn a_shown_overview_holds_no_secret_like_path() {
    let dir = rust_repo();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir(&layout).unwrap();
    let mut database = Connection::open_in_memory().unwrap();
    crate::storage::plugins::initialize(&database).unwrap();
    crate::storage::setup::initialize(&database).unwrap();
    crate::project_setup::initialize(&database).unwrap();
    initialize(&database).unwrap();
    database
        .execute(
            "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
             VALUES ('demo',?1,'main','done','done',?2,1,1)",
            params![
                dir.path().to_str().unwrap(),
                serde_json::to_string(&Drafted::new("demo".into(), &pool())).unwrap()
            ],
        )
        .unwrap();
    let shown = |database: &mut Connection| {
        let tx = database.transaction().unwrap();
        let view = status(&tx, &ProjectId::new("demo").unwrap())
            .unwrap()
            .unwrap();
        (view.overview, view.folder_overview)
    };
    let leaked = concat!(
        "# Project overview\n\nGreets people.\n",
        "The code graph mentions an `api-keys.md` node, probably here. It was not read.\n\n",
        "## Open questions\n\n- Do docs/Api_Key.txt and config/secrets.toml hold keys?\n",
        "- Is notes/ui-direction.md current?",
    );
    let screened = concat!(
        "# Project overview\n\nGreets people.\n\n",
        "## Open questions\n\n",
        "- Is notes/ui-direction.md current?",
    );
    std::fs::write(layout.join("overview.md"), leaked).unwrap();
    assert_eq!(shown(&mut database), (None, Some(screened.to_owned())));
    seal_overview(&database, dir.path(), leaked).unwrap();
    assert_eq!(shown(&mut database), (Some(screened.to_owned()), None));
}

#[test]
fn saving_an_unchanged_draft_leaves_the_file_and_a_changed_one_replaces_it() {
    use std::os::unix::fs::MetadataExt;
    let dir = rust_repo();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir(&layout).unwrap();
    let file = layout.join("setup").join("draft.json");
    let mut saved = Drafted::new("Demo tool".into(), &pool());
    draft::save(&layout, &saved).unwrap();
    let first = std::fs::symlink_metadata(&file).unwrap();
    draft::save(&layout, &saved).unwrap();
    let again = std::fs::symlink_metadata(&file).unwrap();
    assert_eq!(
        (again.ino(), again.modified().unwrap()),
        (first.ino(), first.modified().unwrap())
    );
    let mut longer = std::fs::read(&file).unwrap();
    longer.push(b'\n');
    std::fs::write(&file, &longer).unwrap();
    draft::save(&layout, &saved).unwrap();
    assert_eq!(
        std::fs::read(&file).unwrap(),
        serde_json::to_vec_pretty(&saved).unwrap()
    );
    let kept = std::fs::symlink_metadata(&file).unwrap().ino();
    saved.draft.about = "Changed by the agent.".into();
    draft::save(&layout, &saved).unwrap();
    assert_ne!(std::fs::symlink_metadata(&file).unwrap().ino(), kept);
    assert_eq!(draft::reused(&layout), Some(saved));
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

#[test]
fn a_mapping_answer_without_an_overview_fails_and_keeps_its_fields() {
    let dir = rust_repo();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir(&layout).unwrap();
    let base = Drafted::new("demo".into(), &pool());
    let json_only = "```json\n{\"about\": \"Greets people.\", \"tech_stack\": [\"Rust\"]}\n```\n";
    let (step, kept, wrote) =
        service::answered(&layout, &base, json_only, &MappingFacts::default(), &[]);
    assert_eq!(wrote, None);
    assert_eq!(
        step,
        Step::Failed {
            reason: service::MAPPING_NO_OVERVIEW.into()
        }
    );
    assert_eq!(kept.draft.about, "Greets people.");
    assert_eq!(kept.draft.tech_stack, ["Rust"]);
    assert!(draft::overview(&layout).is_none());
    assert_eq!(draft::reused(&layout), Some(kept));

    let prose = "## Structure\n\nOne binary that greets people.\n\n```json\n{}\n```\n";
    let (step, _, wrote) = service::answered(&layout, &base, prose, &MappingFacts::default(), &[]);
    assert_eq!(step, Step::Done);
    assert_eq!(wrote, draft::overview(&layout));
    assert!(
        draft::overview(&layout)
            .unwrap()
            .contains("One binary that greets people.")
    );

    let (step, _, wrote) =
        service::answered(&layout, &base, json_only, &MappingFacts::default(), &[]);
    assert_eq!(wrote, None);
    assert_eq!(
        step,
        Step::Done,
        "an overview kept from before still counts"
    );
}

#[test]
fn a_secret_like_path_the_agent_writes_is_neither_stored_nor_written() {
    let dir = rust_repo();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir(&layout).unwrap();
    let text = concat!(
        "## Structure\n\nA CLI that prints greetings.\n",
        "The code graph mentions an `api-keys.md` node, probably here.\n\n",
        "## Project documents\n\n- notes/ui-direction.md covers the look.\n",
        "- **config/secrets.toml** and ./notes/api-keys.md were not read.\n\n",
        "```json\n{\"about\": \"Greets people; keys in notes/api-keys.md.\", ",
        "\"tech_stack\": [\"Rust\", \"config/secrets.toml\"], \"languages\": [\"Rust\"], ",
        "\"build_commands\": [\"cargo build\", \"cat notes/api-keys.md\"], ",
        "\"test_commands\": [\"cargo test\", \"source .env && cargo test\"], ",
        "\"entry_points\": [\"src/main.rs\", \"config/secrets.toml\"], ",
        "\"research\": [{\"kind\": \"competitor_research\", ",
        "\"files\": [\"docs/rivals.md\", \"notes/api-keys.md\"]}], ",
        "\"documents\": [\"notes/ui-direction.md\", \"./notes/api-keys.md\", \"docs/Api_Key.txt\"]}\n",
        "```\n",
    );

    let answer = draft::parse_answer(text);
    assert!(answer.overview.contains("A CLI that prints greetings."));
    assert!(
        answer
            .overview
            .contains("- notes/ui-direction.md covers the look.")
    );
    assert_eq!(answer.about, "A CLI that prints greetings.");
    assert_eq!(answer.tech_stack, ["Rust"]);
    assert_eq!(answer.facts.build_commands, ["cargo build"]);
    assert_eq!(answer.facts.test_commands, ["cargo test"]);
    assert_eq!(answer.facts.entry_points, ["src/main.rs"]);
    assert_eq!(answer.facts.research.len(), 1);
    assert_eq!(answer.facts.research[0].files, ["docs/rivals.md"]);
    assert_eq!(answer.facts.documents, ["notes/ui-direction.md"]);

    let found = MappingFacts {
        shown: vec![
            "docs/rivals.md".into(),
            "notes/ui-direction.md".into(),
            "notes/api-keys.md".into(),
            "docs/Api_Key.txt".into(),
        ],
        ..MappingFacts::default()
    };
    let base = Drafted::new("demo".into(), &pool());
    let (step, kept, wrote) = service::answered(&layout, &base, text, &found, &[]);
    assert_eq!(step, Step::Done);
    let stored = [
        format!("{answer:?}"),
        serde_json::to_string(&kept).unwrap(),
        wrote.unwrap_or_default(),
        draft::overview(&layout).unwrap_or_default(),
    ];
    for text in stored {
        let lower = text.to_lowercase();
        for name in ["api-keys", "api_key", "secrets.toml", ".env"] {
            assert!(!lower.contains(name), "{name} was kept in {text}");
        }
    }
}

#[test]
fn a_graph_folder_swapped_for_a_link_mid_gather_is_never_followed() {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = rust_repo();
    let repo = dir.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let other = outside.path().join("graphify-out");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("GRAPH_REPORT.md"), OTHER_GRAPH_MARKER).unwrap();
    let mark = graph::current_mark(&repo, &[]).unwrap();
    std::fs::write(other.join(graph::KEPT_OUT_MARK), &mark).unwrap();
    let graph = repo.join("graphify-out");
    std::fs::create_dir(&graph).unwrap();
    std::fs::write(graph.join("GRAPH_REPORT.md"), "# Own graph\n").unwrap();
    std::fs::write(graph.join(graph::KEPT_OUT_MARK), &mark).unwrap();
    let swap = repo.join("graphify-swap");
    symlink(&other, &swap).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut swaps = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                renameat_with(
                    CWD,
                    graph.as_path(),
                    CWD,
                    swap.as_path(),
                    RenameFlags::EXCHANGE,
                )
                .unwrap();
                swaps += 1;
            }
            swaps
        })
    };
    let deadline = std::time::Instant::now() + GATHER_RACE_TIME;
    let (mut gathers, mut own, mut leaks) = (0_u64, 0_u64, 0_u64);
    while gathers < GATHER_RACE_MAX && std::time::Instant::now() < deadline && leaks == 0 {
        match draft::gather(&repo, &[]).graph {
            Some(text) if text.contains(OTHER_GRAPH_MARKER) => leaks += 1,
            Some(text) if text.contains("Own graph") => own += 1,
            _ => {}
        }
        gathers += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 0);
    assert!(own > 0, "no gather read the own graph in {gathers} gathers");
    assert_eq!(
        leaks, 0,
        "a gather read the linked graph after {gathers} gathers and {swaps} swaps"
    );
}

#[test]
fn a_setup_folder_swapped_for_a_link_mid_save_is_never_written_through() {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = rust_repo();
    let layout = dir
        .path()
        .canonicalize()
        .unwrap()
        .join(CRAZYAGENT_DIRECTORY);
    let setup = layout.join("setup");
    std::fs::create_dir_all(&setup).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let other = outside.path().canonicalize().unwrap().join("setup");
    std::fs::create_dir(&other).unwrap();
    let swap = layout.join("setup-swap");
    symlink(&other, &swap).unwrap();
    let saved = Drafted::new("Demo tool".into(), &pool());
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut swaps = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                renameat_with(
                    CWD,
                    setup.as_path(),
                    CWD,
                    swap.as_path(),
                    RenameFlags::EXCHANGE,
                )
                .unwrap();
                swaps += 1;
            }
            swaps
        })
    };
    let deadline = std::time::Instant::now() + SAVE_RACE_TIME;
    let (mut saves, mut written, mut leaked) = (0_u64, 0_u64, Vec::new());
    while saves < SAVE_RACE_MAX && std::time::Instant::now() < deadline && leaked.is_empty() {
        if draft::save(&layout, &saved).is_ok() {
            written += 1;
        }
        leaked = std::fs::read_dir(&other)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        saves += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 0);
    assert!(written > 0, "no save landed in {saves} saves");
    assert!(
        leaked.is_empty(),
        "a save wrote {leaked:?} into the linked folder after {saves} saves and {swaps} swaps"
    );
}

/// On Linux the graph step itself refuses (DEC-094): `graph::update` ends failed with the
/// plain line before it looks for graphify, and it makes no `graphify-out` in the project.
#[cfg(target_os = "linux")]
#[test]
fn a_graph_build_on_linux_ends_failed_and_writes_nothing() {
    let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let repo = dir.path().canonicalize().unwrap();
    assert_eq!(
        run_graph(&repo, Path::new("/nonexistent/graphify"), &[]),
        graph::GraphEnding::Failed("Graph build is not available on Linux in this version.")
    );
    assert!(std::fs::read_dir(&repo).unwrap().next().is_none());
}

#[test]
fn only_linux_refuses_the_graph_build_with_a_plain_line() {
    assert_eq!(
        graph::unavailable("linux"),
        Some("Graph build is not available on Linux in this version.")
    );
    assert_eq!(graph::unavailable("macos"), None);
}

/// macOS only: by DEC-094 the graph build is refused on Linux, so a Linux host installs no
/// graphify for the engine to find (`only_linux_refuses_the_graph_build_with_a_plain_line`).
#[cfg(target_os = "macos")]
#[test]
fn the_graph_step_runs_the_graphify_setup_installed_for_this_engine() {
    let system = service::installed_graphify()
        .expect("BLOCKED: graphify not installed (~/.local/bin or Homebrew)");
    let root = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let bin = root.path().join("setup/bin");
    std::fs::create_dir_all(&bin).unwrap();
    symlink(&system, bin.join("graphify")).unwrap();
    assert_eq!(
        service::engine_graphify(root.path()),
        Some(bin.join("graphify"))
    );
    std::fs::remove_file(bin.join("graphify")).unwrap();
    let found = service::engine_graphify(root.path()).expect("the system graphify");
    assert_eq!(found.canonicalize().unwrap(), system);
}

/// Makes a stand-in graphify below `root`, laid out as uv 0.13 lays out a tool: the
/// environment's Python is a link into a version link (`cpython-3.12`) that leads to the
/// real installation (`cpython-3.12.16`), and `pyvenv.cfg` names the version link. The
/// stand-in writes an empty graph. Takes the folder. Returns the graphify file.
#[cfg(target_os = "macos")]
fn graphify_behind_a_version_link(root: &Path) -> std::path::PathBuf {
    let real = root.join("python/cpython-3.12.16/bin");
    let bin = root.join("tool/bin");
    for folder in [&real, &bin] {
        std::fs::create_dir_all(folder).unwrap();
    }
    crate::spawn_gate::guarded(|| std::fs::copy("/bin/bash", real.join("python3.12"))).unwrap();
    symlink(
        root.join("python/cpython-3.12.16"),
        root.join("python/cpython-3.12"),
    )
    .unwrap();
    let home = root.join("python/cpython-3.12/bin");
    symlink(home.join("python3.12"), bin.join("python")).unwrap();
    std::fs::write(
        root.join("tool/pyvenv.cfg"),
        format!("home = {}\nimplementation = CPython\n", home.display()),
    )
    .unwrap();
    let graphify = bin.join("graphify");
    let script = format!(
        "#!{}\nprintf '{{}}' > graphify-out/graph.json\n",
        bin.join("python").display()
    );
    crate::spawn_gate::guarded(|| std::fs::write(&graphify, script)).unwrap();
    std::fs::set_permissions(
        &graphify,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    graphify
}

#[cfg(target_os = "macos")]
#[test]
fn a_graph_is_built_by_a_graphify_whose_python_sits_behind_a_version_link() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_init(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    let graphify = graphify_behind_a_version_link(&root.join("uv"));
    assert_eq!(run_graph(&repo, &graphify, &[]), graph::GraphEnding::Built);
    assert_eq!(
        std::fs::read_to_string(repo.join("graphify-out/graph.json")).unwrap(),
        "{}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn the_graph_leaves_out_the_engine_files_in_the_project() {
    let graphify = service::installed_graphify()
        .expect("BLOCKED: graphify not installed (~/.local/bin or Homebrew)");
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/shop.py"),
        "def shop_total(items):\n    return sum(items)\n",
    )
    .unwrap();
    let engine = repo.join(".crazyagent").join("setup");
    std::fs::create_dir_all(&engine).unwrap();
    std::fs::write(
        engine.join("probe.py"),
        "def crazyagent_probe():\n    return 1\n",
    )
    .unwrap();
    std::fs::write(repo.join(".crazyagent/overview.md"), "# Overview\n").unwrap();
    let secret = repo.join("secret");
    std::fs::create_dir(&secret).unwrap();
    std::fs::write(
        secret.join("vault.py"),
        "def owner_vault_key():\n    return 1\n",
    )
    .unwrap();
    let scratch = tempfile::Builder::new()
        .prefix("ca-state-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = scratch.path().canonicalize().unwrap();
    for folder in ["agents", "profiles"] {
        std::fs::create_dir(state.join(folder)).unwrap();
    }

    let ending = graph::update(
        &graphify,
        &repo,
        &state,
        "onboard-g-hide",
        std::slice::from_ref(&secret),
        std::time::Duration::from_secs(300),
        &|| false,
    )
    .unwrap();

    assert_eq!(ending, graph::GraphEnding::Built);
    let graph = std::fs::read_to_string(repo.join("graphify-out/graph.json")).unwrap();
    assert!(graph.contains("shop_total"), "{graph}");
    for name in ["graph.json", "manifest.json", "GRAPH_REPORT.md"] {
        let text = std::fs::read_to_string(repo.join("graphify-out").join(name)).unwrap();
        assert!(
            !text.contains(".crazyagent") && !text.contains("crazyagent_probe"),
            "{name}: {text}"
        );
        assert!(
            !text.contains("owner_vault_key"),
            "the hidden owner folder leaked into {name}: {text}"
        );
    }
    assert!(engine.join("probe.py").is_file());
    assert!(secret.join("vault.py").is_file());
}

/// Builds the code graph of `repo` with the installed graphify and no hidden folders, in a
/// fresh engine state folder. Takes the project folder. Returns how the build ended.
#[cfg(target_os = "macos")]
fn build_graph(repo: &Path) -> graph::GraphEnding {
    let graphify = service::installed_graphify()
        .expect("BLOCKED: graphify not installed (~/.local/bin or Homebrew)");
    run_graph(repo, &graphify, &[])
}

fn run_graph(repo: &Path, graphify: &Path, hidden: &[std::path::PathBuf]) -> graph::GraphEnding {
    let scratch = tempfile::Builder::new()
        .prefix("ca-state-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = scratch.path().canonicalize().unwrap();
    for folder in ["agents", "profiles"] {
        std::fs::create_dir(state.join(folder)).unwrap();
    }
    graph::update(
        graphify,
        repo,
        &state,
        "onboard-g-keep-out",
        hidden,
        std::time::Duration::from_secs(300),
        &|| false,
    )
    .unwrap()
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn the_graph_never_reads_secret_like_or_git_ignored_files() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    write_file(
        &repo,
        "notes/ui-direction.md",
        "# UI direction\n\nDark first.\n",
    );
    let kept_out = [
        (
            "notes/api-keys.md",
            "# API keys\n\nDECOY_CANARY_8080\n\n## Rotation\n\nMonthly.\n",
        ),
        (
            "config/Credentials.py",
            "def credential_loader():\n    return 1\n",
        ),
        (
            "tokens/store.py",
            "def token_store_reader():\n    return 1\n",
        ),
        ("private/plan.md", "# Private plan heading\n"),
        ("src/gen/out.py", "def generated_ignored():\n    return 1\n"),
        ("local.py", "def excluded_local():\n    return 1\n"),
    ];
    for (path, text) in kept_out {
        write_file(&repo, path, text);
    }
    write_file(&repo, ".gitignore", "private/\n");
    write_file(&repo, "src/.gitignore", "gen/\n");
    write_file(&repo, ".git/info/exclude", "local.py\n");

    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    let graph = std::fs::read_to_string(repo.join("graphify-out/graph.json")).unwrap();
    assert!(
        graph.contains("shop_total") && graph.contains("UI direction"),
        "{graph}"
    );
    for name in ["graph.json", "manifest.json", "GRAPH_REPORT.md"] {
        let text = std::fs::read_to_string(repo.join("graphify-out").join(name)).unwrap();
        for leaked in [
            "api-keys",
            "API keys",
            "Rotation",
            "DECOY_CANARY_8080",
            "Credentials.py",
            "credential_loader",
            "token_store_reader",
            "Private plan heading",
            "generated_ignored",
            "excluded_local",
        ] {
            assert!(!text.contains(leaked), "{leaked} reached {name}: {text}");
        }
    }
    for (path, _) in kept_out {
        assert!(repo.join(path).is_file(), "{path} was changed");
    }
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn without_an_answer_from_git_no_graph_is_built() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    broken_git(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    write_file(
        &repo,
        "notes/api-keys.md",
        "# API keys\n\nDECOY_CANARY_8080\n",
    );

    assert_eq!(
        build_graph(&repo),
        graph::GraphEnding::Failed(
            "Graph not built: the project's git could not tell which files it ignores."
        )
    );
    assert!(
        !repo.join("graphify-out").exists(),
        "a graph step that failed its checks made graphify-out"
    );
}

#[test]
fn key_files_and_credential_stores_are_secret_like() {
    let missed: Vec<&str> = [
        "ssh/id_rsa",
        "ssh/id_dsa",
        "ssh/id_ecdsa.pub",
        "ssh/id_ed25519",
        ".htpasswd",
        "android/release.jks",
        "android/app.keystore",
        "kube/kubeconfig",
        "kube-config.yaml",
        "infra/terraform.tfstate",
        "infra/prod.tfvars",
        ".pgpass",
        ".npmrc",
        ".pypirc",
        "putty/server.ppk",
        "vault.kdbx",
    ]
    .into_iter()
    .filter(|name| !draft::names::names_secret(name))
    .collect();
    assert!(missed.is_empty(), "not secret-like: {missed:?}");
    for plain in [
        "src/keyboard.rs",
        "docs/monkey.md",
        "src/config.rs",
        "README.md",
    ] {
        assert!(
            !draft::names::names_secret(plain),
            "{plain} is not a secret"
        );
    }
}

#[test]
fn shell_cloud_and_gpg_credential_files_are_secret_like() {
    let missed: Vec<&str> = [
        ".envrc",
        "deploy/service-account.json",
        "gcp/my-service-account-prod.json",
        ".gnupg/trustdb.gpg",
        "backup/secring.gpg",
        "backup/pubring.kbx",
        ".dockercfg",
        "ops/.s3cfg",
    ]
    .into_iter()
    .filter(|name| !draft::names::names_secret(name))
    .collect();
    assert!(missed.is_empty(), "not secret-like: {missed:?}");
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_graph_marked_under_another_keep_out_never_reaches_the_prompt() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    write_file(
        &repo,
        "notes/rotation.md",
        "# Rotation schedule\n\nROTATION_CANARY_2020\n",
    );
    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    assert!(
        draft::gather(&repo, &[]).graph.is_some(),
        "the graph marked under the current keep-out is read"
    );

    write_file(&repo, ".gitignore", "notes/\n");
    let evidence = draft::gather(&repo, &[]);
    assert!(
        evidence.graph.is_none(),
        "a graph marked under another keep-out was read: {:?}",
        evidence.graph
    );
}

#[cfg(target_os = "macos")]
fn graphed_then_hidden() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Vec<std::path::PathBuf>,
) {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    write_file(
        &repo,
        "clientroadmap/plan.md",
        "# Clientroadmap acquisition heading\n\nACQUIRE_CANARY_6060\n",
    );
    write_file(
        &repo,
        "clientroadmap/deal.py",
        "def acquisition_target_reader():\n    return 1\n",
    );
    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    let graph = std::fs::read_to_string(repo.join("graphify-out/graph.json")).unwrap();
    assert!(graph.contains("acquisition_target_reader"), "{graph}");
    let hidden = vec![repo.join("clientroadmap")];
    (dir, repo, hidden)
}

#[cfg(target_os = "macos")]
fn assert_hidden_out_of_the_prompt(repo: &Path, hidden: &[std::path::PathBuf]) {
    let evidence = draft::gather(repo, hidden);
    assert!(
        evidence.graph.is_none(),
        "a graph built before the folder was hidden was read: {:?}",
        evidence.graph
    );
    let (found, _) = draft::found_facts(&evidence, repo);
    let prompt = draft::prompt(&evidence, &found);
    for leaked in [
        "clientroadmap",
        "Clientroadmap",
        "acquisition",
        "ACQUIRE_CANARY_6060",
    ] {
        assert!(!prompt.contains(leaked), "{leaked} reached the prompt");
    }
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_hidden_after_the_graph_never_reaches_the_prompt_with_the_graph_off() {
    let (_dir, repo, hidden) = graphed_then_hidden();
    assert_hidden_out_of_the_prompt(&repo, &hidden);
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_hidden_after_the_graph_never_reaches_the_prompt_when_graphify_is_missing() {
    let (_dir, repo, hidden) = graphed_then_hidden();
    assert_eq!(
        run_graph(&repo, Path::new("/nonexistent/graphify"), &hidden),
        graph::GraphEnding::Failed(graph::GRAPHIFY_MISSING)
    );
    assert_hidden_out_of_the_prompt(&repo, &hidden);
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_hidden_after_the_graph_never_reaches_the_prompt_when_the_keep_out_is_refused() {
    let (_dir, repo, hidden) = graphed_then_hidden();
    write_file(&repo, ".gitignore", "it's/\n");
    write_file(&repo, "it's/cache.py", "def cached():\n    return 1\n");
    let graphify = service::installed_graphify()
        .expect("BLOCKED: graphify not installed (~/.local/bin or Homebrew)");
    assert_eq!(
        run_graph(&repo, &graphify, &hidden),
        graph::GraphEnding::Failed(graph::GRAPH_KEEP_OUT_REFUSED)
    );
    assert_hidden_out_of_the_prompt(&repo, &hidden);
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn the_graph_never_reads_inside_a_nested_repository_or_submodule() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    let sdk = repo.join("vendor/sdk");
    std::fs::create_dir_all(&sdk).unwrap();
    git_init(&sdk);
    write_file(
        &repo,
        "vendor/sdk/notes/api-keys.md",
        "# SDK keys heading\n\nSDK_CANARY_5150\n",
    );
    write_file(
        &repo,
        "vendor/sdk/client.py",
        "def sdk_client_reader():\n    return 1\n",
    );
    git_in(&sdk, &["add", "-A"]);
    git_in(&sdk, &["commit", "-q", "-m", "sdk"]);
    git_in(&repo, &["add", "vendor/sdk"]);
    let helper = repo.join("tools/helper");
    std::fs::create_dir_all(&helper).unwrap();
    git_init(&helper);
    write_file(&repo, "tools/helper/.gitignore", "local_only.py\n");
    write_file(
        &repo,
        "tools/helper/local_only.py",
        "def nested_ignored_reader():\n    return 1\n",
    );
    write_file(&repo, "tools/helper/.git/info/exclude", "draft.md\n");
    write_file(
        &repo,
        "tools/helper/draft.md",
        "# Nested excluded heading\n",
    );
    write_file(
        &repo,
        "tools/helper/notes/tokens.md",
        "# Nested token notes\n",
    );

    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    let graph = std::fs::read_to_string(repo.join("graphify-out/graph.json")).unwrap();
    assert!(graph.contains("shop_total"), "{graph}");
    for name in ["graph.json", "manifest.json", "GRAPH_REPORT.md"] {
        let text = std::fs::read_to_string(repo.join("graphify-out").join(name)).unwrap();
        for leaked in [
            "SDK keys heading",
            "SDK_CANARY_5150",
            "api-keys",
            "nested_ignored_reader",
            "Nested excluded heading",
            "Nested token notes",
        ] {
            assert!(!text.contains(leaked), "{leaked} reached {name}: {text}");
        }
    }
}

#[test]
fn documents_inside_a_nested_repository_are_never_read() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    write_file(
        root,
        "docs/plan.md",
        "Release plan for sync. PLAN_TEXT_4410\n",
    );
    let helper = root.join("tools/helper");
    std::fs::create_dir_all(&helper).unwrap();
    git_init(&helper);
    write_file(root, "tools/helper/.git/info/exclude", "notes.md\n");
    write_file(
        root,
        "tools/helper/notes.md",
        "Nested private notes. NESTED_PRIVATE_1234\n",
    );

    let prompt = mapping_prompt(root);
    assert!(prompt.contains("PLAN_TEXT_4410"), "{prompt}");
    assert!(
        !prompt.contains("NESTED_PRIVATE_1234") && !prompt.contains("tools/helper/notes.md"),
        "a nested repository's excluded file reached the prompt"
    );
}

#[test]
fn a_folder_outside_git_maps_like_a_repository_without_ignores() {
    let dir = rust_repo();
    let root = dir.path();
    write_file(
        root,
        "docs/plan.md",
        "Release plan for sync. PLAN_TEXT_4410\n",
    );
    write_file(root, "notes/api-keys.md", "# API keys\n\nKEY_TEXT_9090\n");
    write_file(root, "secrets.txt", "SECRET_TEXT_7070\n");
    write_file(
        root,
        "README.md",
        concat!(
            "# Demo\n\nA small greeting tool. README_KEPT_1111\n",
            "Keys live in notes/api-keys.md.\n",
            "Plans live in docs/plan.md.\n",
        ),
    );

    let evidence = draft::gather(root, &[]);
    assert!(!evidence.ignores_unknown);
    for entry in ["docs/", "src/", "Cargo.toml"] {
        assert!(
            evidence.entries.iter().any(|listed| listed == entry),
            "{entry} not listed: {:?}",
            evidence.entries
        );
    }
    assert!(
        evidence
            .documents
            .iter()
            .any(|document| document.path == "docs/plan.md"),
        "the document was not read"
    );
    let prompt = mapping_prompt(root);
    for kept in [
        "PLAN_TEXT_4410",
        "README_KEPT_1111",
        "Plans live in docs/plan.md.",
    ] {
        assert!(prompt.contains(kept), "{kept} missing: {prompt}");
    }
    for leaked in [
        "api-keys",
        "KEY_TEXT_9090",
        "SECRET_TEXT_7070",
        "secrets.txt",
        "could not tell which files it ignores",
    ] {
        assert!(!prompt.contains(leaked), "{leaked} reached the prompt");
    }
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_outside_git_gets_a_graph_without_its_secret_like_files() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    write_file(
        &repo,
        "notes/ui-direction.md",
        "# UI direction\n\nDark first.\n",
    );
    write_file(
        &repo,
        "notes/api-keys.md",
        "# API keys\n\nDECOY_CANARY_8080\n",
    );
    write_file(
        &repo,
        "config/Credentials.py",
        "def credential_loader():\n    return 1\n",
    );
    write_file(
        &repo,
        "tokens/store.py",
        "def token_store_reader():\n    return 1\n",
    );

    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    let graph = std::fs::read_to_string(repo.join("graphify-out/graph.json")).unwrap();
    assert!(
        graph.contains("shop_total") && graph.contains("UI direction"),
        "{graph}"
    );
    for name in ["graph.json", "manifest.json", "GRAPH_REPORT.md"] {
        let text = std::fs::read_to_string(repo.join("graphify-out").join(name)).unwrap();
        for leaked in [
            "api-keys",
            "API keys",
            "DECOY_CANARY_8080",
            "credential_loader",
            "token_store_reader",
        ] {
            assert!(!text.contains(leaked), "{leaked} reached {name}: {text}");
        }
    }
}

#[test]
fn a_graph_made_before_the_keep_out_never_reaches_the_prompt() {
    let dir = rust_repo();
    let root = dir.path();
    git_init(root);
    write_file(
        root,
        "graphify-out/GRAPH_REPORT.md",
        "# Graph Report\n\n- Rotation schedule OLD_GRAPH_3030\n",
    );

    let evidence = draft::gather(root, &[]);
    assert!(
        evidence.graph.is_none(),
        "a graph not made under the keep-out was read: {:?}",
        evidence.graph
    );
    assert!(!mapping_prompt(root).contains("OLD_GRAPH_3030"));
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_graph_made_before_the_keep_out_is_rebuilt_from_nothing() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    write_file(&repo, "graphify-out/old-notes.txt", "OLD_GRAPH_3030\n");
    write_file(
        &repo,
        "graphify-out/GRAPH_REPORT.md",
        "# Graph Report\n\n- OLD_GRAPH_3030\n",
    );

    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    assert!(
        !repo.join("graphify-out/old-notes.txt").exists(),
        "the old graph folder was kept"
    );
    let report = std::fs::read_to_string(repo.join("graphify-out/GRAPH_REPORT.md")).unwrap();
    assert!(!report.contains("OLD_GRAPH_3030"), "{report}");
    let evidence = draft::gather(&repo, &[]);
    assert!(
        evidence
            .graph
            .as_deref()
            .is_some_and(|graph| !graph.contains("OLD_GRAPH_3030")),
        "the new graph is not read: {:?}",
        evidence.graph
    );
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_graph_update_drops_a_file_kept_out_since_the_last_build() {
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    write_file(
        &repo,
        "src/shop.py",
        "def shop_total(items):\n    return sum(items)\n",
    );
    write_file(
        &repo,
        "notes/rotation.md",
        "# Rotation schedule\n\nROTATION_CANARY_2020\n",
    );
    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    let first = std::fs::read_to_string(repo.join("graphify-out/graph.json")).unwrap();
    assert!(first.contains("Rotation schedule"), "{first}");

    write_file(&repo, ".gitignore", "notes/rotation.md\n");
    assert_eq!(build_graph(&repo), graph::GraphEnding::Built);
    for name in ["graph.json", "manifest.json", "GRAPH_REPORT.md"] {
        let text = std::fs::read_to_string(repo.join("graphify-out").join(name)).unwrap();
        for leaked in ["Rotation schedule", "ROTATION_CANARY_2020", "rotation.md"] {
            assert!(!text.contains(leaked), "{leaked} stayed in {name}: {text}");
        }
    }
    assert!(repo.join("notes/rotation.md").is_file());
}

#[test]
fn a_crazyagent_folder_swapped_for_a_link_is_never_read_or_written_through() {
    let dir = rust_repo();
    let outside = tempfile::tempdir().unwrap();
    let elsewhere = outside.path().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("setup")).unwrap();
    let planted = Drafted::new("Planted".into(), &pool());
    let planted_bytes = serde_json::to_vec_pretty(&planted).unwrap();
    std::fs::write(elsewhere.join("setup/draft.json"), &planted_bytes).unwrap();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    symlink(&elsewhere, &layout).unwrap();
    assert!(draft::reused(&layout).is_none());
    let mut changed = planted.clone();
    changed.draft.about = "Written by the engine.".into();
    assert!(draft::save(&layout, &changed).is_err());
    assert_eq!(
        std::fs::read(elsewhere.join("setup/draft.json")).unwrap(),
        planted_bytes
    );

    let empty = outside.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    std::fs::remove_file(&layout).unwrap();
    symlink(&empty, &layout).unwrap();
    assert!(draft::save(&layout, &changed).is_err());
    assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);
}

#[test]
fn an_overview_entry_that_is_not_a_text_file_is_reported_and_left_as_it_is() {
    let prose = "## Structure\n\nOne binary that greets people.\n\n```json\n{\"about\": \"Greets people.\"}\n```\n";
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("notes.md");
    std::fs::write(&target, "# Notes\n\nKept outside.\n").unwrap();
    type Make<'a> = &'a dyn Fn(&Path);
    let entries: [(&str, Make); 3] = [
        ("an empty file", &|path| std::fs::write(path, "").unwrap()),
        ("a link", &|path| symlink(&target, path).unwrap()),
        ("a folder", &|path| std::fs::create_dir(path).unwrap()),
    ];
    for (what, make) in entries {
        let dir = rust_repo();
        let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
        std::fs::create_dir(&layout).unwrap();
        let entry = layout.join("overview.md");
        make(entry.as_path());
        let before = std::fs::symlink_metadata(&entry).unwrap().file_type();
        let base = Drafted::new("demo".into(), &pool());
        let (step, kept, wrote) =
            service::answered(&layout, &base, prose, &MappingFacts::default(), &[]);
        assert_eq!(step, Step::failed(service::OVERVIEW_BLOCKED), "{what}");
        assert_eq!(wrote, None, "{what}");
        assert_eq!(kept.draft.about, "Greets people.", "{what}");
        assert_eq!(draft::reused(&layout), Some(kept), "{what}");
        assert_eq!(
            std::fs::symlink_metadata(&entry).unwrap().file_type(),
            before,
            "{what}"
        );
        assert!(draft::overview(&layout).is_none(), "{what}");
    }
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# Notes\n\nKept outside.\n"
    );
}

#[test]
fn a_setup_saved_without_the_mappings_answers_offers_them_once_the_mapping_is_done() {
    let dir = rust_repo();
    let mut database = Connection::open_in_memory().unwrap();
    crate::storage::plugins::initialize(&database).unwrap();
    crate::storage::setup::initialize(&database).unwrap();
    crate::project_setup::initialize(&database).unwrap();
    initialize(&database).unwrap();
    let mut drafted = Drafted::new("demo".into(), &pool());
    drafted.draft.about = "Finds papers.".into();
    drafted.draft.tech_stack = vec!["Python".into()];
    database
        .execute(
            "INSERT INTO onboarding(project,repo,pool,graph_state,mapping_state,drafted,created_ms,updated_ms)
             VALUES ('demo',?1,'main','failed','running',?2,1,1)",
            params![
                dir.path().to_str().unwrap(),
                serde_json::to_string(&drafted).unwrap()
            ],
        )
        .unwrap();
    let mut stored = StoredSetup {
        project_id: ProjectId::new("demo").unwrap(),
        name: "demo".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: Some(pool()),
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 16,
        },
        documents: vec![],
        folder: dir.path().to_str().unwrap().into(),
        created_ms: 1,
        revision: 2,
    };
    database
        .execute(
            "INSERT INTO project_setups VALUES ('demo',?1)",
            [serde_json::to_string(&stored).unwrap()],
        )
        .unwrap();
    let viewed = |database: &mut Connection| {
        let tx = database.transaction().unwrap();
        status(&tx, &ProjectId::new("demo").unwrap())
            .unwrap()
            .unwrap()
    };

    let running = viewed(&mut database);
    assert!(running.finished);
    assert_eq!(running.proposed, None);
    database
        .execute("UPDATE onboarding SET mapping_state='done'", [])
        .unwrap();
    let done = viewed(&mut database);
    assert_eq!(done.draft.about, "");
    assert_eq!(done.proposed, Some(drafted.draft.clone()));
    stored.tech_stack = vec!["Rust".into()];
    database
        .execute(
            "UPDATE project_setups SET body=?1",
            [serde_json::to_string(&stored).unwrap()],
        )
        .unwrap();
    assert_eq!(viewed(&mut database).proposed, None);
}
