#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::projects_home::{FoundState, ProjectsHome};
use crazyagent::setup::pack::default_pack;
use crazyagent::setup::{Dependency, DependencyState};
use e2e_support::{Engine, Owner, named, profile, rejected, scratch};
use std::fs;

#[tokio::test(flavor = "multi_thread")]
async fn first_run_saves_the_profile_picks_the_home_reads_setup_and_reaches_home() {
    let mut engine = Engine::start("ca-e2e-first-");
    let disk = scratch("ca-e2e-disk-");
    let home = disk.path().join("Projects");
    fs::create_dir(&home).unwrap();
    let mut owner = Owner::attach(&engine).await;

    assert_eq!(owner.controller.owner_profile().await.unwrap(), None);
    assert_eq!(owner.save_profile(profile()).await, OwnerResult::Applied);
    assert_eq!(
        owner.controller.owner_profile().await.unwrap(),
        Some(profile())
    );

    assert_eq!(
        owner.controller.projects_home().await.unwrap(),
        ProjectsHome {
            path: None,
            watching: false
        }
    );
    assert_eq!(
        owner.set_home(engine.path()).await,
        rejected("projects_home_unsafe")
    );
    assert_eq!(
        owner.set_home(&disk.path().join("absent")).await,
        rejected("projects_home_missing")
    );
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        owner.controller.projects_home().await.unwrap(),
        ProjectsHome {
            path: Some(home.to_str().unwrap().into()),
            watching: true
        }
    );

    let status = owner.controller.setup_status().await.unwrap();
    assert!(!status.running);
    let listed = status
        .dependencies
        .iter()
        .map(|dependency| dependency.name)
        .collect::<Vec<_>>();
    assert_eq!(listed, Dependency::ALL);
    for dependency in &status.dependencies {
        assert!(!dependency.label.is_empty());
        match &dependency.state {
            DependencyState::Missing => {}
            DependencyState::Found { version } => {
                assert!(!version.as_str().is_empty(), "{dependency:?}")
            }
            other => panic!("nothing ran yet, so a tool is found or missing: {other:?}"),
        }
    }
    let shipped = default_pack().unwrap();
    let offered = status
        .pack
        .iter()
        .map(|entry| (entry.id.as_str(), entry.on_by_default))
        .collect::<Vec<_>>();
    let expected = shipped
        .entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry.on_by_default))
        .collect::<Vec<_>>();
    assert_eq!(offered, expected);

    assert!(owner.controller.found_projects().await.unwrap().is_empty());
    fs::create_dir(home.join("reef-notes")).unwrap();
    let found = owner
        .found_until("a folder dropped into the home shows on Home", |found| {
            named(found, "reef-notes").is_some()
        })
        .await;
    assert_eq!(
        named(&found, "reef-notes").unwrap().state,
        FoundState::Found
    );

    owner.controller.detach();
    engine.restart();
    let mut owner = Owner::attach(&engine).await;
    assert_eq!(
        owner.controller.owner_profile().await.unwrap(),
        Some(profile()),
        "the next launch opens Home, not Welcome"
    );
    assert_eq!(
        owner.controller.projects_home().await.unwrap(),
        ProjectsHome {
            path: Some(home.to_str().unwrap().into()),
            watching: true
        }
    );
    owner
        .found_until("the home is watched again after a restart", |found| {
            named(found, "reef-notes").is_some()
        })
        .await;
    owner.controller.detach();
    let stderr = engine.stop();
    assert!(!stderr.contains(&profile().bio), "{stderr}");
}
