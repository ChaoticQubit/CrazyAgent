#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::onboarding::Step;
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

fn log(folder: &Path) -> String {
    String::from_utf8(git(folder, &["log", "--format=%s"]).stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_folders_move_and_copy_into_the_home_and_onboarding_reports_its_steps() {
    let mut engine = Engine::start("ca-e2e-add-");
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

    let unconfigured = CredentialRef::new("e2e-unconfigured-pool").unwrap();
    let generation = owner.next();
    let started = result(
        owner
            .controller
            .start_onboarding(
                moved_home.to_str().unwrap().into(),
                unconfigured,
                generation,
            )
            .await
            .unwrap(),
    );
    assert!(
        !matches!(started, OwnerResult::Rejected { .. }),
        "onboarding starts before any pool can run the mapping agent: {started:?}"
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
        if matches!(view.graph, Step::Done | Step::Failed { .. }) {
            break view;
        }
        assert!(
            start.elapsed() < GRAPH_TIME,
            "the graph step never finished: {:?}",
            view.graph
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    if let Step::Failed { reason } = &view.graph {
        panic!("{GRAPH_PREREQUISITE}: {reason}");
    }
    assert!(!view.name.is_empty());
    match &view.mapping {
        Step::Waiting => {}
        Step::Failed { reason } => assert!(
            reason.to_lowercase().contains("pool"),
            "the mapping step names the missing pool in plain words: {reason}"
        ),
        other => panic!("no pool can run the mapping agent, so mapping cannot report {other:?}"),
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let later = owner.controller.onboarding(project).await.unwrap();
    assert_eq!(
        later.mapping, view.mapping,
        "without a pool the mapping step stays put and never reports a made-up result"
    );
    assert_eq!(
        log(&moved_home),
        "first\n",
        "onboarding leaves the history alone"
    );
    owner.controller.detach();
    engine.stop();
}
