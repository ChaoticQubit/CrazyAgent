use super::*;
use std::io::Write;

const NOW: i64 = 1_793_000_000_000;
const MODEL: &str = "claude-opus-4-5";

fn store() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let db = super::super::open(root.path()).unwrap();
    (root, db)
}

fn who() -> Attribution {
    Attribution {
        kind: RunKind::Agent,
        project: Some(ProjectId::new("alpha").unwrap()),
        pool: None,
        account: Some(CredentialRef::new("acct").unwrap()),
        agent: "team-7".into(),
        harness: "claude-code".into(),
    }
}

fn append(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

fn transcript(home: &Path, name: &str) -> PathBuf {
    home.join("projects/-repo").join(format!("{name}.jsonl"))
}

fn line(id: &str, input: u64, output: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-11-09T02:00:00.000Z",
            "message": {
                "id": id,
                "model": MODEL,
                "stop_reason": "end_turn",
                "content": [],
                "usage": {"input_tokens": input, "output_tokens": output}
            }
        })
    )
}

fn scan_it(db: &Connection, home: &Path) -> Tokens {
    scan_home(db, home, Format::Claude, &who(), NOW).unwrap()
}

fn expected(input: u64, output: u64) -> Tokens {
    Tokens {
        input,
        output,
        ..Tokens::default()
    }
}

#[test]
fn repeated_lines_of_one_message_count_once_at_the_largest() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    append(&file, &line("msg_a", 100, 40));
    assert_eq!(scan_it(&db, root.path()), expected(100, 40));
}

#[test]
fn interleaved_lines_of_two_messages_count_each_once() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    append(&file, &line("msg_b", 200, 20));
    append(&file, &line("msg_a", 100, 40));
    assert_eq!(scan_it(&db, root.path()), expected(300, 60));
}

#[test]
fn a_resumed_file_that_copies_a_message_does_not_count_it_again() {
    let (root, db) = store();
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    let resumed = transcript(root.path(), "s2");
    append(&resumed, &line("msg_a", 100, 10));
    append(&resumed, &line("msg_c", 7, 3));
    assert_eq!(scan_it(&db, root.path()), expected(7, 3));
}

#[test]
fn a_later_scan_adds_only_a_messages_growth() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    append(&file, &line("msg_b", 1, 1));
    append(&file, &line("msg_a", 100, 40));
    assert_eq!(scan_it(&db, root.path()), expected(1, 31));
}

#[test]
fn two_homes_holding_one_message_count_it_once() {
    let (root, db) = store();
    let other = tempfile::tempdir().unwrap();
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    append(&transcript(other.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    assert_eq!(scan_it(&db, other.path()), expected(0, 0));
}

#[test]
fn a_line_without_an_id_counts_every_time() {
    let (root, db) = store();
    let text = format!(
        "{}\n",
        serde_json::json!({"type": "assistant", "message": {"usage": {"input_tokens": 5}}})
    );
    let file = transcript(root.path(), "s1");
    append(&file, &text);
    append(&file, &text);
    assert_eq!(scan_it(&db, root.path()), expected(10, 0));
}

#[test]
fn rows_carry_the_model_the_line_names() {
    let (root, db) = store();
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    scan_it(&db, root.path());
    let model: String = db
        .query_row("SELECT model FROM usage_tokens", [], |row| row.get(0))
        .unwrap();
    assert_eq!(model, MODEL);
}

#[test]
fn one_read_stops_at_its_bound_and_the_next_scan_goes_on() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    for index in 0..USAGES_PER_READ_MAX + 3 {
        append(&file, &line(&format!("msg_{index}"), 1, 0));
    }
    let first = scan_it(&db, root.path());
    assert_eq!(first, expected(USAGES_PER_READ_MAX as u64, 0));
    assert_eq!(scan_it(&db, root.path()), expected(3, 0));
}

fn counter(db: &Connection, table: &str) -> i64 {
    db.query_row(
        "SELECT rows FROM usage_row_counts WHERE name=?1",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

fn at_cap(db: &Connection, table: &str) {
    db.execute(
        "UPDATE usage_row_counts SET rows=?2 WHERE name=?1",
        params![table, TABLE_ROWS_MAX],
    )
    .unwrap();
}

#[test]
fn the_row_counters_follow_inserts_repeats_and_deletes() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    append(&file, &line("msg_a", 100, 10));
    append(&file, &line("msg_a", 100, 40));
    append(&file, &line("msg_b", 1, 1));
    scan_it(&db, root.path());
    assert_eq!(
        (counter(&db, "usage_messages"), counter(&db, "usage_tokens")),
        (2, 1)
    );
    db.execute("DELETE FROM usage_messages WHERE id='claude:msg_b'", [])
        .unwrap();
    assert_eq!(counter(&db, "usage_messages"), 1);
}

#[test]
fn counting_rows_again_takes_no_write_lock() {
    let (root, db) = store();
    let second = crate::usage::open(root.path()).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    count_rows(&second).unwrap();
    db.execute_batch("COMMIT").unwrap();
}

#[test]
fn past_the_key_cap_a_new_message_counts_whole_and_stores_no_key() {
    let (root, db) = store();
    at_cap(&db, "usage_messages");
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    let keys: i64 = db
        .query_row("SELECT COUNT(*) FROM usage_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(keys, 0);
    append(&transcript(root.path(), "s2"), &line("msg_a", 100, 10));
    assert_eq!(
        scan_it(&db, root.path()),
        expected(100, 10),
        "with no key a repeat counts high"
    );
}

#[test]
fn past_sixteen_models_in_an_hour_a_session_files_under_no_model() {
    let (root, db) = store();
    let file = transcript(root.path(), "s1");
    for index in 0..17 {
        append(
            &file,
            &line(&format!("msg_{index}"), 1, 0).replace(MODEL, &format!("model-{index}")),
        );
    }
    assert_eq!(scan_it(&db, root.path()), expected(17, 0));
    let (rows, unnamed): (i64, i64) = db
        .query_row(
            "SELECT COUNT(*),SUM(model='') FROM usage_tokens",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((rows, unnamed), (17, 1));
}

#[test]
fn at_the_row_cap_growth_lands_in_the_hours_catch_all_row_and_no_token_is_lost() {
    let (root, db) = store();
    at_cap(&db, "usage_tokens");
    append(&transcript(root.path(), "s1"), &line("msg_a", 100, 10));
    assert_eq!(scan_it(&db, root.path()), expected(100, 10));
    let row: (String, String, i64) = db
        .query_row("SELECT session,model,input FROM usage_tokens", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(row, (String::new(), String::new(), 100));
}

#[test]
fn an_advisor_call_counts_once_beside_its_message() {
    let (root, db) = store();
    let advised = |output: u64| {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-11-09T02:00:00.000Z",
                "message": {"id": "msg_a", "model": MODEL, "usage": {
                    "input_tokens": 10, "output_tokens": 5,
                    "iterations": [
                        {"type": "message", "input_tokens": 10, "output_tokens": 5},
                        {"type": "advisor_message", "model": "claude-fable-5-1", "input_tokens": 70, "output_tokens": output}
                    ]
                }}
            })
        )
    };
    let file = transcript(root.path(), "s1");
    append(&file, &advised(20));
    append(&file, &advised(20));
    assert_eq!(scan_it(&db, root.path()), expected(80, 25));
    let rows: Vec<(String, i64)> = db
        .prepare("SELECT model,input FROM usage_tokens ORDER BY model")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![("claude-fable-5-1".to_owned(), 70), (MODEL.to_owned(), 10)]
    );
}
