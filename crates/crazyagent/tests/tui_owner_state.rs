use crazyagent::tui::app::App;
use crazyagent::tui::views;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn pending_owner_connection_is_visible_in_wide_and_compact_layouts() {
    for (width, height) in [(100, 24), (40, 10)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| views::draw(frame, &App::new()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("Owner: connecting"),
            "pending trusted authority must remain visible at {width}x{height}: {screen}"
        );
    }
}
