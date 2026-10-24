use super::new_project::*;
use super::new_project_input::cells;
use super::new_project_tests::{key, load_catalog, type_text, wizard};
use super::new_project_view::{Look, Row, Screen, spread, wizard_screen};
use crate::project_setup::ResearchKind;
use crate::tui::frame::header;
use crate::tui::glyphs::{ASCII, UNICODE};
use crate::tui::theme::{Appearance, Colors, Depth, Theme};
use crossterm::event::KeyCode;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

fn help_line(width: usize, items: &[(&'static str, &'static str)], look: &Look) -> Row {
    let separator = if width >= 100 { "   " } else { "  " };
    let right = Row::of("?", look.heading).push(" Help ", look.fg);
    let mut row = Row::of(" ", Style::new());
    let mut used = 1;
    for (index, (name, label)) in items.iter().enumerate() {
        let gap = if index == 0 { 0 } else { separator.len() };
        let piece = gap + cells(name) + 1 + cells(label);
        if used + piece + right.width() + 2 > width {
            break;
        }
        if index > 0 {
            row = row.push(separator, Style::new());
        }
        row = row
            .push(*name, look.heading)
            .push(format!(" {label}"), look.fg);
        used += piece;
    }
    spread(width, row, right)
}

/// Draws a screen inside the shared frame on a real ratatui test terminal.
/// Takes the screen, the terminal size, the message line and the look; follows the generator in DESIGN.md.
/// Returns the plain text of every terminal row with the cursor drawn as a full block.
pub(super) fn compose(
    screen: &Screen,
    width: u16,
    height: u16,
    message: &str,
    look: &Look,
) -> String {
    let columns = usize::from(width);
    let body = usize::from(height) - 4;
    assert!(
        screen.rows.len() <= body,
        "body has {} rows, room for {body}",
        screen.rows.len()
    );
    let theme = Theme::new(Depth::NoColor, Colors::Dark, Appearance::Dark);
    let glyphs = if look.ascii { &ASCII } else { &UNICODE };
    let status = screen.status.clone().map(Row::line).unwrap_or_default();
    let top = header(columns, &screen.crumbs, status, &theme, glyphs);
    let mut rows = vec![Row::of("\u{2500}".repeat(columns), look.muted)];
    rows.extend(screen.rows.iter().cloned().map(|row| row.pad(columns)));
    while rows.len() < 1 + body {
        rows.push(Row::default().pad(columns));
    }
    rows.push(if message.is_empty() {
        Row::default().pad(columns)
    } else {
        Row::of(format!(" {message}"), look.fg).pad(columns)
    });
    rows.push(help_line(columns, &screen.help, look));
    let lines: Vec<Line<'static>> = std::iter::once(top)
        .chain(rows.into_iter().map(Row::line))
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buffer.content[usize::from(y) * columns + usize::from(x)];
            if screen.cursor.is_some_and(|(cx, cy)| cx == x && cy + 2 == y) {
                line.push('\u{2588}');
                continue;
            }
            line.push_str(cell.symbol());
            skip = cells(cell.symbol()).saturating_sub(1);
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

/// Checks drawn frames against the reviewed frames in `tui/screens/frames`.
/// Takes the drawn frames by id; `CA_FRAMES_OUT` names a folder that receives copies for review.
/// Returns nothing; fails with every frame that differs or has no reviewed frame.
pub(super) fn check_frames(drawn: &[(String, String)]) {
    if let Ok(folder) = std::env::var("CA_FRAMES_OUT") {
        std::fs::create_dir_all(&folder).unwrap();
        for (id, text) in drawn {
            std::fs::write(
                std::path::Path::new(&folder).join(format!("{id}.txt")),
                text,
            )
            .unwrap();
        }
    }
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui/screens/frames");
    let mut wrong = Vec::new();
    for (id, text) in drawn {
        match std::fs::read_to_string(folder.join(format!("{id}.txt"))) {
            Ok(want) if &want == text => {}
            Ok(want) => wrong.push(format!("{id} differs\n--- want\n{want}--- got\n{text}")),
            Err(_) => wrong.push(format!("{id} has no reviewed frame\n{text}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn about() -> Wizard {
    let mut wizard = wizard();
    type_text(&mut wizard, "Tidepool");
    wizard.key(key(KeyCode::Tab));
    type_text(
        &mut wizard,
        "A live dashboard for marine researchers to watch sensor buoys and get alerts when readings look wrong.",
    );
    wizard
}

fn wizard_frames() -> Vec<(String, String)> {
    let look = Look::plain();
    let mut drawn = Vec::new();
    let mut draw = |id: &str, wizard: &Wizard, width: u16, height: u16| {
        let screen = wizard_screen(wizard, width, height, &look);
        drawn.push((id.to_owned(), compose(&screen, width, height, "", &look)));
    };
    let mut wizard = about();
    draw("wiz-about-120", &wizard, 120, 36);
    draw("wiz-about-80", &wizard, 80, 24);
    wizard.research = vec![
        ResearchKind::MarketResearch,
        ResearchKind::CompetitorResearch,
        ResearchKind::FrontEndDesign,
    ];
    for step in [Step::About, Step::Stack, Step::Documents] {
        wizard.done[step.index()] = true;
    }
    wizard.go(Step::Work);
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Down));
    draw("wiz-work-120", &wizard, 120, 36);
    draw("wiz-work-80", &wizard, 80, 24);
    wizard.done[Step::Work.index()] = true;
    wizard.go(Step::Pool);
    draw("wiz-pool-120", &wizard, 120, 36);
    draw("wiz-pool-80", &wizard, 80, 24);
    load_catalog(&mut wizard, &["c-codex"]);
    wizard.done[Step::Pool.index()] = true;
    wizard.go(Step::Team);
    draw("wiz-team-120", &wizard, 120, 36);
    draw("wiz-team-80", &wizard, 80, 24);
    wizard.done[Step::Team.index()] = true;
    wizard.go(Step::Review);
    draw("wiz-review-120", &wizard, 120, 36);
    draw("wiz-review-80", &wizard, 80, 24);
    drawn
}

#[test]
fn wizard_frames_match_the_design() {
    let drawn = wizard_frames();
    for (id, text) in &drawn {
        let lines: Vec<&str> = text.lines().collect();
        let height = if id.ends_with("-120") { 36 } else { 24 };
        assert_eq!(lines.len(), height, "{id}");
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "{id} has a dash"
        );
    }
    check_frames(&drawn);
}

#[test]
fn static_text_wraps_at_spaces_up_to_the_full_width() {
    use super::new_project_view::wrap_words;
    assert_eq!(
        wrap_words(
            "A live dashboard for marine researchers to watch sensor buoys and get alerts when readings look wrong.",
            76
        ),
        [
            "A live dashboard for marine researchers to watch sensor buoys and get alerts",
            "when readings look wrong."
        ]
    );
    assert_eq!(
        wrap_words("one\n\ntwo  three", 20),
        ["one", "", "two three"]
    );
    assert_eq!(wrap_words("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    assert_eq!(
        wrap_words("\u{6f22}\u{5b57}\u{6f22}", 4),
        ["\u{6f22}\u{5b57}", "\u{6f22}"]
    );
    let long = "word ".repeat(300);
    assert!(wrap_words(&long, 7).iter().all(|line| cells(line) <= 7));
}

fn documents_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let research = home.path().join("Research");
    std::fs::create_dir(&research).unwrap();
    for (name, bytes) in [
        ("buoy-spec-v3.pdf", 2_516_582),
        ("interviews-2026-09.md", 38_912),
        ("competitor-notes.txt", 6_144),
        ("buoy-video.pdf", 51 * 1024 * 1024),
    ] {
        std::fs::File::create(research.join(name))
            .unwrap()
            .set_len(bytes)
            .unwrap();
    }
    home
}

fn state_frames(home: &std::path::Path) -> Vec<(String, String)> {
    let look = Look::plain();
    let mut drawn = Vec::new();
    let mut draw = |id: &str, wizard: &Wizard| {
        let screen = wizard_screen(wizard, 80, 24, &look);
        drawn.push((id.to_owned(), compose(&screen, 80, 24, "", &look)));
    };
    let mut wizard = Wizard::new(
        pool_choices(&super::new_project_tests::config()),
        Some(home.to_path_buf()),
    );
    type_text(&mut wizard, "Tidepool");
    wizard.key(key(KeyCode::Tab));
    type_text(
        &mut wizard,
        "A live dashboard for marine researchers to watch sensor buoys and get alerts when readings look wrong.",
    );
    wizard.tech = ["TypeScript", "React", "Next.js", "Postgres", "TimescaleDB"]
        .map(String::from)
        .to_vec();
    wizard.platforms = vec![crate::project_setup::Platform::Website];
    wizard.research = vec![
        ResearchKind::MarketResearch,
        ResearchKind::CompetitorResearch,
        ResearchKind::FrontEndDesign,
    ];
    wizard.done[Step::About.index()] = true;
    wizard.done[Step::Stack.index()] = true;
    wizard.go(Step::Documents);
    type_text(&mut wizard, "~/Research/");
    draw("wiz-docs-empty-80", &wizard);
    wizard.key(super::new_project_tests::control('u'));
    for name in [
        "buoy-spec-v3.pdf",
        "interviews-2026-09.md",
        "competitor-notes.txt",
    ] {
        type_text(&mut wizard, &format!("~/Research/{name}"));
        wizard.key(key(KeyCode::Enter));
        assert_eq!(
            wizard.note.as_ref().filter(|note| note.error),
            None,
            "{name}"
        );
    }
    type_text(&mut wizard, "~/Research/buoy-video.pdf");
    wizard.key(key(KeyCode::Enter));
    assert!(wizard.note.as_ref().is_some_and(|note| note.error));
    draw("wiz-docs-error-80", &wizard);
    wizard.key(super::new_project_tests::control('u'));
    wizard.note = None;
    wizard.done[Step::Documents.index()] = true;
    wizard.done[Step::Work.index()] = true;
    let mut empty = Wizard::new(Vec::new(), None);
    for step in [Step::About, Step::Stack, Step::Documents, Step::Work] {
        empty.done[step.index()] = true;
    }
    empty.go(Step::Pool);
    draw("wiz-pool-none-80", &empty);
    wizard.done[Step::Pool.index()] = true;
    load_catalog(&mut wizard, &["c-codex"]);
    wizard.go(Step::Team);
    wizard.key(key(KeyCode::Enter));
    assert!(wizard.picker.is_some());
    draw("wiz-team-picker-80", &wizard);
    wizard.key(key(KeyCode::Esc));
    assert!(wizard.picker.is_none());
    wizard.key(key(KeyCode::Down));
    wizard.key(key(KeyCode::Right));
    assert!(wizard.note.as_ref().is_some_and(|note| note.error));
    draw("wiz-team-error-80", &wizard);
    wizard.key(key(KeyCode::Left));
    assert_eq!(wizard.check_team(), Ok(()));
    wizard.done[Step::Team.index()] = true;
    wizard.go(Step::Review);
    assert!(matches!(wizard.key(key(KeyCode::Enter)), Reply::Create(_)));
    draw("wiz-creating-80", &wizard);
    wizard.failed("project_folder_exists");
    draw("wiz-create-error-80", &wizard);
    drawn
}

#[test]
fn wizard_empty_error_and_loading_states_match_the_design() {
    let home = documents_home();
    let drawn = state_frames(home.path());
    for (id, text) in &drawn {
        assert_eq!(text.lines().count(), 24, "{id}");
        assert!(
            !text.contains(home.path().to_str().unwrap()),
            "{id} shows the home path"
        );
    }
    check_frames(&drawn);
}

#[test]
fn rows_never_carry_terminal_escapes_or_control_characters() {
    use super::new_project_view::printable;
    let hostile = "re\u{1b}[2Jport\u{1b}]8;;https://x\u{7}.md\r\nnext\u{202e}txt.exe\ttab";
    let row = Row::of(hostile, Style::new()).push(hostile, Style::new());
    let text = row.text();
    assert_eq!(
        printable(hostile.to_owned()),
        "report.md nexttxt.exe    tab"
    );
    assert_eq!(
        text,
        "report.md nexttxt.exe    tabreport.md nexttxt.exe    tab"
    );
    assert!(!text.chars().any(char::is_control));
    assert_eq!(printable("plain".into()), "plain");
}

#[test]
fn rows_pad_and_cut_to_any_width_including_zero() {
    let name = Row::of("Tidepool", Style::new());
    assert_eq!(name.clone().pad(0), Row::default());
    assert_eq!(name.clone().pad(1).text(), "\u{2026}");
    assert_eq!(name.clone().pad(5).text(), "Tide\u{2026}");
    assert_eq!(name.clone().pad(10).text(), "Tidepool  ");
    let wide = Row::of("\u{6f22}\u{5b57}\u{6f22}", Style::new());
    assert_eq!(wide.clone().pad(4).text(), "\u{6f22} \u{2026}");
    for width in 0..12 {
        assert_eq!(wide.clone().pad(width).width(), width);
        assert_eq!(name.clone().pad(width).width(), width);
    }
    let folder = Row::of(
        format!(
            "/private/tmp/ca-tui-page-x/projects/project-{}",
            "a".repeat(64)
        ),
        Style::new(),
    );
    let row = spread(76, name.clone(), folder.clone());
    assert_eq!(row.width(), 76);
    assert!(row.text().ends_with('\u{2026}'), "{}", row.text());
    assert_eq!(spread(0, name, folder), Row::default());
}
