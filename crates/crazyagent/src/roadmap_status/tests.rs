use super::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    mark_stale(&tx, &project);
    tx.commit().unwrap();
    assert!(!read(&db, &project).unwrap().stale);
    set_refresh(&db, &project, Refresh::AfterLanding).unwrap();
    let tx = db.transaction().unwrap();
    mark_stale(&tx, &project);
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
    mark_stale(&tx, &alpha());
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

/// A temporary git repository with an owner identity, for the evidence a status run reads.
struct RepoCase {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}

fn repo_case() -> RepoCase {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let (home, root) = (base.join("home"), base.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let case = RepoCase {
        _tmp: tmp,
        home,
        root,
    };
    case.git(&["init", "-q", "--template=", "-b", "main"]);
    case
}

impl RepoCase {
    fn git(&self, args: &[&str]) {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("LC_ALL", "C")
                .args(["-c", "user.name=Owner"])
                .args(["-c", "user.email=owner@example.invalid"])
                .args(["-c", "commit.gpgsign=false"])
                .arg("-C")
                .arg(&self.root)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(&self, path: &str, text: &str) {
        let file = self.root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        self.git(&["add", "--", path]);
    }

    fn commit(&self, subject: &str) {
        self.git(&["commit", "-q", "--allow-empty", "-m", subject]);
    }
}

fn root_of(case: &RepoCase) -> &Path {
    &case.root
}

#[test]
fn evidence_holds_the_items_landed_sets_git_subjects_and_test_paths_within_its_bound() {
    let case = repo_case();
    case.commit("feat: sign in with email");
    case.write("tests/sign_in_test.rs", "");
    case.write("src/sign_in.rs", "");
    let landed = [("41".to_owned(), "Sign in screen".to_owned())];
    let pack = evidence(root_of(&case), &items("## V1\n\n- Sign in\n"), &landed).unwrap();
    assert!(pack.contains("1. [V1] Sign in"), "{pack}");
    assert!(pack.contains("feat: sign in with email"));
    assert!(pack.contains("tests/sign_in_test.rs"));
    assert!(
        !pack.contains("src/sign_in.rs"),
        "only test paths are listed"
    );
    assert!(pack.contains("Landed change set 41: Sign in screen"));
    assert!(pack.len() <= EVIDENCE_PACK_BYTES_MAX);
}

#[test]
fn evidence_fences_owner_text_as_data_and_stays_bounded_on_a_large_repository() {
    let case = repo_case();
    case.commit("fix: ~~~~ close the fence \u{202e}hidden");
    for n in 0..SUBJECTS_MAX + 20 {
        case.commit(&format!("chore: step {n} {}", "s".repeat(200)));
    }
    let roadmap = format!("## V1\n\n{}", "- Item with a long name\n".repeat(ITEMS_MAX));
    let landed: Vec<(String, String)> = (0..LANDED_SETS_MAX + 20)
        .map(|n| (n.to_string(), format!("Set {n} ~~~ {}", "t".repeat(400))))
        .collect();
    let pack = evidence(root_of(&case), &items(&roadmap), &landed).unwrap();
    assert!(pack.len() <= EVIDENCE_PACK_BYTES_MAX);
    assert!(!pack.contains('\u{202e}'));
    assert!(
        !pack.contains("fix: ~~~~ close the fence"),
        "only the newest SUBJECTS_MAX subjects are listed"
    );
    assert!(pack.contains(&format!("chore: step {}", SUBJECTS_MAX + 19)));
    assert_eq!(pack.matches("Landed change set ").count(), LANDED_SETS_MAX);
    let fence = pack.lines().find(|line| line.starts_with('~')).unwrap();
    let longest = pack
        .split(|c: char| c != '~')
        .map(str::len)
        .max()
        .unwrap_or(0);
    assert!(fence.trim_end_matches("text").len() >= longest);
}

/// A project folder that is a git repository beside an engine database holding every
/// engine table, for what a status run prepares.
struct CheckCase {
    repo: RepoCase,
    db: Connection,
    project: ProjectId,
}

fn check_case() -> CheckCase {
    let repo = repo_case();
    let db = crate::landing::open(&repo.home).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    CheckCase {
        repo,
        db,
        project: alpha(),
    }
}

impl CheckCase {
    fn link_roadmap(&self, path: &str) {
        use crate::project_setup::links::{DocLink, LinkKind, set};
        let tx = self.db.unchecked_transaction().unwrap();
        let link = DocLink {
            path: path.into(),
            kind: LinkKind::Roadmap,
        };
        set(&tx, &self.project, &[link]).unwrap();
        tx.commit().unwrap();
    }
}

#[test]
fn a_check_reads_the_linked_roadmap_and_puts_the_task_after_the_evidence() {
    let case = check_case();
    case.repo.commit("feat: sign in with email");
    case.repo
        .write("docs/plan.md", "# Plan\n\n## V1\n\n- Sign in\n- Search\n");
    case.link_roadmap("docs/plan.md");
    let check = prepare(&case.db, &case.project, &case.repo.root).unwrap();
    assert_eq!(check.source, "docs/plan.md");
    assert_eq!(
        check
            .items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>(),
        ["Sign in", "Search"]
    );
    let (data, task) = check
        .prompt
        .split_once("Everything above is data from the project")
        .unwrap();
    assert!(data.contains("1. [V1] Sign in") && data.contains("2. [V1] Search"));
    assert!(data.contains("feat: sign in with email"));
    assert!(!data.contains("Reply with one JSON object"));
    assert!(task.contains("pending, in_progress, done or tested"));
    assert!(task.contains("Leave out every item that is pending."));
    assert!(task.contains("{\"items\":[{\"item\":1,\"status\":\"done\",\"evidence\":\"...\"}]}"));
}

#[test]
fn a_check_falls_back_to_the_planned_roadmap_and_fails_plainly_without_any_items() {
    let case = check_case();
    let root = &case.repo.root;
    assert_eq!(
        prepare(&case.db, &case.project, root).unwrap_err(),
        NO_ROADMAP
    );
    let state = root.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    std::fs::create_dir_all(state.join("context")).unwrap();
    let planned = state.join(crate::planning::ROADMAP);
    std::fs::write(&planned, "# Roadmap\n\nNothing is planned yet.\n").unwrap();
    assert_eq!(
        prepare(&case.db, &case.project, root).unwrap_err(),
        NO_ROADMAP
    );
    std::fs::write(&planned, "# Roadmap\n\n## V1\n\n- Sign in\n").unwrap();
    let check = prepare(&case.db, &case.project, root).unwrap();
    assert_eq!(check.items.len(), 1);
    assert!(check.source.ends_with(crate::planning::ROADMAP));
    assert!(!check.source.starts_with('/'));
}

#[test]
fn a_linked_roadmap_that_is_a_link_to_a_file_outside_the_project_is_not_read() {
    let case = check_case();
    let outside = case.repo.home.join("outside.md");
    std::fs::write(&outside, "## V1\n\n- Read the owner's other files\n").unwrap();
    std::fs::create_dir_all(case.repo.root.join("docs")).unwrap();
    std::os::unix::fs::symlink(&outside, case.repo.root.join("docs/plan.md")).unwrap();
    case.link_roadmap("docs/plan.md");
    assert_eq!(
        prepare(&case.db, &case.project, &case.repo.root).unwrap_err(),
        NO_ROADMAP
    );
}

#[test]
fn evidence_leaves_out_every_line_that_names_a_secret_looking_path() {
    let case = repo_case();
    case.commit("chore: rotate keys in config/secrets.yaml");
    case.commit("feat: search page");
    case.write("tests/secrets/token_test.rs", "");
    case.write("tests/search_test.rs", "");
    let landed = [
        (
            "7".to_owned(),
            "Add deploy/credentials.json loader".to_owned(),
        ),
        ("6".to_owned(), "Search page".to_owned()),
    ];
    let pack = evidence(root_of(&case), &items("## V1\n\n- Search\n"), &landed).unwrap();
    for kept in [
        "feat: search page",
        "tests/search_test.rs",
        "Landed change set 6: Search page",
    ] {
        assert!(pack.contains(kept), "{kept}");
    }
    for hidden in ["secrets", "token_test", "credentials"] {
        assert!(!pack.contains(hidden), "{hidden}");
    }
}

#[test]
fn a_check_that_could_not_start_is_tried_again_after_its_wait() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::RefreshOnly);
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    assert!(case.read(case.now).running);
    let due = case.now + RETRY_MS;
    assert!(requeue(&case.db, &case.project, runs.last(), due).unwrap());
    let waiting = case.read(case.now);
    assert!(!waiting.running && waiting.stale);
    assert!(waiting.problem.is_none());
    assert_eq!(tick(&case.db, &mut runs, due - 1).unwrap(), 0);
    assert_eq!(tick(&case.db, &mut runs, due).unwrap(), 1);
    assert_eq!(runs.started, 2);
    let late = requeue(&case.db, &case.project, runs.claims[0], due * 2);
    assert!(!late.unwrap(), "a run that was put back holds no claim");
    assert!(case.read(due).running);
}

#[test]
fn runs_left_open_by_a_stopped_engine_are_closed_at_start_and_not_tried_again() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    assert_eq!(close_orphans(&case.db).unwrap(), 1);
    let closed = case.read(case.now + 1);
    assert!(!closed.running && !closed.stale);
    assert_eq!(closed.problem.as_deref(), Some(UNFINISHED));
    let late = finish(
        &case.db,
        &case.project,
        runs.last(),
        Err("late"),
        case.now + 2,
    );
    assert!(!late.unwrap());
    assert_eq!(tick(&case.db, &mut runs, case.now + QUIET_MS).unwrap(), 0);
    assert_eq!(close_orphans(&case.db).unwrap(), 0);
}

#[test]
fn a_linked_roadmap_whose_path_looks_like_a_secret_is_not_read() {
    let case = check_case();
    case.repo
        .write("docs/credentials/plan.md", "## V1\n\n- Sign in\n");
    case.link_roadmap("docs/credentials/plan.md");
    assert_eq!(
        prepare(&case.db, &case.project, &case.repo.root).unwrap_err(),
        NO_ROADMAP
    );
}

#[test]
fn lines_inside_a_fenced_code_block_are_not_headings_or_items() {
    let roadmap = [
        "## V1",
        "",
        "- Sign in",
        "",
        "```sh",
        "# Release process",
        "- not an item",
        "```",
        "",
        "- Search",
        "",
        "  ~~~",
        "- neither",
        "  ~~~",
        "- Export",
    ]
    .join("\n");
    let found = items(&roadmap);
    let texts: Vec<&str> = found.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, ["Sign in", "Search", "Export"]);
    assert!(found.iter().all(|item| item.section == "V1"));
}

#[test]
fn switching_the_status_off_takes_the_claim_of_a_running_check_and_its_result_is_not_stored() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    let states = parse_reply("{\"items\":[]}", &items("## V1\n\n- Sign in\n")).unwrap();
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    let claim = runs.last();
    assert!(
        held(&case.db, &case.project, claim),
        "a run holds its claim"
    );
    assert!(case.read(case.now + 1000).running);

    set_refresh(&case.db, &case.project, Refresh::Off).unwrap();
    assert!(
        !held(&case.db, &case.project, claim),
        "Off takes the claim, so the running check stops"
    );
    assert!(!case.read(case.now + 2000).running);
    let ended = finish(
        &case.db,
        &case.project,
        claim,
        Ok((states.as_slice(), "roadmap.md")),
        case.now + 3000,
    );
    assert!(
        !ended.unwrap(),
        "a check that ends after Off stores nothing"
    );
    let after = case.read(case.now + 4000);
    assert!(after.checked_ms.is_none() && after.source.is_none() && after.problem.is_none());
    assert!(!requeue(&case.db, &case.project, claim, case.now + 5000).unwrap());
    set_refresh(&case.db, &case.project, Refresh::AfterLanding).unwrap();
    assert_eq!(
        tick(&case.db, &mut runs, case.now + 6000).unwrap(),
        0,
        "nothing waits after Off"
    );
}

#[test]
fn a_run_closed_after_its_time_or_whose_project_was_removed_no_longer_holds_its_claim() {
    let mut runs = FakeRuns::default();
    let case = status_case(Refresh::AfterLanding);
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    let first = runs.last();
    assert!(held(&case.db, &case.project, first));
    let over = case.now + RUN_TIME.as_millis() as i64;
    tick(&case.db, &mut runs, over).unwrap();
    assert!(!held(&case.db, &case.project, first), "a closed run stops");

    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, over + 1000).unwrap(), 1);
    let second = runs.last();
    assert!(held(&case.db, &case.project, second));
    assert!(!held(&case.db, &case.project, first));
    let removed = case
        .db
        .execute("DELETE FROM roadmap_status WHERE project='alpha'", [])
        .unwrap();
    assert_eq!(removed, 1);
    assert!(
        !held(&case.db, &case.project, second),
        "the run of a removed project stops"
    );
}

#[test]
fn the_name_of_a_status_run_is_read_back_as_its_own_projects_only() {
    let demo = ProjectId::new("demo").unwrap();
    let name = run::run_name(&demo, NOW);
    assert_eq!(name, format!("roadmap-status-demo-{NOW}"));
    assert!(names_run(&demo, &name));
    for other in ["demo-2", "dem", "demonstration"] {
        let other = ProjectId::new(other).unwrap();
        assert!(!names_run(&other, &name), "{other:?}");
        assert!(!names_run(&demo, &run::run_name(&other, NOW)), "{other:?}");
    }
    for odd in [
        "roadmap-status-demo",
        "roadmap-status-demo-",
        "roadmap-status-demo-later",
        "roadmap-status-demo-1-",
        "roadmap-status-demo--1",
        "roadmap-status-demo-12345678901234567890",
        "job-1-1",
    ] {
        assert!(!names_run(&demo, odd), "{odd}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_status_run_names_only_its_own_folder_and_profile_for_removal() {
    use crate::research::runner::{
        ReadOnlyRun, ResearchAccount, ResearchLimits, ResearchRunner, RunError,
    };
    use std::os::unix::fs::PermissionsExt;
    let private = |path: &Path| {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    let temp = tempfile::Builder::new()
        .prefix("ca-status-run-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temp.path().canonicalize().unwrap();
    let engine = base.join("engine");
    private(&engine);
    let database = engine.join("engine.db");
    let sqlite = Connection::open(&database).unwrap();
    sqlite.execute_batch("PRAGMA user_version=1").unwrap();
    drop(sqlite);
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
    let state = engine.join("research");
    private(&state);
    let folder = base.join("repo");
    std::fs::create_dir(&folder).unwrap();
    let runner =
        ResearchRunner::without_codex(&database, &state, ResearchLimits::default()).unwrap();
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let project = alpha();
    let ask = |name: &str| {
        let run = ReadOnlyRun {
            name,
            agent: "roadmap-status",
            project: Some(&project),
            folder: &folder,
            prompt: "Check the roadmap.",
            account: &account,
            wall_time: std::time::Duration::from_secs(5),
            output_tokens: Some(OUTPUT_TOKENS),
        };
        run::asked(&runner, &run, &|| false)
    };

    let name = run::run_name(&project, NOW);
    let (ending, left) = ask(&name);
    assert!(
        matches!(ending, Err(RunError::Invalid(_))),
        "this state has no harness to run: {ending:?}"
    );
    let left = left.expect("a run that made its own folder names it for removal");
    assert!(!left.due(), "the usage upkeep has not had its time yet");
    let agents = state.join("agents");
    let profile = state.join("profiles").join(format!("{name}.sb"));
    std::fs::create_dir_all(agents.join(&name).join("home")).unwrap();
    std::fs::create_dir_all(state.join("profiles")).unwrap();
    std::fs::write(&profile, "(version 1)").unwrap();
    left.remove();
    assert!(
        std::fs::symlink_metadata(agents.join(&name)).is_err(),
        "the run's folder is gone"
    );
    assert!(std::fs::symlink_metadata(profile).is_err());

    let taken = run::run_name(&project, NOW + 1);
    std::fs::create_dir_all(agents.join(&taken)).unwrap();
    std::fs::write(agents.join(&taken).join("kept"), "another run's").unwrap();
    let (ending, left) = ask(&taken);
    assert!(matches!(ending, Err(RunError::Invalid(_))));
    assert!(
        left.is_none(),
        "a folder this run did not make is not named"
    );
    assert!(agents.join(&taken).join("kept").is_file());
}

/// S4-2 r1 M1: Delete everything records the project's roadmap status runs as engine files
/// and removes each run's folder and Seatbelt profile; the runs of a project whose id only
/// starts the same, a name in no run's shape and a research job's run all stay.
#[test]
fn delete_everything_removes_the_projects_roadmap_status_runs_and_no_other_projects() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let agents = base.join("research").join("agents");
    let profiles = base.join("research").join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    let own = "roadmap-status-demo-1794000000000";
    let others = [
        "roadmap-status-demo-2-1794000000000",
        "roadmap-status-demonstration-1794000000000",
        "roadmap-status-demo-later",
        "job-7-1",
    ];
    for name in others.iter().chain([&own]) {
        std::fs::create_dir_all(agents.join(name).join("home")).unwrap();
        std::fs::write(agents.join(name).join("prompt.md"), "evidence").unwrap();
        std::fs::write(profiles.join(format!("{name}.sb")), "(version 1)").unwrap();
    }
    let plan = crate::projects_home::DeletePlan {
        database: base.join("engine.db"),
        project: ProjectId::new("demo").unwrap(),
        home: base.join("home"),
        folders: vec![],
        agents: vec![],
    };
    let files = plan.engine_files(&[]);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, [own]);
    assert_eq!(
        crate::projects_home::remove_engine_files(&plan.database, &files),
        [own.to_owned()]
    );
    assert!(std::fs::symlink_metadata(agents.join(own)).is_err());
    assert!(std::fs::symlink_metadata(profiles.join(format!("{own}.sb"))).is_err());
    for name in others {
        assert!(agents.join(name).join("prompt.md").is_file(), "{name}");
        assert!(profiles.join(format!("{name}.sb")).is_file(), "{name}");
    }
}

#[test]
fn a_check_that_got_an_account_drops_the_no_account_problem_and_keeps_any_other() {
    let case = status_case(Refresh::AfterLanding);
    let mut runs = FakeRuns::default();
    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, case.now).unwrap(), 1);
    let due = case.now + RETRY_MS;
    assert!(wait_for_account(&case.db, &case.project, runs.last(), NO_ACCOUNT, due).unwrap());
    let waiting = case.read(case.now + 1);
    assert_eq!(waiting.problem.as_deref(), Some(NO_ACCOUNT));
    assert!(waiting.stale && !waiting.running);

    assert_eq!(tick(&case.db, &mut runs, due).unwrap(), 1);
    assert!(account_found(&case.db, &case.project, runs.last()).unwrap());
    let found = case.read(due + 1);
    assert!(found.running && found.problem.is_none());
    assert!(
        finish(
            &case.db,
            &case.project,
            runs.last(),
            Err(UNFINISHED),
            due + 2
        )
        .unwrap()
    );

    refresh(&case.db, &case.project).unwrap();
    assert_eq!(tick(&case.db, &mut runs, due + 3).unwrap(), 1);
    assert!(
        !account_found(&case.db, &case.project, runs.last()).unwrap(),
        "another problem stays until the check ends"
    );
    assert_eq!(case.read(due + 4).problem.as_deref(), Some(UNFINISHED));
}

/// A status check whose loopback provider refuses connections fails at once with the plain
/// reach line, before its evidence is read or any agent folder is made, and leaves nothing
/// for removal.
#[cfg(target_os = "macos")]
#[test]
fn a_status_check_whose_provider_refuses_connections_fails_at_once_with_the_reach_line() {
    use crate::research::runner::{ResearchAccount, ResearchLimits, ResearchRunner};
    use std::os::unix::fs::PermissionsExt;
    let private = |path: &Path| {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    let temp = tempfile::Builder::new()
        .prefix("ca-reach-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temp.path().canonicalize().unwrap();
    let engine = base.join("engine");
    private(&engine);
    let database = engine.join("engine.db");
    let sqlite = Connection::open(&database).unwrap();
    sqlite.execute_batch("PRAGMA user_version=1").unwrap();
    drop(sqlite);
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
    let state = engine.join("research");
    private(&state);
    let root = base.join("repo");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
    let runner =
        ResearchRunner::without_codex(&database, &state, ResearchLimits::default()).unwrap();
    let closed = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let account = ResearchAccount::Endpoint {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "local-model".into(),
        key: None,
    };
    let line = format!(
        "Can't reach the model provider at 127.0.0.1:{port}. Start it or pick another account in this pool, then try again."
    );
    let db = Connection::open_in_memory().unwrap();
    let project = ProjectId::new("plant-shop").unwrap();
    let mut left = None;
    match run::answer(
        &runner,
        &db,
        &project,
        Claim::at(1),
        &root,
        &account,
        &mut left,
    ) {
        Err(run::Unchecked::Failed(words)) => assert_eq!(words, line),
        _ => panic!("the status check did not fail with the reach line"),
    }
    assert!(
        left.is_none(),
        "a check that made nothing names nothing for removal"
    );
    assert!(
        std::fs::read_dir(state.join("agents"))
            .map_or(true, |mut entries| entries.next().is_none()),
        "no agent folder was made"
    );
}
