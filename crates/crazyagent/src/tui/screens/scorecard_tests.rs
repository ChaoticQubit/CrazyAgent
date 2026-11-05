use super::*;
use crate::scorecard::{Act, Coordination, DayAnswer, Met, TARGETS, Task};
use crate::tui::screens::settings_accounts::tests::ctx;
use crate::tui::screens::usage::UsagePage;
use crate::tui::screens::{Flow, Screen};
use crate::tui::seam::Request;

const HOUR: i64 = 3_600_000;
const START: i64 = 1_792_540_800_000;
const NOW: i64 = START + 13 * DAY_MS + 9 * HOUR;

fn text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tally(messages: u32, answers: u32, interventions: u32, tasks: u32) -> Tally {
    Tally {
        messages,
        answers,
        interventions,
        tasks,
    }
}

fn card() -> Scorecard {
    Scorecard {
        settings: Settings::default(),
        version: 3,
        window: Days {
            start_ms: START,
            end_ms: START + 14 * DAY_MS,
        },
        baseline: Days {
            start_ms: START,
            end_ms: START + 3 * DAY_MS,
        },
        measured: Days {
            start_ms: START + 3 * DAY_MS,
            end_ms: START + 14 * DAY_MS,
        },
        real: 3,
        clean: 2,
        tasks: vec![
            Task {
                ticket: "CA-12".into(),
                title: "Sort the inbox by sender".into(),
                closed_ms: NOW - 2 * DAY_MS,
                change_sets: vec!["cs-4".into()],
                clean: true,
                flaws: vec![],
            },
            Task {
                ticket: "CA-9".into(),
                title: "Fix the export button".into(),
                closed_ms: NOW - 3 * DAY_MS,
                change_sets: vec!["cs-2".into(), "cs-3".into()],
                clean: false,
                flaws: vec![Flaw::Discarded, Flaw::FirstBatchNotPassed],
            },
        ],
        coordination: Coordination {
            baseline: tally(6, 1, 1, 2),
            measured: tally(2, 1, 0, 1),
        },
        acts: vec![Act {
            kind: Kind::OwnerEdit,
            project: "project-harbor".into(),
            subject: "src/inbox.rs".into(),
            at_ms: NOW - 3 * HOUR,
        }],
        chosen_days: 4,
        answered_days: 5,
        days: vec![
            DayAnswer {
                day: "2026-11-02".into(),
                yes: true,
            },
            DayAnswer {
                day: "2026-11-01".into(),
                yes: false,
            },
        ],
        targets: TARGETS,
        met: Met {
            tasks: false,
            clean: false,
            coordination: true,
            chosen: false,
        },
    }
}

fn job(ctx: &mut Ctx) -> SettingsJob {
    match ctx.next_request() {
        Some(Request::Settings(job)) => *job,
        other => panic!("a settings job expected, got {other:?}"),
    }
}

#[test]
fn every_key_steps_every_option_of_every_setting() {
    let rolling = Settings::default();
    let from = changed(&rolling, 'w', "2026-11-03").unwrap();
    assert_eq!(from.window, Window::FromDay("2026-11-03".into()));
    assert_eq!(changed(&from, 'w', "2026-11-04").unwrap(), rolling);
    let earlier = changed(&from, '-', "2026-11-03").unwrap();
    assert_eq!(earlier.window, Window::FromDay("2026-11-02".into()));
    let month = Settings {
        window: Window::FromDay("2026-10-31".into()),
        ..Settings::default()
    };
    assert_eq!(
        changed(&month, '+', "2026-11-03").unwrap().window,
        Window::FromDay("2026-11-01".into())
    );
    assert_eq!(changed(&rolling, '-', "2026-11-03"), None);
    assert_eq!(changed(&rolling, '+', "2026-11-03"), None);
    let mut baseline = rolling.clone();
    for expected in Baseline::ALL.iter().cycle().skip(1).take(3) {
        baseline = changed(&baseline, 'b', "2026-11-03").unwrap();
        assert_eq!(baseline.baseline, *expected);
    }
    let mut daily = rolling.clone();
    for expected in DailyQuestion::ALL.iter().cycle().skip(1).take(3) {
        daily = changed(&daily, 'd', "2026-11-03").unwrap();
        assert_eq!(daily.daily_question, *expected);
    }
    assert_eq!(changed(&rolling, 'x', "2026-11-03"), None);
}

#[test]
fn coordination_per_task_and_its_cut_read_as_plain_words() {
    assert_eq!(per_task_text(per_task(&tally(6, 1, 1, 2))), "4");
    assert_eq!(per_task_text(Some(4.0 / 3.0)), "1.3");
    assert_eq!(per_task_text(per_task(&tally(3, 0, 0, 0))), "no tasks yet");
    assert_eq!(cut(&tally(8, 0, 0, 2), &tally(3, 0, 0, 1)), Some(25));
    assert_eq!(cut(&tally(2, 0, 0, 2), &tally(3, 0, 0, 1)), Some(-200));
    assert_eq!(cut(&tally(0, 0, 0, 2), &tally(3, 0, 0, 1)), None);
    assert_eq!(cut(&tally(8, 0, 0, 0), &tally(3, 0, 0, 1)), None);
    assert_eq!(cut(&tally(8, 0, 0, 2), &tally(3, 0, 0, 0)), None);
    assert_eq!(cut_text(25), "25% less");
    assert_eq!(cut_text(-200), "200% more");
    assert_eq!(cut_text(0), "the same");
}

#[test]
fn the_panel_shows_the_four_numbers_with_targets_and_the_records_behind_them() {
    let ctx = ctx(120, 60);
    let names = HashMap::from([("project-harbor".to_owned(), "Harbor Notes".to_owned())]);
    let shown = text(&card_lines(&card(), 110, &names, NOW, 0, &ctx.theme));
    for words in [
        "Real tasks 3, target 10  Not met yet",
        "Clean 2 of 3, target 8 of 10  Not met yet",
        "Coordination 3 per task, baseline 4, 25% less; target 25% less  Target met",
        "Chose CrazyAgent 4 of 5 days, target 7 of 10 answered days  Not met yet",
        "Window: the last 14 days (w)",
        "Baseline: its first 3 days (b)",
        "Daily question: off (d)",
        "CA-9 Fix the export button",
        "cs-2, cs-3  a change set was discarded; its first check batch didn't pass",
        "Showing the newest 2 of 3.",
        "Edit  Harbor Notes  src/inbox.rs",
        "2026-11-02 Yes",
        "2026-11-01 No",
    ] {
        assert!(shown.contains(words), "{words} in:\n{shown}");
    }
    let quiet = Scorecard {
        real: 0,
        clean: 0,
        tasks: vec![],
        acts: vec![],
        days: vec![],
        answered_days: 0,
        chosen_days: 0,
        ..card()
    };
    let shown = text(&card_lines(&quiet, 110, &names, NOW, 0, &ctx.theme));
    assert!(shown.contains("None in this window yet."), "{shown}");
    assert!(shown.contains("the daily question is off"), "{shown}");
    assert!(!shown.contains("Daily answers"), "{shown}");
}

#[test]
fn agent_written_titles_subjects_and_sets_show_hidden_characters_as_escapes() {
    let ctx = ctx(120, 60);
    let mut hidden = card();
    hidden.tasks[0].title = "Sort\u{200B} the inbox\u{E0041}".into();
    hidden.tasks[0].change_sets = vec!["cs-4\u{2060}".into()];
    hidden.acts[0].subject = "src/in\u{E0062}box.rs".into();
    hidden.acts[0].project = "project-\u{200D}dock".into();
    let names = HashMap::new();
    let shown = text(&card_lines(&hidden, 110, &names, NOW, 0, &ctx.theme));
    assert!(!crate::memory::hidden_text(&shown), "{shown:?}");
    for words in ["CA-12 Sort", "the inbox", "src/in", "box.rs", "project-"] {
        assert!(shown.contains(words), "{words} in:\n{shown}");
    }
}

#[test]
fn a_settings_key_saves_by_version_and_every_answer_reads_again() {
    let mut ctx = ctx(120, 60);
    let mut panel = ScorecardPanel::new();
    panel.reload(0, &mut ctx);
    assert!(matches!(
        job(&mut ctx),
        SettingsJob::Scorecard { offset_minutes: 0 }
    ));
    assert!(panel.busy());
    assert!(panel.key(&Key::Char('d'), "2026-11-03", 0, &mut ctx));
    assert!(
        ctx.next_request().is_none(),
        "nothing is saved before a read"
    );
    panel.apply(&Ok(Box::new(card())), &mut ctx);
    assert!(!panel.busy());
    assert!(panel.key(&Key::Char('d'), "2026-11-03", 0, &mut ctx));
    match job(&mut ctx) {
        SettingsJob::ScorecardSet { settings, version } => {
            assert_eq!(settings.daily_question, DailyQuestion::On);
            assert_eq!(version, 3);
        }
        other => panic!("a save expected, got {other:?}"),
    }
    assert!(panel.key(&Key::Char('b'), "2026-11-03", 0, &mut ctx));
    assert!(ctx.next_request().is_none(), "one save at a time");
    assert_eq!(ctx.message().unwrap().lead, "Still saving.");

    let stale = SettingsEvent::ScorecardSaved(Ok(ScorecardSaved::Stale(4)));
    panel.arrive(&stale, 0, &mut ctx);
    assert_eq!(
        ctx.message().unwrap().lead,
        "These settings changed elsewhere first."
    );
    assert!(matches!(job(&mut ctx), SettingsJob::Scorecard { .. }));
    let refused = Err("scorecard_settings_invalid".to_owned());
    panel.apply(&Ok(Box::new(card())), &mut ctx);
    panel.arrive(&SettingsEvent::ScorecardSaved(refused), 0, &mut ctx);
    assert_eq!(
        ctx.message().unwrap().rest,
        "That start day isn't a calendar day."
    );
    assert!(matches!(job(&mut ctx), SettingsJob::Scorecard { .. }));
    assert!(!panel.key(&Key::Char('t'), "2026-11-03", 0, &mut ctx));
}

#[test]
fn a_scorecard_the_engine_cannot_serve_reads_as_not_ready_or_a_plain_failure() {
    let mut ctx = ctx(120, 60);
    let names = HashMap::new();
    let mut panel = ScorecardPanel::new();
    panel.reload(0, &mut ctx);
    job(&mut ctx);
    let waiting = text(&panel.lines(110, &names, NOW, 0, &ctx));
    assert!(waiting.contains("Reading the scorecard"), "{waiting}");
    panel.apply(&Err("scorecard_outcome_unknown".into()), &mut ctx);
    let failed = text(&panel.lines(110, &names, NOW, 0, &ctx));
    assert!(
        failed.contains("Could not read the scorecard. CrazyAgent couldn't confirm"),
        "{failed}"
    );
    panel.apply(&Err("scorecard_unavailable".into()), &mut ctx);
    let missing = text(&panel.lines(110, &names, NOW, 0, &ctx));
    assert!(
        missing.contains("isn't ready yet in this engine"),
        "{missing}"
    );
}

#[test]
fn g_opens_the_scorecard_in_usage_and_closes_it_again() {
    let mut ctx = ctx(120, 60);
    let mut page = UsagePage::new();
    assert!(matches!(page.key(&Key::Char('g'), &mut ctx), Flow::Handled));
    assert!(matches!(job(&mut ctx), SettingsJob::Scorecard { .. }));
    assert!(page.busy(&ctx));
    let keys: Vec<String> = page
        .hints(&ctx)
        .iter()
        .map(|hint| hint.key.clone())
        .collect();
    assert!(keys.contains(&"d".to_owned()), "{keys:?}");
    let shown = text(&page.body(110, NOW, 0, &ctx));
    assert!(shown.contains(TITLE), "{shown}");
    assert!(matches!(page.key(&Key::Char('c'), &mut ctx), Flow::Ignored));
    assert!(matches!(page.key(&Key::Char('g'), &mut ctx), Flow::Handled));
    assert!(!page.busy(&ctx));
    assert!(!text(&page.body(110, NOW, 0, &ctx)).contains(TITLE));
}
