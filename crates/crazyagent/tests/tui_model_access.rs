use crazyagent::tui::app::{App, Focus, UiEvent};
use crazyagent::tui::views;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn screen(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| views::draw(frame, app)).unwrap();
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

fn settings(app: &mut App) {
    app.reduce(UiEvent::FocusNext).unwrap();
    app.reduce(UiEvent::FocusNext).unwrap();
    assert_eq!(app.focus(), Focus::Settings);
}

#[test]
fn settings_account_action_preserves_conversation_without_owner() {
    let mut app = App::new();
    let conversation = "Keep this conversation draft";
    for character in conversation.chars() {
        app.reduce(UiEvent::Character(character)).unwrap();
    }
    settings(&mut app);

    app.reduce(UiEvent::Character('a')).unwrap();

    assert_eq!(
        app.draft(),
        conversation,
        "the Settings account action must not edit the conversation draft"
    );
    assert!(
        !app.has_command(),
        "an account action without owner authority must not queue an ordinary engine command"
    );
    let rendered = screen(&app, 100, 28);
    assert!(rendered.contains("Model access"), "{rendered}");
    assert!(rendered.contains("Read-only"), "{rendered}");
    assert!(rendered.contains("Metadata only"), "{rendered}");

    app.reduce(UiEvent::FocusPrevious).unwrap();
    app.reduce(UiEvent::FocusPrevious).unwrap();
    assert_eq!(app.focus(), Focus::Conversation);
    assert_eq!(app.draft(), conversation);
}

#[test]
fn model_access_controls_are_visible_without_a_selected_task() {
    let mut app = App::new();
    settings(&mut app);
    assert!(app.selected_task().is_none());
    assert!(app.config.is_none());

    for (width, height) in [(100, 28), (48, 18)] {
        let rendered = screen(&app, width, height);
        assert!(
            rendered.contains("Model access"),
            "{width}x{height}: {rendered}"
        );
        assert!(
            rendered.contains("Read-only"),
            "{width}x{height}: {rendered}"
        );
        assert!(
            rendered.contains("Metadata only"),
            "{width}x{height}: {rendered}"
        );
        assert!(
            rendered.contains("Accounts"),
            "{width}x{height}: {rendered}"
        );
        assert!(
            rendered.contains("Connections"),
            "{width}x{height}: {rendered}"
        );
        assert!(rendered.contains("Pools"), "{width}x{height}: {rendered}");
    }
    assert!(!app.has_command());
}
