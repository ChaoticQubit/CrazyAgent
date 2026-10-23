use super::*;
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{OwnerClientKind, OwnerResponse};
use crate::project_setup::{Platform, ProjectSetup, ResearchKind};
use crate::protocol::ProjectSpec;
use crate::tui::screens::new_project_frames::compose;
use crate::tui::screens::new_project_view::{Screen, tab_strip};
use crate::tui::screens::project::Tab;
use crossterm::event::KeyModifiers;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const NOW: i64 = 1_793_000_000_000;
const MINUTE: i64 = 60_000;

fn project() -> ProjectId {
    ProjectId::new("tidepool").unwrap()
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn job(id: i64, kind: ResearchKind, status: ResearchStatus) -> ResearchJob {
    ResearchJob {
        id,
        project_id: project(),
        kind,
        kind_label: kind.label().into(),
        status,
        status_label: status.label().into(),
        report_path: kind.report_path(),
        harness: "codex".into(),
        tools: true,
        attempts: 1,
        claimer: None,
        error: None,
        cancel_requested: false,
        output_bytes: 0,
        created_ms: NOW - 25 * MINUTE,
        started_ms: None,
        heartbeat_ms: None,
        finished_ms: None,
    }
}

fn jobs() -> Vec<ResearchJob> {
    let mut market = job(1, ResearchKind::MarketResearch, ResearchStatus::Done);
    market.started_ms = Some(NOW - 24 * MINUTE);
    market.finished_ms = Some(NOW - 18 * MINUTE);
    market.output_bytes = 40 * 1024;
    let mut competitors = job(2, ResearchKind::CompetitorResearch, ResearchStatus::Running);
    competitors.harness = "claude-code".into();
    competitors.started_ms = Some(NOW - 19 * MINUTE);
    competitors.heartbeat_ms = Some(NOW - 4_000);
    competitors.output_bytes = 12 * 1024 + 300;
    let front = job(3, ResearchKind::FrontEndDesign, ResearchStatus::Queued);
    let mut stack = job(4, ResearchKind::TechStackResearch, ResearchStatus::Failed);
    stack.attempts = 3;
    stack.started_ms = Some(NOW - 12 * MINUTE);
    stack.finished_ms = Some(NOW - 2 * MINUTE);
    stack.error = Some("Codex stopped: the account is not signed in.".into());
    vec![market, competitors, front, stack]
}

fn loaded() -> ResearchView {
    let mut view = ResearchView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Jobs(jobs())));
    view
}

fn frame(view: &ResearchView, width: u16, height: u16) -> Vec<String> {
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
    let mut rows: Vec<Row> = tab_strip(columns, &items, Tab::Agents as usize, &look).into();
    let room = usize::from(height) - 4 - 2;
    let (lines, help) = view.rows(columns - 4, room, &look);
    for line in lines.into_iter().take(room) {
        rows.push(Row::of("  ", Style::new()).cat(line).pad(columns));
    }
    let screen = Screen {
        rows,
        cursor: None,
        crumbs: vec!["Home".into(), "Tidepool".into(), "Agents".into()],
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

#[test]
fn research_draws_progress_at_120_and_80_columns() {
    let view = loaded();
    let wide = frame(&view, 120, 36);
    let summary = find(&wide, "Research  ");
    for part in ["1 running", "1 failed", "1 waiting", "1 done"] {
        assert!(summary.contains(part), "{summary}");
    }
    assert!(find(&wide, "> ").contains("Market research"));
    assert!(find(&wide, "Market research  ").contains("Report ready"));
    let running = find(&wide, "Competitor research  ");
    assert!(running.contains("Claude Code") && running.contains("Running  19 min"));
    assert!(running.contains("12 KB written, heard 4 s ago"));
    assert!(find(&wide, "Front-end design").contains("Waiting for an agent"));
    let failed = find(&wide, "Tech stack research");
    assert!(failed.contains("Failed  after 3 tries"));
    assert!(failed.contains("Codex stopped: the account is not"));
    assert!(find(&wide, "Started ").contains("took 6 min"));
    assert!(
        wide.iter()
            .any(|row| row.contains("Report        research/market_research.md"))
    );
    assert!(
        wide.iter()
            .any(|row| row.contains("Harness       Codex, tools on"))
    );
    assert!(wide.iter().any(|row| row.contains("Output        40 KB")));
    assert!(
        wide.iter()
            .any(|row| row.contains("Log           The live log is not ready yet."))
    );
    assert!(wide.last().unwrap().contains("Enter Open report"));

    let narrow = frame(&view, 80, 24);
    let running = find(&narrow, "Competitor research  ");
    assert!(
        !running.contains("Claude Code"),
        "the harness column is wide only"
    );
    assert!(running.contains("12 KB written"));
    assert!(narrow.iter().all(|row| row.chars().count() <= 80));
    assert!(narrow.iter().all(|row| !row.contains("context/")));
}

#[test]
fn enter_opens_a_finished_report_only() {
    let mut view = loaded();
    assert_eq!(
        view.key(press(KeyCode::Enter)),
        ResearchReply::Report("context/research/market_research.md".into())
    );
    view.key(press(KeyCode::Down));
    assert_eq!(view.key(press(KeyCode::Enter)), ResearchReply::Handled);
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "The report opens here when this research is done."
        ))
    );
    assert_eq!(view.key(press(KeyCode::Char('z'))), ResearchReply::Ignored);
}

#[test]
fn cancel_and_retry_say_plainly_that_they_are_not_ready() {
    let mut view = loaded();
    view.key(press(KeyCode::Down));
    view.key(press(KeyCode::Char('c')));
    assert_eq!(view.notice, Some(Notice::plain(CANCEL_NOT_READY)));
    view.key(press(KeyCode::Char('r')));
    assert_eq!(
        view.notice,
        Some(Notice::plain(
            "Only failed or stopped research can run again."
        ))
    );
    for _ in 0..5 {
        view.key(press(KeyCode::Down));
    }
    assert_eq!(view.cursor, 3, "the cursor stops at the last job");
    view.key(press(KeyCode::Char('r')));
    assert_eq!(view.notice, Some(Notice::plain(RETRY_NOT_READY)));
    view.key(press(KeyCode::Char('c')));
    assert_eq!(
        view.notice,
        Some(Notice::plain("This research is not running."))
    );
    let rows = frame(&view, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("This research is not running."))
    );
    assert!(find(&rows, "Failed at").contains("Tech stack research"));
    assert!(rows.iter().any(|row| row.contains("Tries         3")));
}

#[test]
fn reads_stop_once_no_research_is_live() {
    let mut view = ResearchView::new(project());
    assert!(
        frame(&view, 80, 24)
            .iter()
            .any(|row| row.contains("Loading..."))
    );
    let query = view.next_query(NOW).unwrap();
    assert_eq!(query, SetupQuery::Jobs { project: project() });
    assert!(view.next_query(NOW + 1).is_none(), "one read at a time");
    view.forget_reads();
    let query = view.next_query(NOW + 2).unwrap();
    view.apply(&query, Err("trusted_connection_unavailable".into()));
    assert!(view.notice.as_ref().unwrap().error);
    assert!(
        view.next_query(NOW + 3).is_none(),
        "a failed read waits for the next period"
    );
    let query = view.next_query(NOW + 2 + REFRESH_MS).unwrap();
    let mut done = jobs();
    done.retain(|job| job.status == ResearchStatus::Done);
    view.apply(&query, Ok(SetupView::Jobs(done)));
    assert!(!view.live());
    assert!(
        view.next_query(NOW + 10 * REFRESH_MS).is_none(),
        "finished research is not read again"
    );
    view.apply(
        &SetupQuery::Jobs {
            project: ProjectId::new("ledgerline").unwrap(),
        },
        Ok(SetupView::Jobs(Vec::new())),
    );
    assert_eq!(
        view.jobs.as_ref().map(Vec::len),
        Some(1),
        "another project is ignored"
    );
    view.apply(
        &SetupQuery::Team { project: project() },
        Ok(SetupView::Jobs(Vec::new())),
    );
    assert_eq!(
        view.jobs.as_ref().map(Vec::len),
        Some(1),
        "other queries are ignored"
    );
}

#[test]
fn no_research_says_where_it_comes_from() {
    let mut view = ResearchView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Jobs(Vec::new())));
    let rows = frame(&view, 80, 24);
    assert!(rows.iter().any(|row| row.contains("No research yet.")));
    assert_eq!(view.key(press(KeyCode::Enter)), ResearchReply::Handled);
}

#[test]
fn many_jobs_scroll_with_the_cursor() {
    let mut many = Vec::new();
    for id in 0..30 {
        let mut queued = job(id, ResearchKind::DesignResearch, ResearchStatus::Queued);
        queued.kind_label = format!("Design pass {id}");
        many.push(queued);
    }
    let mut view = ResearchView::new(project());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Jobs(many)));
    for _ in 0..29 {
        view.key(press(KeyCode::Down));
    }
    let rows = frame(&view, 80, 24);
    assert!(find(&rows, "> ").contains("Design pass 29"));
    assert!(rows.iter().all(|row| !row.contains("Design pass 0 ")));
}

#[test]
fn progress_and_sizes_read_plainly() {
    let mut stopping = job(9, ResearchKind::MarketResearch, ResearchStatus::Running);
    stopping.cancel_requested = true;
    assert_eq!(progress(&stopping, NOW), "Stopping");
    let pool = job(9, ResearchKind::MarketResearch, ResearchStatus::NeedsPool);
    assert_eq!(progress(&pool, NOW), "Assign a pool in Settings");
    let cancelled = job(9, ResearchKind::MarketResearch, ResearchStatus::Cancelled);
    assert_eq!(progress(&cancelled, NOW), "Stopped");
    assert_eq!(bytes(512), "512 B");
    assert_eq!(bytes(12 * 1024 + 300), "12 KB");
    assert_eq!(bytes(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
}

#[test]
#[should_panic(expected = "assertion failed")]
fn jobs_of_another_project_are_a_bug() {
    let mut view = ResearchView::new(ProjectId::new("ledgerline").unwrap());
    let query = view.next_query(NOW).unwrap();
    view.apply(&query, Ok(SetupView::Jobs(jobs())));
}

#[tokio::test]
async fn real_engine_research_without_a_pool_says_so() {
    let root = tempfile::Builder::new()
        .prefix("ca-tui-research-")
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
                research: vec![ResearchKind::MarketResearch, ResearchKind::DesignResearch],
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
    let mut view = ResearchView::new(project());
    let query = view.next_query(NOW).unwrap();
    let read = controller
        .research_jobs(project())
        .await
        .map(SetupView::Jobs)
        .map_err(|error| error.0);
    view.apply(&query, read);
    let rows = frame(&view, 120, 36);
    assert!(find(&rows, "Research  ").contains("2 waiting"));
    let market = find(&rows, "Market research  ");
    assert!(market.contains("Needs a pool") && market.contains("Assign a pool in Settings"));
    assert!(find(&rows, "Design research  ").contains("Needs a pool"));
    frame(&view, 80, 24);
    assert!(view.live(), "research waiting for a pool is still read");
}
