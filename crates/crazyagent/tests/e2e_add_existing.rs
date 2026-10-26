#![cfg(target_os = "macos")]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::onboarding::Step;
use crazyagent::onboarding::service::CODEX_MISSING;
use crazyagent::plugins::Scope;
use crazyagent::projects_home::{AddMode, FoundState};
use crazyagent::protocol::CredentialRef;
use e2e_support::{Engine, Owner, git, named, rejected, repository, result, scratch};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const GRAPH_TIME: Duration = Duration::from_secs(180);
const GRAPH_PREREQUISITE: &str =
    "UNAVAILABLE: graphify could not build the project graph; run Setup to install uv and graphify";
const README: &str = "# Tide gauge\n\nReads buoy sensors.\n";
const POOL: &str = "e2e-local-pool";
const CATALOG: &str = "version = 1\ntimeout_ms = 5000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"e2e-local-pool\"\nmembers = [\"local-connection\"]\nprojects = [\"tide-gauge\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n";

async fn add(owner: &mut Owner, source: &Path, mode: AddMode) -> OwnerResult {
    let generation = owner.next();
    result(
        owner
            .controller
            .add_existing_folder(source.to_str().unwrap().into(), mode, generation)
            .await
            .unwrap(),
    )
}

fn settled(step: &Step) -> bool {
    matches!(step, Step::Done | Step::Failed { .. })
}

fn log(folder: &Path) -> String {
    String::from_utf8(git(folder, &["log", "--format=%s"]).stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_folders_move_and_copy_into_the_home_and_onboarding_reports_its_steps() {
    let mut engine = Engine::new("ca-e2e-add-");
    fs::write(engine.path().join("global.toml"), CATALOG).unwrap();
    engine.launch();
    let disk = scratch("ca-e2e-src-");
    let home = disk.path().join("Projects");
    let sources = disk.path().join("sources");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&sources).unwrap();
    let moved = sources.join("tide-gauge");
    let copied = sources.join("buoy-alerts");
    repository(&moved, "README.md", README);
    repository(&copied, "README.md", README);
    let mut owner = Owner::attach(&engine).await;
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);

    assert_eq!(
        add(&mut owner, &moved, AddMode::Move).await,
        OwnerResult::Applied
    );
    assert!(!moved.exists(), "a move leaves nothing at the source");
    let moved_home = home.join("tide-gauge");
    assert_eq!(
        fs::read_to_string(moved_home.join("README.md")).unwrap(),
        README
    );
    assert_eq!(log(&moved_home), "first\n");

    let size = owner
        .controller
        .folder_size(copied.to_str().unwrap().into())
        .await
        .unwrap();
    assert!(size >= README.len() as u64, "{size}");
    assert_eq!(
        add(&mut owner, &copied, AddMode::Copy).await,
        OwnerResult::Applied
    );
    assert_eq!(
        fs::read_to_string(copied.join("README.md")).unwrap(),
        README
    );
    let copied_home = home.join("buoy-alerts");
    assert_eq!(
        fs::read_to_string(copied_home.join("README.md")).unwrap(),
        README
    );
    assert_eq!(log(&copied_home), "first\n");
    assert_eq!(log(&copied), "first\n");
    assert_eq!(
        add(&mut owner, &copied, AddMode::Copy).await,
        rejected("home_destination_exists")
    );

    let found = owner
        .found_until("both added folders show as Found", |found| {
            named(found, "tide-gauge").is_some() && named(found, "buoy-alerts").is_some()
        })
        .await;
    for name in ["tide-gauge", "buoy-alerts"] {
        let entry = named(&found, name).unwrap();
        assert_eq!(entry.state, FoundState::Found, "{entry:?}");
        assert!(
            entry.git && !entry.known && entry.project_id.is_none(),
            "{entry:?}"
        );
        assert_eq!(entry.path, home.join(name).to_str().unwrap());
    }

    let skill = disk.path().join("skills").join("graphify");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: graphify\ndescription: Reads the project's code graph.\n---\n",
    )
    .unwrap();
    owner
        .controller
        .add_skill(Scope::Global, skill.to_str().unwrap().into())
        .await
        .unwrap();
    let pool = CredentialRef::new(POOL).unwrap();
    let generation = owner.next();
    let started = result(
        owner
            .controller
            .start_onboarding(moved_home.to_str().unwrap().into(), pool, generation)
            .await
            .unwrap(),
    );
    assert!(
        !matches!(started, OwnerResult::Rejected { .. }),
        "onboarding starts on the pool the owner picked: {started:?}"
    );
    let found = owner
        .found_until("the onboarded folder is registered as a project", |found| {
            named(found, "tide-gauge").is_some_and(|entry| entry.project_id.is_some())
        })
        .await;
    let entry = named(&found, "tide-gauge").unwrap();
    assert!(
        matches!(entry.state, FoundState::Onboarding | FoundState::Ready),
        "{entry:?}"
    );
    assert_eq!(
        named(&found, "buoy-alerts").unwrap().state,
        FoundState::Found,
        "only the chosen folder starts onboarding"
    );
    let project = entry.project_id.clone().unwrap();

    let start = Instant::now();
    let view = loop {
        let view = owner.controller.onboarding(project.clone()).await.unwrap();
        if settled(&view.graph) && settled(&view.mapping) {
            break view;
        }
        assert!(
            start.elapsed() < GRAPH_TIME,
            "onboarding never settled: {view:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    if let Step::Failed { reason } = &view.graph {
        panic!("{GRAPH_PREREQUISITE}: {reason}");
    }
    assert!(!view.name.is_empty());
    assert_eq!(
        view.mapping,
        Step::Failed {
            reason: CODEX_MISSING.into()
        },
        "an engine started without Codex or Claude Code names the missing harness in plain words"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let later = owner.controller.onboarding(project).await.unwrap();
    assert_eq!(
        later.mapping, view.mapping,
        "without a harness the mapping step stays put and never reports a made-up result"
    );
    assert_eq!(
        log(&moved_home),
        "first\n",
        "onboarding leaves the history alone"
    );
    owner.controller.detach();
    engine.stop();
}
