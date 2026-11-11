use super::*;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::OwnerResult;
use crate::project_setup::{HarnessSlot, Platform, ProjectSetup, TeamPlan};
use crate::team::agent_messages::MessageSettings;
use crate::team::events::EventKind;
use crate::tui::screens::new_project_view::Look;
use crate::tui::screens::project_team::{Notice, TeamReply, TeamView};
use crate::tui::screens::project_time::clock;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::os::unix::fs::PermissionsExt;

const AT: i64 = 1_793_000_000_000;
const MINUTE: i64 = 60_000;

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn texts(view: &TeamView, width: usize) -> Vec<String> {
    let (rows, _, _) = view.draw(width, 30, &[], &Look::plain());
    rows.iter()
        .map(|row| row.0.iter().map(|span| span.content.as_ref()).collect())
        .collect()
}

fn find<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row has {needle:?}:\n{}", rows.join("\n")))
}

async fn read(native: &mut NativeOwner, view: &mut TeamView, now: i64) -> SetupQuery {
    let query = view.next_query(now).expect("a read is due");
    let event = native.project_setup(query.clone()).await;
    view.apply(&query, event.result);
    query
}

#[tokio::test]
async fn the_team_view_reads_a_sub_agents_messages_and_saves_their_settings_through_the_engine() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-msg-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let _service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, _access) = native.attach().await;
    assert!(native.ready);
    let project = ProjectId::new("tidepool").unwrap();
    {
        let controller = native.controller.as_mut().unwrap();
        let _home = crate::tui::test_home::projects_home(controller, 1).await;
        let created = controller
            .setup_project(
                project.clone(),
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
                    team: Some(TeamPlan {
                        master: "codex".into(),
                        master_tools: true,
                        sub_harnesses: vec![HarnessSlot {
                            harness: "claude-code".into(),
                            agents: 1,
                            tools: true,
                        }],
                        min_agents: 1,
                        max_agents: 2,
                    }),
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
    }

    let connection = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    crate::team::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO team_agents(project, role, harness, tools, status, created_ms, heartbeat_ms)
             VALUES (?1, 'sub', 'claude-code', 1, 'stopped', ?2, ?2)",
            rusqlite::params![project.as_str(), AT],
        )
        .unwrap();
    let agent = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO team_tasks(project, source, harness, tools, prompt, status, agent, result,
             created_ms, started_ms, finished_ms)
             VALUES (?1, 'master', 'claude-code', 1, 'Write the readings API', 'done', ?2,
             'Added GET /readings.', ?3, ?3, ?4)",
            rusqlite::params![project.as_str(), agent, AT, AT + 3 * MINUTE],
        )
        .unwrap();
    let task = connection.last_insert_rowid();
    assert!(
        crate::team::events::record(
            &connection,
            &project,
            agent,
            Some(task),
            EventKind::Question,
            "Which database\u{1b}[31m should I use?",
            AT + MINUTE,
        )
        .unwrap()
        .is_some()
    );

    let mut view = TeamView::new(project.clone());
    read(&mut native, &mut view, AT + 10 * MINUTE).await;
    let at = view
        .agents()
        .iter()
        .position(|shown| shown.id == agent)
        .expect("the stopped sub-agent is listed");
    for _ in 0..at {
        view.key(press(KeyCode::Down));
    }
    assert_eq!(view.key(press(KeyCode::Char('M'))), TeamReply::Handled);
    let query = view.next_query(AT + 10 * MINUTE).unwrap();
    assert_eq!(
        query,
        SetupQuery::TeamMessages {
            project: project.clone(),
            agent,
            before: None,
        }
    );
    let event = native.project_setup(query.clone()).await;
    let Ok(SetupView::TeamMessages(page)) = &event.result else {
        panic!("the engine answers a Messages read: {:?}", event.result);
    };
    assert_eq!((&page.project_id, page.agent), (&project, agent));
    assert_eq!(page.settings, MessageSettings::default());
    view.apply(&query, event.result);
    for width in [116, 76] {
        let rows = texts(&view, width);
        assert!(
            find(&rows, "Master > task: Write the readings API").contains(&clock(AT, 0)),
            "{}",
            rows.join("\n")
        );
        assert!(
            find(&rows, "Agent < question: Which database(31m should I use?")
                .contains(&clock(AT + MINUTE, 0))
        );
        for row in &rows {
            assert!(!row.chars().any(char::is_control), "{row:?}");
        }
    }

    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert_eq!(view.key(press(KeyCode::Char('o'))), TeamReply::Handled);
    for _ in 0..5 {
        view.key(press(KeyCode::Down));
    }
    let TeamReply::Change(action) = view.key(press(KeyCode::Right)) else {
        panic!("a Messages settings change expected: {:?}", view.notice);
    };
    let change = SetupQuery::TeamChange {
        action,
        generation: 7,
    };
    let event = native.project_setup(change.clone()).await;
    assert_eq!(event.result, Ok(SetupView::Changed(OwnerResult::Applied)));
    view.apply(&change, event.result);
    assert_eq!(view.notice, Some(Notice::plain("Entries per page: 100.")));
    let reread = read(&mut native, &mut view, AT + 11 * MINUTE).await;
    assert!(matches!(reread, SetupQuery::Team { .. }));
    assert_eq!(
        view.status.as_ref().unwrap().message_settings,
        MessageSettings {
            page_size: 100,
            ..MessageSettings::default()
        }
    );
    assert!(
        find(&texts(&view, 76), "Entries per page")
            .contains(&format!("{:<40}100", "Entries per page"))
    );

    assert_eq!(view.key(press(KeyCode::Esc)), TeamReply::Handled);
    assert_eq!(view.key(press(KeyCode::Char('M'))), TeamReply::Handled);
    let query = view.next_query(AT + 12 * MINUTE).unwrap();
    let event = native.project_setup(query).await;
    let Ok(SetupView::TeamMessages(page)) = event.result else {
        panic!("the engine answers a Messages read: {:?}", event.result);
    };
    assert_eq!(
        page.settings.page_size, 100,
        "the list reads under the saved setting"
    );
}
