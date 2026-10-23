use super::*;
use crate::config::{AccountPool, ModelAccess};
use crate::engine::owner::{OwnerMutation, OwnerResponse, OwnerView};
use crate::protocol::{
    ClientId, Command as OrdinaryCommand, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec,
    WireRequest,
};

async fn unassigned(case: &Case) -> Ui {
    let mut config: toml::Value = toml::from_str(CATALOG).unwrap();
    let pool = &mut config["model_access"]["pools"][0];
    pool["projects"] = toml::Value::Array(Vec::new());
    pool["roles"] = toml::Value::Array(Vec::new());
    let document = toml_edit::ser::to_document(&config).unwrap();
    std::fs::write(&case.global, document.to_string()).unwrap();
    case.restart().await;
    let mut ui = Ui::new(case).await;
    ui.key(KeyCode::Char('3'));
    ui.drain().await;
    assert!(ui.app.selected_project().is_none());
    ui
}

fn catalog(case: &Case) -> ModelAccess {
    crate::config::resolve_config(&case.global, &case.project)
        .unwrap()
        .settings
        .model_access
        .unwrap()
}

fn commits(case: &Case) -> i64 {
    case.observer()
        .query_row(
            "SELECT count(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn assert_no_auth(case: &Case) {
    let count: i64 = case
        .observer()
        .query_row("SELECT count(*) FROM native_auth_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0, "pool assignment must not start authentication");
}

async fn selected_project(case: &Case, ui: &mut Ui) -> ProjectId {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    assert!(repository.join(".git").exists());
    let project_id = ProjectId::new("pool-project-1038").unwrap();
    let mut owner = case.controller().await;
    owner
        .read_page(OwnerView::Projects, None, Some(1))
        .await
        .unwrap();
    owner
        .begin_mutation(
            OwnerMutation::CreateProject {
                project_id: project_id.clone(),
                spec: ProjectSpec {
                    name: "Pool assignment project".into(),
                    repositories: vec![repository.to_str().unwrap().to_owned()],
                },
            },
            1,
        )
        .unwrap();
    assert!(matches!(
        owner.send_pending().await.unwrap(),
        OwnerResponse::Applied { .. }
    ));
    let client = Client::new(case.root.path().join("e.sock"), Duration::from_secs(2)).unwrap();
    let snapshot = client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("pool-ui-1038").unwrap(),
            request_id: request("pool-project-snapshot-1038"),
            expected_revision: Cursor::new(0),
            command: OrdinaryCommand::Snapshot,
        })
        .await
        .unwrap();
    ui.app.apply_reply(snapshot).unwrap();
    let actual = ui.app.selected_project().unwrap();
    assert_eq!(actual.project_id, project_id);
    assert_eq!(actual.spec.name, "Pool assignment project");
    assert_eq!(
        actual.spec.repositories,
        vec![repository.to_str().unwrap().to_owned()]
    );
    project_id
}

async fn observed_pool(case: &Case, id: &CredentialRef) -> (AccountPool, String) {
    let mut owner = case.controller().await;
    let reply = owner
        .read(OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Pools,
            id: id.clone(),
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessRecord {
        source_hash,
        writable,
        record: ModelAccessRecord::Pool { pool },
    } = reply.data
    else {
        panic!("actual owner pool record required");
    };
    assert!(writable);
    assert_eq!(&pool.id, id);
    (pool, source_hash)
}

async fn master_catalog(ui: &mut Ui) {
    ui.app.reduce(UiEvent::FocusPrevious).unwrap();
    ui.app.reduce(UiEvent::FocusPrevious).unwrap();
    assert_eq!(ui.app.focus(), Focus::Conversation);
    ui.key(KeyCode::F(2));
    let job = ui.app.master_request().unwrap().unwrap();
    assert!(matches!(
        &job,
        crate::tui::master_session::Job::Catalog { .. }
    ));
    ui.connection.master(job).unwrap();
    let event = ui.owner_event().await;
    ui.app.apply_owner(event).unwrap();
}

async fn master_guidance(ui: &mut Ui) {
    master_catalog(ui).await;
    let body = views::body(&ui.app);
    assert!(body.contains("Unassigned"));
    assert!(body.contains("Pools"));
    assert!(body.contains("Assign"));
    assert!(!body.contains("Enter Open"));
    assert!(!ui.app.master.has_pending());
    ui.key(KeyCode::Esc);
    ui.app.reduce(UiEvent::FocusNext).unwrap();
    ui.app.reduce(UiEvent::FocusNext).unwrap();
    assert_eq!(ui.app.focus(), Focus::Settings);
    ui.key(KeyCode::Char('3'));
    ui.drain().await;
}

#[tokio::test]
async fn unassigned_pool_is_visible_without_a_project_and_cannot_gain_implicit_scope() {
    run(|case| async move {
        let mut ui = unassigned(&case).await;
        let original = std::fs::read(&case.global).unwrap();
        let saved = catalog(&case);
        let before = commits(&case);
        assert_eq!(saved.pools.len(), 1);
        assert!(saved.pools[0].projects.is_empty());
        assert!(saved.pools[0].roles.is_empty());
        let body = views::body(&ui.app);
        assert!(body.contains(saved.pools[0].id.as_str()));
        assert!(body.contains("Unassigned"));
        assert!(body.contains("a Assign"));
        ui.key(KeyCode::Char('a'));
        ui.drain().await;
        assert!(views::body(&ui.app).contains("Select a project"));
        assert!(!ui.app.model_access.has_pending());
        ui.key(KeyCode::Esc);
        ui.drain().await;
        assert_eq!(std::fs::read(&case.global).unwrap(), original);
        assert_eq!(catalog(&case), saved);
        assert_eq!(commits(&case), before);
        assert_no_auth(&case);
    })
    .await;
}

#[tokio::test]
async fn explicit_assignment_persists_only_the_selected_real_project_and_master_role() {
    run(|case| async move {
        let mut ui = unassigned(&case).await;
        let project = selected_project(&case, &mut ui).await;
        master_guidance(&mut ui).await;
        let saved = catalog(&case);
        let original = std::fs::read(&case.global).unwrap();
        let (old_pool, old_source) = observed_pool(&case, &saved.pools[0].id).await;
        let before = commits(&case);
        ui.key(KeyCode::Char('a'));
        ui.drain().await;
        let body = views::body(&ui.app);
        assert!(body.contains("Assign pool to project"));
        assert!(body.contains("Pool assignment project"));
        assert!(body.contains("Project master"));
        assert_eq!(std::fs::read(&case.global).unwrap(), original);
        assert_eq!(commits(&case), before);
        ui.key(KeyCode::Esc);
        ui.drain().await;
        assert_eq!(catalog(&case), saved);
        ui.key(KeyCode::Char('a'));
        ui.drain().await;
        ui.key(KeyCode::Enter);
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let (actual, source) = observed_pool(&case, &old_pool.id).await;
        let mut expected = old_pool;
        expected.projects = vec![CredentialRef::new(project.as_str()).unwrap()];
        expected.roles = vec![reference("project_leader")];
        assert_eq!(actual, expected);
        assert_ne!(source, old_source);
        assert_eq!(commits(&case), before + 1);
        let persisted = catalog(&case);
        assert_eq!(persisted.pools, vec![actual]);
        assert_eq!(persisted.connections, saved.connections);
        assert_eq!(persisted.accounts, saved.accounts);
        let body = views::body(&ui.app);
        assert!(body.contains("Assigned to project"));
        assert!(!ui.app.model_access.has_pending());
        master_catalog(&mut ui).await;
        assert!(views::body(&ui.app).contains(&source));
        assert!(!ui.app.master.has_pending());
        assert_eq!(commits(&case), before + 1);
        assert_no_auth(&case);
    })
    .await;
}
