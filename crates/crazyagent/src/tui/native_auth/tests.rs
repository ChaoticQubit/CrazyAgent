use super::*;
use crate::adapters::{claude, codex};
use crate::client_controller::OwnerController;
use crate::config::ModelConnection;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, NativeAuthAccount, NativeAuthAction,
    NativeAuthCancel, NativeAuthFact, NativeAuthLogout, NativeAuthMethod, NativeAuthPhase,
    NativeAuthQuery, NativeAuthReply, NativeAuthScope, NativeAuthStart, NativeProfileBinding,
    OwnerClientKind, OwnerData, OwnerRead, OwnerReadReply,
};
use crate::protocol::{ConfigId, CredentialRef, RequestId};
use crate::tui::app::{App, Focus, UiEvent};
use crate::tui::client::{Connection, OwnerEvent};
use crate::tui::{key_event, model_access, views};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rusqlite::OpenFlags;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-codex", "account-claude", "account-api", "account-local"]
[[model_access.connections]]
id = "a-codex"
provider = "provider-codex"
harness = "codex"
protocol = "native"
account_ref = "account-codex"
endpoint = { kind = "native" }
model = { id = "model-codex", author = "author-codex" }
auth = { method = "native_managed", profile_ref = "profile-codex" }
[[model_access.connections]]
id = "b-claude"
provider = "provider-claude"
harness = "claude"
protocol = "native"
account_ref = "account-claude"
endpoint = { kind = "native" }
model = { id = "model-claude", author = "author-claude" }
auth = { method = "native_managed", profile_ref = "profile-claude" }
[[model_access.connections]]
id = "c-api"
provider = "provider-api"
harness = "rcli"
protocol = "openai"
account_ref = "account-api"
endpoint = { kind = "http", url = "https://api.example.invalid/v1" }
model = { id = "model-api", author = "author-api" }
auth = { method = "api_key", credential_ref = "key-reference-only" }
[[model_access.connections]]
id = "d-local"
provider = "provider-local"
harness = "rcli"
protocol = "openai"
account_ref = "account-local"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "model-local", author = "author-local" }
auth = { method = "none" }
[[model_access.pools]]
id = "pool-fixture"
members = ["a-codex", "b-claude", "c-api", "d-local"]
projects = ["project-fixture"]
roles = ["leader"]
fallback_allowed = false
"#;

struct Case {
    root: tempfile::TempDir,
    global: PathBuf,
    project: PathBuf,
    service: Mutex<Option<EngineService>>,
    stopped: tokio::sync::watch::Receiver<bool>,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-ui736-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        let project = root.path().join("project.toml");
        std::fs::write(&global, CATALOG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &project,
            &stopped,
        )
        .await
        .unwrap();
        Self {
            root,
            global,
            project,
            stopped,
            service: Mutex::new(Some(service)),
            _stop: stop,
        }
    }

    async fn controller(&self) -> OwnerController {
        OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(2),
        )
        .await
        .unwrap()
    }

    async fn selected(&self, id: &str) -> (model_access::Basis, ModelConnection) {
        let mut controller = self.controller().await;
        let reply = controller.read(record_query(id)).await.unwrap();
        let OwnerData::ModelAccessRecord {
            source_hash,
            writable,
            record: ModelAccessRecord::Connection { connection },
        } = reply.data
        else {
            panic!("actual selected connection read required");
        };
        assert_eq!(connection.id.as_str(), id);
        (
            model_access::Basis {
                identity: controller.identity().clone(),
                revision: reply.revision,
                config_id: reply.config_id,
                source_hash,
                writable,
            },
            connection,
        )
    }

    fn observer(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn counts(&self) -> Vec<i64> {
        let db = self.observer();
        [
            "operations",
            "events",
            "projects",
            "tasks",
            "configs",
            "native_auth_operations",
        ]
        .iter()
        .map(|table| {
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        })
        .collect()
    }

    async fn restart(&self) {
        self.service
            .lock()
            .await
            .take()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        let service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.global,
            &self.project,
            &self.stopped,
        )
        .await
        .unwrap();
        *self.service.lock().await = Some(service);
    }
}

async fn run<F, T>(body: F)
where
    F: FnOnce(Arc<Case>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let case = Arc::new(Case::new().await);
    let mut task = tokio::spawn(body(Arc::clone(&case)));
    let result = tokio::time::timeout(Duration::from_secs(20), &mut task).await;
    if result.is_err() {
        task.abort();
        let _ = task.await;
    }
    let cleanup = case.service.lock().await.take().unwrap().shutdown().await;
    assert!(cleanup.is_ok(), "actual engine cleanup failed");
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => std::panic::resume_unwind(error.into_panic()),
        Err(_) => panic!("TUI native authentication case exceeded 20 seconds"),
    }
}

struct Ui {
    connection: Connection,
    app: App,
}

impl Ui {
    async fn new(case: &Case) -> Self {
        let client = Client::new(case.root.path().join("e.sock"), Duration::from_secs(2)).unwrap();
        let mut ui = Self {
            connection: Connection::connect_paired(client).unwrap(),
            app: App::new(),
        };
        ui.app.reduce(UiEvent::FocusNext).unwrap();
        ui.app.reduce(UiEvent::FocusNext).unwrap();
        ui.drain().await;
        assert_eq!(ui.app.focus(), Focus::Settings);
        assert!(ui.app.owner_ready);
        ui
    }

    async fn owner_event(&mut self) -> OwnerEvent {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(event) = self.connection.poll_owner().unwrap() {
                return event;
            }
            assert!(
                Instant::now() < deadline,
                "bounded actual owner reply deadline"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn drain(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while let Some(event) = self.connection.poll_owner().unwrap() {
                self.app.apply_owner(event).unwrap();
            }
            if !self.connection.busy()
                && let Some(job) = self.app.model_access_request().unwrap()
            {
                self.connection.model_access(job).unwrap();
            }
            if !self.connection.busy()
                && !self.app.model_access.loading()
                && (self.app.owner_ready || self.app.owner_problem.is_some())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "bounded actual UI metadata drain"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn key(&mut self, code: KeyCode) {
        let event = key_event(KeyEvent::new(code, KeyModifiers::NONE), &self.app).unwrap();
        self.app.reduce(event).unwrap();
    }

    async fn connections(&mut self, selected: usize) {
        self.key(KeyCode::Char('2'));
        self.drain().await;
        for _ in 0..selected {
            self.key(KeyCode::Down);
        }
    }

    async fn exchange(&mut self, job: Job) -> Event {
        self.connection
            .model_access(model_access::Job::NativeAuth(Box::new(job)))
            .unwrap();
        let OwnerEvent::ModelAccess(event) = self.owner_event().await else {
            panic!("actual model access worker reply required");
        };
        let model_access::Event::NativeAuth(event) = *event else {
            panic!("actual native authentication worker reply required");
        };
        *event
    }

    async fn auth(&mut self) -> Event {
        let model_access::Job::NativeAuth(job) = self.app.model_access_request().unwrap().unwrap()
        else {
            panic!("actual selected connection must enqueue native authentication");
        };
        self.exchange(*job).await
    }

    fn apply(&mut self, event: Event) {
        self.app
            .apply_owner(OwnerEvent::ModelAccess(Box::new(
                model_access::Event::NativeAuth(Box::new(event)),
            )))
            .unwrap();
    }

    fn render(&self, width: u16, height: u16) -> String {
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
}

fn reference(id: &str) -> CredentialRef {
    CredentialRef::new(id).unwrap()
}

fn request(id: &str) -> RequestId {
    RequestId::new(id).unwrap()
}

fn record_query(id: &str) -> OwnerRead {
    OwnerRead::ModelAccessRecord {
        collection: ModelAccessCollection::Connections,
        id: reference(id),
    }
}

fn scope(basis: &model_access::Basis, connection: &ModelConnection) -> NativeAuthScope {
    NativeAuthScope {
        expected_database_instance: basis.identity.database_instance().into(),
        connection: connection.id.clone(),
        expected_config_id: basis.config_id.clone(),
        expected_source_hash: basis.source_hash.clone(),
    }
}

fn advance(state: &mut State, count: usize) {
    for _ in 0..count {
        assert!(state.reduce(&UiEvent::SelectionNext).unwrap());
    }
}

fn real_reply(event: &Event) -> &NativeAuthReply {
    let Ok(Output::Auth(reply)) = &event.result else {
        panic!("actual owner authentication reply required");
    };
    reply
}

fn assert_unavailable(reply: &NativeAuthReply, connection: &str) {
    assert_eq!(reply.fact.binding.connection.id.as_str(), connection);
    assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
    assert_eq!(
        reply.fact.code.as_deref(),
        Some("native_auth_host_unavailable")
    );
    assert!(reply.account.is_none());
    assert!(reply.challenge.is_none());
    assert!(reply.fact.generation > 0);
}

fn unit_inspected(basis: model_access::Basis, connection: ModelConnection) -> State {
    let mut state = State::default();
    state.open(basis.clone(), connection.clone()).unwrap();
    let job = state.request().unwrap();
    assert!(matches!(job.command, Command::Inspect));
    let reply = OwnerReadReply {
        revision: basis.revision,
        config_id: basis.config_id,
        data: OwnerData::ModelAccessRecord {
            source_hash: basis.source_hash,
            writable: basis.writable,
            record: ModelAccessRecord::Connection { connection },
        },
    };
    state
        .apply(Event {
            job,
            result: Ok(Output::Inspected(Box::new(reply))),
        })
        .unwrap();
    assert!(state.request().is_none());
    state
}

fn unit_started(basis: model_access::Basis, connection: ModelConnection) -> (State, Job) {
    let mut state = unit_inspected(basis, connection);
    state.reduce(&UiEvent::Submit).unwrap();
    let job = state.request().unwrap();
    assert!(matches!(job.command, Command::Start(_)));
    (state, job)
}

fn unit_signed_out_reply(job: &Job, email: &str) -> NativeAuthReply {
    let Command::Start(start) = &job.command else {
        panic!("unit input requires a retained start command");
    };
    let basis = &job.context.basis;
    let status = serde_json::json!({
        "loggedIn": false, "authMethod": "none", "email": email,
        "apiProvider": "unit-provider", "configDirectory": "/synthetic/unit-profile",
    });
    NativeAuthReply {
        request_id: start.request_id.clone(),
        database_instance: basis.identity.database_instance().into(),
        engine_epoch: basis.identity.engine_epoch().into(),
        fact: NativeAuthFact {
            operation_id: start.request_id.clone(),
            action: NativeAuthAction::SignIn {
                method: start.method,
            },
            binding: NativeProfileBinding {
                database_instance: basis.identity.database_instance().into(),
                engine_epoch: basis.identity.engine_epoch().into(),
                config_id: basis.config_id.clone(),
                source_hash: basis.source_hash.clone(),
                at_revision: basis.revision.clone(),
                connection: job.context.connection.clone(),
                custody_id: "a".repeat(64),
            },
            generation: 1,
            phase: NativeAuthPhase::UnauthenticatedObserved,
            code: None,
        },
        challenge: None,
        account: Some(NativeAuthAccount::Claude(
            claude::decode_auth_status(&serde_json::to_vec(&status).unwrap()).unwrap(),
        )),
    }
}

#[tokio::test]
async fn real_selected_connection_panel_inspects_without_starting_authentication() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.connections(1).await;
        let before = case.counts();
        ui.key(KeyCode::F(2));
        let event = ui.auth().await;
        assert!(matches!(event.job.command, Command::Inspect));
        assert_eq!(event.job.context.connection.id.as_str(), "b-claude");
        assert!(matches!(&event.result, Ok(Output::Inspected(_))));
        ui.apply(event);
        assert!(ui.app.model_access_request().unwrap().is_none());
        assert_eq!(case.counts(), before);
        let rendered = ui.render(120, 40);
        assert!(rendered.contains("b-claude"));
        assert!(rendered.contains("Claude"));
        assert!(!rendered.contains("Ready"));
    })
    .await;
}

#[tokio::test]
async fn real_worker_start_read_cancel_preserve_original_operation_and_new_request_ids() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.connections(0).await;
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        ui.apply(inspected);
        ui.key(KeyCode::Enter);
        let started = ui.auth().await;
        let Command::Start(start) = &started.job.command else {
            panic!("start required")
        };
        let operation = start.request_id.clone();
        let original_scope = start.scope.clone();
        assert_eq!(start.method, NativeAuthMethod::CodexBrowser);
        assert_eq!(real_reply(&started).request_id, operation);
        assert_eq!(real_reply(&started).fact.operation_id, operation);
        assert_unavailable(real_reply(&started), "a-codex");
        ui.apply(started);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let read = ui.auth().await;
        let Command::Read(query) = &read.job.command else {
            panic!("read required")
        };
        assert_eq!(query.operation_id, operation);
        assert_eq!(query.scope, original_scope);
        assert_ne!(query.request_id, operation);
        let read_id = query.request_id.clone();
        assert_eq!(real_reply(&read).request_id, read_id);
        assert_eq!(real_reply(&read).fact.operation_id, operation);
        ui.apply(read);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let canceled = ui.auth().await;
        let Command::Cancel(cancel) = &canceled.job.command else {
            panic!("cancel required")
        };
        assert_eq!(cancel.operation_id, operation);
        assert_eq!(cancel.scope, original_scope);
        assert_ne!(cancel.request_id, operation);
        assert_ne!(cancel.request_id, read_id);
        assert_eq!(
            canceled.result.as_ref().err().map(String::as_str),
            Some("native_auth_not_active")
        );
        ui.apply(canceled);
        assert!(ui.app.owner_ready, "domain rejection must not detach owner");
        assert_eq!(case.counts().last(), Some(&1));
    })
    .await;
}

#[tokio::test]
async fn real_worker_dispatches_codex_device_method_without_fabricating_challenge() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.connections(0).await;
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        ui.apply(inspected);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let event = ui.auth().await;
        let Command::Start(start) = &event.job.command else {
            panic!("start required")
        };
        assert_eq!(start.method, NativeAuthMethod::CodexDeviceCode);
        assert_unavailable(real_reply(&event), "a-codex");
        ui.apply(event);
        assert!(!ui.render(120, 40).contains("verification_url"));
    })
    .await;
}

#[tokio::test]
async fn real_worker_dispatches_claude_official_for_the_selected_profile() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.connections(1).await;
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        ui.apply(inspected);
        ui.key(KeyCode::Enter);
        let event = ui.auth().await;
        let Command::Start(start) = &event.job.command else {
            panic!("start required")
        };
        assert_eq!(start.method, NativeAuthMethod::ClaudeOfficial);
        assert_eq!(start.scope.connection.as_str(), "b-claude");
        assert_unavailable(real_reply(&event), "b-claude");
        assert_eq!(
            real_reply(&event).fact.binding.connection.harness.as_str(),
            "claude"
        );
        ui.apply(event);
        assert_eq!(case.counts().last(), Some(&1));
    })
    .await;
}

#[tokio::test]
async fn real_logout_requires_confirmation_and_has_its_own_operation() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.connections(0).await;
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        ui.apply(inspected);
        let before = case.counts();
        for _ in 0..4 {
            ui.key(KeyCode::Down);
        }
        ui.key(KeyCode::Enter);
        assert!(ui.app.model_access_request().unwrap().is_none());
        ui.key(KeyCode::Enter);
        assert!(ui.app.model_access_request().unwrap().is_none());
        assert_eq!(case.counts(), before, "Back must be confirmation default");
        ui.key(KeyCode::Enter);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let event = ui.auth().await;
        let Command::Logout(logout) = &event.job.command else {
            panic!("logout required")
        };
        assert_eq!(real_reply(&event).request_id, logout.request_id);
        assert_eq!(real_reply(&event).fact.operation_id, logout.request_id);
        assert_eq!(real_reply(&event).fact.action, NativeAuthAction::Logout);
        assert_unavailable(real_reply(&event), "a-codex");
        ui.apply(event);
        assert_eq!(case.counts().last(), Some(&1));
    })
    .await;
}

#[tokio::test]
async fn real_controller_forwards_all_four_methods_without_metadata_or_pool_mutations() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let scope = scope(&basis, &connection);
        let mut controller = case.controller().await;
        let before = case.counts();
        let started = controller
            .start_native_auth(NativeAuthStart {
                request_id: request("controller-start"),
                scope: scope.clone(),
                method: NativeAuthMethod::CodexBrowser,
            })
            .await
            .unwrap();
        assert_unavailable(&started, "a-codex");
        let read = controller
            .read_native_auth(NativeAuthQuery {
                request_id: request("controller-read"),
                scope: scope.clone(),
                operation_id: started.fact.operation_id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(read.fact, started.fact);
        let canceled = controller
            .cancel_native_auth(NativeAuthCancel {
                request_id: request("controller-cancel"),
                scope: scope.clone(),
                operation_id: started.fact.operation_id.clone(),
            })
            .await;
        assert_eq!(canceled.unwrap_err().0, "native_auth_not_active");
        let logout = controller
            .logout_native_auth(NativeAuthLogout {
                request_id: request("controller-logout"),
                scope,
            })
            .await
            .unwrap();
        assert_unavailable(&logout, "a-codex");
        assert_eq!(logout.fact.action, NativeAuthAction::Logout);
        let after = case.counts();
        assert_eq!(&after[..5], &before[..5]);
        assert_eq!(after[5], before[5] + 2);
        assert_eq!(std::fs::read(&case.global).unwrap(), CATALOG.as_bytes());
        assert!(!case.project.exists());
    })
    .await;
}

#[tokio::test]
async fn real_controller_detach_returns_actual_disconnected_error_without_dispatch() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let mut controller = case.controller().await;
        controller.detach();
        let before = case.counts();
        let result = controller
            .logout_native_auth(NativeAuthLogout {
                request_id: request("detached-logout"),
                scope: scope(&basis, &connection),
            })
            .await;
        assert_eq!(result.unwrap_err().0, "controller_disconnected");
        assert_eq!(case.counts(), before);
    })
    .await;
}

#[tokio::test]
async fn real_api_and_local_connections_cannot_dispatch_native_authentication() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        for selected in [2, 3] {
            ui.connections(selected).await;
            let before = case.counts();
            let event =
                key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE), &ui.app).unwrap();
            let _ = ui.app.reduce(event);
            assert!(ui.app.model_access_request().unwrap().is_none());
            assert_eq!(case.counts(), before);
            assert!(!ui.render(120, 40).contains("Sign in"));
        }
    })
    .await;
}

#[tokio::test]
async fn real_accounts_pools_and_pinned_views_do_not_open_a_connection_panel() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        for collection in ['1', '3', '4'] {
            ui.key(KeyCode::Char(collection));
            ui.drain().await;
            let before = case.counts();
            let _ = ui.app.reduce(UiEvent::ConfigureMaster);
            assert!(ui.app.model_access_request().unwrap().is_none());
            assert_eq!(case.counts(), before);
            assert!(!ui.render(120, 40).contains("Sign in"));
        }
    })
    .await;
}

#[tokio::test]
async fn real_form_and_conversation_keys_preserve_existing_drafts_and_focus_cycle() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.app.reduce(UiEvent::FocusPrevious).unwrap();
        ui.app.reduce(UiEvent::FocusPrevious).unwrap();
        for character in "retained conversation".chars() {
            ui.app.reduce(UiEvent::Character(character)).unwrap();
        }
        ui.app.reduce(UiEvent::FocusNext).unwrap();
        ui.app.reduce(UiEvent::FocusNext).unwrap();
        ui.connections(0).await;
        ui.key(KeyCode::Char('n'));
        ui.key(KeyCode::Char('q'));
        let form = ui
            .app
            .model_access
            .render(false)
            .split_once("Add/Create Connections")
            .unwrap()
            .1
            .to_owned();
        let event = key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE), &ui.app).unwrap();
        let _ = ui.app.reduce(event);
        assert!(ui.app.model_access_request().unwrap().is_none());
        assert!(ui.app.model_access.render(false).ends_with(&form));
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        ui.apply(inspected);
        for code in [
            KeyCode::Char('x'),
            KeyCode::Backspace,
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            ui.key(code);
        }
        ui.key(KeyCode::Esc);
        ui.key(KeyCode::Char('n'));
        assert!(ui.app.model_access.render(false).ends_with(&form));
        ui.key(KeyCode::Esc);
        for expected in [
            Focus::Plan,
            Focus::Supervision,
            Focus::Memory,
            Focus::Evidence,
            Focus::Conversation,
            Focus::Projects,
            Focus::Settings,
        ] {
            ui.key(KeyCode::Tab);
            assert_eq!(ui.app.focus(), expected);
        }
        assert_eq!(ui.app.draft(), "retained conversation");
    })
    .await;
}

#[tokio::test]
async fn real_changed_source_is_rejected_before_start_and_does_not_rebase_scope() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let mut state = State::default();
        state.open(basis.clone(), connection).unwrap();
        let job = state.request().unwrap();
        let mut ui = Ui::new(&case).await;
        std::fs::write(
            &case.global,
            CATALOG.replace("model-codex", "model-changed"),
        )
        .unwrap();
        let event = ui.exchange(job).await;
        let _ = state.apply(event);
        let _ = state.reduce(&UiEvent::Submit);
        assert!(state.request().is_none());
        assert!(state.operation_id().is_none());
        assert_eq!(case.counts().last(), Some(&0));
        assert!(!state.render(false).contains("Ready"));
    })
    .await;
}

#[tokio::test]
async fn real_old_owner_context_is_rejected_after_engine_restart() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let mut state = State::default();
        state.open(basis.clone(), connection).unwrap();
        let job = state.request().unwrap();
        case.restart().await;
        let mut ui = Ui::new(&case).await;
        let event = ui.exchange(job).await;
        assert!(event.result.is_err());
        state.apply(event).unwrap();
        let _ = state.reduce(&UiEvent::Submit);
        assert!(state.request().is_none());
        assert_eq!(case.counts().last(), Some(&0));
        let current = case.controller().await;
        assert_eq!(
            basis.identity.database_instance(),
            current.identity().database_instance()
        );
        assert_ne!(
            basis.identity.engine_epoch(),
            current.identity().engine_epoch()
        );
    })
    .await;
}

#[tokio::test]
async fn unit_open_requires_a_fresh_inspection_before_any_sign_in() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let mut state = State::default();
        state.open(basis, connection).unwrap();
        let inspect = state.request().unwrap();
        assert!(matches!(inspect.command, Command::Inspect));
        assert!(state.is_open());
        let _ = state.reduce(&UiEvent::Submit);
        assert!(state.request().is_none());
        assert!(state.operation_id().is_none());
        state
            .apply(Event {
                job: inspect,
                result: Err("metadata_owner_unavailable".into()),
            })
            .unwrap();
        let _ = state.reduce(&UiEvent::Submit);
        assert!(state.request().is_none());
    })
    .await;
}

#[tokio::test]
async fn unit_read_and_cancel_without_original_operation_are_disabled() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let mut state = unit_inspected(basis, connection);
        advance(&mut state, 2);
        let _ = state.reduce(&UiEvent::Submit);
        assert!(state.request().is_none());
        advance(&mut state, 1);
        let _ = state.reduce(&UiEvent::Submit);
        assert!(state.request().is_none());
        assert!(state.operation_id().is_none());
    })
    .await;
}

#[tokio::test]
async fn unit_one_flight_prevents_duplicate_start_and_retains_original_identity() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let (mut state, job) = unit_started(basis, connection);
        let Command::Start(start) = job.command else {
            panic!("start required")
        };
        assert_eq!(state.operation_id(), Some(&start.request_id));
        for _ in 0..3 {
            let _ = state.reduce(&UiEvent::Submit);
        }
        assert!(state.request().is_none());
        assert_eq!(state.operation_id(), Some(&start.request_id));
    })
    .await;
}

#[tokio::test]
async fn unit_detach_drops_account_presentation_but_preserves_original_operation() {
    run(|case| async move {
        let (basis, connection) = case.selected("b-claude").await;
        let (mut state, job) = unit_started(basis.clone(), connection);
        let reply = unit_signed_out_reply(&job, "unit-private@example.invalid");
        let operation = reply.fact.operation_id.clone();
        state
            .apply(Event {
                job,
                result: Ok(Output::Auth(Box::new(reply))),
            })
            .unwrap();
        assert!(state.render(false).contains("unit-private@example.invalid"));
        state.access(None, false);
        assert!(!state.render(false).contains("unit-private@example.invalid"));
        assert_eq!(state.operation_id(), Some(&operation));
        state.access(Some(basis.identity), true);
        if let Some(job) = state.request() {
            let Command::Read(query) = job.command else {
                panic!("only reconciliation read")
            };
            assert_eq!(query.operation_id, operation);
            assert_ne!(query.request_id, operation);
        }
    })
    .await;
}

#[tokio::test]
async fn unit_escape_and_focus_changes_hide_locally_without_canceling_native_operation() {
    run(|case| async move {
        let (basis, connection) = case.selected("b-claude").await;
        for key in [UiEvent::Cancel, UiEvent::FocusNext, UiEvent::FocusPrevious] {
            let (mut state, job) = unit_started(basis.clone(), connection.clone());
            let reply = unit_signed_out_reply(&job, "hide-me@example.invalid");
            let operation = reply.fact.operation_id.clone();
            state
                .apply(Event {
                    job,
                    result: Ok(Output::Auth(Box::new(reply))),
                })
                .unwrap();
            let consumed = state.reduce(&key).unwrap();
            assert_eq!(consumed, matches!(key, UiEvent::Cancel));
            assert!(!state.is_open());
            assert!(!state.render(false).contains("hide-me@example.invalid"));
            assert_eq!(state.operation_id(), Some(&operation));
            assert!(state.request().is_none());
        }
    })
    .await;
}

#[tokio::test]
async fn unit_late_reply_after_local_dismissal_cannot_reopen_or_show_account() {
    run(|case| async move {
        let (basis, connection) = case.selected("b-claude").await;
        let (mut state, job) = unit_started(basis, connection);
        let reply = unit_signed_out_reply(&job, "late-private@example.invalid");
        let operation = reply.fact.operation_id.clone();
        state.reduce(&UiEvent::Cancel).unwrap();
        state
            .apply(Event {
                job,
                result: Ok(Output::Auth(Box::new(reply))),
            })
            .unwrap();
        assert!(!state.is_open());
        assert!(!state.render(false).contains("late-private@example.invalid"));
        assert_eq!(state.operation_id(), Some(&operation));
    })
    .await;
}

#[tokio::test]
async fn unit_stale_request_operation_database_epoch_config_source_and_binding_are_rejected() {
    run(|case| async move {
        let (basis, connection) = case.selected("b-claude").await;
        for mismatch in 0..10 {
            let (mut state, job) = unit_started(basis.clone(), connection.clone());
            let mut reply = unit_signed_out_reply(&job, "stale-private@example.invalid");
            match mismatch {
                0 => reply.request_id = request("other-request"),
                1 => reply.fact.operation_id = request("other-operation"),
                2 => reply.database_instance = "f".repeat(64),
                3 => reply.engine_epoch = "f".repeat(64),
                4 => reply.fact.binding.config_id = ConfigId::new("other-config").unwrap(),
                5 => reply.fact.binding.source_hash = "f".repeat(64),
                6 => reply.fact.binding.connection.id = reference("other-connection"),
                7 => reply.fact.action = NativeAuthAction::Logout,
                8 => reply.fact.binding.engine_epoch = "f".repeat(64),
                _ => reply.fact.generation = 0,
            }
            let _ = state.apply(Event {
                job: job.clone(),
                result: Ok(Output::Auth(Box::new(reply))),
            });
            assert!(
                !state
                    .render(false)
                    .contains("stale-private@example.invalid"),
                "mismatched pure unit input {mismatch}"
            );
            let valid = unit_signed_out_reply(&job, "current-private@example.invalid");
            state
                .apply(Event {
                    job,
                    result: Ok(Output::Auth(Box::new(valid))),
                })
                .unwrap();
            assert!(
                state
                    .render(false)
                    .contains("current-private@example.invalid")
            );
        }
    })
    .await;
}

#[tokio::test]
async fn unit_new_selection_or_source_cannot_accept_old_view_reply() {
    run(|case| async move {
        let (basis, connection) = case.selected("b-claude").await;
        for changed_source in [false, true] {
            let (mut state, job) = unit_started(basis.clone(), connection.clone());
            let reply = unit_signed_out_reply(&job, "old-view@example.invalid");
            let mut next_basis = basis.clone();
            let mut next = connection.clone();
            if changed_source {
                next_basis.source_hash = "e".repeat(64);
            } else {
                next.id = reference("selected-other");
            }
            let _ = state.open(next_basis, next);
            state
                .apply(Event {
                    job,
                    result: Ok(Output::Auth(Box::new(reply))),
                })
                .unwrap();
            assert!(!state.render(false).contains("old-view@example.invalid"));
        }
    })
    .await;
}

#[tokio::test]
async fn unit_replacement_reply_without_account_clears_previous_presentation() {
    run(|case| async move {
        let (basis, connection) = case.selected("b-claude").await;
        let (mut state, job) = unit_started(basis, connection);
        let mut reply = unit_signed_out_reply(&job, "previous-private@example.invalid");
        let fact = reply.fact.clone();
        state
            .apply(Event {
                job,
                result: Ok(Output::Auth(Box::new(reply))),
            })
            .unwrap();
        assert!(
            state
                .render(false)
                .contains("previous-private@example.invalid")
        );
        advance(&mut state, 1);
        state.reduce(&UiEvent::Submit).unwrap();
        let read = state.request().unwrap();
        let Command::Read(query) = &read.command else {
            panic!("read required")
        };
        reply = NativeAuthReply {
            request_id: query.request_id.clone(),
            database_instance: read.context.basis.identity.database_instance().into(),
            engine_epoch: read.context.basis.identity.engine_epoch().into(),
            fact,
            challenge: None,
            account: None,
        };
        state
            .apply(Event {
                job: read,
                result: Ok(Output::Auth(Box::new(reply))),
            })
            .unwrap();
        assert!(
            !state
                .render(false)
                .contains("previous-private@example.invalid")
        );
    })
    .await;
}

#[tokio::test]
async fn unit_generation_and_custody_changes_cannot_replace_pinned_operation() {
    run(|case| async move {
        let (basis, connection) = case.selected("b-claude").await;
        for generation_changed in [false, true] {
            let (mut state, job) = unit_started(basis.clone(), connection.clone());
            let reply = unit_signed_out_reply(&job, "initial-private@example.invalid");
            let mut fact = reply.fact.clone();
            state
                .apply(Event {
                    job,
                    result: Ok(Output::Auth(Box::new(reply))),
                })
                .unwrap();
            advance(&mut state, 1);
            state.reduce(&UiEvent::Submit).unwrap();
            let read = state.request().unwrap();
            let Command::Read(query) = &read.command else {
                panic!("read required")
            };
            if generation_changed {
                fact.generation += 1;
            } else {
                fact.binding.custody_id = "b".repeat(64);
            }
            let reply = NativeAuthReply {
                request_id: query.request_id.clone(),
                database_instance: basis.identity.database_instance().into(),
                engine_epoch: basis.identity.engine_epoch().into(),
                fact,
                challenge: None,
                account: None,
            };
            let _ = state.apply(Event {
                job: read,
                result: Ok(Output::Auth(Box::new(reply))),
            });
            assert!(
                state
                    .render(false)
                    .contains("initial-private@example.invalid")
            );
        }
    })
    .await;
}

#[tokio::test]
async fn unit_transport_error_is_fixed_safe_text_and_keeps_unresolved_operation() {
    run(|case| async move {
        let (basis, connection) = case.selected("a-codex").await;
        let (mut state, job) = unit_started(basis, connection);
        let operation = state.operation_id().unwrap().clone();
        state
            .apply(Event {
                job,
                result: Err("native secret \u{1b}[2J private@example.invalid".into()),
            })
            .unwrap();
        let rendered = state.render(false);
        assert!(!rendered.contains("native secret"));
        assert!(!rendered.contains("private@example.invalid"));
        assert!(!rendered.contains('\u{1b}'));
        assert_eq!(state.operation_id(), Some(&operation));
        let _ = state.reduce(&UiEvent::Submit);
        assert!(
            state.request().is_none(),
            "uncertain start must not be repeated"
        );
    })
    .await;
}

#[test]
fn unit_codex_challenge_rendering_preserves_exact_safe_url_and_code_without_login_id() {
    let challenge = codex::owner_login_challenge(
        "unit-secret-login-id",
        "https://auth.example.invalid/device?state=unit%2Bstate",
        Some("UNIT-CODE"),
    )
    .unwrap();
    let rendered = render_challenge(&challenge);
    assert!(rendered.contains("https://auth.example.invalid/device?state=unit%2Bstate"));
    assert!(rendered.contains("UNIT-CODE"));
    assert!(!rendered.contains("unit-secret-login-id"));
    assert!(rendered.len() <= 65536);
}

#[test]
fn unit_unsafe_challenge_is_not_silently_rewritten_into_an_exact_action_url() {
    let url = "https://auth.example.invalid/\u{202e}unit-reversed";
    let challenge = codex::owner_login_challenge("unit-login", url, None).unwrap();
    let rendered = render_challenge(&challenge);
    assert!(!rendered.contains(url));
    assert!(!rendered.contains("auth.example.invalid"));
    assert!(rendered.contains("cannot display safely"));
    assert!(!rendered.contains('\u{202e}'));
}

#[test]
fn unit_account_rendering_visibly_escapes_controls_and_bidi_without_claiming_readiness() {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "loggedIn": false, "authMethod": "none",
        "apiProvider": "unit-provider",
        "email": "unit\u{1b}[2J\n\t\u{202e}@example.invalid",
        "configDirectory": "/observed/not-authority", "subscriptionType": null,
    }))
    .unwrap();
    let account = NativeAuthAccount::Claude(claude::decode_auth_status(&bytes).unwrap());
    let rendered = render_account(&account);
    assert!(!rendered.contains('\u{1b}'));
    assert!(!rendered.contains('\u{202e}'));
    assert!(
        !rendered.contains("unit[2J"),
        "terminal control must not disappear silently"
    );
    assert!(!rendered.contains("Ready"));
    assert!(rendered.len() <= 65536);
    assert!(rendered.contains("false") || rendered.contains("Signed out"));
}

#[test]
fn unit_unknown_claude_account_method_is_observed_not_promoted_to_subscription() {
    let account = NativeAuthAccount::Claude(
        claude::decode_auth_status(
            br#"{
            "loggedIn":false,"authMethod":"future_method","apiProvider":"future_provider",
            "configDirectory":"/synthetic/unit-profile"
        }"#,
        )
        .unwrap(),
    );
    let rendered = render_account(&account);
    assert!(rendered.contains("future_method"));
    assert!(!rendered.contains("Ready"));
    assert!(!rendered.contains("subscription authenticated"));
}

#[tokio::test]
async fn real_compact_and_tiny_rendering_use_panel_body_without_changing_metadata() {
    run(|case| async move {
        let mut ui = Ui::new(&case).await;
        ui.connections(0).await;
        let before = case.counts();
        ui.key(KeyCode::F(2));
        let inspected = ui.auth().await;
        ui.apply(inspected);
        for (width, height) in [(120, 40), (40, 10), (12, 6)] {
            let rendered = ui.render(width, height);
            assert!(rendered.contains("a-codex"));
            assert!(!rendered.contains("Ready"));
            assert!(!rendered.contains('\u{1b}'));
        }
        let tiny = ui.render(11, 5);
        assert!(tiny.contains("Resize"));
        assert!(!tiny.contains("a-codex"));
        assert_eq!(case.counts(), before);
    })
    .await;
}
