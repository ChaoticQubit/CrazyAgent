use super::super::app::{App, Focus, UiEvent};
use super::*;
use crate::coordination::{OwnerAction, PlanAuthority, Role, WorkItem, WorkPlan};
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerControl, OwnerDecision, OwnerResult};
use crate::protocol::{
    CredentialRef, ExecutionContext, Outcome, PlanId, SessionId, SnapshotId, TaskId,
};
use crate::routing::{ResourceBudget, RouteRequest};
use std::os::unix::fs::PermissionsExt;
use tokio::sync::Mutex;

fn plan(config_id: ConfigId) -> WorkPlan {
    let scope = ExecutionContext {
        project_id: ProjectId::new("dispatch-ui-project").unwrap(),
        task_id: TaskId::new("dispatch-ui-parent").unwrap(),
        config_id: config_id.clone(),
        snapshot_id: SnapshotId::new("dispatch-ui-snapshot").unwrap(),
        baseline_id: BaselineId::new("dispatch-ui-baseline").unwrap(),
        allowed_destinations: vec![],
        budget_ref: CredentialRef::new("dispatch-ui-budget").unwrap(),
    };
    WorkPlan {
        id: PlanId::new("dispatch-ui-plan").unwrap(),
        config_id,
        tasks: vec![WorkItem {
            id: TaskId::new("dispatch-ui-item").unwrap(),
            session_id: SessionId::new("dispatch-ui-session").unwrap(),
            role: Role::Worker,
            native_descendant: None,
            pool_ref: None,
            route: RouteRequest {
                preferred_target: None,
                required_target: None,
                required_capabilities: vec![],
                account_ref: CredentialRef::new("dispatch-ui-account").unwrap(),
                budget_ref: scope.budget_ref.clone(),
                resources: ResourceBudget::unit(),
            },
        }],
        scope,
        dependencies: vec![],
        authority: PlanAuthority::Human,
    }
}

async fn read(client: &Client, command: Command, revision: Cursor) -> WireReply {
    client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("dispatch-ui-test").unwrap(),
            request_id: RequestId::new(crate::engine::owner::random_id().unwrap()).unwrap(),
            expected_revision: revision,
            command,
        })
        .await
        .unwrap()
}

async fn setup(client: &Client, native: &mut NativeOwner, plan: &WorkPlan) {
    let mut revision = Cursor::new(0);
    for command in [
        Command::CreateProject {
            project_id: plan.scope.project_id.clone(),
            spec: ProjectSpec {
                name: "Dispatch UI".into(),
                repositories: vec![],
            },
        },
        Command::PinTask {
            context: plan.scope.clone(),
        },
        Command::ProposePlan { plan: plan.clone() },
    ] {
        let reply = read(client, command, revision).await;
        assert_eq!(reply.outcome, Outcome::Applied);
        revision = reply.revision;
    }
    let controller = native.controller.as_mut().unwrap();
    for action in [
        OwnerAction::ApprovePlan {
            plan_id: plan.id.clone(),
        },
        OwnerAction::Reserve {
            plan_id: plan.id.clone(),
            item_id: plan.tasks[0].id.clone(),
        },
    ] {
        controller
            .read(OwnerRead::CurrentBrief {
                project_id: plan.scope.project_id.clone(),
            })
            .await
            .unwrap();
        controller
            .begin_mutation(OwnerMutation::Coordination { action }, 1)
            .unwrap();
        let response = controller.send_pending().await.unwrap();
        assert!(
            matches!(response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
        );
    }
}

async fn owner_event(connection: &mut Connection) -> OwnerEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(event) = connection.poll_owner().unwrap() {
                return event;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("actual owner event exceeded three seconds")
}

async fn load(app: &mut App, client: &Client, task: &ExecutionContext) {
    let snapshot = read(client, Command::Snapshot, Cursor::new(0)).await;
    app.apply_reply(snapshot).unwrap();
    let stored = read(
        client,
        Command::TaskPlan {
            task_id: task.task_id.clone(),
        },
        Cursor::new(0),
    )
    .await;
    app.apply_reply(stored).unwrap();
    assert_eq!(app.selected_task(), Some(task));
    assert!(app.plan.is_some());
}

async fn exercise(socket: PathBuf, config_id: ConfigId, service: Arc<Mutex<EngineService>>) {
    let mut native = NativeOwner::new(socket.clone(), Duration::from_secs(2));
    let (client, _) = native.attach().await;
    let plan = plan(config_id);
    setup(&client, &mut native, &plan).await;
    let mut connection =
        Connection::connect_paired(Client::new(socket.clone(), Duration::from_secs(2)).unwrap())
            .unwrap();
    let mut app = App::new();
    app.apply_owner(owner_event(&mut connection).await).unwrap();
    load(&mut app, &client, &plan.scope).await;
    for _ in 0..3 {
        app.reduce(UiEvent::FocusNext).unwrap();
    }
    assert_eq!(app.focus(), Focus::Plan);
    let query = app.dispatch_request().unwrap().unwrap();
    assert_eq!(query.attempt.plan_id, plan.id);
    assert_eq!(query.attempt.item_id, plan.tasks[0].id);
    assert_ne!(query.attempt.item_id, plan.scope.task_id);
    assert_eq!(
        query.attempt.reservation_cursor,
        app.plan.as_ref().unwrap().reservations[&plan.tasks[0].id].cursor
    );
    connection.dispatch(query.clone()).unwrap();
    assert!(connection.dispatch(query.clone()).is_err());
    assert!(app.dispatch_request().unwrap().is_none());
    let event = owner_event(&mut connection).await;
    assert!(
        matches!(&event, OwnerEvent::Dispatch(event) if matches!(&event.result, Ok(reply) if matches!(&reply.data, OwnerData::WorkerDispatch { dispatch: None })))
    );
    app.apply_owner(event).unwrap();
    let rendered = super::super::views::body(&app);
    assert!(rendered.contains("Not found: no recorded dispatch"));
    assert!(rendered.contains(query.dispatch_id.as_str()));
    assert!(rendered.contains("No provider or native execution is confirmed"));
    assert!(!connection.busy());
    app.reduce(UiEvent::FocusNext).unwrap();
    assert_eq!(app.focus(), Focus::Supervision);
    assert!(super::super::views::body(&app).contains(query.dispatch_id.as_str()));
    let query = app.dispatch_request().unwrap().unwrap();
    connection.dispatch(query).unwrap();
    app.reduce(UiEvent::TaskNext).unwrap();
    app.apply_owner(owner_event(&mut connection).await).unwrap();
    assert!(!super::super::views::body(&app).contains("Not found: no recorded dispatch"));
    load(&mut app, &client, &plan.scope).await;
    let runtime = service.lock().await;
    let runtime = runtime.owner_runtime().unwrap();
    let revoked = runtime
        .control(OwnerControl {
            operation_id: RequestId::new("dispatch-ui-revoke").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
            decision: OwnerDecision::Revoke,
        })
        .await
        .unwrap();
    assert!(!revoked.active);
    connection
        .dispatch(app.dispatch_request().unwrap().unwrap())
        .unwrap();
    app.apply_owner(owner_event(&mut connection).await).unwrap();
    assert!(!app.owner_ready);
    app.apply_owner(owner_event(&mut connection).await).unwrap();
    assert!(app.dispatch_request().unwrap().is_none());
    assert!(super::super::views::body(&app).contains("limited mode"));
    assert!(!connection.busy());
    let ordinary = Client::new(socket, Duration::from_secs(2)).unwrap();
    let snapshot = read(&ordinary, Command::Snapshot, Cursor::new(0)).await;
    assert_eq!(snapshot.revision, app.revision());
    assert!(
        matches!(snapshot.outcome, Outcome::Snapshot { projects, tasks } if projects.len() == 1 && tasks == vec![plan.scope])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn actual_selected_attempt_owner_socket_read_and_revoke_are_honest() {
    let root = tempfile::Builder::new()
        .prefix("ca-dispatch-ui-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    std::fs::write(
        &global,
        "[routing]\napproved_accounts = [\"dispatch-ui-account\"]\n",
    )
    .unwrap();
    let config = crate::config::resolve_config(&global, &root.path().join("project.toml")).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let service = Arc::new(Mutex::new(
        EngineService::start(root.path(), "engine.sock", &config, &stopped)
            .await
            .unwrap(),
    ));
    let mut task = tokio::spawn(exercise(
        root.path().join("engine.sock"),
        config.id,
        service.clone(),
    ));
    let (result, timed_out) = match tokio::time::timeout(Duration::from_secs(15), &mut task).await {
        Ok(result) => (result, false),
        Err(_) => {
            task.abort();
            (task.await, true)
        }
    };
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        service.lock().await.shutdown().await
    })
    .await;
    assert!(
        matches!(closed, Ok(Ok(()))),
        "actual engine shutdown failed: {closed:?}"
    );
    assert!(!timed_out, "actual dispatch flow exceeded fifteen seconds");
    result.unwrap();
}
