use super::*;
use crate::engine::owner::{OwnerMutation, OwnerResponse};
use crate::protocol::{
    ClientId, Command as OrdinaryCommand, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec,
    WireRequest,
};

#[tokio::test]
async fn preview_account_setup_exposes_real_selected_sign_in_route() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        let before = case.counts();
        let accounts = views::body(&ui.app);
        assert!(accounts.contains("account-codex"));
        assert!(accounts.contains("account-claude"));
        ui.connections(0).await;
        let connections = views::body(&ui.app);
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        let (basis, connection) = case.selected("a-codex").await;
        assert!(matches!(inspected.job.command, Command::Inspect));
        assert_eq!(inspected.job.context.connection, connection);
        assert_eq!(inspected.job.context.basis, basis);
        assert!(matches!(&inspected.result, Ok(Output::Inspected(_))));
        ui.apply(inspected);
        assert_eq!(ui.app.focus(), Focus::Settings);
        assert!(ui.app.model_access_request().unwrap().is_none());
        assert_eq!(case.counts(), before);
        assert!(accounts.contains("Account setup"));
        assert!(connections.contains("F2 Sign in"));
        assert!(!accounts.contains("authentication unavailable"));
        assert!(!connections.contains("authentication and native execution unavailable"));
        let panel = views::body(&ui.app);
        assert!(panel.contains("a-codex"));
        assert!(panel.contains("account-codex"));
        assert!(!panel.contains("Ready"));
    })
    .await;
}

#[tokio::test]
async fn preview_actual_auth_failure_stays_attributed_in_pool_and_out_of_conversation() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.connections(0).await;
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        ui.apply(inspected);
        ui.key(KeyCode::Enter);
        let started = ui.auth().await;
        let fact = real_reply(&started).fact.clone();
        assert_unavailable(real_reply(&started), "a-codex");
        ui.apply(started);
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Char('3'));
        ui.drain().await;
        ui.key(KeyCode::Enter);
        ui.drain().await;
        let pool = views::body(&ui.app);
        let stored: Vec<u8> = case
            .observer()
            .query_row(
                "SELECT fact FROM native_auth_operations WHERE id=?1 AND generation=?2",
                rusqlite::params![
                    fact.operation_id.as_str(),
                    i64::try_from(fact.generation).unwrap()
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<NativeAuthFact>(&stored).unwrap(),
            fact
        );
        assert_eq!(case.counts().last(), Some(&1));
        assert!(pool.contains("pool-fixture"));
        assert!(pool.contains("a-codex") && pool.contains("b-claude"));
        assert!(pool.contains("Last authentication"));
        assert!(pool.contains("account-codex"));
        assert!(pool.contains("Unavailable"));
        assert!(!pool.contains("AuthenticatedObserved") && !pool.contains("Ready"));
        for _ in 0..5 {
            ui.app.reduce(UiEvent::FocusNext).unwrap();
        }
        assert_eq!(ui.app.focus(), Focus::Conversation);
        for character in "Literal owner draft 873 日本".chars() {
            ui.app.reduce(UiEvent::Character(character)).unwrap();
        }
        assert_eq!(ui.app.draft(), "Literal owner draft 873 日本");
        assert!(ui.app.take_conversation_job().is_none());
        assert!(!views::body(&ui.app).contains(fact.operation_id.as_str()));
    })
    .await;
}

async fn declared_project(case: &Case, ui: &mut Ui) -> ProjectId {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    assert!(repository.join(".git").exists());
    let project_id = ProjectId::new("project-fixture").unwrap();
    let mut owner = case.controller().await;
    owner
        .read_page(crate::engine::owner::OwnerView::Projects, None, Some(1))
        .await
        .unwrap();
    owner
        .begin_mutation(
            OwnerMutation::CreateProject {
                project_id: project_id.clone(),
                spec: ProjectSpec {
                    name: "Preview repository project".into(),
                    repositories: vec![repository.to_str().unwrap().to_owned()],
                },
            },
            1,
        )
        .unwrap();
    let receipt = owner.send_pending().await.unwrap();
    assert!(matches!(receipt, OwnerResponse::Applied { .. }));
    let client = Client::new(case.root.path().join("e.sock"), Duration::from_secs(2)).unwrap();
    let snapshot = client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("preview-ui-873").unwrap(),
            request_id: request("preview-project-snapshot"),
            expected_revision: Cursor::new(0),
            command: OrdinaryCommand::Snapshot,
        })
        .await
        .unwrap();
    ui.app.apply_reply(snapshot).unwrap();
    let actual = ui.app.selected_project().unwrap();
    assert_eq!(actual.project_id, project_id);
    assert_eq!(
        actual.spec.repositories,
        vec![repository.to_str().unwrap().to_owned()]
    );
    project_id
}

#[tokio::test]
async fn preview_enter_opens_actual_project_then_explicit_codex_setup_without_submitting_draft() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        let project_id = declared_project(&case, &mut ui).await;
        ui.app.reduce(UiEvent::FocusPrevious).unwrap();
        ui.app.reduce(UiEvent::FocusPrevious).unwrap();
        for character in "Retained project prompt 873".chars() {
            ui.app.reduce(UiEvent::Character(character)).unwrap();
        }
        ui.app.reduce(UiEvent::FocusNext).unwrap();
        assert_eq!(ui.app.focus(), Focus::Projects);
        let counts = case.counts();
        ui.key(KeyCode::Enter);
        assert_eq!(ui.app.focus(), Focus::Conversation);
        assert_eq!(ui.app.selected_project().unwrap().project_id, project_id);
        assert_eq!(ui.app.draft(), "Retained project prompt 873");
        assert!(ui.app.take_command().is_none());
        assert!(ui.app.take_conversation_job().is_none());
        assert!(!ui.app.master.has_pending());
        ui.key(KeyCode::F(2));
        assert!(ui.app.master.is_open());
        let job = ui.app.master_request().unwrap().unwrap();
        assert!(matches!(
            job,
            crate::tui::master_session::Job::Catalog { .. }
        ));
        ui.connection.master(job).unwrap();
        let event = ui.owner_event().await;
        assert!(matches!(event, OwnerEvent::Master(_)));
        ui.app.apply_owner(event).unwrap();
        ui.key(KeyCode::Down);
        let setup = views::body(&ui.app);
        assert!(setup.contains("Codex"));
        assert!(setup.contains("Open disabled"));
        assert_eq!(ui.app.draft(), "Retained project prompt 873");
        assert_eq!(case.counts(), counts);
    })
    .await;
}
