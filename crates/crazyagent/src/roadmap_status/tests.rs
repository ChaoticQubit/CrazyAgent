use super::*;

const NOW: i64 = 1_800_000_000_000;

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db
}

fn alpha() -> ProjectId {
    ProjectId::new("alpha").unwrap()
}

/// Counts the status runs `tick` starts, for which projects and under which claims.
#[derive(Default)]
struct FakeRuns {
    started: usize,
    projects: Vec<String>,
    claims: Vec<Claim>,
}

impl Runs for FakeRuns {
    fn start(&mut self, project: &ProjectId, claim: Claim) {
        self.started += 1;
        self.projects.push(project.as_str().to_owned());
        self.claims.push(claim);
    }
}

impl FakeRuns {
    /// The claim of the run started last.
    fn last(&self) -> Claim {
        *self.claims.last().unwrap()
    }
}

/// A database with one project whose refresh setting is saved, at a fixed time.
struct StatusCase {
    db: Connection,
    project: ProjectId,
    now: i64,
}

fn status_case(refresh: Refresh) -> StatusCase {
    let db = db();
    let project = alpha();
    set_refresh(&db, &project, refresh).unwrap();
    StatusCase {
        db,
        project,
        now: NOW,
    }
}

impl StatusCase {
    /// Two change sets land a second apart, the last one a full quiet period before now.
    fn land_two_sets(&self) {
        mark_stale_at(&self.db, &self.project, self.now - QUIET_MS - 1000).unwrap();
        mark_stale_at(&self.db, &self.project, self.now - QUIET_MS).unwrap();
    }

    fn read(&self, now: i64) -> RoadmapStatus {
        read_at(&self.db, &self.project, now).unwrap()
    }
}

#[test]
fn items_are_the_bullets_under_each_version_heading() {
    let markdown = "# Roadmap\n\n## V1\n\n- Sign in\n- Search papers\n\n## V2\n\n- Share lists\n\n# Open questions\n\n- None\n";
    let found = items(markdown);
    assert_eq!(found.len(), 3);
    assert_eq!(
        found[0],
        RoadmapItem {
            section: "V1".into(),
            text: "Sign in".into()
        }
    );
    assert_eq!(found[2].section, "V2");
}

#[test]
fn items_take_star_numbered_and_boxed_lines_at_column_0_or_2_and_skip_engine_sections() {
    let markdown = "Preface\n\n- Before any heading\n\n### M1: Core\n\n* Star item\n1. Numbered item\n  - [x] Boxed child\n    - Too deep\n- [ ] Open box\n#### Notes\n\n- Still M1\n\n## Open questions\n\n- Which name?\n\n## Release process\n\n- Tag it\n\n## Owner answers\n\n- Yes\n\n## M2\n\n-\n- Later\n#NoSpace\n";
    let found = items(markdown);
    let pairs: Vec<(&str, &str)> = found
        .iter()
        .map(|item| (item.section.as_str(), item.text.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("", "Before any heading"),
            ("M1: Core", "Star item"),
            ("M1: Core", "Numbered item"),
            ("M1: Core", "Boxed child"),
            ("M1: Core", "Open box"),
            ("M1: Core", "Still M1"),
            ("M2", "Later"),
        ]
    );
}

#[test]
fn items_stay_within_their_count_and_byte_bounds_and_show_hidden_characters() {
    let mut markdown = format!("## {}\n\n", "s".repeat(SECTION_BYTES_MAX * 2));
    markdown.push_str(&format!("- {}\u{e9}\n", "a".repeat(ITEM_BYTES_MAX - 1)));
    markdown.push_str("- Ship \u{202e}now\n");
    for n in 0..ITEMS_MAX * 2 {
        markdown.push_str(&format!("- Item {n}\n"));
    }
    let found = items(&markdown);
    assert_eq!(found.len(), ITEMS_MAX);
    assert_eq!(found[0].section.len(), SECTION_BYTES_MAX);
    assert_eq!(
        found[0].text,
        "a".repeat(ITEM_BYTES_MAX - 1),
        "a cut never splits a character"
    );
    assert_eq!(found[1].text, "Ship \\u202enow");
    assert!(found.iter().all(|item| item.text.len() <= ITEM_BYTES_MAX));
}

#[test]
fn a_reply_names_each_item_by_number_with_a_known_status() {
    let found = items("## V1\n\n- Sign in\n- Search papers\n");
    let reply = r#"{"items":[{"item":1,"status":"tested","evidence":"tests/sign_in.rs, landed set 41"},{"item":2,"status":"in_progress","evidence":"branch search"}]}"#;
    let states = parse_reply(reply, &found).unwrap();
    assert_eq!(states[0].status, ItemStatus::Tested);
    assert_eq!(states[0].evidence, "tests/sign_in.rs, landed set 41");
    assert_eq!(states[1].status, ItemStatus::InProgress);
    assert_eq!(states[1].item, found[1]);
}

#[test]
fn an_item_the_reply_leaves_out_is_pending_and_an_unknown_number_is_dropped() {
    let found = items("## V1\n\n- Sign in\n- Search papers\n");
    let reply = r#"{"items":[{"item":9,"status":"done","evidence":"x"},{"item":2,"status":"done","evidence":"y"}]}"#;
    let states = parse_reply(reply, &found).unwrap();
    assert_eq!(states[0].status, ItemStatus::Pending);
    assert_eq!(states[1].status, ItemStatus::Done);
}

#[test]
fn a_reply_is_read_from_its_json_object_and_anything_else_in_it_is_dropped_or_bounded() {
    let found = items("## V1\n\n- Sign in\n- Search papers\n- Share\n");
    let long = "e".repeat(EVIDENCE_BYTES_MAX * 2);
    let reply = format!(
        "Here is the status.\n```json\n{{\"items\":[{{\"item\":1,\"status\":\"shipped\",\"evidence\":\"x\"}},{{\"item\":0,\"status\":\"done\"}},{{\"item\":\"2\",\"status\":\"done\"}},{{\"item\":2,\"status\":\"done\"}},{{\"item\":3,\"status\":\"tested\",\"evidence\":\"two\\nlines \u{202e}{long}\"}},\"junk\"]}}\n```\nDone."
    );
    let states = parse_reply(&reply, &found).unwrap();
    assert_eq!(states.len(), 3);
    assert_eq!(
        states[0].status,
        ItemStatus::Pending,
        "an unknown status is dropped"
    );
    assert_eq!(states[1].status, ItemStatus::Done);
    assert_eq!(states[1].evidence, "", "evidence left out is empty");
    assert_eq!(states[2].status, ItemStatus::Tested);
    assert!(states[2].evidence.starts_with("two lines \\u202e"));
    assert!(states[2].evidence.len() <= EVIDENCE_BYTES_MAX);
}

#[test]
fn a_reply_without_a_status_object_fails_with_plain_words() {
    let found = items("## V1\n\n- Sign in\n");
    for reply in [
        "",
        "I could not decide.",
        "}{",
        "{\"items\": \"none\"}",
        "{\"status\": []}",
        "{not json}",
    ] {
        assert_eq!(parse_reply(reply, &found), Err(NO_STATUS), "{reply}");
    }
    let huge = format!(
        "{{\"items\":[],\"pad\":\"{}\"}}",
        "p".repeat(REPLY_BYTES_MAX)
    );
    assert_eq!(parse_reply(&huge, &found), Err(NO_STATUS));
    assert_eq!(parse_reply("{\"items\":[]}", &[]), Ok(Vec::new()));
}

#[test]
fn a_landed_change_set_marks_the_status_stale_only_when_refresh_follows_landing() {
    let mut db = db();
    let project = alpha();
    set_refresh(&db, &project, Refresh::RefreshOnly).unwrap();
    let tx = db.transaction().unwrap();
    mark_stale(&tx, &project).unwrap();
    tx.commit().unwrap();
    assert!(!read(&db, &project).unwrap().stale);
    set_refresh(&db, &project, Refresh::AfterLanding).unwrap();
    let tx = db.transaction().unwrap();
    mark_stale(&tx, &project).unwrap();
    tx.commit().unwrap();
    assert!(read(&db, &project).unwrap().stale);
}

#[test]
fn a_project_never_checked_reads_empty_with_the_default_setting() {
    let db = db();
    let status = read(&db, &alpha()).unwrap();
    assert_eq!(
        status,
        RoadmapStatus {
            items: Vec::new(),
            checked_ms: None,
            running: false,
            stale: false,
            refresh: Refresh::AfterLanding,
            source: None,
            problem: None,
        }
    );
    let mut db = db;
    let tx = db.transaction().unwrap();
    mark_stale(&tx, &alpha()).unwrap();
    tx.commit().unwrap();
    assert!(
        read(&db, &alpha()).unwrap().stale,
        "with no saved setting a landing marks the status stale"
    );
}

#[test]
fn a_finished_run_stores_its_states_source_and_time_and_a_failed_one_keeps_them() {
    let case = status_case(Refresh::AfterLanding);
    let found = items("## V1\n\n- Sign in\n- Search papers\n");
    let reply = r#"{"items":[{"item":1,"status":"tested","evidence":"tests/sign_in.rs"}]}"#;
    let states = parse_reply(reply, &found).unwrap();
    refresh(&case.db, &case.project).unwrap();
    let mut runs = FakeRuns::default();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    let during = case.read(case.now + 1000);
    assert!(during.running && !during.stale);
    let ended = finish(
        &case.db,
        &case.project,
        runs.last(),
        Ok((states.as_slice(), "docs/plan-2026.md")),
        case.now + 2000,
    );
    assert!(ended.unwrap());
    let after = case.read(case.now + 3000);
    assert_eq!(after.items, states);
    assert_eq!(after.checked_ms, Some(case.now + 2000));
    assert_eq!(after.source.as_deref(), Some("docs/plan-2026.md"));
    assert!(!after.running && !after.stale && after.problem.is_none());

    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now + 4000).unwrap(), 1);
    let ended = finish(
        &case.db,
        &case.project,
        runs.last(),
        Err("No account in this project's pool can run the check"),
        case.now + 5000,
    );
    assert!(ended.unwrap());
    let failed = case.read(case.now + 6000);
    assert_eq!(
        failed.items, states,
        "a failed check keeps the last good states"
    );
    assert_eq!(failed.checked_ms, Some(case.now + 2000));
    assert_eq!(
        failed.problem.as_deref(),
        Some("No account in this project's pool can run the check")
    );
    assert!(!failed.running && !failed.stale);
    assert_eq!(
        tick(&case.db, &mut runs, case.now + QUIET_MS * 3).unwrap(),
        0,
        "a failed check is not tried again on its own"
    );
}

#[test]
fn a_stale_project_runs_once_even_when_two_sets_land_inside_the_quiet_period() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    case.land_two_sets();
    tick(&case.db, &mut runs, case.now).unwrap();
    tick(&case.db, &mut runs, case.now + QUIET_MS / 2).unwrap();
    assert_eq!(runs.started, 1);
    assert_eq!(runs.projects, ["alpha"]);
}

#[test]
fn a_stale_project_waits_for_the_quiet_period_after_its_last_landing() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    mark_stale_at(&case.db, &case.project, case.now).unwrap();
    mark_stale_at(&case.db, &case.project, case.now + QUIET_MS / 2).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now + QUIET_MS).unwrap(), 0);
    assert!(case.read(case.now + QUIET_MS).stale);
    assert_eq!(
        tick(&case.db, &mut runs, case.now + QUIET_MS / 2 + QUIET_MS).unwrap(),
        1
    );
    assert_eq!(runs.started, 1);
}

#[test]
fn a_refresh_runs_at_once_without_the_quiet_period() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    refresh(&case.db, &case.project).unwrap();
    tick(&case.db, &mut runs, case.now).unwrap();
    assert_eq!(runs.started, 1);
}

#[test]
fn a_refresh_still_runs_at_once_when_a_set_lands_before_the_tick() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::RefreshOnly);
    refresh(&case.db, &case.project).unwrap();
    set_refresh(&case.db, &case.project, Refresh::AfterLanding).unwrap();
    mark_stale_at(&case.db, &case.project, case.now).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
}

#[test]
fn a_set_that_lands_during_a_run_is_checked_once_more_after_it() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    mark_stale_at(&case.db, &case.project, case.now + 1000).unwrap();
    let later = case.now + 1000 + QUIET_MS;
    assert_eq!(tick(&case.db, &mut runs, case.now + 2000).unwrap(), 0);
    let ended = finish(
        &case.db,
        &case.project,
        runs.last(),
        Ok((&[][..], "roadmap.md")),
        case.now + 3000,
    );
    assert!(ended.unwrap(), "the run ends inside its time");
    assert!(case.read(later).stale);
    assert_eq!(
        tick(&case.db, &mut runs, later - 1).unwrap(),
        0,
        "the landing's quiet period still holds after the run"
    );
    assert_eq!(tick(&case.db, &mut runs, later).unwrap(), 1);
    assert_eq!(runs.started, 2);
}

#[test]
fn a_run_that_never_reports_back_is_closed_after_its_time_and_says_so() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    let over = case.now + RUN_TIME.as_millis() as i64;
    assert!(case.read(over - 1).running);
    assert!(!case.read(over).running);
    assert_eq!(tick(&case.db, &mut runs, over).unwrap(), 0);
    assert_eq!(case.read(over).problem.as_deref(), Some(UNFINISHED));
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, over + 1).unwrap(), 1);
}

#[test]
fn off_never_runs_refresh_included_and_only_refresh_runs_under_refresh_only() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    case.land_two_sets();
    set_refresh(&case.db, &case.project, Refresh::Off).unwrap();
    assert_eq!(case.read(case.now).refresh, Refresh::Off);
    assert!(
        !case.read(case.now).stale,
        "turning it off drops the due check"
    );
    assert_eq!(
        refresh(&case.db, &case.project).unwrap_err().0,
        "roadmap_status_off"
    );
    mark_stale_at(&case.db, &case.project, case.now - QUIET_MS).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 0);

    set_refresh(&case.db, &case.project, Refresh::RefreshOnly).unwrap();
    mark_stale_at(&case.db, &case.project, case.now - QUIET_MS).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 0);
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    assert_eq!(runs.started, 1);
}

#[test]
fn one_tick_starts_a_bounded_number_of_runs_the_longest_waiting_first() {
    let mut runs = FakeRuns::default();
    let db = db();
    let count = STARTS_PER_TICK_MAX + 2;
    for n in 0..count {
        let project = ProjectId::new(format!("p{n}")).unwrap();
        mark_stale_at(&db, &project, NOW - QUIET_MS - (n as i64) * 1000).unwrap();
    }
    assert_eq!(tick(&db, &mut runs, NOW).unwrap(), STARTS_PER_TICK_MAX);
    assert_eq!(runs.projects[0], format!("p{}", count - 1));
    assert_eq!(tick(&db, &mut runs, NOW).unwrap(), 2);
    assert_eq!(tick(&db, &mut runs, NOW).unwrap(), 0);
    assert_eq!(runs.started, count);
}

fn rows(db: &Connection) -> i64 {
    let count = "SELECT (SELECT COUNT(*) FROM roadmap_status)
        + (SELECT COUNT(*) FROM roadmap_status_settings)";
    db.query_row(count, [], |row| row.get(0)).unwrap()
}

#[test]
fn a_late_finish_of_a_closed_run_changes_nothing_and_the_newer_run_stays_the_only_one() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    let states = parse_reply("{\"items\":[]}", &items("## V1\n\n- Sign in\n")).unwrap();
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    let first = runs.last();
    refresh(&case.db, &case.project).unwrap();
    let over = case.now + RUN_TIME.as_millis() as i64;
    assert_eq!(
        tick(&case.db, &mut runs, over).unwrap(),
        1,
        "the overdue run is closed and the waiting check starts"
    );
    let second = runs.last();
    assert_ne!(first, second);

    let late = finish(
        &case.db,
        &case.project,
        first,
        Ok((states.as_slice(), "old.md")),
        over + 1000,
    );
    assert!(!late.unwrap(), "a closed run ends nothing");
    let during = case.read(over + 2000);
    assert!(during.running, "the newer run is still in flight");
    assert!(during.items.is_empty() && during.checked_ms.is_none() && during.source.is_none());
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(
        tick(&case.db, &mut runs, over + 3000).unwrap(),
        0,
        "one run per project at a time"
    );

    let ended = finish(
        &case.db,
        &case.project,
        second,
        Ok((states.as_slice(), "new.md")),
        over + 4000,
    );
    assert!(ended.unwrap());
    let after = case.read(over + 5000);
    assert_eq!(after.source.as_deref(), Some("new.md"));
    assert_eq!(after.checked_ms, Some(over + 4000));
    let again = finish(&case.db, &case.project, second, Err("twice"), over + 6000);
    assert!(!again.unwrap(), "a run ends once");
    assert!(case.read(over + 7000).problem.is_none());
}

#[test]
fn a_finish_after_the_project_was_removed_stores_nothing() {
    for failed in [false, true] {
        let mut runs = FakeRuns::default();
        let case = status_case(Refresh::AfterLanding);
        refresh(&case.db, &case.project).unwrap();
        assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
        for table in ["roadmap_status", "roadmap_status_settings"] {
            let removed = case
                .db
                .execute(&format!("DELETE FROM {table} WHERE project='alpha'"), [])
                .unwrap();
            assert_eq!(removed, 1, "{table}");
        }
        let outcome = if failed {
            Err("The agent failed")
        } else {
            Ok((&[][..], "roadmap.md"))
        };
        let ended = finish(
            &case.db,
            &case.project,
            runs.last(),
            outcome,
            case.now + 1000,
        );
        assert!(!ended.unwrap(), "failed={failed}");
        assert_eq!(rows(&case.db), 0, "failed={failed}");
    }
}

#[test]
fn a_project_switched_off_after_it_was_listed_as_due_is_not_claimed() {
    let case = status_case(Refresh::AfterLanding);
    case.land_two_sets();
    let listed = due(&case.db, case.now).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].as_str(), "alpha");
    let set = |name: &str| {
        let saved = case
            .db
            .execute("UPDATE roadmap_status_settings SET refresh=?1", [name])
            .unwrap();
        assert_eq!(saved, 1);
    };
    set("off");
    assert_eq!(claim(&case.db, &case.project, case.now).unwrap(), None);
    let off = case.read(case.now);
    assert!(!off.running, "nothing runs after the owner said never");
    set("after_landing");
    assert!(claim(&case.db, &case.project, case.now).unwrap().is_some());
    assert!(case.read(case.now).running);
    assert_eq!(claim(&case.db, &case.project, case.now).unwrap(), None);
}

/// Stores a status row as a later engine could have, past what this version writes.
fn store(db: &Connection, body: &str, source: &str) {
    let stored = db
        .execute(
            "INSERT INTO roadmap_status(project,body,source,problem,checked_ms,running_ms)
             VALUES('alpha',?1,?2,?2,?3,?3)",
            params![body, source, NOW],
        )
        .unwrap();
    assert_eq!(stored, 1);
}

#[test]
fn a_stored_status_past_this_versions_bounds_is_cut_when_read() {
    let db = db();
    let long = |bytes: usize| "x".repeat(bytes);
    let mut stored: Vec<serde_json::Value> = (0..ITEMS_MAX + 5)
        .map(|n| {
            serde_json::json!({
                "item": {
                    "section": long(SECTION_BYTES_MAX + 30),
                    "text": format!("{n:03}{}", long(ITEM_BYTES_MAX + 30)),
                },
                "status": "done",
                "evidence": long(EVIDENCE_BYTES_MAX + 30),
            })
        })
        .collect();
    stored.insert(
        1,
        serde_json::json!({
            "item": {"section": "V9", "text": "A later kind of item"},
            "status": "blocked",
            "evidence": "",
        }),
    );
    let body = serde_json::to_string(&stored).unwrap();
    assert!(body.len() <= BODY_BYTES_MAX);
    store(&db, &body, &long(NOTE_BYTES_MAX + 30));

    let status = read_at(&db, &alpha(), NOW).unwrap();
    assert_eq!(status.items.len(), ITEMS_MAX);
    assert!(status.items[0].item.text.starts_with("000"));
    assert!(
        status.items[1].item.text.starts_with("001"),
        "an item in a status this version does not know is left out"
    );
    for state in &status.items {
        assert_eq!(state.item.section.len(), SECTION_BYTES_MAX);
        assert_eq!(state.item.text.len(), ITEM_BYTES_MAX);
        assert_eq!(state.evidence.len(), EVIDENCE_BYTES_MAX);
    }
    assert_eq!(
        status.source.map(|source| source.len()),
        Some(NOTE_BYTES_MAX)
    );
    assert_eq!(
        status.problem.map(|words| words.len()),
        Some(NOTE_BYTES_MAX)
    );
    assert!(status.running && status.checked_ms == Some(NOW));
}

#[test]
fn a_stored_body_over_the_byte_bound_is_an_error_and_never_reads_as_never_checked() {
    let wide = "\u{e9}".repeat(BODY_BYTES_MAX / 2 + 1);
    let bodies = [
        format!("[{}]", " ".repeat(BODY_BYTES_MAX)),
        serde_json::json!([{
            "item": {"section": wide, "text": "Sign in"},
            "status": "done",
            "evidence": "",
        }])
        .to_string(),
    ];
    for body in bodies {
        assert!(body.len() > BODY_BYTES_MAX);
        let db = db();
        store(&db, &body, "docs/plan.md");
        let read = read_at(&db, &alpha(), NOW);
        assert_eq!(
            read.map(|status| (status.items.len(), status.running))
                .unwrap_err()
                .0,
            "roadmap_status_unavailable",
            "{} characters",
            body.chars().count()
        );
    }
}
