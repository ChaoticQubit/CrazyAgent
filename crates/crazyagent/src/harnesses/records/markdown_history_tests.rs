use super::*;
use crate::harnesses::transcript::EVENTS_PER_READ_MAX;
use std::io::Write;
use std::path::Path;

fn append(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

/// A reader of the history file at `path`, held below the folder that holds it. Inputs: the
/// file, the offset and the time now. Output: the reader.
fn history(path: PathBuf, offset: u64, now: Instant) -> MarkdownHistory {
    let base = path.parent().unwrap().to_owned();
    let below = PathBuf::from(path.file_name().unwrap());
    MarkdownHistory::new(base, below, offset, now)
}

fn text(line: &str) -> TranscriptEvent {
    TranscriptEvent::Text { text: line.into() }
}

fn finished() -> TranscriptEvent {
    TranscriptEvent::TurnFinished { success: true }
}

/// Plan Task 15: each user heading starts a turn, the plain lines after it are the reply,
/// and the next heading ends the turn before it; a turn still open ends once the file has
/// stayed the same for the quiet time after reply text, and only once.
#[test]
fn a_history_file_gives_a_turn_for_each_user_heading() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(".aider.chat.history.md");
    let start = Instant::now();
    append(
        &path,
        "\n# aider chat started at 2026-11-15 18:00:00\n\n#### hello\n\nHi there.\nSecond line.\n\n#### next\n",
    );
    let mut reader = history(path.clone(), 0, start);

    let first = reader.read(start).unwrap();
    let thinking = reader.read(start + QUIET * 4).unwrap();
    append(&path, "Done.\n");
    let reply = reader.read(start + QUIET * 5).unwrap();
    let early = reader
        .read(start + QUIET * 6 - Duration::from_millis(1))
        .unwrap();
    let quiet = reader.read(start + QUIET * 6).unwrap();
    let after = reader.read(start + QUIET * 9).unwrap();

    assert_eq!(
        first,
        vec![
            TranscriptEvent::TurnStarted,
            text("Hi there."),
            text("Second line."),
            finished(),
            TranscriptEvent::TurnStarted,
        ]
    );
    assert_eq!(thinking, Vec::new());
    assert_eq!(reply, vec![text("Done.")]);
    assert_eq!(early, Vec::new());
    assert_eq!(quiet, vec![finished()]);
    assert_eq!(after, Vec::new());
    assert_eq!(reader.offset(), std::fs::metadata(&path).unwrap().len());
}

/// Plan Task 15: quoted lines are tool output, not reply text, so a turn that has only
/// tool output stays open through a quiet file.
#[test]
fn quoted_tool_output_is_not_reply_text() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.md");
    let start = Instant::now();
    append(&path, "#### run it\n> $ ls\n> a.txt\n");
    let mut reader = history(path.clone(), 0, start);

    let tools = reader.read(start).unwrap();
    let quiet = reader.read(start + QUIET * 3).unwrap();
    append(&path, "Listed.\n");
    let reply = reader.read(start + QUIET * 4).unwrap();

    assert_eq!(tools, vec![TranscriptEvent::TurnStarted]);
    assert_eq!(quiet, Vec::new());
    assert_eq!(reply, vec![text("Listed.")]);
}

/// Plan Task 15: a line the harness has not finished writing is held back, and the offset
/// stays at its start so a resumed reader reads it whole.
#[test]
fn a_half_written_line_waits_for_its_end() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.md");
    let start = Instant::now();
    append(&path, "#### hi\nHal");
    let mut reader = history(path.clone(), 0, start);

    let half = reader.read(start).unwrap();
    let held = reader.offset();
    append(&path, "f done.\n");
    let whole = reader.read(start + Duration::from_millis(10)).unwrap();

    assert_eq!(half, vec![TranscriptEvent::TurnStarted]);
    assert_eq!(held, u64::try_from("#### hi\n".len()).unwrap());
    assert_eq!(whole, vec![text("Half done.")]);
}

/// Plan Task 15: a prompt typed over several heading lines, with no reply between them, is
/// one turn.
#[test]
fn a_prompt_over_several_heading_lines_is_one_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.md");
    let start = Instant::now();
    append(&path, "#### line one\n#### line two\n\nOk.\n");

    let events = history(path, 0, start).read(start).unwrap();

    assert_eq!(events, vec![TranscriptEvent::TurnStarted, text("Ok.")]);
}

/// Plan Task 15: a session header while a turn is open means the harness started again, so
/// that turn was interrupted; a header with no turn open gives nothing.
#[test]
fn a_new_session_header_interrupts_an_open_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.md");
    let start = Instant::now();
    append(
        &path,
        "# aider chat started at 2026-11-15 17:00:00\n#### hi\nWorking\n\n# aider chat started at 2026-11-15 18:00:00\n\n#### again\n",
    );

    let events = history(path, 0, start).read(start).unwrap();

    assert_eq!(
        events,
        vec![
            TranscriptEvent::TurnStarted,
            text("Working"),
            TranscriptEvent::Interrupted,
            TranscriptEvent::TurnStarted,
        ]
    );
}

/// Plan Task 15: a link or a folder at the history path is refused, and a path with nothing
/// there yet reads as no facts.
#[cfg(unix)]
#[test]
fn only_a_plain_history_file_is_read() {
    let tmp = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let real = tmp.path().join("real.md");
    append(&real, "#### hi\n");
    let link = tmp.path().join("link.md");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let folder = tmp.path().join("folder.md");
    std::fs::create_dir(&folder).unwrap();

    let read = |path: PathBuf| history(path, 0, start).read(start);

    assert_eq!(read(link).unwrap_err().0, UNSAFE);
    assert_eq!(read(folder).unwrap_err().0, UNSAFE);
    assert_eq!(read(tmp.path().join("missing.md")).unwrap(), Vec::new());
    assert_eq!(read(real).unwrap(), vec![TranscriptEvent::TurnStarted]);
}

/// Plan Task 15: a line longer than the line bound is passed over whole, whether it comes
/// in one read or across two, and the lines after it are read.
#[test]
fn a_line_past_the_line_bound_is_passed_over() {
    let tmp = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let long = "x".repeat(LINE_BYTES_MAX + 1);
    let whole = tmp.path().join("whole.md");
    append(&whole, &format!("#### hi\n{long}\nShort.\n"));
    let split = tmp.path().join("split.md");
    append(&split, &format!("#### hi\n{long}"));
    let mut reader = history(split.clone(), 0, start);

    let one = history(whole, 0, start).read(start).unwrap();
    let first = reader.read(start).unwrap();
    append(&split, "tail\nShort.\n");
    let second = reader.read(start + Duration::from_millis(10)).unwrap();

    assert_eq!(one, vec![TranscriptEvent::TurnStarted, text("Short.")]);
    assert_eq!(first, vec![TranscriptEvent::TurnStarted]);
    assert_eq!(second, vec![text("Short.")]);
}

/// Plan Task 15: one read takes at most its byte bound from the file, and the reads after it
/// go on from there until every line is read once.
#[test]
fn a_read_keeps_to_its_byte_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.md");
    let start = Instant::now();
    let line = format!("{}\n", "y".repeat(1023));
    append(&path, &format!("#### hi\n{}", line.repeat(2048)));
    let mut reader = history(path.clone(), 0, start);

    let first = reader.read(start).unwrap();
    let mut texts = first.len() - 1;
    assert!(reader.offset() <= u64::try_from(READ_BYTES_MAX).unwrap());
    assert!(texts < 2048);
    for _ in 0..4 {
        texts += reader.read(start).unwrap().len();
    }

    assert_eq!(first[0], TranscriptEvent::TurnStarted);
    assert_eq!(texts, 2048);
    assert_eq!(reader.offset(), std::fs::metadata(&path).unwrap().len());
}

/// Plan Task 15: a reader resumed at an offset reads only the lines from there.
#[test]
fn a_resumed_reader_starts_at_its_offset() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.md");
    let start = Instant::now();
    let earlier = "#### one\nA.\n";
    append(&path, &format!("{earlier}#### two\nB.\n"));
    let offset = u64::try_from(earlier.len()).unwrap();

    let events = history(path, offset, start).read(start).unwrap();

    assert_eq!(events, vec![TranscriptEvent::TurnStarted, text("B.")]);
}

/// Review t15-sec F4: the history file is opened one part at a time below the folder the
/// engine holds, so a folder on the way that was swapped for a link is refused, not followed.
#[cfg(unix)]
#[test]
fn a_linked_folder_below_the_held_folder_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let project = tmp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let other = tmp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    append(&other.join("history.md"), "#### hi\nsecret\n");
    std::os::unix::fs::symlink(&other, project.join("sub")).unwrap();
    std::fs::create_dir(project.join("real")).unwrap();
    append(&project.join("real").join("history.md"), "#### hi\n");

    let read = |below: &str| {
        MarkdownHistory::new(project.clone(), PathBuf::from(below), 0, start)
            .read(start)
            .map_err(|error| error.0)
    };

    assert_eq!(read("sub/history.md"), Err(UNSAFE));
    assert_eq!(
        read("real/history.md"),
        Ok(vec![TranscriptEvent::TurnStarted])
    );
    assert_eq!(read("missing/history.md"), Ok(Vec::new()));
}

/// Review t15-sec F6: one read gives at most `EVENTS_PER_READ_MAX` facts and stops at a whole
/// line, and the reads after it go on from there until every line is read once.
#[test]
fn a_read_keeps_to_its_fact_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("history.md");
    let start = Instant::now();
    append(&path, &format!("#### hi\n{}", "x\n".repeat(10_000)));
    let mut reader = history(path.clone(), 0, start);

    let first = reader.read(start).unwrap();
    let held = reader.offset();
    let mut texts = first.len() - 1;
    for _ in 0..4 {
        texts += reader.read(start).unwrap().len();
    }

    assert!(first.len() <= EVENTS_PER_READ_MAX, "{}", first.len());
    assert_eq!(first[0], TranscriptEvent::TurnStarted);
    assert_eq!((held - 8) % 2, 0);
    assert_eq!(texts, 10_000);
    assert_eq!(reader.offset(), std::fs::metadata(&path).unwrap().len());
}
