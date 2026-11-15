use super::*;
use crate::harnesses::interactive::settings::TerminalSettings;
use crate::harnesses::manifest;
use crate::harnesses::records::json_files::tests::{Home, NEW, SESSION_ID};
use serde_json::json;
use std::io::Write;

const APPEND: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-append.yaml");
const REWRITE: &str = include_str!("../../../tests/fixtures/harness/standin-json-rewrite.yaml");
const STARTED: TranscriptEvent = TranscriptEvent::TurnStarted;

/// Gives the transcript a launch of a stand-in would follow: its settings read from its
/// manifest and its record path filled for the rows' session in the home. Inputs: the
/// manifest text and the home. Output: the transcript source.
fn source(text: &str, home: &Home) -> Transcript {
    let manifest = manifest::parse(text.as_bytes()).unwrap();
    let settings = TerminalSettings::read(&manifest).unwrap();
    let records = settings.records(&home.path(), SESSION_ID);
    assert!(records.is_ok(), "{records:?}");
    Transcript::Records(records.unwrap())
}

fn fresh(text: &str, home: &Home) -> TranscriptReader {
    TranscriptReader::new(source(text, home), &home.path(), SystemTime::now())
}

fn below(ending: &str) -> String {
    format!("standin/records/{SESSION_ID}.{ending}")
}

fn jsonl(objects: &[Value]) -> String {
    objects.iter().map(|object| format!("{object}\n")).collect()
}

/// Appends JSON lines to a record as the stand-in does. Inputs: the record and the
/// objects. Output: none.
fn appended(record: &Path, objects: &[Value]) {
    let mut file = File::options().append(true).open(record).unwrap();
    file.write_all(jsonl(objects).as_bytes()).unwrap();
}

/// Writes the record anew as the json-rewrite stand-in does: the whole list into a scratch
/// file beside it, then moved over it. Inputs: the home and the list. Output: the record.
fn rewrite(home: &Home, list: &Value) -> PathBuf {
    let record = home.path().join(below("json"));
    let scratch = home.write("standin/records/scratch", &list.to_string(), NEW);
    std::fs::rename(scratch, &record).unwrap();
    record
}

fn text(said: &str) -> TranscriptEvent {
    TranscriptEvent::Text {
        text: said.to_owned(),
    }
}

fn finished(success: bool) -> TranscriptEvent {
    TranscriptEvent::TurnFinished { success }
}

/// DEC-180 Task 16: a jsonl-append record's lines become facts by the manifest's rules:
/// a turn's start, its text, a tool and its end, with a line no rule matches passed over.
#[test]
fn a_jsonl_append_record_gives_the_facts_its_rules_match() {
    let home = Home::new();
    let mut reader = fresh(APPEND, &home);
    assert!(reader.read().unwrap().is_empty());
    assert!(!reader.reports_background());
    let turn = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "hi"}),
        json!({"type": "unknown"}),
        json!({"type": "tool", "name": "Read"}),
        json!({"type": "done", "ok": true}),
    ];
    let record = home.write(&below("jsonl"), &jsonl(&turn), NEW);
    let tool = TranscriptEvent::ToolUse {
        name: "Read".to_owned(),
    };
    let facts = [STARTED, text("hi"), tool, finished(true)];
    assert_eq!(reader.read().unwrap(), facts);
    assert_eq!(reader.conversation(), Some((SESSION_ID.to_owned(), record)));
    assert!(reader.bytes() > 0);
}

/// DEC-180 Task 16: a json-rewrite record gives the facts of the entries each rewrite
/// adds, never those of entries it gave before.
#[test]
fn a_json_rewrite_record_gives_the_facts_of_its_new_entries_only() {
    let home = Home::new();
    let mut reader = fresh(REWRITE, &home);
    let first = json!([{"type": "started"}, {"type": "text", "delta": "a"}]);
    rewrite(&home, &first);
    let started = [STARTED, text("a")];
    assert_eq!(reader.read().unwrap(), started);
    let all = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "a"}),
        json!({"type": "done"}),
    ];
    let record = rewrite(&home, &Value::Array(all.to_vec()));
    assert_eq!(reader.read().unwrap(), [finished(true)]);
    assert_eq!(reader.conversation(), Some((SESSION_ID.to_owned(), record)));
}

/// DEC-180 Task 16: an error with no rate-limit path, an interrupt and an end that says it
/// failed give the facts Task 11 gives a piped harness: not rate limited, a failed end.
#[test]
fn an_error_an_interrupt_and_a_failed_end_take_the_defaults_of_task_11() {
    let home = Home::new();
    let mut reader = fresh(APPEND, &home);
    let turn = [
        json!({"type": "started"}),
        json!({"type": "error", "message": "slow down"}),
        json!({"type": "stopped"}),
        json!({"type": "done", "ok": false}),
    ];
    home.write(&below("jsonl"), &jsonl(&turn), NEW);
    let facts = [
        STARTED,
        TranscriptEvent::TurnError {
            rate_limited: false,
        },
        TranscriptEvent::Interrupted,
        finished(false),
    ];
    assert_eq!(reader.read().unwrap(), facts);
}

/// DEC-180 Task 16: a resumed manifest record reads only what is added after it, names
/// its conversation, and a record elsewhere than its path finds is refused.
#[test]
fn a_resumed_manifest_record_reads_only_the_turns_added_after_it() {
    let home = Home::new();
    let one = [json!({"type": "started"}), json!({"type": "done"})];
    let record = home.write(&below("jsonl"), &jsonl(&one), NEW);
    let cwd = home.path();
    let resumed = TranscriptReader::resumed(source(APPEND, &home), &cwd, &record, None);
    assert!(resumed.is_ok());
    let mut resumed = resumed.unwrap();
    assert_eq!(resumed.start(), jsonl(&one).len() as u64);
    assert!(resumed.read().unwrap().is_empty());
    let two = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "b"}),
    ];
    appended(&record, &two);
    let again = [STARTED, text("b")];
    assert_eq!(resumed.read().unwrap(), again);
    let named = Some((SESSION_ID.to_owned(), record));
    assert_eq!(resumed.conversation(), named);
    let elsewhere = home.write("standin/records/other.jsonl", &jsonl(&one), NEW);
    let refused = TranscriptReader::resumed(source(APPEND, &home), &cwd, &elsewhere, None);
    assert_eq!(refused.err(), Some(failure("launch_resume_unavailable")));
}

/// DEC-180 Task 16: an adopted manifest record skipped to its end reports nothing
/// written before, and the model's work after starts the turn it could not see begin.
#[test]
fn a_manifest_record_skipped_to_its_end_restarts_a_turn_with_the_model_s_work() {
    let home = Home::new();
    let since = SystemTime::now();
    let mut reader = TranscriptReader::adopted(source(APPEND, &home), &home.path(), since);
    let one = [
        json!({"type": "started"}),
        json!({"type": "text", "delta": "old"}),
    ];
    let record = home.write(&below("jsonl"), &jsonl(&one), NEW);
    reader.skip_to_end().unwrap();
    assert!(reader.read().unwrap().is_empty());
    let more = [
        json!({"type": "text", "delta": "new"}),
        json!({"type": "done"}),
    ];
    appended(&record, &more);
    let facts = [STARTED, text("new"), finished(true)];
    assert_eq!(reader.read().unwrap(), facts);
}
