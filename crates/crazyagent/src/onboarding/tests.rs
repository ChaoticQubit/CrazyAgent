use super::draft::{self, Drafted};
use super::*;
use crate::project_setup::Platform;
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

#[test]
fn a_mapping_answer_without_an_overview_fails_and_keeps_its_fields() {
    let dir = rust_repo();
    let layout = dir.path().join(CRAZYAGENT_DIRECTORY);
    std::fs::create_dir(&layout).unwrap();
    let base = Drafted::new("demo".into(), &pool());
    let json_only = "```json\n{\"about\": \"Greets people.\", \"tech_stack\": [\"Rust\"]}\n```\n";
    let (step, kept) = service::answered(&layout, &base, json_only, &MappingFacts::default(), &[]);
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
    let (step, _) = service::answered(&layout, &base, prose, &MappingFacts::default(), &[]);
    assert_eq!(step, Step::Done);
    assert!(
        draft::overview(&layout)
            .unwrap()
            .contains("One binary that greets people.")
    );

    let (step, _) = service::answered(&layout, &base, json_only, &MappingFacts::default(), &[]);
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
    }
    assert!(engine.join("probe.py").is_file());
}
