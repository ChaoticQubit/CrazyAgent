use super::app::{App, Focus};
use super::{key_event, views};
use crate::protocol::Command;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if let Some(event) = key_event(KeyEvent::new(code, modifiers), app)
        && let Err(error) = app.reduce(event)
    {
        app.notice(&error);
    }
}

fn text(app: &mut App, value: &str) {
    assert!(value.len() <= 65536);
    assert!(!value.chars().any(char::is_control));
    for character in value.chars() {
        key(app, KeyCode::Char(character), KeyModifiers::NONE);
    }
}

fn screen(app: &App, width: u16, height: u16) -> String {
    assert!((12..=300).contains(&width));
    assert!((6..=120).contains(&height));
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| views::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let mut continuation = 0;
            (0..width)
                .filter_map(|x| {
                    if continuation > 0 {
                        continuation -= 1;
                        return None;
                    }
                    let symbol = buffer[(x, y)].symbol();
                    continuation = ratatui::text::Line::from(symbol).width().saturating_sub(1);
                    Some(symbol)
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn declaration() -> App {
    let mut app = App::new();
    text(&mut app, "Retained conversation");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.focus(), Focus::Projects);
    assert!(!app.has_command());
    key(&mut app, KeyCode::Char('N'), KeyModifiers::SHIFT);
    app
}

#[test]
fn uppercase_n_edits_literal_repository_membership() {
    let mut app = declaration();
    text(&mut app, "Product 日本");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    text(&mut app, "/declared/日本 repo, literal");

    assert_eq!(
        app.draft(),
        "/declared/日本 repo, literal",
        "N/name/Tab/path must edit repository membership, not another view or conversation"
    );
    assert_eq!(app.focus(), Focus::Projects);
    let rendered = screen(&app, 100, 28);
    assert!(rendered.contains("Product 日本"), "{rendered}");
    assert!(
        rendered.contains("/declared/日本 repo, literal"),
        "{rendered}"
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.draft(), "/declared/日本 repo, literal");
    assert!(app.take_command().is_none());
}

#[test]
fn declaration_escape_reopens_literal_fields_without_losing_conversation() {
    let mut app = declaration();
    text(&mut app, "Retained product");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    text(&mut app, "/declared/retained path");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('N'), KeyModifiers::SHIFT);

    let rendered = screen(&app, 100, 28);
    assert!(rendered.contains("Retained product"), "{rendered}");
    assert!(rendered.contains("/declared/retained path"), "{rendered}");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(app.focus(), Focus::Conversation);
    assert_eq!(app.draft(), "Retained conversation");
    assert!(!app.has_command());
}

#[test]
fn declaration_row_controls_add_and_remove_literal_membership() {
    let mut app = declaration();
    text(&mut app, "Two repositories");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    text(&mut app, "/declared/frontend path");
    key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    text(&mut app, "/declared/backend, literal");

    assert_eq!(app.draft(), "/declared/backend, literal");
    let rendered = screen(&app, 100, 28);
    assert!(rendered.contains("/declared/frontend path"), "{rendered}");
    assert!(
        rendered.contains("/declared/backend, literal"),
        "{rendered}"
    );
    key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    assert_eq!(app.draft(), "/declared/frontend path");
    assert!(!app.has_command());
}

#[test]
fn compact_declaration_keeps_the_active_literal_path_visible() {
    let mut app = declaration();
    text(&mut app, "Compact product");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    text(&mut app, "/declared/界 repo");
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    text(&mut app, "o");

    let rendered = screen(&app, 48, 18);
    assert!(rendered.contains("/declared/界 repo"), "{rendered}");
    assert!(!rendered.contains('\u{001b}'));
    assert_eq!(app.draft(), "/declared/界 repo");
    assert_eq!(app.focus(), Focus::Projects);
}

#[test]
fn lowercase_n_preserves_ordinary_empty_repository_creation() {
    let mut app = App::new();
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
    text(&mut app, "Legacy 日本");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    let Some(Command::CreateProject { spec, .. }) = app.take_command() else {
        panic!("legacy n must queue the existing ordinary CreateProject command");
    };
    assert_eq!(spec.name, "Legacy 日本");
    assert!(spec.repositories.is_empty());
    assert!(app.take_command().is_none());
}

#[test]
fn declaration_validates_literal_paths_and_retains_duplicate_membership() {
    for path in ["", "relative", "https://example.invalid/repo", "/tmp/a://b"] {
        let mut app = declaration();
        text(&mut app, "Literal name");
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        text(&mut app, path);
        assert!(app.declaration.spec().is_err(), "{path:?}");
        assert_eq!(app.draft(), path);
    }
    let mut app = declaration();
    text(&mut app, " Literal 日本 ");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    text(&mut app, "/absent/a b,c");
    key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    text(&mut app, "/absent/a b,c");
    let spec = app.declaration.spec().unwrap();
    assert_eq!(spec.name, " Literal 日本 ");
    assert_eq!(spec.repositories, ["/absent/a b,c", "/absent/a b,c"]);
}

#[test]
fn declaration_enforces_row_byte_and_aggregate_bounds_without_splitting_unicode() {
    let mut app = declaration();
    text(&mut app, "x");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    text(&mut app, &format!("/{}", "界".repeat(1365)));
    assert_eq!(app.draft().len(), 4096);
    key(&mut app, KeyCode::Char('界'), KeyModifiers::NONE);
    assert_eq!(app.draft().len(), 4096);
    for _ in 1..64 {
        key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    }
    assert_eq!(app.declaration.repositories.len(), 64);
    key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert_eq!(app.declaration.repositories.len(), 64);
    for _ in 0..64 {
        key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    }
    assert_eq!(app.declaration.repositories.len(), 1);
    assert_eq!(app.draft().len(), 4096);
    app.declaration.repositories = vec!["/".repeat(4096); 15];
    app.declaration.repositories.push("/".repeat(4095));
    app.declaration.field = 16;
    assert_eq!(app.declaration.bytes(), 65536);
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.declaration.bytes(), 65536);
}

#[test]
fn declaration_control_input_and_generation_exhaustion_preserve_content() {
    let mut app = declaration();
    text(&mut app, "Name");
    let generation = app.declaration.generation;
    assert!(app.reduce(super::app::UiEvent::Character('\n')).is_err());
    assert_eq!(app.draft(), "Name");
    assert_eq!(app.declaration.generation, generation);
    app.declaration.generation = u64::MAX;
    assert!(app.reduce(super::app::UiEvent::Character('x')).is_err());
    assert_eq!(app.draft(), "Name");
}

struct RealCreation {
    root: tempfile::TempDir,
    service: Option<crate::engine::bootstrap::EngineService>,
    connection: super::client::Connection,
    app: App,
    _stopped: tokio::sync::watch::Receiver<bool>,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl RealCreation {
    async fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .prefix("ca-create373-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let config = crate::config::resolve_config(
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
        )
        .unwrap();
        let service = crate::engine::bootstrap::EngineService::start(
            root.path(),
            "engine.sock",
            &config,
            &stopped,
        )
        .await
        .unwrap();
        let connection = super::client::Connection::connect_paired(
            crate::engine::Client::new(
                root.path().join("engine.sock"),
                std::time::Duration::from_secs(2),
            )
            .unwrap(),
        )
        .unwrap();
        let mut h = Self {
            root,
            service: Some(service),
            connection,
            app: App::new(),
            _stopped: stopped,
            _stop: stop,
        };
        h.settle().await;
        assert!(h.app.owner_ready);
        h
    }

    async fn settle(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            while let Some(event) = self.connection.poll_owner().unwrap() {
                self.app.apply_owner(event).unwrap();
            }
            if !self.connection.busy() {
                if let Some(job) = self.app.take_creation_job() {
                    self.connection.creation(job).unwrap();
                } else if self.app.owner_ready || self.app.owner_problem.is_some() {
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "{}", self.app.status);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        self.snapshot().await;
    }

    async fn snapshot(&mut self) {
        use crate::protocol::{ClientId, Cursor, PROTOCOL_VERSION, RequestId, WireRequest};
        let reply = crate::engine::Client::new(
            self.root.path().join("engine.sock"),
            std::time::Duration::from_secs(2),
        )
        .unwrap()
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("creation-observer").unwrap(),
            request_id: RequestId::new("snapshot").unwrap(),
            expected_revision: Cursor::new(0),
            command: Command::Snapshot,
        })
        .await
        .unwrap();
        self.app.apply_reply(reply).unwrap();
    }

    fn input(&mut self) {
        key(&mut self.app, KeyCode::Tab, KeyModifiers::NONE);
        key(&mut self.app, KeyCode::Char('N'), KeyModifiers::SHIFT);
        text(&mut self.app, "Real declared 日本");
        key(&mut self.app, KeyCode::Tab, KeyModifiers::NONE);
        text(&mut self.app, "/absent/declared 日本 path");
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap()
    }

    async fn reconnect(&mut self) {
        self.connection.reconnect().unwrap();
        self.app.reconnect_owner().unwrap();
        self.settle().await;
    }

    async fn restart(&mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
        let config = crate::config::resolve_config(
            &self.root.path().join("global.toml"),
            &self.root.path().join("project.toml"),
        )
        .unwrap();
        self.service = Some(
            crate::engine::bootstrap::EngineService::start(
                self.root.path(),
                "engine.sock",
                &config,
                &self._stopped,
            )
            .await
            .unwrap(),
        );
        self.reconnect().await;
    }

    async fn close(mut self) {
        drop(self.connection);
        self.service.take().unwrap().shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn real_owner_creation_selects_exact_identity_and_has_no_implicit_effects() {
    let mut h = RealCreation::new().await;
    let db = h.db();
    let protected = [
        "tasks",
        "configs",
        "repository_admissions",
        "worktree_leases",
        "settings_publication",
        "verification_baselines",
        "conversation_entries",
    ];
    let before = protected.map(|table| {
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    });
    assert_eq!(before[0], 0);
    assert_eq!(before[6], 0);
    h.input();
    key(&mut h.app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(h.app.take_command().is_none());
    h.settle().await;
    assert_eq!(h.app.focus(), Focus::Conversation);
    let project = h.app.selected_project().unwrap();
    assert_eq!(project.spec.name, "Real declared 日本");
    assert_eq!(project.spec.repositories, ["/absent/declared 日本 path"]);
    for (table, before) in protected.into_iter().zip(before) {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, before, "{table}");
    }
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert!(!std::path::Path::new("/absent/declared 日本 path").exists());
    drop(db);
    h.close().await;
}

#[tokio::test]
async fn real_not_recorded_creation_reconciles_and_retries_the_exact_intent_once() {
    let mut h = RealCreation::new().await;
    let db = h.db();
    db.execute_batch("CREATE TRIGGER creation_stop BEFORE INSERT ON projects BEGIN SELECT RAISE(ABORT, 'creation-stop'); END;").unwrap();
    h.input();
    key(&mut h.app, KeyCode::Enter, KeyModifiers::NONE);
    h.settle().await;
    let original = h.app.creation_pending_label().unwrap();
    assert!(h.app.projects.is_empty());
    assert!(!h.app.retry_creation());
    h.restart().await;
    assert_eq!(h.app.creation_pending_label().as_ref(), Some(&original));
    db.execute_batch("DROP TRIGGER creation_stop").unwrap();
    assert!(h.app.retry_creation());
    h.settle().await;
    assert!(h.app.creation_pending_label().is_none());
    assert_eq!(h.app.projects.len(), 1);
    let recorded: (String, String) = db.query_row(
        "SELECT operation_id, lower(hex(request_hash)) FROM owner_operations WHERE origin='native_owner'",
        [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(
        original,
        format!("Creation operation {} {}", recorded.0, recorded.1)
    );
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    drop(db);
    h.close().await;
}

#[tokio::test]
async fn real_completion_after_edit_or_cancel_does_not_clear_or_select_replacement() {
    for cancel in [false, true] {
        let mut h = RealCreation::new().await;
        h.input();
        key(&mut h.app, KeyCode::Enter, KeyModifiers::NONE);
        let job = h.app.take_creation_job().unwrap();
        if cancel {
            key(&mut h.app, KeyCode::Esc, KeyModifiers::NONE);
            key(&mut h.app, KeyCode::BackTab, KeyModifiers::SHIFT);
        } else {
            text(&mut h.app, " replacement");
        }
        h.connection.creation(job).unwrap();
        h.settle().await;
        assert_eq!(h.app.projects.len(), 1);
        if cancel {
            assert_eq!(h.app.focus(), Focus::Conversation);
            key(&mut h.app, KeyCode::Tab, KeyModifiers::NONE);
            key(&mut h.app, KeyCode::Char('N'), KeyModifiers::SHIFT);
            assert_eq!(h.app.draft(), "/absent/declared 日本 path");
        } else {
            assert_eq!(h.app.focus(), Focus::Projects);
            assert_eq!(h.app.draft(), "/absent/declared 日本 path replacement");
        }
        h.close().await;
    }
}

#[test]
fn corrupt_declaration_shape_trips_internal_assertion() {
    let mut app = declaration();
    app.declaration.generation = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.reduce(super::app::UiEvent::Resize(80, 24)).unwrap();
    }));
    assert!(result.is_err());
}

#[test]
fn rendered_text_preserves_adjacent_wide_unicode_glyphs() {
    let mut app = App::new();
    text(&mut app, "日本");
    let rendered = screen(&app, 100, 28);
    assert!(rendered.contains("日本"), "{rendered}");
}

#[tokio::test]
async fn real_unknown_creation_blocks_legacy_conversation_and_metadata_mutations() {
    let mut h = RealCreation::new().await;
    let db = h.db();
    db.execute_batch("CREATE TRIGGER creation_cross_stop BEFORE INSERT ON projects BEGIN SELECT RAISE(ABORT, 'creation-cross-stop'); END;").unwrap();
    h.input();
    key(&mut h.app, KeyCode::Enter, KeyModifiers::NONE);
    h.settle().await;
    let original = h.app.creation_pending_label().unwrap();
    key(&mut h.app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut h.app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(h.app.focus(), Focus::Settings);
    assert!(h.app.model_access_request().unwrap().is_none());
    assert!(h.app.reduce(super::app::UiEvent::Submit).is_err());
    key(&mut h.app, KeyCode::BackTab, KeyModifiers::SHIFT);
    key(&mut h.app, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(h.app.focus(), Focus::Conversation);
    assert!(h.app.reduce(super::app::UiEvent::Submit).is_err());
    key(&mut h.app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut h.app, KeyCode::Char('n'), KeyModifiers::NONE);
    text(&mut h.app, "Blocked legacy project");
    assert!(h.app.reduce(super::app::UiEvent::Submit).is_err());
    assert!(h.app.take_command().is_none());
    assert_eq!(h.app.creation_pending_label().as_ref(), Some(&original));
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    drop(db);
    h.close().await;
}

#[tokio::test]
async fn real_same_content_new_generation_and_reopened_form_fence_completion() {
    for reopen in [false, true] {
        let mut h = RealCreation::new().await;
        h.input();
        key(&mut h.app, KeyCode::Enter, KeyModifiers::NONE);
        let job = h.app.take_creation_job().unwrap();
        if reopen {
            key(&mut h.app, KeyCode::Esc, KeyModifiers::NONE);
            key(&mut h.app, KeyCode::Char('N'), KeyModifiers::SHIFT);
        } else {
            key(&mut h.app, KeyCode::Backspace, KeyModifiers::NONE);
            text(&mut h.app, "h");
        }
        assert_eq!(h.app.draft(), "/absent/declared 日本 path");
        h.connection.creation(job).unwrap();
        h.settle().await;
        assert_eq!(h.app.focus(), Focus::Projects);
        assert_eq!(h.app.draft(), "/absent/declared 日本 path");
        assert_eq!(h.app.projects.len(), 1);
        assert!(h.app.creation_pending_label().is_none());
        h.close().await;
    }
}

#[tokio::test]
async fn real_frame_rejection_is_not_reported_as_an_unresolved_mutation() {
    let mut h = RealCreation::new().await;
    h.input();
    h.app.declaration.name = "x".into();
    h.app.declaration.repositories = vec![format!("/{}", "\"".repeat(4095)); 15];
    h.app
        .declaration
        .repositories
        .push(format!("/{}", "\"".repeat(4094)));
    h.app.declaration.field = 16;
    assert_eq!(h.app.declaration.bytes(), 65536);
    key(&mut h.app, KeyCode::Enter, KeyModifiers::NONE);
    h.settle().await;
    assert!(!h.app.has_unresolved_creation());
    assert!(h.app.status.contains("not submitted"), "{}", h.app.status);
    assert_eq!(h.app.declaration.bytes(), 65536);
    assert!(h.app.projects.is_empty());
    let db = h.db();
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    drop(db);
    h.close().await;
}
