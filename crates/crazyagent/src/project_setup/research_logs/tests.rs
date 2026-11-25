use super::*;

const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";
const BODY_NO_DIGIT: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz+/AbCdEfGhIjKlMnOpQrStUvWxYz+/AbCdEfGh";
const BODY_SHORT: &str = "Zm9vYmFy==";

const SCANNED_BEFORE: usize = 4096;
const UNRELATED_AFTER: usize = 4096;

/// Makes a fresh runtime folder with an empty research agents folder. Output: the folder,
/// removed when dropped.
fn runtime() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("research-log-")
        .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
        .unwrap();
    std::fs::create_dir_all(root.path().join("research/agents")).unwrap();
    root
}

/// Writes one run folder's stderr log. Inputs: the runtime folder, the run folder name and
/// the log text. Output: none.
fn write_run(root: &tempfile::TempDir, name: &str, log: &str) {
    let run = root.path().join("research/agents").join(name);
    std::fs::create_dir(&run).unwrap();
    std::fs::write(run.join("stderr.log"), log).unwrap();
}

/// Reads a job's log the way the owner does. Inputs: the runtime folder and the job id.
/// Output: the lines the reply holds.
fn read_job(root: &tempfile::TempDir, job: i64) -> Vec<String> {
    research_log(&root.path().join("engine.db"), job).unwrap()
}

/// Writes one research job's stderr log under a fresh runtime folder and reads it back the
/// way the owner does. Input: the log text. Output: the folder, kept alive for the read, and
/// the lines the reply holds.
fn reply_of(log: &str) -> (tempfile::TempDir, Vec<String>) {
    let root = runtime();
    write_run(&root, "job-1-1", log);
    let lines = read_job(&root, 1);
    (root, lines)
}

/// Asserts no reply line shows any of the given body rows. Inputs: the reply lines and the
/// rows that must stay hidden. Output: none.
fn assert_no_body(lines: &[String], rows: &[&str]) {
    for line in lines {
        for row in rows {
            assert!(!line.contains(row), "body row shown: {line}");
        }
    }
}

#[test]
fn the_shapes_a_word_check_misses_are_shown_outside_a_block() {
    let (_root, lines) = reply_of(&format!("{BODY_NO_DIGIT}\n{BODY_SHORT}\n"));
    assert_eq!(lines, [BODY_NO_DIGIT, BODY_SHORT]);
}

#[test]
fn every_body_row_of_a_key_block_is_hidden_and_the_row_after_end_is_shown() {
    let log =
        format!("starting\n{BEGIN}\n{BODY_NO_DIGIT}\n{BODY_NO_DIGIT}\n{BODY_SHORT}\n{END}\ndone\n");
    let (_root, lines) = reply_of(&log);
    assert_no_body(&lines, &[BODY_NO_DIGIT, BODY_SHORT]);
    assert_eq!(lines.len(), 7);
    assert_eq!(lines.first().map(String::as_str), Some("starting"));
    assert_eq!(lines.last().map(String::as_str), Some("done"));
    assert!(
        lines[1..6].iter().all(|line| line == HIDDEN_LINE),
        "{lines:?}"
    );
}

#[test]
fn a_block_with_no_end_hides_every_row_after_begin() {
    let log = format!("before\n{BEGIN}\n{BODY_NO_DIGIT}\n{BODY_SHORT}\n");
    let (_root, lines) = reply_of(&log);
    assert_no_body(&lines, &[BODY_NO_DIGIT, BODY_SHORT]);
    assert_eq!(lines, ["before", HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE]);
}

#[test]
fn a_tail_window_that_starts_inside_a_block_hides_its_body_rows() {
    let rows = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap() / BODY_NO_DIGIT.len() + 64;
    let body = vec![BODY_NO_DIGIT; rows].join("\n");
    let log = format!("{BEGIN}\n{body}\n{BODY_SHORT}\n{END}\nafter\n");
    assert!(log.find(END).unwrap() > usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap());
    let (_root, lines) = reply_of(&log);
    assert!(
        !lines.iter().any(|line| line.contains(BEGIN)),
        "BEGIN lies before the window"
    );
    assert_no_body(&lines, &[BODY_NO_DIGIT, BODY_SHORT]);
    assert_eq!(lines.last().map(String::as_str), Some("after"));
}

#[test]
fn the_wrapped_tail_of_a_token_on_the_next_row_is_hidden() {
    let (_root, lines) =
        reply_of("config ghp_0123456789abcdefABCD\nEF0123456789abcd\nquokkafern done\n");
    assert_no_body(&lines, &["EF0123456789abcd", "ghp_0123456789abcdefABCD"]);
    assert_eq!(lines.last().map(String::as_str), Some("quokkafern done"));
}

#[test]
fn an_end_with_no_begin_hides_every_line_before_it() {
    let shown = redact(&format!("{BODY_NO_DIGIT}\n{BODY_SHORT}\n{END}\nafter"));
    assert_eq!(
        shown,
        [HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE, "after"].join("\n")
    );
}

#[test]
fn only_an_end_before_any_marker_hides_the_lines_before_it() {
    let shown = redact(&format!(
        "{BEGIN}\n{BODY_SHORT}\n{END}\n{BODY_NO_DIGIT}\n{END}\nafter"
    ));
    let hidden = [HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE];
    assert_eq!(
        shown,
        [&hidden[..], &[BODY_NO_DIGIT, HIDDEN_LINE, "after"]]
            .concat()
            .join("\n")
    );
    let joined = format!("before\nkey {BEGIN}\\n{BODY_SHORT}\\n{END}\nafter");
    assert_eq!(redact(&joined), ["before", HIDDEN_LINE, "after"].join("\n"));
}

#[test]
fn a_run_listed_after_many_unrelated_entries_is_still_found() -> std::io::Result<()> {
    let root = runtime();
    let agents = root.path().join("research/agents");
    let unrelated = |from: usize, count: usize| -> std::io::Result<()> {
        for at in from..from + count {
            let path = agents.join(format!("job-{}-1", 1000 + at));
            std::fs::File::create(&path).map_err(|error| {
                std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            })?;
        }
        Ok(())
    };
    unrelated(0, SCANNED_BEFORE)?;
    write_run(&root, "job-42-1", "found it");
    unrelated(SCANNED_BEFORE, UNRELATED_AFTER)?;
    assert_eq!(read_job(&root, 42), ["found it"]);
    Ok(())
}

#[test]
fn the_newest_attempt_then_the_newest_launch_is_read() {
    let root = runtime();
    write_run(&root, "job-42-1-5", "first attempt, sixth launch");
    write_run(&root, "job-42-2", "second attempt, first launch");
    write_run(&root, "job-42-2-3", "second attempt, fourth launch");
    write_run(&root, "job-4-9", "another job");
    assert_eq!(read_job(&root, 42), ["second attempt, fourth launch"]);
    assert_eq!(read_job(&root, 7), Vec::<String>::new());
}
