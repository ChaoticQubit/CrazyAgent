use super::*;
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse};
use crate::plugins::ServerSpec;
use crate::project_setup::{Platform, ProjectSetup};
use crate::protocol::ProjectSpec;
use crate::tui::client::Connection;
use crate::tui::ctx::{Ctx, Link};
use crate::tui::glyphs::UNICODE;
use crate::tui::keys::Key;
use crate::tui::run::owner_arrival;
use crate::tui::screens::new_project_frames::compose;
use crate::tui::screens::new_project_view::{Screen, tab_strip};
use crate::tui::screens::project::Tab;
use crate::tui::screens::{Place, ProjectTab, open};
use crate::tui::seam::{Request, ShellRequest};
use crate::tui::shell::Shell;
use crate::tui::theme::{Appearance, Colors, Depth};
use crossterm::event::KeyModifiers;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Line;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const LONG_RUNS: &str = "/opt/homebrew/bin/uvx mcp-server-postgres --connection postgresql://localhost:5432/tidepool --read-only";

fn project() -> ProjectId {
    ProjectId::new("tidepool").unwrap()
}

fn here() -> Scope {
    Scope::Project {
        project_id: project(),
    }
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn skill(name: &str, scope: Scope, enabled: bool, on_here: Option<bool>) -> SkillView {
    SkillView {
        id: format!("pl0x{name}"),
        name: name.into(),
        description: format!("How to do {name} work."),
        line: format!("How to do {name} work."),
        scope_label: scope.label().into(),
        scope,
        enabled,
        on_here,
        source: format!("/Users/mira/Skills/{name}"),
        folder: format!("/private/tmp/engine/skills/{name}"),
        version: 2,
        refused: None,
    }
}

fn server(name: &str, scope: Scope, status: ServerStatus, on_here: Option<bool>) -> ServerView {
    let spec = ServerSpec::Stdio {
        command: "/opt/homebrew/bin/uvx".into(),
        args: vec![format!("mcp-{name}")],
    };
    ServerView {
        id: format!("pl0x{name}"),
        name: name.into(),
        scope_label: scope.label().into(),
        scope,
        runs: spec.runs(),
        spec,
        enabled: status != ServerStatus::Off,
        status,
        status_label: status.label().into(),
        problem: None,
        tools: Vec::new(),
        checks: 0,
        secrets: Vec::new(),
        on_here,
        version: 2,
    }
}

fn lists() -> (Vec<SkillView>, Vec<ServerView>) {
    let skills = vec![
        skill("frontend-design", Scope::Global, true, Some(true)),
        skill("linear-method", Scope::Global, true, Some(false)),
        skill("release-notes", Scope::Global, false, Some(true)),
        skill("tidepool-style", here(), true, None),
    ];
    let mut github = server("github", Scope::Global, ServerStatus::Approved, Some(true));
    github.tools = vec!["search_code".into(), "open_issue".into()];
    let mut postgres = server("postgres", here(), ServerStatus::WaitingForApproval, None);
    postgres.runs = LONG_RUNS.into();
    (skills, vec![github, postgres])
}

fn loaded() -> ProjectPlugins {
    let mut view = ProjectPlugins::new(project());
    let job = view.next_job().unwrap();
    assert!(
        matches!(job, SettingsJob::ProjectPlugins { ref project } if project.as_str() == "tidepool")
    );
    view.apply(&SettingsEvent::ProjectPlugins(project(), Ok(lists())));
    view
}

fn frame(view: &ProjectPlugins, width: u16, height: u16) -> Vec<String> {
    let look = Look::plain();
    let columns = usize::from(width);
    let items = crate::tui::screens::project_view::tab_items(0, columns, &look);
    let mut rows: Vec<Row> = tab_strip(columns, &items, Tab::Settings as usize, &look).into();
    let room = usize::from(height) - 4 - 2;
    let (lines, help) = view.rows(columns - 4, room, &look);
    for line in lines.into_iter().take(room) {
        rows.push(Row::of("  ", Style::new()).cat(line).pad(columns));
    }
    let screen = Screen {
        rows,
        cursor: None,
        crumbs: vec!["Home".into(), "Tidepool".into(), "Settings".into()],
        help,
        status: None,
    };
    let text = compose(&screen, width, height, "", &look);
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), usize::from(height));
    lines
}

fn find<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}:\n{}", rows.join("\n")))
}

fn sent(reply: PluginsReply) -> PluginChange {
    match reply {
        PluginsReply::Send(SettingsJob::Plugin(change)) => change,
        other => panic!("a plugin change expected, got {other:?}"),
    }
}

#[test]
fn both_lists_draw_what_is_on_here_at_120_and_80_columns() {
    let view = loaded();
    let wide = frame(&view, 120, 36);
    assert!(find(&wide, "Skills  4").contains("From Settings, plus this project"));
    let design = find(&wide, "> frontend-design");
    assert!(design.contains("On") && design.contains("Every project"));
    assert!(design.contains("How to do frontend-design work."));
    assert!(find(&wide, "linear-method").contains("Off here"));
    assert!(find(&wide, "release-notes").contains("Off in Settings"));
    assert!(find(&wide, "tidepool-style").contains("This project only"));
    assert!(wide.iter().any(|row| row.contains("MCP servers  2")));
    let github = find(&wide, "github");
    assert!(github.contains("On") && github.contains("uvx mcp-github"));
    assert!(find(&wide, "  postgres").contains("Needs approval"));
    assert!(
        wide.iter()
            .any(|row| row.contains("frontend-design") && row.contains("For every project"))
    );
    assert!(wide.last().unwrap().contains("Space On or off here"));
    assert!(
        wide.iter().all(|row| !row.contains("pl0x")),
        "ids never show"
    );

    let narrow = frame(&view, 80, 24);
    let design = find(&narrow, "> frontend-design");
    assert!(design.contains("Every project") && !design.contains("How to do"));
    assert!(find(&narrow, "release-notes").contains("Off in Settings"));
    assert!(find(&narrow, "  postgres").contains("This project only"));
    assert!(narrow.iter().all(|row| row.chars().count() <= 80));
}

#[test]
fn space_switches_an_every_project_item_for_this_project_only() {
    let mut view = loaded();
    let change = sent(view.key(press(KeyCode::Char(' '))));
    assert!(matches!(
        change,
        PluginChange::Switch { ref project, ref id, on: false, version: 2 }
            if project.as_str() == "tidepool" && id == "pl0xfrontend-design"
    ));
    assert!(view.busy());
    assert!(matches!(
        view.key(press(KeyCode::Down)),
        PluginsReply::Handled
    ));
    let refused = view.key(press(KeyCode::Char(' ')));
    assert!(
        matches!(refused, PluginsReply::Handled),
        "one change at a time"
    );
    assert!(view.notice.as_ref().unwrap().error);
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert!(!view.busy());
    assert_eq!(
        view.notice,
        Some(Notice::plain("frontend-design is off for this project."))
    );
    assert!(
        matches!(view.next_job(), Some(SettingsJob::ProjectPlugins { .. })),
        "a change reads the lists again"
    );

    let mut view = loaded();
    view.key(press(KeyCode::Down));
    let change = sent(view.key(press(KeyCode::Char(' '))));
    assert!(matches!(change, PluginChange::Switch { on: true, .. }));
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert_eq!(
        view.notice,
        Some(Notice::plain("linear-method is on for this project."))
    );
}

#[test]
fn an_item_off_in_settings_cannot_be_switched_on_here() {
    let mut view = loaded();
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    assert!(matches!(
        view.key(press(KeyCode::Char(' '))),
        PluginsReply::Handled
    ));
    assert!(!view.busy());
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "release-notes is off in Settings for every project. Turn it on there first."
        ))
    );
}

#[test]
fn project_only_items_turn_on_and_off_for_this_project() {
    let mut view = loaded();
    for _ in 0..3 {
        view.key(press(KeyCode::Down));
    }
    let change = sent(view.key(press(KeyCode::Char(' '))));
    assert!(matches!(
        change,
        PluginChange::SetSkill { ref id, enabled: false, version: 2 } if id == "pl0xtidepool-style"
    ));
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert_eq!(view.notice, Some(Notice::plain("tidepool-style is off.")));
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    let change = sent(view.key(press(KeyCode::Char(' '))));
    assert!(matches!(
        change,
        PluginChange::SetServer { ref id, enabled: false, version: 2 } if id == "pl0xpostgres"
    ));
}

#[test]
fn approval_shows_the_whole_command_first() {
    let mut view = loaded();
    for _ in 0..4 {
        view.key(press(KeyCode::Down));
    }
    assert!(
        matches!(view.key(press(KeyCode::Enter)), PluginsReply::Ignored),
        "Enter on an approved server is the host's"
    );
    view.key(press(KeyCode::Down));
    assert!(
        frame(&view, 120, 36)
            .last()
            .unwrap()
            .contains("Enter Approve")
    );
    assert!(matches!(
        view.key(press(KeyCode::Enter)),
        PluginsReply::Handled
    ));
    assert_eq!(view.confirm.as_deref(), Some("pl0xpostgres"));
    let narrow = frame(&view, 80, 24);
    assert!(find(&narrow, "Approve postgres?").contains("y Approve"));
    let shown: String = narrow
        .iter()
        .skip_while(|row| !row.contains("It runs:"))
        .skip(1)
        .take_while(|row| row.starts_with("    ") && !row.trim().is_empty())
        .map(|row| row.trim_start_matches(' ').trim_end_matches(' '))
        .collect();
    assert_eq!(shown, LONG_RUNS, "every character of the command shows");
    assert!(narrow.last().unwrap().contains("n Not now"));
    assert!(matches!(
        view.key(press(KeyCode::Char('x'))),
        PluginsReply::Handled
    ));
    assert!(matches!(
        view.key(press(KeyCode::Char('n'))),
        PluginsReply::Handled
    ));
    assert_eq!(view.confirm, None);

    view.key(press(KeyCode::Enter));
    frame(&view, 80, 24);
    let change = sent(view.key(press(KeyCode::Char('y'))));
    let PluginChange::Approve { id, spec } = change else {
        panic!("an approval expected");
    };
    assert_eq!(id, "pl0xpostgres");
    assert_eq!(spec, lists().1[1].spec);
    let mut started = lists().1[1].clone();
    started.status = ServerStatus::CantStart;
    started.problem = Some("The command exited at once.".into());
    view.apply(&SettingsEvent::PluginChanged(Ok(Some(started))));
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "postgres can't start. The command exited at once."
        ))
    );
}

#[test]
fn at_80x24_the_project_settings_tab_scrolls_a_long_command_and_y_waits_for_its_last_line() {
    use crate::tui::screens::project::ProjectScreen;
    use crate::tui::screens::project_view::project_screen;

    let long = format!("{LONG_RUNS}{}", " --table notes_archive_2026".repeat(14));
    let read = |runs: &str| {
        let (skills, mut servers) = lists();
        servers[1].runs = runs.to_owned();
        SettingsEvent::ProjectPlugins(project(), Ok((skills, servers)))
    };
    let mut screen = ProjectScreen::new(project(), "Tidepool".into(), None);
    screen.open(Tab::Settings);
    screen.plugins.next_job().unwrap();
    screen.plugins.apply(&read(&long));
    for _ in 0..5 {
        screen.plugins.key(press(KeyCode::Down));
    }
    assert!(matches!(
        screen.plugins.key(press(KeyCode::Enter)),
        PluginsReply::Handled
    ));
    assert!(
        matches!(
            screen.plugins.key(press(KeyCode::Char('y'))),
            PluginsReply::Handled
        ),
        "nothing has shown the command yet"
    );
    assert_eq!(screen.plugins.confirm.as_deref(), Some("pl0xpostgres"));

    let look = Look::plain();
    let expected = exact_lines(&long, 74);
    assert!(
        expected.len() > 4,
        "the command needs more rows than 80x24 leaves"
    );
    let mut read_lines = vec![false; expected.len()];
    let mut reached_end = false;
    for _ in 0..expected.len() + 2 {
        let drawn = project_screen(&mut screen, 80, 24, &look);
        let rows: Vec<String> = drawn.rows.iter().map(Row::text).collect();
        assert!(rows.len() <= 24 && rows.iter().all(|row| row.chars().count() <= 80));
        assert!(rows.iter().any(|row| row.contains("Project settings")));
        assert!(
            rows.iter()
                .any(|row| row.contains("Instructions for this project's agents"))
        );
        assert!(drawn.help.contains(&("y", "Approve")));
        let question = rows
            .iter()
            .position(|row| row.contains("Approve postgres?"))
            .unwrap_or_else(|| panic!("the question shows:\n{}", rows.join("\n")));
        assert!(rows[question].contains("y Approve"));
        assert!(rows[question + 1].contains("It runs:"));
        let marker = rows
            .iter()
            .position(|row| row.trim_start().starts_with("Lines "))
            .unwrap_or_else(|| panic!("a line marker shows:\n{}", rows.join("\n")));
        let numbers: Vec<usize> = rows[marker]
            .split(|character: char| !character.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse().unwrap())
            .collect();
        let [first, last, total] = numbers[..] else {
            panic!("the marker names its lines: {}", rows[marker]);
        };
        assert_eq!(total, expected.len());
        assert_eq!(marker - (question + 2), last + 1 - first);
        for (offset, row) in rows[question + 2..marker].iter().enumerate() {
            let line = &expected[first - 1 + offset];
            assert_eq!(&row[..4 + line.len()], format!("    {line}"));
            assert!(row[4 + line.len()..].trim().is_empty());
            read_lines[first - 1 + offset] = true;
        }
        if last == total {
            assert!(rows[marker].contains("the end"));
            reached_end = true;
            break;
        }
        assert!(rows[marker].contains("y waits for the end"));
        assert!(
            matches!(
                screen.plugins.key(press(KeyCode::Char('y'))),
                PluginsReply::Handled
            ),
            "y waits until the last line has shown"
        );
        assert_eq!(screen.plugins.confirm.as_deref(), Some("pl0xpostgres"));
        assert!(!screen.plugins.busy());
        screen.plugins.key(press(KeyCode::Down));
    }
    assert!(reached_end, "Down reaches the last line");
    assert!(read_lines.iter().all(|line| *line), "every line showed");

    let changed = format!("{long} --write");
    screen.plugins.apply(&read(&changed));
    assert_eq!(screen.plugins.confirm.as_deref(), Some("pl0xpostgres"));
    assert!(
        matches!(
            screen.plugins.key(press(KeyCode::Char('y'))),
            PluginsReply::Handled
        ),
        "a changed command is read again"
    );
    let rows: Vec<String> = project_screen(&mut screen, 80, 24, &look)
        .rows
        .iter()
        .map(Row::text)
        .collect();
    assert!(
        rows.iter().any(|row| row.contains("Lines 1-")),
        "a changed command starts at its first line:\n{}",
        rows.join("\n")
    );
    assert!(matches!(
        screen.plugins.key(press(KeyCode::Char('y'))),
        PluginsReply::Handled
    ));
    for _ in 0..expected.len() + 2 {
        screen.plugins.key(press(KeyCode::Down));
        project_screen(&mut screen, 80, 24, &look);
    }
    screen.plugins.key(press(KeyCode::Up));
    project_screen(&mut screen, 80, 24, &look);
    let PluginChange::Approve { id, .. } = sent(screen.plugins.key(press(KeyCode::Char('y'))))
    else {
        panic!("an approval expected");
    };
    assert_eq!(
        id, "pl0xpostgres",
        "reading back up keeps the approval open"
    );
}

#[test]
fn a_window_too_short_for_any_command_line_never_approves() {
    let look = Look::plain();
    let mut view = loaded();
    for _ in 0..5 {
        view.key(press(KeyCode::Down));
    }
    view.key(press(KeyCode::Enter));
    for room in 0..4 {
        let (rows, _) = view.rows(76, room, &look);
        assert!(rows.len() <= room, "{} rows in {room}", rows.len());
        assert!(matches!(
            view.key(press(KeyCode::Char('y'))),
            PluginsReply::Handled
        ));
    }
    let (rows, _) = view.rows(76, 4, &look);
    assert!(
        rows.iter()
            .any(|row| row.text().contains("Make the window taller")),
        "{:?}",
        rows.iter().map(Row::text).collect::<Vec<_>>()
    );
    assert!(matches!(
        view.key(press(KeyCode::Char('y'))),
        PluginsReply::Handled
    ));
    assert_eq!(view.confirm.as_deref(), Some("pl0xpostgres"));
    view.rows(76, 5, &look);
    assert!(matches!(
        sent(view.key(press(KeyCode::Char('y')))),
        PluginChange::Approve { .. }
    ));
}

#[test]
fn reads_are_paced_refusals_are_plain_and_other_projects_are_ignored() {
    let mut view = ProjectPlugins::new(project());
    assert!(
        frame(&view, 80, 24)
            .iter()
            .any(|row| row.contains("Loading..."))
    );
    assert!(view.next_job().is_some());
    assert!(view.next_job().is_none(), "one read at a time");
    assert!(view.busy());
    view.forget_reads();
    assert!(view.next_job().is_some(), "a dropped read is asked again");
    view.apply(&SettingsEvent::ProjectPlugins(
        ProjectId::new("other").unwrap(),
        Ok((Vec::new(), Vec::new())),
    ));
    assert!(!view.loaded);
    view.apply(&SettingsEvent::ProjectPlugins(
        project(),
        Err("plugin_store_unavailable".into()),
    ));
    assert_eq!(
        view.notice,
        Some(Notice::problem(
            "Could not read this project's plugins. The engine cannot save plugins right now."
        ))
    );
    assert!(
        matches!(view.next_job(), Some(SettingsJob::Hooks { project: Some(ref asked) }) if *asked == project()),
        "the hooks read follows each plugins read once"
    );
    view.apply(&SettingsEvent::Hooks(Some(project()), Ok(Vec::new())));
    assert!(
        view.next_job().is_none(),
        "a failed read waits for a change"
    );
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert!(
        view.next_job().is_none(),
        "a change this view did not send is not its own"
    );

    let mut view = loaded();
    assert!(matches!(view.next_job(), Some(SettingsJob::Hooks { .. })));
    view.apply(&SettingsEvent::Hooks(Some(project()), Ok(Vec::new())));
    assert!(view.next_job().is_none());
    sent(view.key(press(KeyCode::Char(' '))));
    view.apply(&SettingsEvent::PluginChanged(Err(
        "plugin_switch_global_only".into(),
    )));
    assert!(view.notice.as_ref().unwrap().error);
    assert_eq!(
        view.notice.as_ref().unwrap().text,
        plugin_plain("plugin_switch_global_only")
    );

    let mut empty = ProjectPlugins::new(project());
    empty.next_job();
    empty.apply(&SettingsEvent::ProjectPlugins(
        project(),
        Ok((Vec::new(), Vec::new())),
    ));
    let rows = frame(&empty, 80, 24);
    assert!(rows.iter().any(|row| row.contains("No skills yet.")));
    assert!(rows.iter().any(|row| row.contains("No MCP servers yet.")));
    assert!(matches!(
        empty.key(press(KeyCode::Char(' '))),
        PluginsReply::Handled
    ));
}

#[test]
fn enter_on_a_skill_asks_the_host_for_its_page_and_skill_names_share_the_accent() {
    let mut view = loaded();
    assert!(
        frame(&view, 120, 36)
            .last()
            .unwrap()
            .contains("Enter Details")
    );
    match view.key(press(KeyCode::Enter)) {
        PluginsReply::OpenSkill { id, name } => {
            assert_eq!(id, "pl0xfrontend-design");
            assert_eq!(name, "frontend-design");
        }
        other => panic!("the skill page expected, got {other:?}"),
    }
    for _ in 0..4 {
        view.key(press(KeyCode::Down));
    }
    assert!(
        !frame(&view, 120, 36)
            .last()
            .unwrap()
            .contains("Enter Details")
    );
    assert!(
        matches!(view.key(press(KeyCode::Enter)), PluginsReply::Ignored),
        "Enter on an approved server stays with the host"
    );

    let mut look = Look::plain();
    look.accent = Style::new().fg(ratatui::style::Color::Cyan);
    let (rows, _) = loaded().rows(116, 30, &look);
    let lines: Vec<Line<'static>> = rows.into_iter().map(Row::line).collect();
    let fg = |name: &str| {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.trim() == name)
            .unwrap_or_else(|| panic!("{name} is drawn"))
            .style
            .fg
    };
    assert_eq!(fg("linear-method"), look.accent.fg);
    assert_eq!(fg("tidepool-style"), look.accent.fg);
    assert_ne!(fg("github"), look.accent.fg, "server names stay plain");
}

#[test]
fn a_and_m_ask_the_host_for_the_project_add_forms() {
    let mut view = loaded();
    assert!(matches!(
        view.key(press(KeyCode::Char('a'))),
        PluginsReply::AddSkill
    ));
    assert!(matches!(
        view.key(press(KeyCode::Char('m'))),
        PluginsReply::AddServer
    ));
    let help = frame(&view, 120, 36);
    assert!(help.last().unwrap().contains("a Add skill"));
    assert!(help.last().unwrap().contains("m Add MCP server"));
    let mut confirming = loaded();
    for _ in 0..5 {
        confirming.key(press(KeyCode::Down));
    }
    confirming.key(press(KeyCode::Enter));
    assert!(
        matches!(
            confirming.key(press(KeyCode::Char('a'))),
            PluginsReply::Handled
        ),
        "the approval question keeps every key"
    );
}

#[test]
fn only_this_projects_own_items_are_removed_here_after_asking() {
    let mut view = loaded();
    view.key(press(KeyCode::Char('d')));
    assert_eq!(view.removing, None);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "frontend-design is for every project. Switch it off here, or remove it in Settings."
        ))
    );
    for _ in 0..3 {
        view.key(press(KeyCode::Down));
    }
    assert!(frame(&view, 120, 36).last().unwrap().contains("d Remove"));
    view.key(press(KeyCode::Char('d')));
    assert_eq!(view.removing.as_deref(), Some("pl0xtidepool-style"));
    let narrow = frame(&view, 80, 24);
    assert!(find(&narrow, "Remove tidepool-style?").contains("y Remove"));
    assert!(narrow.iter().any(|row| row.contains("Your folder stays.")));
    assert!(narrow.last().unwrap().contains("n Keep"));
    assert!(matches!(
        view.key(press(KeyCode::Char(' '))),
        PluginsReply::Handled
    ));
    assert!(!view.busy(), "the question keeps every key");
    view.key(press(KeyCode::Char('n')));
    assert_eq!(view.removing, None);
    view.key(press(KeyCode::Char('d')));
    let change = sent(view.key(press(KeyCode::Char('y'))));
    assert!(matches!(
        change,
        PluginChange::RemoveSkill { ref id, version: 2 } if id == "pl0xtidepool-style"
    ));
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert_eq!(view.notice, Some(Notice::plain("Removed tidepool-style.")));

    let mut view = loaded();
    for _ in 0..5 {
        view.key(press(KeyCode::Down));
    }
    view.key(press(KeyCode::Char('d')));
    assert!(
        frame(&view, 80, 24)
            .iter()
            .any(|row| row.contains("Its saved secrets are deleted."))
    );
    let change = sent(view.key(press(KeyCode::Char('y'))));
    assert!(matches!(
        change,
        PluginChange::RemoveServer { ref id, version: 2 } if id == "pl0xpostgres"
    ));
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert_eq!(
        view.notice,
        Some(Notice::plain("Removed postgres and its secrets."))
    );
}

#[test]
fn a_short_window_keeps_the_question_and_the_notice_before_the_details() {
    let look = Look::plain();
    let texts = |view: &ProjectPlugins, room: usize| -> Vec<String> {
        let (rows, _) = view.rows(76, room, &look);
        assert!(rows.len() <= room, "{} rows in {room}", rows.len());
        rows.iter()
            .map(|row| row.text().trim_end().to_owned())
            .collect()
    };
    let mut view = loaded();
    for _ in 0..3 {
        view.key(press(KeyCode::Down));
    }
    view.key(press(KeyCode::Char(' ')));
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    let shown = texts(&view, 8);
    assert!(
        shown.iter().any(|row| row == "tidepool-style is off."),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|row| row.starts_with("tidepool-style")),
        "the details keep their name line: {shown:?}"
    );

    view.key(press(KeyCode::Char('d')));
    let shown = texts(&view, 8);
    assert!(
        shown
            .iter()
            .any(|row| row.starts_with("Remove tidepool-style?")),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|row| row.contains("Your folder stays.")),
        "{shown:?}"
    );
    let roomy = texts(&view, 18);
    assert!(
        roomy
            .iter()
            .any(|row| row == "How to do tidepool-style work."),
        "a window with room keeps the whole panel: {roomy:?}"
    );
}

#[test]
fn e_edits_only_this_projects_own_servers() {
    let mut view = loaded();
    assert!(
        matches!(view.key(press(KeyCode::Char('e'))), PluginsReply::Ignored),
        "a skill has no edit page"
    );
    for _ in 0..4 {
        view.key(press(KeyCode::Down));
    }
    assert!(matches!(
        view.key(press(KeyCode::Char('e'))),
        PluginsReply::Handled
    ));
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "github is for every project. Change it in Settings, Plugins and skills."
        ))
    );
    view.key(press(KeyCode::Down));
    match view.key(press(KeyCode::Char('e'))) {
        PluginsReply::Edit(server) => {
            assert_eq!(server.id, "pl0xpostgres");
            assert_eq!(server.runs, LONG_RUNS);
        }
        other => panic!("the edit page expected, got {other:?}"),
    }
}

#[test]
fn long_lists_scroll_with_the_cursor() {
    let mut view = ProjectPlugins::new(project());
    view.next_job();
    let skills = (0..30)
        .map(|index| {
            skill(
                &format!("skill-number-{index:02}"),
                Scope::Global,
                true,
                Some(true),
            )
        })
        .collect();
    view.apply(&SettingsEvent::ProjectPlugins(
        project(),
        Ok((skills, Vec::new())),
    ));
    for _ in 0..29 {
        view.key(press(KeyCode::Down));
    }
    let rows = frame(&view, 80, 24);
    assert!(find(&rows, "skill-number-29").contains("> skill-number-29"));
    assert!(rows.iter().all(|row| !row.contains("skill-number-00 ")));
    view.key(press(KeyCode::Down));
    assert_eq!(view.cursor, 29, "the cursor stops at the last item");
}

#[test]
#[should_panic(expected = "assertion failed: visible(scope, &self.project)")]
fn another_projects_item_in_this_read_is_a_bug() {
    let mut view = ProjectPlugins::new(project());
    let stray = skill(
        "elsewhere",
        Scope::Project {
            project_id: ProjectId::new("other").unwrap(),
        },
        true,
        None,
    );
    view.apply(&SettingsEvent::ProjectPlugins(
        project(),
        Ok((vec![stray], Vec::new())),
    ));
}

#[test]
#[should_panic(expected = "assertion `left == right` failed")]
fn an_every_project_item_without_its_switch_is_a_bug() {
    let mut view = ProjectPlugins::new(project());
    let unswitched = skill("frontend-design", Scope::Global, true, None);
    view.apply(&SettingsEvent::ProjectPlugins(
        project(),
        Ok((vec![unswitched], Vec::new())),
    ));
}

#[test]
fn exact_lines_keep_every_character() {
    assert_eq!(exact_lines("abcdef", 4), ["abcd", "ef"]);
    assert_eq!(exact_lines("a  b", 2), ["a ", " b"]);
    assert_eq!(exact_lines("", 5), [""]);
    assert_eq!(
        exact_lines("\u{6f22}\u{5b57}x", 3),
        ["\u{6f22}", "\u{5b57}x"]
    );
}

struct Engine {
    root: tempfile::TempDir,
    _home: tempfile::TempDir,
    _service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    controller: OwnerController,
}

async fn engine() -> Engine {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-plug-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    )
    .unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
    let home = crate::tui::test_home::projects_home(&mut controller, 1).await;
    let created = controller
        .setup_project(
            project(),
            ProjectSpec {
                name: "Tidepool".into(),
                repositories: vec![],
            },
            ProjectSetup {
                about: "Live dashboard for sensor buoys.".into(),
                tech_stack: vec![],
                platforms: vec![Platform::Website],
                research: vec![],
                pool: None,
                team: None,
                documents: vec![],
            },
            1,
        )
        .await
        .unwrap();
    assert!(matches!(
        created,
        OwnerResponse::Applied { .. } | OwnerResponse::Committed { .. }
    ));
    let folder = root.path().join("picked/release");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("SKILL.md"),
        "---\nname: release-notes\ndescription: Teaches release notes.\n---\n\nSteps.\n",
    )
    .unwrap();
    controller
        .add_skill(Scope::Global, folder.display().to_string())
        .await
        .unwrap();
    Engine {
        root,
        _home: home,
        _service: service,
        _stop: stop,
        controller,
    }
}

#[tokio::test]
async fn a_real_engine_project_switches_an_every_project_skill_off() {
    let engine = engine().await;
    let controller = &engine.controller;
    controller
        .add_mcp_server(
            here(),
            "echo".into(),
            ServerSpec::Stdio {
                command: "/bin/echo".into(),
                args: vec!["hello world".into()],
            },
            Vec::new(),
        )
        .await
        .unwrap();

    let mut view = ProjectPlugins::new(project());
    let read = |skills: Result<Vec<SkillView>, crate::engine::EngineError>,
                servers: Result<Vec<ServerView>, crate::engine::EngineError>| {
        SettingsEvent::ProjectPlugins(
            project(),
            skills
                .and_then(|skills| servers.map(|servers| (skills, servers)))
                .map_err(|error| error.0),
        )
    };
    assert!(view.next_job().is_some());
    view.apply(&read(
        controller.plugin_skills(Some(project())).await,
        controller.plugin_servers(Some(project())).await,
    ));
    let rows = frame(&view, 120, 36);
    let release = find(&rows, "> release-notes");
    assert!(release.contains("On") && release.contains("Every project"));
    let echo = find(&rows, "  echo");
    assert!(echo.contains("Needs approval") && echo.contains("This project only"));
    frame(&view, 80, 24);

    let PluginChange::Switch {
        project: switched,
        id,
        on,
        version,
    } = sent(view.key(press(KeyCode::Char(' '))))
    else {
        panic!("a project switch expected");
    };
    assert_eq!(switched, project());
    assert!(!on);
    controller
        .switch_plugin(switched, id, on, version)
        .await
        .unwrap();
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert_eq!(
        view.notice,
        Some(Notice::plain("release-notes is off for this project."))
    );
    assert!(view.next_job().is_some());
    view.apply(&read(
        controller.plugin_skills(Some(project())).await,
        controller.plugin_servers(Some(project())).await,
    ));
    assert_eq!(view.skills[0].on_here, Some(false));
    assert!(find(&frame(&view, 80, 24), "release-notes").contains("Off here"));
}

async fn drive(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut quiet = 0;
    loop {
        let mut moved = false;
        while let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(owner_arrival(event));
            moved = true;
        }
        if !connection.busy()
            && shell.ctx.link == Link::Ready
            && let Some(request) = shell.ctx.next_request()
        {
            moved = true;
            match request {
                Request::Pools => connection.shell(ShellRequest::Pools).unwrap(),
                Request::PoolProjects => connection.shell(ShellRequest::PoolProjects).unwrap(),
                Request::Projects => connection.shell(ShellRequest::Projects).unwrap(),
                Request::Process(query) => connection.process(*query).unwrap(),
                Request::Setup(query) => connection.project_setup(query).unwrap(),
                Request::Master(job) => connection.master(*job).unwrap(),
                Request::Recovery(query) => connection.recovery(*query).unwrap(),
                Request::Settings(job) => {
                    sent.push(match job.as_ref() {
                        SettingsJob::ProjectPlugins { .. } => "project plugins",
                        SettingsJob::Plugin(_) => "plugin change",
                        SettingsJob::SkillDetail { .. } => "skill detail",
                        SettingsJob::Hooks { .. } => "hooks",
                        _ => "settings",
                    });
                    connection.settings(*job).unwrap();
                }
                Request::Questions(query)
                    if *query == crate::tui::client::questions::QuestionsQuery::Counts =>
                {
                    connection.questions(*query).unwrap();
                }
                other => panic!("the project page never sends {other:?}"),
            }
        }
        quiet = if moved || connection.busy() || crate::tui::test_home::connecting(&shell.ctx.link)
        {
            0
        } else {
            quiet + 1
        };
        if quiet > 20 {
            return sent;
        }
        assert!(
            Instant::now() < deadline,
            "engine reads never settled: {sent:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn render(shell: &mut Shell, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| shell.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_owned())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_project_settings_tab_switches_and_adds_through_the_engine() {
    let engine = engine().await;
    let mut connection = Connection::connect_paired(
        Client::new(
            engine.root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap(),
    )
    .unwrap();
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = 80;
    ctx.height = 24;
    let screen = open(
        &Place::Project {
            id: project(),
            name: "Tidepool".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    let sent = drive(&mut shell, &mut connection).await;
    assert!(sent.contains(&"project plugins"), "{sent:?}");
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Project settings"), "{shown}");
    let release = shown
        .lines()
        .find(|line| line.contains("> release-notes"))
        .unwrap_or_else(|| panic!("no skill row:\n{shown}"));
    assert!(release.contains("Every project"), "{shown}");
    assert!(
        shown.contains("No MCP servers yet. Press m to add one for this project."),
        "{shown}"
    );

    shell.key(Key::Char(' '));
    let sent = drive(&mut shell, &mut connection).await;
    assert_eq!(sent, ["plugin change", "project plugins", "hooks"]);
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Off here"), "{shown}");
    assert!(
        shown.contains("release-notes is off for this project."),
        "{shown}"
    );
    let skills = engine
        .controller
        .plugin_skills(Some(project()))
        .await
        .unwrap();
    assert_eq!(skills[0].on_here, Some(false));

    shell.key(Key::Char('a'));
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Only this project can use it."), "{shown}");
    assert!(
        shown.contains("Tidepool  /  Settings  /  Add skill"),
        "{shown}"
    );
    shell.key(Key::Esc);
    drive(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Project settings"), "{shown}");
    assert!(shown.contains("Off here"), "{shown}");

    shell.key(Key::Enter);
    let sent = drive(&mut shell, &mut connection).await;
    assert_eq!(sent, ["skill detail"]);
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("Tidepool  /  Settings  /  release-notes"),
        "{shown}"
    );
    assert!(
        shown.contains("release-notes  Off for this project"),
        "{shown}"
    );
    shell.key(Key::Char(' '));
    let sent = drive(&mut shell, &mut connection).await;
    assert_eq!(sent, ["plugin change", "skill detail"]);
    let shown = render(&mut shell, 80, 24);
    assert!(
        shown.contains("release-notes  On for this project"),
        "{shown}"
    );
    let skills = engine
        .controller
        .plugin_skills(Some(project()))
        .await
        .unwrap();
    assert_eq!(skills[0].on_here, Some(true));
    shell.key(Key::Esc);
    drive(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Project settings"), "{shown}");
    assert!(!shown.contains("Off here"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_remove_question_warning_stays_visible_at_80x24() {
    let engine = engine().await;
    engine
        .controller
        .add_mcp_server(
            here(),
            "postgres".into(),
            ServerSpec::Stdio {
                command: "/bin/echo".into(),
                args: vec!["hello".into()],
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let mut connection = Connection::connect_paired(
        Client::new(
            engine.root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap(),
    )
    .unwrap();
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = 80;
    ctx.height = 24;
    let screen = open(
        &Place::Project {
            id: project(),
            name: "Tidepool".into(),
            tab: ProjectTab::Settings,
        },
        &mut ctx,
    );
    let mut shell = Shell::with(ctx, screen);
    drive(&mut shell, &mut connection).await;
    for _ in 0..6 {
        if render(&mut shell, 80, 24).contains("> postgres") {
            break;
        }
        shell.key(Key::Down);
    }
    assert!(
        render(&mut shell, 80, 24).contains("> postgres"),
        "the server is selected"
    );

    shell.key(Key::Char('d'));
    let shown = render(&mut shell, 80, 24);
    assert!(shown.contains("Remove postgres?"), "{shown}");
    assert!(
        shown.contains("Its saved secrets are deleted."),
        "the remove warning stays visible at 80x24: {shown}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_typed_into_the_read_only_folders_box_never_reach_the_plugins_list() {
    let engine = engine().await;
    engine
        .controller
        .add_mcp_server(
            here(),
            "postgres".into(),
            ServerSpec::Stdio {
                command: "/bin/echo".into(),
                args: vec!["hello".into()],
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let mut connection = Connection::connect_paired(
        Client::new(
            engine.root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap(),
    )
    .unwrap();
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = 100;
    ctx.height = 40;
    let settings = Place::Project {
        id: project(),
        name: "Tidepool".into(),
        tab: ProjectTab::Settings,
    };
    let screen = open(&settings, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    drive(&mut shell, &mut connection).await;
    for _ in 0..4 {
        if render(&mut shell, 100, 40).contains("> postgres") {
            break;
        }
        shell.key(Key::Down);
    }
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains("> postgres") && shown.contains("Needs approval"),
        "{shown}"
    );

    shell.key(Key::Char('f'));
    let typed = "/Users/x/st[u]dy?";
    for character in typed.chars() {
        shell.key(Key::Char(character));
    }
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains(typed),
        "every typed key reaches the box: {shown}"
    );
    shell.key(Key::Enter);
    for character in "my".chars() {
        shell.key(Key::Char(character));
    }
    let sent = drive(&mut shell, &mut connection).await;
    assert!(!sent.contains(&"plugin change"), "{sent:?}");
    assert_eq!(shell.place(), Some(settings));
    let servers = engine
        .controller
        .plugin_servers(Some(project()))
        .await
        .unwrap();
    let postgres = servers
        .iter()
        .find(|server| server.name == "postgres")
        .expect("typing in the box removed no server");
    assert_eq!(postgres.status, ServerStatus::WaitingForApproval);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_typed_into_the_hidden_folders_box_never_reach_the_plugins_list() {
    let engine = engine().await;
    engine
        .controller
        .add_mcp_server(
            here(),
            "postgres".into(),
            ServerSpec::Stdio {
                command: "/bin/echo".into(),
                args: vec!["hello".into()],
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let mut connection = Connection::connect_paired(
        Client::new(
            engine.root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap(),
    )
    .unwrap();
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = 100;
    ctx.height = 40;
    let settings = Place::Project {
        id: project(),
        name: "Tidepool".into(),
        tab: ProjectTab::Settings,
    };
    let screen = open(&settings, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    drive(&mut shell, &mut connection).await;

    shell.key(Key::Char('k'));
    let shown = render(&mut shell, 100, 40);
    assert!(shown.contains("Hidden folders"), "the box opens: {shown}");

    let typed = "/Users/x/vau[l]t?";
    for character in typed.chars() {
        shell.key(Key::Char(character));
    }
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains(typed),
        "every typed key reaches the hidden box: {shown}"
    );
    assert!(
        !shown.contains("This help") && shown.contains("Hidden folders"),
        "the banner keys fire nothing while typing: {shown}"
    );
    let sent = drive(&mut shell, &mut connection).await;
    assert!(!sent.contains(&"plugin change"), "{sent:?}");
    assert_eq!(shell.place(), Some(settings));
    let servers = engine
        .controller
        .plugin_servers(Some(project()))
        .await
        .unwrap();
    assert!(
        servers.iter().any(|server| server.name == "postgres"),
        "typing in the hidden box removed no server"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quitting_with_the_hidden_box_open_warns_the_hidden_folders_are_unsaved() {
    let engine = engine().await;
    let mut connection = Connection::connect_paired(
        Client::new(
            engine.root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap(),
    )
    .unwrap();
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = 100;
    ctx.height = 40;
    let settings = Place::Project {
        id: project(),
        name: "Tidepool".into(),
        tab: ProjectTab::Settings,
    };
    let screen = open(&settings, &mut ctx);
    let mut shell = Shell::with(ctx, screen);
    drive(&mut shell, &mut connection).await;

    shell.key(Key::Char('k'));
    assert!(
        render(&mut shell, 100, 40).contains("Hidden folders"),
        "the hidden box opens"
    );

    shell.key(Key::Ctrl('q'));
    let shown = render(&mut shell, 100, 40);
    assert!(
        shown.contains("Your hidden folders will not be saved."),
        "quit warns the open hidden box is a draft: {shown}"
    );
    assert_eq!(shell.place(), Some(settings));
}

#[test]
fn l_asks_for_the_tools_page_with_every_server_starting_on_the_selected_one() {
    let mut view = loaded();
    let every = vec![
        ("pl0xgithub".to_owned(), "github".to_owned()),
        ("pl0xpostgres".to_owned(), "postgres".to_owned()),
    ];
    match view.key(press(KeyCode::Char('l'))) {
        PluginsReply::Tools { servers, at } => assert_eq!((servers, at), (every.clone(), 0)),
        _ => panic!("l opens the tools page"),
    }
    for _ in 0..5 {
        view.key(press(KeyCode::Down));
    }
    match view.key(press(KeyCode::Char('l'))) {
        PluginsReply::Tools { servers, at } => assert_eq!((servers, at), (every, 2)),
        _ => panic!("l opens the tools page"),
    }
    assert!(view.help().contains(&("l", "Tools")), "{:?}", view.help());
}

async fn drive_settings(shell: &mut Shell, connection: &mut Connection) -> Vec<&'static str> {
    let mut sent = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut quiet = 0;
    loop {
        let mut moved = false;
        while let Some(event) = connection.poll_owner().unwrap() {
            shell.arrive(owner_arrival(event));
            moved = true;
        }
        if !connection.busy()
            && shell.ctx.link == Link::Ready
            && let Some(request) = shell.ctx.next_request()
        {
            moved = true;
            match request {
                Request::Profile => connection.shell(ShellRequest::Profile).unwrap(),
                Request::Pools => connection.shell(ShellRequest::Pools).unwrap(),
                Request::PoolProjects => connection.shell(ShellRequest::PoolProjects).unwrap(),
                Request::Projects => connection.shell(ShellRequest::Projects).unwrap(),
                Request::Process(query) => connection.process(*query).unwrap(),
                Request::Setup(query) => connection.project_setup(query).unwrap(),
                Request::Master(job) => connection.master(*job).unwrap(),
                Request::Settings(job) => {
                    sent.push(match job.as_ref() {
                        SettingsJob::Plugin(_) => "plugin change",
                        SettingsJob::Hooks { .. } => "hooks",
                        _ => "settings",
                    });
                    connection.settings(*job).unwrap();
                }
                Request::Questions(query)
                    if *query == crate::tui::client::questions::QuestionsQuery::Counts =>
                {
                    connection.questions(*query).unwrap();
                }
                other => panic!("the plugins pages never send {other:?}"),
            }
        }
        quiet = if moved || connection.busy() || crate::tui::test_home::connecting(&shell.ctx.link)
        {
            0
        } else {
            quiet + 1
        };
        if quiet > 20 {
            return sent;
        }
        assert!(
            Instant::now() < deadline,
            "engine reads never settled: {sent:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn shell_at(engine: &Engine, place: &Place) -> (Shell, Connection) {
    let connection = Connection::connect_paired(
        Client::new(
            engine.root.path().join("e.sock"),
            crate::tui::test_home::ENGINE_WAIT,
        )
        .unwrap(),
    )
    .unwrap();
    let mut ctx = Ctx::new(
        Depth::NoColor,
        Colors::Dark,
        Appearance::Dark,
        &UNICODE,
        "/private/tmp/engine.sock".into(),
    );
    ctx.width = 120;
    ctx.height = 36;
    let screen = open(place, &mut ctx);
    (Shell::with(ctx, screen), connection)
}

fn hook_row(shown: &str) -> &str {
    shown
        .lines()
        .find(|line| line.contains("PreToolUse Bash:") || line.contains("  guard  "))
        .unwrap_or_else(|| panic!("no hook row:\n{shown}"))
}

/// A real engine's hooks through the TUI: h in Settings, Plugins refuses a folder without
/// plugin.json with its plain reason, then adds the folder's hooks for every project once it
/// has one, Space turns the hook off and on again there, and a project's Settings tab
/// switches that every-project hook off for the project only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_are_added_with_h_switched_with_space_and_switched_per_project_through_the_engine() {
    let engine = engine().await;
    let plugin = engine.root.path().join("picked/guard");
    std::fs::create_dir_all(plugin.join("hooks")).unwrap();
    std::fs::write(plugin.join("hooks/guard.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        plugin.join("hooks/hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"sh \"${CLAUDE_PLUGIN_ROOT}/hooks/guard.sh\""}]}]}}"#,
    )
    .unwrap();
    let global = || engine.controller.plugin_hooks(None);

    let (mut shell, mut connection) = shell_at(
        &engine,
        &Place::Settings(crate::tui::screens::SettingsSection::Plugins),
    );
    drive_settings(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 36);
    assert!(
        shown.contains("No hooks yet. Press h to add a plugin folder's hooks."),
        "{shown}"
    );

    shell.key(Key::Char('h'));
    let shown = render(&mut shell, 120, 36);
    assert!(
        shown.contains("Every project's agents run them."),
        "{shown}"
    );
    for character in plugin.display().to_string().chars() {
        shell.key(Key::Char(character));
    }
    shell.key(Key::Enter);
    let sent = drive_settings(&mut shell, &mut connection).await;
    assert!(sent.contains(&"plugin change"), "{sent:?}");
    let shown = render(&mut shell, 120, 36);
    assert!(
        shown
            .contains("This folder has hooks but no .claude-plugin/plugin.json naming the plugin."),
        "{shown}"
    );
    assert!(
        global().await.unwrap().is_empty(),
        "a folder without plugin.json adds no hooks"
    );
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"guard"}"#,
    )
    .unwrap();
    shell.key(Key::Enter);
    let sent = drive_settings(&mut shell, &mut connection).await;
    assert!(sent.contains(&"plugin change"), "{sent:?}");
    let hooks = global().await.unwrap();
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    assert_eq!(
        (
            hooks[0].plugin.as_str(),
            hooks[0].event.as_str(),
            hooks[0].matcher.as_deref(),
            hooks[0].enabled,
            &hooks[0].scope,
        ),
        ("guard", "PreToolUse", Some("Bash"), true, &Scope::Global)
    );
    let shown = render(&mut shell, 120, 36);
    assert!(shown.contains("Hooks added for every project."), "{shown}");
    assert!(hook_row(&shown).contains("On"), "{shown}");

    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let sent = drive_settings(&mut shell, &mut connection).await;
    assert!(sent.contains(&"plugin change"), "{sent:?}");
    assert!(
        !global().await.unwrap()[0].enabled,
        "Space turned the hook off"
    );
    let shown = render(&mut shell, 120, 36);
    assert!(hook_row(&shown).contains("Off"), "{shown}");
    shell.key(Key::Char(' '));
    let sent = drive_settings(&mut shell, &mut connection).await;
    assert!(sent.contains(&"plugin change"), "{sent:?}");
    assert!(
        global().await.unwrap()[0].enabled,
        "Space turned the hook on again"
    );

    let (mut shell, mut connection) = shell_at(
        &engine,
        &Place::Project {
            id: project(),
            name: "Tidepool".into(),
            tab: ProjectTab::Settings,
        },
    );
    drive_settings(&mut shell, &mut connection).await;
    let shown = render(&mut shell, 120, 36);
    assert!(hook_row(&shown).contains("On"), "{shown}");
    shell.key(Key::Down);
    shell.key(Key::Char(' '));
    let sent = drive_settings(&mut shell, &mut connection).await;
    assert!(sent.contains(&"plugin change"), "{sent:?}");
    let here = engine
        .controller
        .plugin_hooks(Some(project()))
        .await
        .unwrap();
    assert_eq!(here[0].on_here, Some(false), "{here:?}");
    assert!(
        global().await.unwrap()[0].enabled,
        "every other project keeps it"
    );
    let shown = render(&mut shell, 120, 36);
    assert!(hook_row(&shown).contains("Off here"), "{shown}");
    assert!(shown.contains("is off for this project."), "{shown}");
}
