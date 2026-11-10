use super::*;
use crate::usage::capture::RunKind;
use std::io::Write;

const NOW: i64 = 1_795_000_000_000;

pub(super) fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = crate::usage::open(root.path()).unwrap();
    (root, db)
}

pub(super) fn who() -> Attribution {
    Attribution {
        kind: RunKind::Agent,
        project: Some(ProjectId::new("alpha").unwrap()),
        pool: None,
        account: Some(CredentialRef::new("acct").unwrap()),
        agent: "team-7".into(),
        harness: "claude-code".into(),
    }
}

pub(super) fn message(path: &Path, id: &str, input: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-11-09T02:00:00.000Z",
            "message": {"id": id, "model": "claude-opus-4-5", "stop_reason": "end_turn",
                "usage": {"input_tokens": input, "output_tokens": 1}}
        })
    )
    .unwrap();
}

pub(super) fn old_row(db: &Connection, session: &str, input: i64, read: i64) {
    db.execute(
        "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,input,output,cache_read,cache_write)
         VALUES (3600000,'agent','alpha','','acct','team-7','claude-code',?1,?2,1,?3,0)",
        params![session, input, read],
    )
    .unwrap();
}

pub(super) fn recount_all(db: &Connection, home: &Path) {
    begin(
        db,
        &[Home {
            path: home.to_path_buf(),
            format: Format::Claude,
            attribution: who(),
            removed: false,
        }],
        NOW,
    )
    .unwrap();
    while step(db, BYTES_PER_STEP, NOW).unwrap() {}
}

fn home_of(path: PathBuf) -> Home {
    Home {
        path,
        format: Format::Claude,
        attribution: who(),
        removed: false,
    }
}

#[test]
fn the_recount_reports_old_and_new_totals_and_changes_no_row() {
    let (root, db) = store();
    let file = root.path().join("home/projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    message(&file, "msg_b", 50);
    message(&file, "msg_a", 100);
    old_row(&db, "s1", 400, 7);
    recount_all(&db, &root.path().join("home"));
    let status = status(&db).unwrap().unwrap();
    assert_eq!((status.old.input, status.new.input), (400, 150));
    assert_eq!(status.sessions_recounted, 1);
    assert!(status.new_cost_nanos > 0);
    let kept: i64 = db
        .query_row("SELECT SUM(input) FROM usage_tokens", [], |row| row.get(0))
        .unwrap();
    assert_eq!(kept, 400);
}

#[test]
fn a_session_whose_transcript_is_gone_keeps_its_old_numbers() {
    let (root, db) = store();
    let file = root.path().join("home/projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "gone", 900, 0);
    recount_all(&db, &root.path().join("home"));
    let status = status(&db).unwrap().unwrap();
    assert_eq!((status.old.input, status.new.input), (1300, 1000));
    assert_eq!(
        (status.sessions_recounted, status.sessions_kept_old),
        (1, 1)
    );
}

#[test]
fn a_home_that_is_gone_is_no_failure() {
    let (root, db) = store();
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &root.path().join("no-such-home"));
    assert_eq!(status(&db).unwrap(), None);
    assert_eq!(state(&db).unwrap().as_deref(), Some("nothing"));
}

#[test]
fn a_step_stays_within_its_bytes_and_a_restart_goes_on() {
    let (root, db) = store();
    let home = root.path().join("home");
    for index in 0..3 {
        message(
            &home.join(format!("projects/-repo/s{index}.jsonl")),
            &format!("msg_{index}"),
            10,
        );
    }
    old_row(&db, "s0", 99, 0);
    begin(
        &db,
        &[Home {
            path: home.clone(),
            format: Format::Claude,
            attribution: who(),
            removed: false,
        }],
        NOW,
    )
    .unwrap();
    assert!(step(&db, 1, NOW).unwrap(), "the listing spends the budget");
    assert!(step(&db, 1, NOW).unwrap());
    let staged: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_recount_rows", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(staged, 1, "one byte of budget reads one chunk");
    drop(db);
    let db = crate::usage::open(root.path()).unwrap();
    while step(&db, BYTES_PER_STEP, NOW).unwrap() {}
    assert_eq!(status(&db).unwrap().unwrap().new.input, 30);
}

#[test]
fn a_recount_runs_once() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    assert!(started(&db).unwrap());
    begin(&db, &[], NOW + 1).unwrap();
    assert_eq!(status(&db).unwrap().unwrap().new.input, 100);
}

#[test]
fn the_status_names_no_path_session_or_transcript_text() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(
        &home.join("projects/-repo/secret-session.jsonl"),
        "msg_private",
        100,
    );
    old_row(&db, "secret-session", 400, 0);
    recount_all(&db, &home);
    let text = serde_json::to_string(&status(&db).unwrap().unwrap()).unwrap();
    for word in [
        "secret-session",
        "msg_private",
        "projects",
        root.path().to_str().unwrap(),
    ] {
        assert!(!text.contains(word), "{word}");
    }
}

#[test]
fn a_line_is_filed_in_the_hour_it_was_written() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    let hour: i64 = db
        .query_row("SELECT hour_ms FROM usage_recount_rows", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        hour,
        super::super::rules::timestamp_ms("2026-11-09T02:00:00Z").unwrap()
    );
}

#[test]
fn an_original_and_its_copy_count_once_in_either_read_order() {
    for swapped in [false, true] {
        let (root, db) = store();
        let one = root.path().join("one");
        let two = root.path().join("two");
        message(&one.join("projects/-repo/s1.jsonl"), "msg_a", 100);
        message(&two.join("projects/-repo/s2.jsonl"), "msg_a", 100);
        message(&two.join("projects/-repo/s2.jsonl"), "msg_b", 5);
        old_row(&db, "s1", 100, 0);
        old_row(&db, "s2", 105, 0);
        let homes = if swapped { [two, one] } else { [one, two] };
        begin(&db, &homes.map(home_of), NOW).unwrap();
        while step(&db, BYTES_PER_STEP, NOW).unwrap() {}
        let status = status(&db).unwrap().unwrap();
        assert_eq!(
            (status.old.input, status.new.input),
            (205, 105),
            "swapped {swapped}"
        );
        assert_eq!(
            (status.sessions_recounted, status.sessions_kept_old),
            (2, 0),
            "swapped {swapped}"
        );
    }
}

#[test]
fn a_read_that_stopped_part_way_replaces_nothing() {
    let (root, db) = store();
    let home = root.path().join("home");
    let big = home.join("projects/-repo/s1.jsonl");
    for index in 0..capture::USAGES_PER_READ_MAX + 3 {
        message(&big, &format!("msg_{index}"), 1);
    }
    message(&home.join("projects/-repo/s2.jsonl"), "msg_whole", 7);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "s2", 50, 0);
    begin(&db, &[home_of(home)], NOW).unwrap();
    assert!(step(&db, 1, NOW).unwrap(), "the listing spends the budget");
    while !db
        .query_row(
            "SELECT progress<>'' FROM usage_recount_files WHERE session='s1'",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    {
        assert!(
            step(&db, 1, NOW).unwrap(),
            "the big file takes more than one chunk"
        );
    }
    std::fs::remove_file(&big).unwrap();
    while step(&db, BYTES_PER_STEP, NOW).unwrap() {}
    let status = status(&db).unwrap().unwrap();
    assert_eq!(
        status.new.input, 407,
        "s1 keeps 400 and its staged lines are not used"
    );
    assert_eq!(
        (status.sessions_recounted, status.sessions_kept_old),
        (1, 1)
    );
}

#[test]
fn a_last_line_still_being_written_does_not_stop_the_read() {
    let (root, db) = store();
    let home = root.path().join("home");
    let file = home.join("projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    let mut open = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    write!(open, "{{\"type\":\"assistant\"").unwrap();
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    let status = status(&db).unwrap().unwrap();
    assert_eq!((status.new.input, status.sessions_recounted), (100, 1));
}

#[test]
fn the_read_gives_the_live_table_its_keys() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    let seeded: i64 = db
        .query_row(
            "SELECT input FROM usage_messages WHERE id='claude:msg_a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(seeded, 100);
}

#[test]
fn seeding_moves_one_bounded_page_per_step() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    begin(&db, &[home_of(home)], NOW).unwrap();
    db.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1)
         INSERT INTO usage_recount_messages(id,input,output,cache_read,cache_write,web,last_ms)
         SELECT printf('k%07d',i),1,0,0,0,0,0 FROM n",
        [SEEDED_PER_STEP + 1],
    )
    .unwrap();
    let keys = |db: &Connection| -> i64 {
        db.query_row("SELECT COUNT(*) FROM usage_messages", [], |row| row.get(0))
            .unwrap()
    };
    assert!(
        step(&db, BYTES_PER_STEP, NOW).unwrap(),
        "the step that read a chunk does not seed"
    );
    assert_eq!(keys(&db), 0);
    assert!(step(&db, BYTES_PER_STEP, NOW).unwrap());
    assert_eq!(keys(&db), SEEDED_PER_STEP);
    assert!(step(&db, BYTES_PER_STEP, NOW).unwrap());
    assert!(!step(&db, BYTES_PER_STEP, NOW).unwrap());
    assert_eq!(keys(&db), SEEDED_PER_STEP + 2);
}

#[test]
fn past_the_cap_a_live_key_is_not_noted() {
    let (root, db) = store();
    let home = root.path().join("home");
    let file = home.join("projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    recount_all(&db, &home);
    let noted = |db: &Connection| -> i64 {
        db.query_row("SELECT COUNT(*) FROM usage_recount_live", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    message(&file, "msg_y", 3);
    capture::scan_home(&db, &home, Format::Claude, &who(), NOW + 1).unwrap();
    assert_eq!(noted(&db), 1);
    db.execute(
        "UPDATE usage_row_counts SET rows=?1 WHERE name='usage_recount_live'",
        [capture::TABLE_ROWS_MAX],
    )
    .unwrap();
    message(&file, "msg_z", 7);
    assert_eq!(
        capture::scan_home(&db, &home, Format::Claude, &who(), NOW + 2)
            .unwrap()
            .input,
        7
    );
    assert_eq!(noted(&db), 1);
}

#[test]
fn a_thousand_tiny_files_cost_their_floor_and_a_listing_is_charged() {
    let (root, db) = store();
    let home = root.path().join("home");
    for index in 0..1000 {
        message(
            &home.join(format!("projects/-repo/s{index}.jsonl")),
            &format!("msg_{index}"),
            1,
        );
    }
    begin(&db, &[home_of(home)], NOW).unwrap();
    assert!(step(&db, BYTES_PER_STEP, NOW).unwrap());
    let done: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM usage_recount_files WHERE done=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        done as u64,
        (BYTES_PER_STEP - LISTING_COST) / CHUNK_COST_MIN
    );
}

#[test]
fn a_file_swapped_after_its_listing_is_refused() {
    let (root, db) = store();
    let home = root.path().join("home");
    let file = home.join("projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    message(&home.join("projects/-repo/s2.jsonl"), "msg_c", 5);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "s2", 50, 0);
    begin(&db, &[home_of(home)], NOW).unwrap();
    assert!(step(&db, 1, NOW).unwrap(), "the listing spends the budget");
    std::fs::rename(&file, root.path().join("aside.jsonl")).unwrap();
    message(&file, "msg_b", 7);
    while step(&db, BYTES_PER_STEP, NOW).unwrap() {}
    let status = status(&db).unwrap().unwrap();
    assert_eq!(status.new.input, 405);
    assert_eq!(
        (status.sessions_recounted, status.sessions_kept_old),
        (1, 1)
    );
}

#[test]
fn a_line_time_out_of_range_files_at_the_file_time() {
    for stamp in ["2099-01-01T00:00:00.000Z", "2001-01-01T00:00:00.000Z"] {
        let (root, db) = store();
        let home = root.path().join("home");
        let file = home.join("projects/-repo/s1.jsonl");
        message(&file, "msg_a", 100);
        let moved = std::fs::read_to_string(&file)
            .unwrap()
            .replace("2026-11-09T02:00:00.000Z", stamp);
        std::fs::write(&file, moved).unwrap();
        old_row(&db, "s1", 400, 0);
        recount_all(&db, &home);
        let hour: i64 = db
            .query_row("SELECT hour_ms FROM usage_recount_rows", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            (LINE_TIME_FLOOR_MS..=NOW).contains(&hour),
            "{stamp} filed at {hour}"
        );
    }
}

#[test]
fn a_file_that_keeps_failing_is_given_up_after_three_tries() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    message(&home.join("projects/-repo/s2.jsonl"), "msg_b", 5);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "s2", 50, 0);
    begin(&db, &[home_of(home)], NOW).unwrap();
    assert!(step(&db, 1, NOW).unwrap(), "the listing spends the budget");
    db.execute(
        "UPDATE usage_recount_files SET attempts=?1 WHERE session='s1'",
        [ATTEMPTS_MAX],
    )
    .unwrap();
    while step(&db, BYTES_PER_STEP, NOW).unwrap() {}
    let (done, complete): (bool, bool) = db
        .query_row(
            "SELECT done,complete FROM usage_recount_files WHERE session='s1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((done, complete), (true, false));
    let status = status(&db).unwrap().unwrap();
    assert_eq!((status.new.input, status.sessions_kept_old), (405, 1));
}

#[test]
fn a_project_removed_during_the_recount_is_not_put_back() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    message(&home.join("projects/-repo/s2.jsonl"), "msg_b", 5);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "s2", 50, 0);
    recount_all(&db, &home);
    db.execute(
        "INSERT INTO usage_recount_sessions (project,agent,session) VALUES (?1,'team-7','s2')",
        [crate::usage::REMOVED_PROJECTS],
    )
    .unwrap();
    crate::usage::forget_project(&db, "alpha").unwrap();
    for table in [
        "usage_recount_rows",
        "usage_recount_homes",
        "usage_recount_sessions",
    ] {
        let left: i64 = db
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE project='alpha'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
    let status = status(&db).unwrap().unwrap();
    assert_eq!((status.old.input, status.new.input), (450, 105));
    assert_eq!(
        (status.sessions_recounted, status.sessions_kept_old),
        (2, 0)
    );
}

#[test]
fn seeding_leaves_a_larger_live_key_as_it_is() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    db.execute(
        "INSERT INTO usage_messages (id,input,output,cache_read,cache_write,web,last_ms)
         VALUES ('claude:msg_a',500,9,0,0,0,?1)",
        [NOW],
    )
    .unwrap();
    recount_all(&db, &home);
    let live: (i64, i64) = db
        .query_row(
            "SELECT input,output FROM usage_messages WHERE id='claude:msg_a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(live, (500, 9));
}

#[test]
fn a_chunk_read_while_its_project_is_removed_files_under_the_removed_name() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    begin(&db, &[home_of(home)], NOW).unwrap();
    assert!(step(&db, 1, NOW).unwrap(), "the listing spends the budget");
    db.execute_batch(
        "CREATE TEMP TRIGGER removed_during_the_read AFTER UPDATE OF attempts ON usage_recount_files
         BEGIN UPDATE usage_recount_homes SET project='<removed>' WHERE home=NEW.home; END;",
    )
    .unwrap();
    step(&db, BYTES_PER_STEP, NOW).unwrap();
    let staged: Vec<String> = db
        .prepare("SELECT DISTINCT project FROM usage_recount_rows")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(staged, vec![crate::usage::REMOVED_PROJECTS.to_owned()]);
}

#[test]
fn a_session_with_one_file_not_read_to_its_end_keeps_its_old_numbers() {
    let (root, db) = store();
    let home = root.path().join("home");
    let swapped = home.join("projects/-a/s1.jsonl");
    message(&swapped, "msg_a", 100);
    message(&home.join("projects/-b/s1.jsonl"), "msg_b", 20);
    message(&home.join("projects/-repo/s2.jsonl"), "msg_c", 5);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "s2", 50, 0);
    begin(&db, &[home_of(home)], NOW).unwrap();
    assert!(step(&db, 1, NOW).unwrap(), "the listing spends the budget");
    std::fs::rename(&swapped, root.path().join("aside.jsonl")).unwrap();
    message(&swapped, "msg_x", 7);
    while step(&db, BYTES_PER_STEP, NOW).unwrap() {}
    let status = status(&db).unwrap().unwrap();
    assert_eq!(
        status.new.input, 405,
        "s1 keeps 400 though one of its files was read to its end"
    );
    assert_eq!(
        (status.sessions_recounted, status.sessions_kept_old),
        (1, 1)
    );
}

fn scan_live(db: &Connection, home: &Path, now_ms: i64) -> Tokens {
    capture::scan_home(db, home, Format::Claude, &who(), now_ms).unwrap()
}

fn input_of(db: &Connection, session: &str) -> i64 {
    db.query_row(
        "SELECT COALESCE(SUM(input),0) FROM usage_tokens WHERE session=?1",
        [session],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn use_new_replaces_only_recounted_sessions_and_keeps_a_copy() {
    let (root, db) = store();
    let home = root.path().join("home");
    let file = home.join("projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    message(&file, "msg_b", 50);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "gone", 900, 0);
    recount_all(&db, &home);
    decide(&db, true, NOW).unwrap();
    assert_eq!((input_of(&db, "s1"), input_of(&db, "gone")), (150, 900));
    let copied: i64 = db
        .query_row(
            "SELECT SUM(input) FROM usage_tokens_before_recount",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(copied, 400);
    let staged: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_recount_rows", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(staged, 0);
    assert_eq!(status(&db).unwrap(), None);
}

#[test]
fn a_line_written_between_the_recount_and_the_choice_counts_exactly_once() {
    let (root, db) = store();
    let home = root.path().join("home");
    let file = home.join("projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    message(&file, "msg_z", 7);
    scan_live(&db, &home, NOW + 1);
    decide(&db, true, NOW + 2).unwrap();
    assert_eq!(input_of(&db, "s1"), 100);
    assert_eq!(scan_live(&db, &home, NOW + 3).input, 7);
    assert_eq!(input_of(&db, "s1"), 107);
    assert_eq!(scan_live(&db, &home, NOW + 4).input, 0);
}

#[test]
fn a_message_counted_by_the_recount_is_not_counted_again_when_copied_later() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    decide(&db, true, NOW).unwrap();
    message(&home.join("projects/-repo/s2.jsonl"), "msg_a", 100);
    assert_eq!(scan_live(&db, &home, NOW + 1).input, 0);
}

#[test]
fn keep_old_leaves_every_row_and_is_never_offered_again() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    decide(&db, false, NOW).unwrap();
    assert_eq!(input_of(&db, "s1"), 400);
    assert_eq!(status(&db).unwrap(), None);
    let staged: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_recount_files", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(staged, 0);
    assert_eq!(state(&db).unwrap().as_deref(), Some("kept"));
    assert!(!step(&db, BYTES_PER_STEP, NOW).unwrap());
}

#[test]
fn a_choice_needs_a_recount_ready_to_show() {
    let (_root, db) = store();
    assert_eq!(
        decide(&db, true, NOW).unwrap_err().0,
        "usage_recount_not_ready"
    );
}

#[test]
fn the_copy_of_the_old_rows_goes_after_thirty_days() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    decide(&db, true, NOW).unwrap();
    let copy = |db: &Connection| -> bool {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='usage_tokens_before_recount')",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    forget_copy(&db, NOW + 29 * 24 * HOUR_MS).unwrap();
    assert!(copy(&db));
    forget_copy(&db, NOW + 31 * 24 * HOUR_MS).unwrap();
    assert!(!copy(&db));
}

#[test]
fn use_new_leaves_a_session_not_read_to_its_end_as_it_was() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    message(&home.join("projects/-repo/s2.jsonl"), "msg_b", 5);
    old_row(&db, "s1", 400, 0);
    old_row(&db, "s2", 50, 0);
    begin(&db, &[home_of(home)], NOW).unwrap();
    assert!(step(&db, 1, NOW).unwrap(), "the listing spends the budget");
    db.execute(
        "UPDATE usage_recount_files SET attempts=?1 WHERE session='s1'",
        [ATTEMPTS_MAX],
    )
    .unwrap();
    while step(&db, BYTES_PER_STEP, NOW).unwrap() {}
    decide(&db, true, NOW).unwrap();
    assert_eq!((input_of(&db, "s1"), input_of(&db, "s2")), (400, 5));
}

#[test]
fn before_the_choice_a_resumed_copy_counts_nothing() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    message(&home.join("projects/-repo/s2.jsonl"), "msg_a", 100);
    assert_eq!(scan_live(&db, &home, NOW + 1).input, 0);
}

#[test]
fn keep_old_still_stops_resumed_history_counting_again() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    decide(&db, false, NOW).unwrap();
    message(&home.join("projects/-repo/s2.jsonl"), "msg_a", 100);
    assert_eq!(scan_live(&db, &home, NOW + 1).input, 0);
}

#[test]
fn use_new_keeps_the_keys_of_a_session_it_did_not_replace() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    message(&home.join("projects/-repo/s9.jsonl"), "msg_new", 30);
    scan_live(&db, &home, NOW + 1);
    decide(&db, true, NOW + 2).unwrap();
    assert_eq!(input_of(&db, "s9"), 30);
    message(&home.join("projects/-repo/s10.jsonl"), "msg_new", 30);
    assert_eq!(scan_live(&db, &home, NOW + 3).input, 0);
}

#[test]
fn use_new_keeps_the_growth_of_a_message_crossing_the_read_point() {
    let (root, db) = store();
    let home = root.path().join("home");
    let file = home.join("projects/-repo/s1.jsonl");
    message(&file, "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    message(&file, "msg_a", 130);
    assert_eq!(scan_live(&db, &home, NOW + 1).input, 30);
    decide(&db, true, NOW + 2).unwrap();
    assert_eq!(input_of(&db, "s1"), 100);
    assert_eq!(scan_live(&db, &home, NOW + 3).input, 30);
    assert_eq!(input_of(&db, "s1"), 130);
}

#[test]
fn the_copy_of_replaced_rows_goes_with_a_removed_project() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    decide(&db, true, NOW).unwrap();
    crate::usage::forget_project(&db, "alpha").unwrap();
    let copied: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM usage_tokens_before_recount",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(copied, 0);
}

#[test]
fn the_old_counting_notice_lasts_until_the_owner_chooses_and_says_keep_old_after() {
    let (root, db) = store();
    assert_eq!(counting_notice(&db).unwrap().as_deref(), Some(OLD_COUNTING));
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    assert_eq!(counting_notice(&db).unwrap().as_deref(), Some(OLD_COUNTING));
    decide(&db, false, NOW).unwrap();
    assert_eq!(
        counting_notice(&db).unwrap().as_deref(),
        Some(KEPT_COUNTING)
    );
}

#[test]
fn use_new_ends_the_old_counting_notice() {
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    decide(&db, true, NOW).unwrap();
    assert_eq!(counting_notice(&db).unwrap(), None);
}

#[test]
fn a_store_with_nothing_to_compare_has_no_notice() {
    let (root, db) = store();
    recount_all(&db, &root.path().join("no-such-home"));
    assert_eq!(counting_notice(&db).unwrap(), None);
}

#[test]
fn only_the_whole_store_view_carries_the_comparison_and_prices_off_strips_its_dollars() {
    use crate::usage::money::{MainNumber, MoneySettings, PriceSource};
    let (root, db) = store();
    let home = root.path().join("home");
    message(&home.join("projects/-repo/s1.jsonl"), "msg_a", 100);
    old_row(&db, "s1", 400, 0);
    recount_all(&db, &home);
    let report = |project: Option<&ProjectId>| {
        crate::usage::report::report(&db, project, None, 0, NOW, &[]).unwrap()
    };
    let alpha = ProjectId::new("alpha").unwrap();
    let whole = report(None);
    assert!(whole.recount.as_ref().unwrap().new_cost_nanos > 0);
    assert_eq!(whole.counting_notice.as_deref(), Some(OLD_COUNTING));
    let scoped = report(Some(&alpha));
    assert_eq!(scoped.recount, None);
    assert_eq!(scoped.counting_notice.as_deref(), Some(OLD_COUNTING));
    crate::usage::money::write(
        &db,
        MoneySettings {
            main_number: MainNumber::Tokens,
            prices: PriceSource::Off,
        },
    )
    .unwrap();
    assert_eq!(report(None).recount.unwrap().new_cost_nanos, 0);
}
