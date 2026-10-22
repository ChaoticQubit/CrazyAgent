use super::super::app::{App, Focus, UiEvent};
use super::super::client::Connection;
use super::super::{key_event, views};
use crate::client_controller::OwnerController;
use crate::config::{ModelAccess, ModelAccessEdit, resolve_config};
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    ModelAccessCollection, OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse,
    OwnerResult,
};
use crate::protocol::{
    ClientId, Command, CredentialRef, Outcome, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId,
    WireRequest,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Harness {
    root: tempfile::TempDir,
    global: PathBuf,
    project: PathBuf,
    service: Option<EngineService>,
    connection: Option<Connection>,
    app: App,
    _stop: tokio::sync::watch::Sender<bool>,
    stopped: tokio::sync::watch::Receiver<bool>,
}

impl Harness {
    async fn new(writable: bool, accounts: &[String]) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-tui272-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        let bytes = format!(
            "version = 1\n[model_access]\naccounts = {}\n",
            serde_json::to_string(accounts).unwrap()
        );
        std::fs::write(&global, bytes).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = if writable {
            EngineService::start_with_settings_sources(
                root.path(),
                "engine.sock",
                &global,
                &project,
                &stopped,
            )
            .await
            .unwrap()
        } else {
            let config = resolve_config(&global, &project).unwrap();
            EngineService::start(root.path(), "engine.sock", &config, &stopped)
                .await
                .unwrap()
        };
        let connection = Connection::connect_paired(
            Client::new(root.path().join("engine.sock"), Duration::from_secs(2)).unwrap(),
        )
        .unwrap();
        let mut app = App::new();
        app.reduce(UiEvent::FocusNext).unwrap();
        app.reduce(UiEvent::FocusNext).unwrap();
        assert_eq!(app.focus(), Focus::Settings);
        let mut harness = Self {
            root,
            global,
            project,
            service: Some(service),
            connection: Some(connection),
            app,
            _stop: stop,
            stopped,
        };
        harness.drain().await;
        assert!(harness.app.owner_ready, "{}", harness.render());
        harness
    }

    async fn drain(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let connection = self.connection.as_mut().unwrap();
            while let Some(event) = connection.poll_owner().unwrap() {
                self.app.apply_owner(event).unwrap();
            }
            if !connection.busy()
                && let Some(job) = self.app.model_access_request().unwrap()
            {
                connection.model_access(job).unwrap();
            }
            if !connection.busy()
                && !self.app.model_access.loading()
                && (self.app.owner_ready || self.app.owner_problem.is_some())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "bounded TUI owner request deadline: {}",
                self.render()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn key(&mut self, code: KeyCode) {
        self.modified_key(code, KeyModifiers::NONE);
    }

    fn modified_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let event = key_event(KeyEvent::new(code, modifiers), &self.app)
            .expect("editor key must be mapped");
        if let Err(error) = self.app.reduce(event) {
            self.app.notice(&error);
        }
    }

    fn text(&mut self, value: &str) {
        for character in value.chars() {
            self.key(KeyCode::Char(character));
        }
    }

    fn fields(&mut self, values: &[&str]) {
        for value in values {
            self.text(value);
            self.key(KeyCode::Tab);
        }
    }

    fn render(&self) -> String {
        self.render_at(120, 40)
    }

    fn render_at(&self, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| views::draw(frame, &self.app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn metadata(&self) -> ModelAccess {
        resolve_config(&self.global, &self.project)
            .unwrap()
            .settings
            .model_access
            .unwrap()
    }

    async fn collection(&mut self, key: char) {
        self.key(KeyCode::Char(key));
        self.drain().await;
    }

    async fn add_account(&mut self, id: &str) {
        self.collection('1').await;
        self.key(KeyCode::Char('n'));
        self.text(id);
        self.key(KeyCode::Enter);
        self.key(KeyCode::Enter);
        self.drain().await;
        assert!(!self.app.model_access.has_pending(), "{}", self.render());
        assert!(
            self.metadata()
                .accounts
                .iter()
                .any(|reference| reference.as_str() == id)
        );
    }

    async fn create_connection(&mut self, account: &str) {
        self.collection('2').await;
        self.key(KeyCode::Char('n'));
        self.fields(&[
            "connection-one",
            "provider-one",
            "codex",
            "native",
            account,
            "",
            "",
            "model-one",
            "author-one",
            "",
            "",
        ]);
        self.key(KeyCode::Enter);
        self.drain().await;
    }

    async fn remove_selected(&mut self) {
        let before = std::fs::read(&self.global).unwrap();
        self.key(KeyCode::Char('d'));
        self.drain().await;
        assert_eq!(
            std::fs::read(&self.global).unwrap(),
            before,
            "opening confirmation must have no file effect"
        );
        assert!(self.render().contains("Enter confirms removal"));
        self.key(KeyCode::Enter);
        self.drain().await;
    }

    async fn external_account(&self, id: &str) {
        let (_, mut controller) = OwnerController::attach_configured(
            Client::new(self.root.path().join("engine.sock"), Duration::from_secs(2)).unwrap(),
            OwnerClientKind::Desktop,
        )
        .await
        .unwrap();
        let state = controller
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Accounts,
                cursor: None,
                limit: 1,
            })
            .await
            .unwrap();
        let OwnerData::ModelAccessPage { source_hash, .. } = state.data else {
            panic!("actual source hash required")
        };
        controller
            .begin_mutation(
                OwnerMutation::ModelAccess {
                    expected_config_id: state.config_id,
                    expected_source_hash: source_hash,
                    edit: ModelAccessEdit::AddAccount {
                        account_ref: CredentialRef::new(id).unwrap(),
                    },
                },
                1,
            )
            .unwrap();
        let reply = controller.send_pending().await.unwrap();
        assert!(
            matches!(reply, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
        );
    }

    async fn external_revision(&self) {
        let (ordinary, mut controller) = OwnerController::attach_configured(
            Client::new(self.root.path().join("engine.sock"), Duration::from_secs(2)).unwrap(),
            OwnerClientKind::Desktop,
        )
        .await
        .unwrap();
        let query = OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 1,
        };
        let before = controller.read(query.clone()).await.unwrap();
        let file = std::fs::read(&self.global).unwrap();
        let response = ordinary
            .request(WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("revision-peer").unwrap(),
                request_id: RequestId::new("revision-project").unwrap(),
                expected_revision: before.revision.clone(),
                command: Command::CreateProject {
                    project_id: ProjectId::new("revision-project").unwrap(),
                    spec: ProjectSpec {
                        name: "Revision only".into(),
                        repositories: Vec::new(),
                    },
                },
            })
            .await
            .unwrap();
        assert_eq!(response.outcome, Outcome::Applied);
        let after = controller.read(query).await.unwrap();
        assert!(after.revision.value() > before.revision.value());
        assert_eq!(after.config_id, before.config_id);
        assert_eq!(after.data, before.data);
        assert_eq!(std::fs::read(&self.global).unwrap(), file);
    }

    async fn close(mut self) {
        drop(self.connection.take());
        self.service.take().unwrap().shutdown().await.unwrap();
    }

    async fn restart(&mut self, writable: bool) {
        self.service.take().unwrap().shutdown().await.unwrap();
        self.service = Some(if writable {
            EngineService::start_with_settings_sources(
                self.root.path(),
                "engine.sock",
                &self.global,
                &self.project,
                &self.stopped,
            )
            .await
            .unwrap()
        } else {
            let config = resolve_config(&self.global, &self.project).unwrap();
            EngineService::start(self.root.path(), "engine.sock", &config, &self.stopped)
                .await
                .unwrap()
        });
        self.connection.as_mut().unwrap().reconnect().unwrap();
        self.app.reconnect_owner().unwrap();
        self.drain().await;
    }
}

#[tokio::test]
async fn all_eight_keyboard_edits_persist_through_the_real_owner_engine() {
    let mut h = Harness::new(true, &[]).await;
    h.add_account("account-one").await;
    h.create_connection("account-one").await;
    let connection = &h.metadata().connections[0];
    assert_eq!(connection.provider.as_str(), "provider-one");
    assert_eq!(connection.harness.as_str(), "codex");
    assert_eq!(connection.account_ref.as_str(), "account-one");
    assert_eq!(connection.model.author.as_str(), "author-one");

    h.key(KeyCode::Char('e'));
    h.drain().await;
    h.text("cannot-change-id");
    for _ in 0..7 {
        h.key(KeyCode::Tab);
    }
    for _ in "model-one".chars() {
        h.key(KeyCode::Backspace);
    }
    h.text("model-two");
    h.key(KeyCode::Enter);
    h.drain().await;
    assert_eq!(h.metadata().connections[0].id.as_str(), "connection-one");
    assert_eq!(h.metadata().connections[0].model.id, "model-two");

    h.collection('3').await;
    h.key(KeyCode::Char('n'));
    assert!(h.render().contains("Pool label (optional)"));
    h.text("Pool one");
    h.key(KeyCode::Enter);
    h.drain().await;
    assert!(h.render().contains("connection-one"));
    h.key(KeyCode::Char(' '));
    h.key(KeyCode::Enter);
    h.key(KeyCode::Enter);
    h.drain().await;
    assert_eq!(h.metadata().pools.len(), 1);
    let created = h.metadata().pools[0].clone();
    assert_eq!(created.members[0].as_str(), "connection-one");
    h.key(KeyCode::Char('e'));
    h.drain().await;
    assert!(h.render().contains("Choose connections"), "{}", h.render());
    h.key(KeyCode::Enter);
    h.key(KeyCode::Enter);
    h.drain().await;
    assert_eq!(h.metadata().pools, vec![created]);

    let before = std::fs::read(&h.global).unwrap();
    h.key(KeyCode::Char('d'));
    h.drain().await;
    h.key(KeyCode::Esc);
    assert_eq!(std::fs::read(&h.global).unwrap(), before);
    h.remove_selected().await;
    assert!(h.metadata().pools.is_empty());
    h.collection('2').await;
    h.remove_selected().await;
    assert!(h.metadata().connections.is_empty());
    h.collection('1').await;
    h.remove_selected().await;
    assert!(h.metadata().accounts.is_empty());
    assert!(h.render().contains("Metadata only"));
    assert!(h.render().contains("authentication"));
    let db = rusqlite::Connection::open(h.root.path().join("engine.db")).unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count, 8,
        "repeated Enter must not duplicate any of the eight edits"
    );
    drop(db);
    h.close().await;
}

#[tokio::test]
async fn actual_read_only_grant_lists_pages_and_records_without_file_writes() {
    let accounts: Vec<_> = (0..17).map(|i| format!("account-{i:02}")).collect();
    let mut h = Harness::new(false, &accounts).await;
    let before = std::fs::read(&h.global).unwrap();
    assert!(h.render().contains("Accounts: 16 records"));
    h.key(KeyCode::Char('n'));
    assert!(h.render().contains("Read-only"));
    assert!(!h.app.model_access.has_pending());
    h.modified_key(KeyCode::Char('n'), KeyModifiers::CONTROL);
    h.drain().await;
    assert!(h.render().contains("Accounts: 1 records"));
    assert!(h.render().contains("account-16"));
    h.key(KeyCode::Enter);
    h.drain().await;
    assert!(h.render().contains("Account reference: account-16"));
    h.modified_key(KeyCode::Char('p'), KeyModifiers::CONTROL);
    h.drain().await;
    assert!(h.render().contains("Accounts: 16 records"));
    assert_eq!(std::fs::read(&h.global).unwrap(), before);
    h.close().await;
}

#[tokio::test]
async fn actual_validation_and_dependency_errors_retain_fields_and_references() {
    let mut h = Harness::new(true, &[]).await;
    h.add_account("account-one").await;
    h.create_connection("missing-account").await;
    assert!(h.metadata().connections.is_empty());
    assert!(
        h.app.model_access.notice.contains("invalid_model_access"),
        "{}",
        h.render()
    );
    assert!(h.render().contains("missing-account"));
    for _ in 0..4 {
        h.key(KeyCode::Tab);
    }
    for _ in "missing-account".chars() {
        h.key(KeyCode::Backspace);
    }
    h.text("account-one");
    h.external_revision().await;
    h.key(KeyCode::Enter);
    h.drain().await;
    assert!(!h.app.model_access.has_pending());
    assert!(h.app.model_access.notice.contains("stale"));
    h.modified_key(KeyCode::Char('p'), KeyModifiers::CONTROL);
    h.drain().await;
    h.key(KeyCode::Enter);
    h.drain().await;
    assert_eq!(h.metadata().connections.len(), 1);
    h.collection('1').await;
    h.remove_selected().await;
    assert_eq!(h.metadata().accounts.len(), 1);
    assert!(
        h.app.model_access.notice.contains("rejected"),
        "{}",
        h.render()
    );
    assert!(!h.app.model_access.has_pending());
    h.close().await;
}

#[tokio::test]
async fn actual_competing_owner_requires_explicit_refresh_of_retained_form() {
    let mut h = Harness::new(true, &[]).await;
    h.key(KeyCode::Char('n'));
    h.text("tui-account");
    h.external_account("desktop-account").await;
    h.key(KeyCode::Enter);
    h.drain().await;
    assert!(
        h.app.model_access.notice.contains("stale_form"),
        "{}",
        h.render()
    );
    assert!(h.render().contains("tui-account"));
    assert_eq!(h.metadata().accounts.len(), 1);
    assert!(!h.app.model_access.has_pending());
    h.modified_key(KeyCode::Char('p'), KeyModifiers::CONTROL);
    h.drain().await;
    h.key(KeyCode::Enter);
    h.drain().await;
    assert_eq!(h.metadata().accounts.len(), 2);
    h.close().await;
}

#[tokio::test]
async fn real_publication_failure_reconciles_original_request_after_restart() {
    let mut h = Harness::new(true, &[]).await;
    let db = rusqlite::Connection::open(h.root.path().join("engine.db")).unwrap();
    db.execute_batch("CREATE TRIGGER tui272_stop BEFORE INSERT ON owner_operations BEGIN SELECT RAISE(ABORT, 'tui272-publication-stop'); END;").unwrap();
    h.key(KeyCode::Char('n'));
    h.text("recover-account");
    h.key(KeyCode::Enter);
    h.drain().await;
    assert!(h.app.model_access.has_pending());
    let original = h
        .app
        .model_access
        .pending_label()
        .expect("actual original request identity retained");
    assert!(h.render().contains("recover-account"));
    let pending: i64 = db
        .query_row("SELECT COUNT(*) FROM settings_publication", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(pending, 1);
    h.connection.as_mut().unwrap().reconnect().unwrap();
    h.app.reconnect_owner().unwrap();
    h.drain().await;
    assert!(h.app.model_access.has_pending());
    assert_eq!(h.app.model_access.pending_label().as_ref(), Some(&original));
    assert!(
        h.app.model_access.notice.contains("unknown"),
        "{}",
        h.render()
    );
    let recorded: i64 = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(recorded, 0);
    db.execute_batch("DROP TRIGGER tui272_stop").unwrap();
    drop(db);
    h.service.take().unwrap().shutdown().await.unwrap();
    let service = EngineService::start_with_settings_sources(
        h.root.path(),
        "engine.sock",
        &h.global,
        &h.project,
        &h.stopped,
    )
    .await
    .unwrap();
    h.service = Some(service);
    h.connection.as_mut().unwrap().reconnect().unwrap();
    h.app.reconnect_owner().unwrap();
    assert_eq!(h.app.model_access.pending_label().as_ref(), Some(&original));
    h.drain().await;
    assert!(!h.app.model_access.has_pending(), "{}", h.render());
    assert_eq!(h.metadata().accounts.len(), 1);
    assert_eq!(h.metadata().accounts[0].as_str(), "recover-account");
    let db = rusqlite::Connection::open(h.root.path().join("engine.db")).unwrap();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let pending: i64 = db
        .query_row("SELECT COUNT(*) FROM settings_publication", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(pending, 0);
    drop(db);
    h.close().await;
}

#[tokio::test]
async fn actual_stale_cursor_is_discarded_before_first_page_refresh() {
    let accounts: Vec<_> = (0..17).map(|i| format!("account-{i:02}")).collect();
    let mut h = Harness::new(true, &accounts).await;
    h.external_account("account-17").await;
    h.modified_key(KeyCode::Char('n'), KeyModifiers::CONTROL);
    h.drain().await;
    assert!(
        h.app.model_access.notice.contains("stale"),
        "{}",
        h.render()
    );
    assert!(h.app.model_access.next.is_none());
    h.modified_key(KeyCode::Char('p'), KeyModifiers::CONTROL);
    h.drain().await;
    assert!(h.render().contains("Accounts: 16 records"));
    h.modified_key(KeyCode::Char('n'), KeyModifiers::CONTROL);
    h.drain().await;
    assert!(h.render().contains("account-16"));
    assert!(h.render().contains("account-17"));
    h.close().await;
}

#[tokio::test]
async fn reconnect_preserves_form_and_obeys_new_host_write_grant() {
    let mut h = Harness::new(true, &[]).await;
    h.key(KeyCode::Char('n'));
    h.text("retained-account");
    h.restart(false).await;
    assert!(h.render().contains("retained-account"));
    assert!(h.render().contains("Read-only"));
    let before = std::fs::read(&h.global).unwrap();
    h.key(KeyCode::Enter);
    assert_eq!(std::fs::read(&h.global).unwrap(), before);
    assert!(!h.app.model_access.has_pending());
    h.restart(true).await;
    h.key(KeyCode::Enter);
    assert!(
        !h.app.model_access.has_pending(),
        "old form basis cannot acquire a new owner identity silently"
    );
    h.modified_key(KeyCode::Char('p'), KeyModifiers::CONTROL);
    h.drain().await;
    h.key(KeyCode::Enter);
    h.drain().await;
    assert_eq!(h.metadata().accounts[0].as_str(), "retained-account");
    h.close().await;
}

#[tokio::test]
async fn metadata_auth_choices_roundtrip_references_without_authentication() {
    use crate::config::ModelAuthentication;
    let mut h = Harness::new(true, &[]).await;
    h.add_account("account-one").await;
    h.create_connection("account-one").await;
    assert!(matches!(
        h.metadata().connections[0].auth,
        ModelAuthentication::None {}
    ));
    for (method, reference) in [
        (1, "api-reference"),
        (2, "oauth-reference"),
        (3, "profile-reference"),
    ] {
        h.key(KeyCode::Char('e'));
        h.drain().await;
        for _ in 0..9 {
            h.key(KeyCode::Tab);
        }
        h.key(KeyCode::Down);
        h.key(KeyCode::Tab);
        for _ in 0..128 {
            h.key(KeyCode::Backspace);
        }
        h.text(reference);
        h.key(KeyCode::Enter);
        h.drain().await;
        let auth = h.metadata().connections[0].auth.clone();
        match (method, auth) {
            (1, ModelAuthentication::ApiKey { credential_ref })
            | (2, ModelAuthentication::Oauth { credential_ref }) => {
                assert_eq!(credential_ref.as_str(), reference)
            }
            (3, ModelAuthentication::NativeManaged { profile_ref }) => {
                assert_eq!(profile_ref.as_str(), reference)
            }
            other => panic!("actual reference-only auth metadata mismatch: {other:?}"),
        }
        assert!(h.render().contains("authentication"));
    }
    h.close().await;
}

#[tokio::test]
async fn actual_reply_after_collection_change_cannot_replace_current_view() {
    let mut h = Harness::new(true, &["account-one".into()]).await;
    h.modified_key(KeyCode::Char('p'), KeyModifiers::CONTROL);
    let job = h.app.model_access_request().unwrap().unwrap();
    h.connection.as_mut().unwrap().model_access(job).unwrap();
    h.key(KeyCode::Char('3'));
    h.drain().await;
    assert!(h.render().contains("Pools: 0 records"));
    assert!(!h.render().contains("> account-one"));
    h.close().await;
}

#[tokio::test]
async fn actual_editor_state_invariants_trip_on_corrupt_selection_and_form() {
    let mut h = Harness::new(true, &["account-one".into()]).await;
    h.app.model_access.selected = 1;
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h
            .app
            .model_access
            .assert_consistent()))
        .is_err()
    );
    h.app.model_access.selected = 0;
    h.key(KeyCode::Char('n'));
    h.app.model_access.form.as_mut().unwrap().selected = 99;
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h
            .app
            .model_access
            .assert_consistent()))
        .is_err()
    );
    h.app.model_access.form.as_mut().unwrap().selected = 0;
    h.text("x");
    h.app.model_access.form.as_mut().unwrap().fields[0]
        .value
        .push('\u{1b}');
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h
            .app
            .model_access
            .assert_consistent()))
        .is_err()
    );
    h.app.model_access.form.as_mut().unwrap().fields[0]
        .value
        .pop();
    h.close().await;
}

#[tokio::test]
async fn bounded_form_validation_and_escape_retain_values_without_a_write() {
    let mut h = Harness::new(true, &[]).await;
    let before = std::fs::read(&h.global).unwrap();
    h.key(KeyCode::Char('n'));
    h.text(&"x".repeat(256));
    h.key(KeyCode::Char('界'));
    h.key(KeyCode::Char('\u{1b}'));
    assert_eq!(
        h.app.model_access.form.as_ref().unwrap().fields[0].value,
        "x".repeat(256)
    );
    h.key(KeyCode::Enter);
    assert!(!h.app.model_access.has_pending());
    assert!(h.app.model_access.notice.contains("1..128"));
    h.key(KeyCode::Esc);
    h.key(KeyCode::Char('n'));
    assert_eq!(
        h.app.model_access.form.as_ref().unwrap().fields[0].value,
        "x".repeat(256)
    );
    assert_eq!(std::fs::read(&h.global).unwrap(), before);
    h.key(KeyCode::Esc);
    h.key(KeyCode::Char('x'));
    h.key(KeyCode::Char('n'));
    assert!(
        h.app.model_access.form.as_ref().unwrap().fields[0]
            .value
            .is_empty()
    );
    h.close().await;
}

#[tokio::test]
async fn real_not_recorded_barrier_allows_only_the_exact_explicit_retry() {
    let mut h = Harness::new(true, &[]).await;
    let db = rusqlite::Connection::open(h.root.path().join("engine.db")).unwrap();
    db.execute_batch("CREATE TRIGGER tui272_prepare_stop BEFORE INSERT ON settings_publication BEGIN SELECT RAISE(ABORT, 'tui272-prepare-stop'); END;").unwrap();
    let before = std::fs::read(&h.global).unwrap();
    h.key(KeyCode::Char('n'));
    h.text("retry-account");
    h.key(KeyCode::Enter);
    h.drain().await;
    assert!(h.app.model_access.has_pending(), "{}", h.render());
    let original = h
        .app
        .model_access
        .pending_label()
        .expect("actual prepared original intent");
    assert_eq!(std::fs::read(&h.global).unwrap(), before);
    h.connection.as_mut().unwrap().reconnect().unwrap();
    h.app.reconnect_owner().unwrap();
    h.drain().await;
    assert!(h.app.model_access.retry_allowed, "{}", h.render());
    assert_eq!(h.app.model_access.pending_label().as_ref(), Some(&original));
    db.execute_batch("DROP TRIGGER tui272_prepare_stop")
        .unwrap();
    assert!(h.app.model_access.retry());
    h.drain().await;
    assert!(!h.app.model_access.has_pending(), "{}", h.render());
    assert_eq!(h.metadata().accounts[0].as_str(), "retry-account");
    let recorded: (String, String) = db
        .query_row(
            "SELECT operation_id, lower(hex(request_hash)) FROM owner_operations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        original,
        format!("Original request: {} {}", recorded.0, recorded.1)
    );
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    drop(db);
    h.close().await;
}

#[tokio::test]
async fn compact_form_keeps_the_active_field_visible_after_a_long_url() {
    let mut h = Harness::new(true, &["account-one".into()]).await;
    h.collection('2').await;
    h.key(KeyCode::Char('n'));
    for _ in 0..6 {
        h.key(KeyCode::Tab);
    }
    h.text(&format!("https://example.invalid/{}", "x".repeat(2000)));
    h.app.reduce(UiEvent::Resize(48, 18)).unwrap();
    assert!(h.render_at(48, 18).contains("> HTTP URL"));
    h.key(KeyCode::Tab);
    let rendered = h.render_at(48, 18);
    assert!(
        rendered.contains("> Model ID"),
        "active field must stay visible: {rendered}"
    );
    assert!(
        h.app.model_access.form.as_ref().unwrap().fields[6]
            .value
            .len()
            > 2000
    );
    h.close().await;
}
