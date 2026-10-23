use super::*;
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse};
use crate::plugins::ServerSpec;
use crate::project_setup::{Platform, ProjectSetup};
use crate::protocol::ProjectSpec;
use crate::tui::screens::new_project_frames::compose;
use crate::tui::screens::new_project_view::{Screen, tab_strip};
use crate::tui::screens::project::Tab;
use crossterm::event::KeyModifiers;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

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
        scope_label: scope.label().into(),
        scope,
        enabled,
        on_here,
        source: format!("/Users/mira/Skills/{name}"),
        folder: format!("/private/tmp/engine/skills/{name}"),
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
    let items: Vec<(String, String)> = Tab::ALL
        .iter()
        .map(|tab| {
            (
                format!("{} {}", look.glyph(tab.glyph()), tab.label()),
                String::new(),
            )
        })
        .collect();
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
        PluginChange::Switch { ref project, ref id, on: false }
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
        PluginChange::SetSkill { ref id, enabled: false } if id == "pl0xtidepool-style"
    ));
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert_eq!(view.notice, Some(Notice::plain("tidepool-style is off.")));
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Down));
    let change = sent(view.key(press(KeyCode::Char(' '))));
    assert!(matches!(
        change,
        PluginChange::SetServer { ref id, enabled: false } if id == "pl0xpostgres"
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
        view.next_job().is_none(),
        "a failed read waits for a change"
    );
    view.apply(&SettingsEvent::PluginChanged(Ok(None)));
    assert!(
        view.next_job().is_none(),
        "a change this view did not send is not its own"
    );

    let mut view = loaded();
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

#[tokio::test]
async fn a_real_engine_project_switches_an_every_project_skill_off() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-plug-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let ordinary = Client::new(root.path().join("e.sock"), Duration::from_secs(2)).unwrap();
    let (_, mut controller) = OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
        .await
        .unwrap();
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
    } = sent(view.key(press(KeyCode::Char(' '))))
    else {
        panic!("a project switch expected");
    };
    assert_eq!(switched, project());
    assert!(!on);
    controller.switch_plugin(switched, id, on).await.unwrap();
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
