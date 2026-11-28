#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::Client;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerData, OwnerSummary, OwnerView};
use crazyagent::protocol::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

/// The most projects the desktop list keeps, the bridge's `PROJECT_LIST_MAX`.
const LIST_MAX: usize = 200;

/// The page size the desktop list reads, the bridge's `PAGE_SIZE`.
const PAGE_SIZE: u8 = 16;

/// Creates one project with no repositories through the engine's wire socket. Inputs: the
/// engine folder and the project id. Output: nothing, or the engine error.
async fn add_project(root: &Path, id: &str) -> Result<(), crazyagent::engine::EngineError> {
    let client = Client::new(root.join("e.sock"), Duration::from_secs(2))?;
    let state = client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("list-seed").unwrap(),
            request_id: RequestId::new(format!("snapshot-{id}")).unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::Snapshot,
        })
        .await?;
    let reply = client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("list-seed").unwrap(),
            request_id: RequestId::new(format!("create-{id}")).unwrap(),
            expected_revision: state.revision,
            command: Command::CreateProject {
                project_id: ProjectId::new(id).unwrap(),
                spec: ProjectSpec {
                    name: id.into(),
                    repositories: vec![],
                },
            },
        })
        .await?;
    if reply.outcome != Outcome::Applied {
        return Err(crazyagent::engine::EngineError(format!(
            "create failed: {:?}",
            reply.outcome
        )));
    }
    Ok(())
}

/// Reads the project list page by page as the desktop bridge's `project_list` does: pages
/// of 16 until 200 are held or no page follows, at most 201 reads, then cut to 200. Input:
/// the controller. Output: the kept ids and whether the engine holds more.
async fn project_list(
    controller: &mut OwnerController,
) -> Result<(Vec<String>, bool), crazyagent::engine::EngineError> {
    let mut ids = Vec::new();
    let mut cursor = None;
    let mut more = true;
    for _ in 0..=LIST_MAX {
        let state = controller
            .read_page(OwnerView::Projects, cursor.take(), Some(PAGE_SIZE))
            .await?;
        let OwnerData::Page { items, next } = state.data else {
            panic!("a projects page");
        };
        assert!(items.len() <= usize::from(PAGE_SIZE));
        for item in items {
            let OwnerSummary::Project { project_id, .. } = item else {
                panic!("a project item");
            };
            ids.push(project_id.as_str().to_owned());
        }
        match next {
            Some(next) if ids.len() < LIST_MAX => cursor = Some(next),
            next => {
                more = next.is_some();
                break;
            }
        }
    }
    more = more || ids.len() > LIST_MAX;
    ids.truncate(LIST_MAX);
    Ok((ids, more))
}

/// Seeds projects `p000` up to the count on a fresh engine and reads the desktop list.
/// Input: how many projects. Output: the kept ids and whether more follow.
async fn listed(count: usize) -> (Vec<String>, bool) {
    let root = tempfile::Builder::new()
        .prefix("ca-list-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config =
        crazyagent::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let observed: Result<_, crazyagent::engine::EngineError> = async {
        for n in 0..count {
            add_project(root.path(), &format!("p{n:03}")).await?;
        }
        let mut controller = OwnerController::attach(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await?;
        project_list(&mut controller).await
    }
    .await;
    service.shutdown().await.unwrap();
    observed.unwrap()
}

/// The desktop list over the projects plugin keeps the first 200 projects in id order and
/// says more follow when 201 exist, and keeps all 200 with none following when 200 exist.
#[tokio::test]
async fn project_list_cuts_at_200_like_2_4_4() {
    let expected: Vec<String> = (0..LIST_MAX).map(|n| format!("p{n:03}")).collect();
    let (ids, more) = listed(LIST_MAX + 1).await;
    assert_eq!(ids, expected);
    assert!(more);
    let (ids, more) = listed(LIST_MAX).await;
    assert_eq!(ids, expected);
    assert!(!more);
}
