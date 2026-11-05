use super::*;
use crate::checks::settings::RemovedTests;
use crate::engine::owner::CheckGapView;
use crate::tui::client::checks::run;
use crate::tui::client::checks::tests::{
    EARLIER, Engine, HEAD, Ran, Recorder, answer, change_set, fieldnotes, text_of, with_runs,
};
use crate::tui::screens::new_project_frames::{check_frames, compose};
use crate::tui::screens::project::{ProjectScreen, Tab};
use crate::tui::screens::project_view::project_screen;
use trackwright_core::runs::ResultOutcome;

const NOW: i64 = 1_800_000_000_000;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn text(rows: &[Row]) -> String {
    rows.iter().map(Row::text).collect::<Vec<_>>().join("\n")
}

fn settle(tests: &mut ProjectTests, engine: &mut Engine) {
    for _ in 0..16 {
        let Some(job) = tests.next_job() else {
            return;
        };
        tests.apply(&answer(job, engine));
    }
    panic!("the Tests tab kept asking");
}

fn tab(engine: &mut Engine) -> ProjectTests {
    let mut tests = ProjectTests::new(fieldnotes(), "Fieldnotes Mobile".into());
    settle(&mut tests, engine);
    tests
}

fn screen(engine: &mut Engine, keys: &[KeyCode]) -> ProjectScreen {
    let mut screen = ProjectScreen::new(fieldnotes(), "Fieldnotes Mobile".into(), None);
    screen.open(Tab::Tests);
    settle(&mut screen.tests, engine);
    for code in keys {
        assert_eq!(screen.tests.key(key(*code)), TestsReply::Handled);
    }
    settle(&mut screen.tests, engine);
    screen
}

fn frame(screen: &mut ProjectScreen, id: &str, width: u16, height: u16) -> (String, String) {
    let look = Look::plain();
    screen.width = width;
    let drawn = project_screen(screen, width, height, &look);
    (
        format!("{id}-{width}"),
        compose(&drawn, width, height, "", &look),
    )
}

fn block(checks: &SetChecks, head: &str) -> Vec<String> {
    checks
        .rows(&change_set(), head, 76, &Look::plain())
        .iter()
        .map(Row::text)
        .collect()
}

#[test]
fn the_run_page_renders_the_covered_block_at_80_columns() {
    let mut engine = with_runs(Engine::new());
    let mut screen = screen(&mut engine, &[KeyCode::Tab, KeyCode::Down, KeyCode::Enter]);
    check_frames(&[frame(&mut screen, "proj-tests-run", 80, 24)]);
}

#[test]
fn the_runs_page_renders_the_engine_backed_frames() {
    let mut engine = with_runs(Engine::new());
    let mut screen = screen(&mut engine, &[KeyCode::Tab]);
    let drawn = [
        frame(&mut screen, "proj-tests-runs", 80, 24),
        frame(&mut screen, "proj-tests-runs", 120, 36),
    ];
    check_frames(&drawn);
}

#[test]
fn f_reads_the_runs_under_each_verdict_in_turn_then_every_run_again() {
    let mut filter = None;
    let mut seen = Vec::new();
    for _ in 0..=Verdict::ALL.len() {
        filter = next_filter(filter);
        seen.push(filter);
    }
    let mut every: Vec<Option<Verdict>> = Verdict::ALL.iter().copied().map(Some).collect();
    every.push(None);
    assert_eq!(seen, every);

    let mut engine = with_runs(Engine::new());
    let mut tests = tab(&mut engine);
    assert_eq!(tests.key(key(KeyCode::Tab)), TestsReply::Handled);
    settle(&mut tests, &mut engine);
    assert_eq!(tests.key(key(KeyCode::Char('f'))), TestsReply::Handled);
    let Some(ChecksJob::Views { views, steps, .. }) = tests.next_job() else {
        panic!("a filter goes to the worker");
    };
    assert_eq!(steps, vec![Step::Filter]);
    let mut recorder = Recorder {
        engine: &mut engine,
        sent: Vec::new(),
    };
    run(&views, &steps, &mut recorder);
    assert_eq!(
        recorder.sent,
        vec![(
            OpKind::RunList,
            json!({ "project": "FM", "verdict": "passed" })
        )]
    );
    let views = views.lock().unwrap();
    assert_eq!(views.filter(), Some(Verdict::Passed));
    let rows = text_of(&views, 76, 6);
    assert!(rows[0].ends_with("1 run  passed"), "{rows:?}");
    assert!(
        rows[3].starts_with(&format!(" > {}passed", pad("FM-R2", ID_CELLS))),
        "{rows:?}"
    );
    assert_eq!(rows[4], "", "{rows:?}");
}

#[test]
fn a_ticket_id_scopes_coverage_to_the_ticket_and_other_text_to_a_release() {
    assert_eq!(
        scope_of(" FM-3 "),
        CoverageOf::Ticket(ShortId::parse("FM-3").unwrap())
    );
    assert_eq!(scope_of("1.2.0"), CoverageOf::Version("1.2.0".to_owned()));

    let mut engine = with_runs(Engine::new());
    let mut tests = tab(&mut engine);
    tests.key(key(KeyCode::BackTab));
    settle(&mut tests, &mut engine);
    let (rows, _, help) = tests.rows(76, 18, &Look::plain());
    assert!(
        text(&rows).contains(" Press c to show the coverage of a ticket or a release."),
        "{}",
        text(&rows)
    );
    assert!(help.contains(&("c", "Ticket or release")), "{help:?}");
    assert_eq!(tests.key(key(KeyCode::Char('c'))), TestsReply::Handled);
    assert!(tests.editing());
    for letter in "FM-9".chars() {
        tests.key(key(KeyCode::Char(letter)));
    }
    let (rows, cursor, help) = tests.rows(76, 18, &Look::plain());
    let field = rows
        .iter()
        .position(|row| row.text().starts_with("Ticket or release   [ FM-9 "))
        .expect("the coverage field shows what was typed");
    assert_eq!(cursor, Some((FIELD_LEFT + 4, field)));
    assert_eq!(help, vec![("Enter", "Show coverage"), ("Esc", "Cancel")]);
    assert_eq!(tests.key(key(KeyCode::Enter)), TestsReply::Handled);
    assert!(!tests.editing());
    let Some(ChecksJob::Views { steps, .. }) = tests.next_job() else {
        panic!("the scope goes to the worker");
    };
    assert_eq!(steps, vec![Step::Cover("FM-9".to_owned())]);
}

#[test]
fn esc_closes_a_run_detail_without_a_call_then_leaves_the_tab() {
    let mut engine = with_runs(Engine::new());
    let mut tests = tab(&mut engine);
    tests.key(key(KeyCode::Tab));
    tests.key(key(KeyCode::Enter));
    settle(&mut tests, &mut engine);
    let look = Look::plain();
    let (rows, _, help) = tests.rows(76, 18, &look);
    assert!(text(&rows).contains(" FM-R2  lint"), "{}", text(&rows));
    assert_eq!(help[0], ("\u{2191}\u{2193}", "Scroll"));
    assert!(!help.contains(&("Esc", "Home")), "{help:?}");

    assert_eq!(tests.key(key(KeyCode::Esc)), TestsReply::Handled);
    assert!(tests.next_job().is_none());
    let (rows, _, help) = tests.rows(76, 18, &look);
    assert!(text(&rows).contains(" FM  Runs"), "{}", text(&rows));
    assert_eq!(help.last(), Some(&("Esc", "Home")));
    assert_eq!(tests.key(key(KeyCode::Esc)), TestsReply::Ignored);
}

#[test]
fn a_project_without_tickets_says_where_its_tests_come_from() {
    let mut engine = Engine::bare();
    let mut tests = tab(&mut engine);
    let (rows, _, help) = tests.rows(76, 18, &Look::plain());
    let shown = text(&rows);
    assert!(
        shown.starts_with("Tests\nFieldnotes Mobile has no tests yet."),
        "{shown}"
    );
    assert!(
        shown
            .replace('\n', " ")
            .contains("check runs record here once a check profile is approved."),
        "{shown}"
    );
    assert_eq!(help[..2], [("r", "Read again"), ("s", "Settings")]);
    assert_eq!(tests.key(key(KeyCode::Char('s'))), TestsReply::Settings);
    assert_eq!(tests.key(key(KeyCode::Char('r'))), TestsReply::Handled);
    assert!(matches!(tests.next_job(), Some(ChecksJob::Tracked { .. })));
}

#[test]
fn a_stale_run_shows_its_line() {
    let mut engine = with_runs(Engine::new());
    let mut checks = SetChecks::new(fieldnotes());
    checks.want(&change_set(), NOW);
    let job = checks.next_job().expect("the block reads its checks");
    assert!(matches!(job, ChecksJob::Runs { .. }));
    checks.apply(&answer(job, &mut engine));
    assert!(!checks.busy());
    assert_eq!(
        block(&checks, HEAD),
        vec![
            String::new(),
            "Checks".to_owned(),
            format!(
                "  {}{}FM-R1",
                pad("unit", CHECK_CELLS),
                pad("failed", VERDICT_CELLS)
            ),
            format!(
                "  {}{}FM-R2",
                pad("lint", CHECK_CELLS),
                pad("passed", VERDICT_CELLS)
            ),
            "    stale: covered a7e3d1c, head is 4f1c2b9; it does not count for Accept".to_owned(),
            "  Could not read the Accept gate. That change set is gone.".to_owned(),
        ]
    );
    checks.want(&change_set(), NOW);
    assert!(checks.next_job().is_none());

    engine.ran(&Ran {
        check: "lint",
        commit: HEAD,
        results: &[("clippy", ResultOutcome::Passed)],
    });
    let mut fresh = SetChecks::new(fieldnotes());
    fresh.want(&change_set(), NOW);
    let job = fresh.next_job().unwrap();
    fresh.apply(&answer(job, &mut engine));
    let rows = block(&fresh, HEAD);
    assert_eq!(
        rows[3],
        format!(
            "  {}{}FM-R3",
            pad("lint", CHECK_CELLS),
            pad("passed", VERDICT_CELLS)
        )
    );
    assert!(rows.iter().all(|row| !row.contains("stale")), "{rows:?}");
    assert!(
        block(&fresh, EARLIER)
            .iter()
            .any(|row| row.contains("stale"))
    );
}

#[test]
fn k_starts_the_checks_once_and_shows_what_the_engine_said() {
    let mut engine = with_runs(Engine::new());
    let mut checks = SetChecks::new(fieldnotes());
    checks.run(&change_set());
    checks.run(&change_set());
    assert_eq!(
        block(&checks, HEAD).last().map(String::as_str),
        Some("Starting the checks...")
    );
    let Some(job @ ChecksJob::Run { .. }) = checks.next_job() else {
        panic!("k sends one start");
    };
    checks.run(&change_set());
    assert!(checks.next_job().is_none());
    checks.apply(&answer(job, &mut engine));
    assert_eq!(
        block(&checks, HEAD).last().map(String::as_str),
        Some("The change set moved on since you opened it. Read it again.")
    );
    assert!(matches!(checks.next_job(), Some(ChecksJob::Runs { .. })));
}

#[test]
fn a_dropped_start_says_so_and_a_dropped_read_is_asked_again() {
    let mut checks = SetChecks::new(fieldnotes());
    checks.run(&change_set());
    assert!(checks.next_job().is_some());
    checks.forget_reads();
    assert_eq!(
        block(&checks, HEAD).last().map(String::as_str),
        Some(
            "The engine connection dropped before the checks started. Press k to start them again."
        )
    );
    assert!(checks.next_job().is_none());

    let mut engine = with_runs(Engine::new());
    checks.want(&change_set(), NOW);
    assert!(checks.next_job().is_some());
    checks.forget_reads();
    assert!(checks.busy());
    let again = checks.next_job().expect("the read is asked again");
    assert!(matches!(&again, ChecksJob::Runs { change_set: id, .. } if *id == change_set()));
    checks.apply(&answer(again, &mut engine));
    assert!(!checks.busy());
    assert_eq!(
        block(&checks, HEAD)[2],
        format!(
            "  {}{}FM-R1",
            pad("unit", CHECK_CELLS),
            pad("failed", VERDICT_CELLS)
        )
    );
}

#[test]
fn the_block_reads_again_while_a_check_waits_and_stops_once_none_does() {
    let mut engine = with_runs(Engine::new());
    let mut checks = SetChecks::new(fieldnotes());
    checks.want(&change_set(), NOW);
    let job = checks.next_job().unwrap();
    let SettingsEvent::Checks(mut event) = answer(job, &mut engine) else {
        panic!("a checks job answers a checks event");
    };
    if let ChecksEvent::Runs {
        result: Ok(read), ..
    } = event.as_mut()
    {
        read.pending = 2;
    }
    checks.apply(&SettingsEvent::Checks(event));
    assert_eq!(
        block(&checks, HEAD).last().map(String::as_str),
        Some("  2 checks are waiting or running.")
    );
    checks.want(&change_set(), NOW + CHECKS_POLL_MS - 1);
    assert!(checks.next_job().is_none());
    checks.want(&change_set(), NOW + CHECKS_POLL_MS);
    let job = checks.next_job().expect("a waiting check is read again");
    checks.apply(&answer(job, &mut engine));
    checks.want(&change_set(), NOW + 10 * CHECKS_POLL_MS);
    assert!(checks.next_job().is_none());
}

#[test]
fn the_gate_says_what_blocks_accept_and_marks_the_run_it_finds_stale() {
    let mut engine = with_runs(Engine::new());
    let job = ChecksJob::Runs {
        project: fieldnotes(),
        change_set: change_set(),
    };
    let SettingsEvent::Checks(event) = answer(job, &mut engine) else {
        panic!("a checks job answers a checks event");
    };
    let ChecksEvent::Runs {
        result: Ok(mut read),
        ..
    } = *event
    else {
        panic!("the engine lists the change set's runs");
    };
    let gap =
        |code: &str, check: Option<&str>, run: Option<&str>, paths: &[&str], total| CheckGapView {
            code: code.to_owned(),
            check: check.map(str::to_owned),
            run: run.map(str::to_owned),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            paths_total: total,
        };
    let mut gate = CheckGateView {
        gate: GateMode::Require,
        removed_tests: RemovedTests::Flag,
        head: HEAD.to_owned(),
        profile: None,
        refusal: Some("checks_protected_change".to_owned()),
        gaps: vec![
            gap(STALE, Some("unit"), Some("FM-R1"), &[], 0),
            gap(
                "checks_protected_change",
                None,
                None,
                &["Cargo.lock", "ci/check.sh"],
                5,
            ),
            gap("checks_failed", Some("unit"), Some("FM-R1"), &[], 0),
        ],
        gaps_total: 3,
    };
    read.gate = Ok(gate.clone());
    let rows: Vec<String> = check_rows(&read, HEAD, 76, &Look::plain())
        .iter()
        .map(Row::text)
        .collect();
    assert_eq!(
        rows,
        vec![
            format!(
                "  {}{}FM-R1",
                pad("unit", CHECK_CELLS),
                pad("failed", VERDICT_CELLS)
            ),
            "    stale: another profile or approval; it does not count for Accept".to_owned(),
            format!(
                "  {}{}FM-R2",
                pad("lint", CHECK_CELLS),
                pad("passed", VERDICT_CELLS)
            ),
            "    stale: covered a7e3d1c, head is 4f1c2b9; it does not count for Accept".to_owned(),
            "  It changes a protected path. Cargo.lock, ci/check.sh and 3 more.".to_owned(),
            "  unit: A required check did not pass on this head.".to_owned(),
            "  Accept is refused: It changes a protected path.".to_owned(),
        ]
    );

    gate.gate = GateMode::Warn;
    gate.refusal = None;
    gate.gaps_total = 9;
    read.gate = Ok(gate.clone());
    let rows: Vec<String> = check_rows(&read, HEAD, 76, &Look::plain())
        .iter()
        .map(Row::text)
        .collect();
    assert_eq!(
        rows[rows.len() - 2..],
        [
            "  6 more gaps.".to_owned(),
            "  Accept goes ahead; the change set records these gaps.".to_owned(),
        ]
    );

    gate.gate = GateMode::Off;
    gate.gaps.clear();
    gate.gaps_total = 0;
    read.gate = Ok(gate);
    let rows: Vec<String> = check_rows(&read, HEAD, 76, &Look::plain())
        .iter()
        .map(Row::text)
        .collect();
    assert_eq!(
        rows.last().map(String::as_str),
        Some("  The Accept gate is off.")
    );
    assert_eq!(rows.iter().filter(|row| row.contains("stale")).count(), 1);
}
