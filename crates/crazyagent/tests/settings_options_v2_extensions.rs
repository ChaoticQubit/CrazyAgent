#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::ide::extensions::{self, AskView, Asked};
use crazyagent::protocol::ProjectId;
use e2e_support::{Engine, Owner, scratch};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const EXTENSION: &str = "redhat.vscode-yaml";
const VERSION: &str = "1.15.0";

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// The versions the IDE server is let install for one extension, from the locked
/// `extensions.allowed` setting the engine writes before each server start.
/// Input: the engine. Output: the allowed versions, none when it is not allowed.
fn allowed(engine: &Engine, id: &str) -> Option<serde_json::Value> {
    let setting = extensions::allowed_setting(engine.path());
    let listed = setting["extensions.allowed"]
        .as_object()
        .expect("the setting lists the allowed extensions");
    assert!(
        !listed.is_empty(),
        "the built-in extension is always allowed"
    );
    listed.get(id).cloned()
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_decline_drops_the_ask_and_keeps_the_extension_out_of_the_ide() {
    let mut engine = Engine::start("ca-v2opt-ext-decline-");
    let mut owner = Owner::attach(&engine).await;
    let disk = scratch("ca-v2opt-ext-home-");
    let home = disk.path().join("Projects");
    fs::create_dir(&home).unwrap();
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        owner.create_project("v2-ext", "Buoy extensions").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new("v2-ext").unwrap();

    assert_eq!(
        extensions::ask(engine.path(), &project, EXTENSION, VERSION, now_ms()).unwrap(),
        Asked::Confirm,
        "the IDE's install asks the owner first"
    );
    let client = owner.controller.owner_client().unwrap();
    assert_eq!(
        client.ide_extension_asks(project.clone()).await.unwrap(),
        vec![AskView {
            id: EXTENSION.into(),
            version: VERSION.into(),
            current: None,
        }]
    );
    assert_eq!(allowed(&engine, EXTENSION), None);

    client
        .ide_extension_decline(project.clone(), EXTENSION.into(), VERSION.into())
        .await
        .unwrap();
    assert_eq!(
        client.ide_extension_asks(project.clone()).await.unwrap(),
        Vec::<AskView>::new(),
        "Not now drops the ask"
    );
    assert_eq!(
        allowed(&engine, EXTENSION),
        None,
        "a declined extension is never allowed in the IDE"
    );

    engine.restart();
    let owner = Owner::attach(&engine).await;
    let client = owner.controller.owner_client().unwrap();
    assert_eq!(
        client.ide_extension_asks(project.clone()).await.unwrap(),
        Vec::<AskView>::new(),
        "the declined ask stays dropped after a restart"
    );
    assert_eq!(
        extensions::ask(engine.path(), &project, EXTENSION, VERSION, now_ms()).unwrap(),
        Asked::Confirm,
        "the IDE must ask again: a decline approves nothing"
    );
}
