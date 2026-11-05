use super::draft::{self, Drafted};
use super::*;
use crate::project_setup::{Platform, ResearchKind};
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

fn write_file(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn documents_with_unrelated_names_are_read_from_their_text_and_mapped_to_research() {
    let dir = rust_repo();
    let root = dir.path();
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
    write_file(root, "docs/plan.md", "Release plan for sync. PLAN_TEXT_4410\n");
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

    let evidence = draft::gather(root);
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
    for index in 0..60 {
        write_file(
            dir.path(),
            &format!("docs/note-{index:02}.md"),
            &format!("NOTE_{index:02} {}\n", "y".repeat(7 * 1024)),
        );
    }
    let evidence = draft::gather(dir.path());
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
    for index in 0..45 {
        write_file(
            many.path(),
            &format!("notes/n-{index:02}.txt"),
            &format!("Short note {index}.\n"),
        );
    }
    let evidence = draft::gather(many.path());
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
    let graph = repo.join("graphify-out");
    std::fs::create_dir(&graph).unwrap();
    std::fs::write(graph.join("GRAPH_REPORT.md"), "# Own graph\n").unwrap();
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
        match draft::gather(&repo).graph {
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

#[test]
fn only_linux_refuses_the_graph_build_with_a_plain_line() {
    assert_eq!(
        graph::unavailable("linux"),
        Some("Graph build is not available on Linux in this version.")
    );
    assert_eq!(graph::unavailable("macos"), None);
}

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
