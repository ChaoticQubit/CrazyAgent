use super::client::{Connection, OwnerEvent};
use super::{App, Focus, UiEvent, key_events};
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::protocol::{
    ClientId, Command, Cursor, Outcome, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId,
    WireReply, WireRequest,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::os::unix::fs::PermissionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;
use tokio::sync::watch;

const UNASSIGNED: &str = "unassigned draft retained 721";
const MESSAGE: &str = "owner message retained 721 ";
const BRIEF: &str = "brief replacement retained 721";

struct BriefCase {
    app: App,
    connection: Connection,
    service: EngineService,
    _stop: watch::Sender<bool>,
    _root: tempfile::TempDir,
}

async fn engine_request(client: &Client, id: &str, command: Command) -> WireReply {
    client
        .request(WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("tui-key-721").unwrap(),
            request_id: RequestId::new(id).unwrap(),
            expected_revision: Cursor::new(0),
            command,
        })
        .await
        .unwrap()
}

async fn next_owner(connection: &mut Connection) -> OwnerEvent {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(event) = connection.poll_owner().unwrap() {
                return event;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual owner event deadline")
}

impl BriefCase {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-key721-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "engine.sock",
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client = Client::new(root.path().join("engine.sock"), Duration::from_secs(2)).unwrap();
        let created = engine_request(
            &client,
            "create-key-project",
            Command::CreateProject {
                project_id: ProjectId::new("key-project").unwrap(),
                spec: ProjectSpec {
                    name: "Key routing project".into(),
                    repositories: Vec::new(),
                },
            },
        )
        .await;
        assert_eq!(created.outcome, Outcome::Applied);
        let snapshot = engine_request(&client, "key-project-snapshot", Command::Snapshot).await;
        let mut app = App::new();
        type_text(&mut app, UNASSIGNED);
        app.apply_reply(snapshot).unwrap();
        let mut connection = Connection::connect_paired(client).unwrap();
        let access = next_owner(&mut connection).await;
        assert!(matches!(&access, OwnerEvent::Access { ready: true, .. }));
        app.apply_owner(access).unwrap();
        type_text(&mut app, MESSAGE);
        app.reduce(UiEvent::ToggleBrief).unwrap();
        for _ in 0..2 {
            let job = app.conversation_request().unwrap().unwrap();
            connection.conversation(job).unwrap();
            app.apply_owner(next_owner(&mut connection).await).unwrap();
        }
        assert!(app.owner_ready && app.is_editing_brief());
        assert_eq!(
            app.selected_project().unwrap().project_id.as_str(),
            "key-project"
        );
        type_text(&mut app, BRIEF);
        Self {
            app,
            connection,
            service,
            _stop: stop,
            _root: root,
        }
    }
}

async fn with_brief(check: impl FnOnce(&mut App)) {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut case = BriefCase::new().await;
        let outcome = catch_unwind(AssertUnwindSafe(|| check(&mut case.app)));
        drop(case.connection);
        case.service.shutdown().await.unwrap();
        if let Err(payload) = outcome {
            resume_unwind(payload);
        }
    })
    .await
    .expect("real brief key case deadline");
}

fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        app.reduce(UiEvent::Character(character)).unwrap();
    }
}

fn focus(app: &mut App, target: Focus) {
    for _ in 0..7 {
        if app.focus() == target {
            return;
        }
        app.reduce(UiEvent::FocusNext).unwrap();
    }
    assert_eq!(app.focus(), target);
}

fn assert_one(app: &App, key: KeyEvent, expected: Option<UiEvent>) {
    let [actual, extra] = key_events(app, key);
    assert!(extra.is_none(), "unexpected second event for {key:?}");
    match (actual, expected) {
        (None, None) => {}
        (Some(UiEvent::Character(actual)), Some(UiEvent::Character(expected))) => {
            assert_eq!(actual, expected);
        }
        (Some(UiEvent::Scroll(actual)), Some(UiEvent::Scroll(expected))) => {
            assert_eq!(actual, expected);
        }
        (Some(actual), Some(expected)) => {
            assert_eq!(
                std::mem::discriminant(&actual),
                std::mem::discriminant(&expected),
                "ordinary key variant changed for {key:?}"
            );
        }
        _ => panic!("ordinary key presence changed for {key:?}"),
    }
}

fn reduce_key(app: &mut App, key: KeyEvent) {
    for event in key_events(app, key).into_iter().flatten() {
        app.reduce(event).unwrap();
    }
}

#[tokio::test]
async fn alt_printable_brief_keys_emit_cancel_then_normalized_mapping() {
    with_brief(|app| {
        for character in ['m', 'M', 'é', '界', '🦀', ' ', ']'] {
            for modifiers in [KeyModifiers::ALT, KeyModifiers::ALT | KeyModifiers::SHIFT] {
                for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
                    let key = KeyEvent::new_with_kind(KeyCode::Char(character), modifiers, kind);
                    assert!(matches!(
                        key_events(app, key),
                        [Some(UiEvent::Cancel), Some(UiEvent::Character(actual))]
                            if actual == character
                    ));
                    assert!(app.is_editing_brief());
                    assert_eq!(app.draft(), BRIEF);
                }
            }
        }
        assert_eq!(app.focus(), Focus::Conversation);
        assert!(!app.is_creating_project() && !app.declaration.open && !app.master.is_open());
    })
    .await;
}

#[tokio::test]
async fn ordered_escape_sequence_preserves_all_three_drafts() {
    with_brief(|app| {
        let [cancel, character] =
            key_events(app, KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT));
        assert!(matches!(&cancel, Some(UiEvent::Cancel)));
        assert!(matches!(&character, Some(UiEvent::Character('m'))));
        app.reduce(cancel.unwrap()).unwrap();
        assert!(!app.is_editing_brief());
        assert_eq!(app.draft(), MESSAGE);
        app.reduce(character.unwrap()).unwrap();
        assert_eq!(app.draft(), format!("{MESSAGE}m"));
        reduce_key(
            app,
            KeyEvent::new_with_kind(KeyCode::Char('m'), KeyModifiers::ALT, KeyEventKind::Repeat),
        );
        assert_eq!(app.draft(), format!("{MESSAGE}m"));
        for character in "aster setup retained 721 newer".chars() {
            reduce_key(
                app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        let newer = format!("{MESSAGE}master setup retained 721 newer");
        assert_eq!(app.draft(), newer);
        app.reduce(UiEvent::ToggleUnassigned).unwrap();
        assert_eq!(app.draft(), UNASSIGNED);
        app.reduce(UiEvent::ToggleUnassigned).unwrap();
        assert_eq!(app.draft(), newer);
        app.reduce(UiEvent::ToggleBrief).unwrap();
        assert!(app.is_editing_brief());
        assert_eq!(app.draft(), BRIEF);
        app.reduce(UiEvent::Cancel).unwrap();
        assert_eq!(app.draft(), newer);
        assert!(!app.has_command() && !app.has_unresolved_conversation());
    })
    .await;
}

#[tokio::test]
async fn alt_keys_outside_active_conversation_brief_keep_original_mapping() {
    assert_one(
        &App::new(),
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
        None,
    );
    with_brief(|app| {
        for target in [
            Focus::Projects,
            Focus::Settings,
            Focus::Plan,
            Focus::Supervision,
            Focus::Memory,
            Focus::Evidence,
        ] {
            focus(app, target);
            assert!(app.is_editing_brief());
            for modifiers in [KeyModifiers::ALT, KeyModifiers::ALT | KeyModifiers::SHIFT] {
                assert_one(app, KeyEvent::new(KeyCode::Char('m'), modifiers), None);
            }
        }
        focus(app, Focus::Conversation);
        app.reduce(UiEvent::Cancel).unwrap();
        assert!(!app.is_editing_brief());
        assert_one(
            app,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
            None,
        );
        app.reduce(UiEvent::ToggleUnassigned).unwrap();
        assert_eq!(app.draft(), UNASSIGNED);
        assert_one(
            app,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
            None,
        );
        app.reduce(UiEvent::ToggleUnassigned).unwrap();
        app.reduce(UiEvent::ToggleBrief).unwrap();
        app.reduce(UiEvent::BeginDeclaration).unwrap();
        assert!(app.declaration.open && app.is_editing_brief());
        assert_one(
            app,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
            None,
        );
        app.reduce(UiEvent::Cancel).unwrap();
        app.reduce(UiEvent::ConfigureMaster).unwrap();
        assert!(app.master.is_open() && app.is_editing_brief());
        assert_one(
            app,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
            None,
        );
        app.reduce(UiEvent::Cancel).unwrap();
        focus(app, Focus::Projects);
        app.reduce(UiEvent::BeginProject).unwrap();
        focus(app, Focus::Conversation);
        assert!(app.is_creating_project());
        assert_one(
            app,
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT),
            None,
        );
    })
    .await;
}

#[tokio::test]
async fn extra_modifiers_noncharacters_and_control_characters_keep_original_mapping() {
    with_brief(|app| {
        for extra in [
            KeyModifiers::CONTROL,
            KeyModifiers::SUPER,
            KeyModifiers::HYPER,
            KeyModifiers::META,
        ] {
            for shift in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                let modifiers = KeyModifiers::ALT | extra | shift;
                assert_one(app, KeyEvent::new(KeyCode::Char('m'), modifiers), None);
            }
        }
        for code in [
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Delete,
            KeyCode::F(2),
            KeyCode::F(3),
            KeyCode::F(4),
        ] {
            assert_one(app, KeyEvent::new(code, KeyModifiers::ALT), None);
        }
        for character in ['\0', '\n', '\r', '\t', '\u{1b}', '\u{7f}'] {
            for modifiers in [KeyModifiers::ALT, KeyModifiers::ALT | KeyModifiers::SHIFT] {
                assert_one(
                    app,
                    KeyEvent::new(KeyCode::Char(character), modifiers),
                    None,
                );
            }
        }
        assert!(app.is_editing_brief());
        assert_eq!(app.draft(), BRIEF);
    })
    .await;
}

#[tokio::test]
async fn press_repeat_release_and_plain_keys_preserve_legacy_routes() {
    with_brief(|app| {
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::ALT,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
        ] {
            for code in [
                KeyCode::Char('m'),
                KeyCode::Char('b'),
                KeyCode::Char('q'),
                KeyCode::Char('r'),
                KeyCode::Esc,
                KeyCode::Enter,
                KeyCode::Tab,
                KeyCode::Backspace,
                KeyCode::F(2),
            ] {
                let key = KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Release);
                assert_one(app, key, None);
                reduce_key(app, key);
            }
        }
        for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
            assert_one(
                app,
                KeyEvent::new_with_kind(KeyCode::Char('é'), KeyModifiers::NONE, kind),
                Some(UiEvent::Character('é')),
            );
            assert_one(
                app,
                KeyEvent::new_with_kind(KeyCode::Esc, KeyModifiers::NONE, kind),
                Some(UiEvent::Cancel),
            );
            assert_one(
                app,
                KeyEvent::new_with_kind(KeyCode::Char('b'), KeyModifiers::CONTROL, kind),
                Some(UiEvent::ToggleBrief),
            );
        }
        assert!(app.is_editing_brief());
        assert_eq!(app.draft(), BRIEF);
    })
    .await;
}

fn assert_conversation_routes(app: &App) {
    for (code, expected) in [
        (KeyCode::Char('x'), UiEvent::Character('x')),
        (KeyCode::Char(']'), UiEvent::Character(']')),
        (KeyCode::Char('N'), UiEvent::Character('N')),
        (KeyCode::Tab, UiEvent::FocusNext),
        (KeyCode::Up, UiEvent::SelectionPrevious),
        (KeyCode::Down, UiEvent::SelectionNext),
        (KeyCode::PageUp, UiEvent::Scroll(-5)),
        (KeyCode::PageDown, UiEvent::Scroll(5)),
        (KeyCode::Enter, UiEvent::Submit),
        (KeyCode::Esc, UiEvent::Cancel),
        (KeyCode::Backspace, UiEvent::Backspace),
        (KeyCode::F(2), UiEvent::ConfigureMaster),
        (KeyCode::F(3), UiEvent::CancelMaster),
        (KeyCode::F(4), UiEvent::CloseMaster),
    ] {
        assert_one(app, KeyEvent::new(code, KeyModifiers::NONE), Some(expected));
    }
    for (character, modifiers) in [
        ('M', KeyModifiers::SHIFT),
        ('x', KeyModifiers::HYPER),
        ('x', KeyModifiers::META),
    ] {
        assert_one(
            app,
            KeyEvent::new(KeyCode::Char(character), modifiers),
            Some(UiEvent::Character(character)),
        );
    }
    assert_one(
        app,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        Some(UiEvent::FocusPrevious),
    );
    for code in [KeyCode::F(1), KeyCode::Delete] {
        assert_one(app, KeyEvent::new(code, KeyModifiers::NONE), None);
    }
}

fn assert_control_alt_rejected(app: &App, characters: &[char]) {
    for modifiers in [
        KeyModifiers::CONTROL | KeyModifiers::ALT,
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ] {
        for &character in characters {
            assert_one(
                app,
                KeyEvent::new(KeyCode::Char(character), modifiers),
                None,
            );
        }
    }
}

#[tokio::test]
async fn control_shortcuts_and_form_routes_keep_existing_mapping() {
    with_brief(|app| {
        assert_conversation_routes(app);
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            for (character, expected) in [
                ('b', UiEvent::ToggleBrief),
                ('u', UiEvent::ToggleUnassigned),
                ('n', UiEvent::HistoryNext),
                ('p', UiEvent::HistoryFirst),
            ] {
                assert_one(
                    app,
                    KeyEvent::new(KeyCode::Char(character), modifiers),
                    Some(expected),
                );
            }
        }
        assert_control_alt_rejected(app, &['b', 'u', 'n', 'p']);
        for character in ['q', 'c', 'r'] {
            assert_one(
                app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL),
                None,
            );
        }
        focus(app, Focus::Settings);
        assert_control_alt_rejected(app, &['n', 'p']);
        for (character, expected) in [('n', UiEvent::HistoryNext), ('p', UiEvent::HistoryFirst)] {
            assert_one(
                app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL),
                Some(expected),
            );
        }
        focus(app, Focus::Projects);
        for (character, expected) in [
            ('n', UiEvent::BeginProject),
            ('N', UiEvent::BeginDeclaration),
            (']', UiEvent::TaskNext),
        ] {
            assert_one(
                app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                Some(expected),
            );
        }
        app.reduce(UiEvent::BeginDeclaration).unwrap();
        assert_control_alt_rejected(app, &['n', 'd']);
        for (character, expected) in [
            ('n', UiEvent::DeclarationAdd),
            ('d', UiEvent::DeclarationRemove),
        ] {
            assert_one(
                app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL),
                Some(expected),
            );
        }
        assert!(app.declaration.open);
    })
    .await;
}
