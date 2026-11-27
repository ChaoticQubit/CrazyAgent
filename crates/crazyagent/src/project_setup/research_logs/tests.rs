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

/// A separator that only starts like an END line, met before any marker, hides nothing above
/// it and is shown itself, since it is not the END line of a key block.
#[test]
fn a_separator_starting_like_an_end_line_keeps_the_lines_above_it() {
    let text =
        "Searching ocean sensor pricing\nRead buoywatch.io/pricing\n-----End of output-----\nafter";
    assert_eq!(redact(text), text);
}

/// A key block's END line met before any BEGIN still hides every line above it, also after
/// a separator that only starts like an END line and in the joined form a JSON key file
/// holds, so the body above an END is never shown.
#[test]
fn a_key_end_after_a_separator_still_hides_the_lines_above_it() {
    let shown = redact(&format!(
        "-----End of output-----\n{BODY_NO_DIGIT}\n{END}\nafter"
    ));
    assert_eq!(
        shown,
        [HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE, "after"].join("\n")
    );
    let rsa = redact(&format!(
        "{BODY_NO_DIGIT}\n{BODY_SHORT}\n-----END RSA PRIVATE KEY-----\nafter"
    ));
    assert_eq!(
        rsa,
        [HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE, "after"].join("\n")
    );
    let joined = redact(&format!(
        "{BODY_NO_DIGIT}\nBg/kq+hk/iG9w\\n-----END PRIVATE KEY-----\\n\"}}\nafter"
    ));
    assert_eq!(joined, [HIDDEN_LINE, HIDDEN_LINE, "after"].join("\n"));
}

/// A key block's END line whose label holds a hyphen, as RFC 7468 labels may, met before any
/// BEGIN still hides every line above it.
#[test]
fn an_end_with_a_hyphenated_label_hides_the_lines_above_it() {
    let shown = redact(&format!(
        "{BODY_NO_DIGIT}\n{BODY_SHORT}\n-----END RSA-PSS PRIVATE KEY-----\nafter"
    ));
    assert_eq!(
        shown,
        [HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE, "after"].join("\n")
    );
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

/// Plain research rows no word check hides, each ending in a line break. Input: how many.
/// Output: the rows.
fn plain_rows(count: usize) -> String {
    (0..count)
        .map(|at| format!("read shop page {at} and noted the prices\n"))
        .collect()
}

/// Key body rows of the shapes the word check misses: `rows` full rows, then one short row of
/// `last` bytes, each ending in a line break. Inputs: the two counts. Output: the rows.
fn body_rows(rows: usize, last: usize) -> String {
    assert!((1..BODY_NO_DIGIT.len()).contains(&last));
    let full = format!("{BODY_NO_DIGIT}\n").repeat(rows);
    format!("{full}{}\n", &BODY_NO_DIGIT[..last])
}

/// Where the tail window of a log starts. Input: the log's length in bytes, past the window.
/// Output: the window's first byte.
fn window_start(length: usize) -> usize {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    assert!(length > tail);
    length - tail
}

/// A key block whose BEGIN row lies in the 64 KiB before the tail window and whose body runs
/// through the window to the log's end with no END hides every row the window shows.
#[test]
fn a_block_opened_before_the_tail_window_and_never_ended_hides_its_body_rows() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let rows = tail * 3 / 2 / (BODY_NO_DIGIT.len() + 1);
    let log = format!("starting\n{BEGIN}\n{}", body_rows(rows, 40));
    let start = window_start(log.len());
    assert!(log.find(BEGIN).unwrap() + BEGIN.len() < start && start < tail);
    assert!(!log.contains(END));
    let (_root, lines) = reply_of(&log);
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| line == HIDDEN_LINE), "{lines:?}");
}

/// A key block whose BEGIN row the tail window's start cuts, so the window drops it, and
/// whose body runs to the log's end with no END hides every row the window shows.
#[test]
fn a_block_whose_begin_row_the_tail_window_cuts_hides_its_body_rows() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let row = BODY_NO_DIGIT.len() + 1;
    let body = tail + 5 - (BEGIN.len() + 1);
    let log = format!(
        "starting\n{BEGIN}\n{}",
        body_rows((body - 1) / row, (body - 1) % row)
    );
    assert_eq!(window_start(log.len()), log.find(BEGIN).unwrap() + 5);
    let (_root, lines) = reply_of(&log);
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| line == HIDDEN_LINE), "{lines:?}");
}

/// A key block whose whole BEGIN row starts right after a line break, at the first byte read
/// before the tail window, and whose body runs to the log's end with no END hides every row
/// the window shows.
#[test]
fn a_block_whose_begin_row_starts_at_the_first_byte_read_hides_its_body_rows() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let row = BODY_NO_DIGIT.len() + 1;
    let head = "starting\n";
    let body = 2 * tail - (BEGIN.len() + 1);
    let log = format!(
        "{head}{BEGIN}\n{}",
        body_rows((body - 1) / row, (body - 1) % row)
    );
    assert_eq!(log.len(), head.len() + 2 * tail);
    assert_eq!(window_start(log.len()) - tail, log.find(BEGIN).unwrap());
    assert_eq!(log.as_bytes()[head.len() - 1], b'\n');
    assert!(!log.contains(END));
    let (_root, lines) = reply_of(&log);
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| line == HIDDEN_LINE), "{lines:?}");
}

/// A key block that ends before the tail window leaves the window's plain rows shown, though
/// the 64 KiB before the window holds the whole block.
#[test]
fn a_block_ended_before_the_tail_window_leaves_its_rows_shown() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let rows = plain_rows(tail * 3 / 2 / 40);
    let log = format!("starting\n{BEGIN}\n{BODY_SHORT}\n{END}\n{rows}");
    let start = window_start(log.len());
    assert!(log.find(END).unwrap() + END.len() < start && start < tail);
    let (_root, lines) = reply_of(&log);
    assert!(!lines.iter().any(|line| line == HIDDEN_LINE), "{lines:?}");
    assert_eq!(lines.last().map(String::as_str), rows.lines().next_back());
}

/// A separator that only starts like a BEGIN line, before the tail window and with no END
/// after it, leaves the window's rows shown, since it opens no key block.
#[test]
fn a_separator_starting_like_a_begin_line_before_the_tail_window_hides_nothing() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let rows = plain_rows(tail * 3 / 2 / 40);
    let log = format!("starting\n-----Begin output-----\n{rows}");
    let start = window_start(log.len());
    assert!(log.find("-----Begin").unwrap() < start && start < tail);
    let (_root, lines) = reply_of(&log);
    assert!(!lines.iter().any(|line| line == HIDDEN_LINE), "{lines:?}");
    assert_eq!(lines.last().map(String::as_str), rows.lines().next_back());
}

const OPENVPN_STATIC_END: &str = "-----END OpenVPN Static key V1-----";
const TLS_CRYPT_CLIENT_BEGIN: &str = "-----BEGIN OpenVPN tls-crypt-v2 client key-----";
const TLS_CRYPT_CLIENT_END: &str = "-----END OpenVPN tls-crypt-v2 client key-----";
const TLS_CRYPT_SERVER_END: &str = "-----END OpenVPN tls-crypt-v2 server key-----";
const CHUNK_SEPARATOR: &str = "-----End of chunk-----";

/// An OpenVPN static key's END line, whose label mixes cases, met before any BEGIN hides
/// every line above it and itself, as a PEM key's END does.
#[test]
fn an_openvpn_static_key_end_with_no_begin_hides_every_line_before_it() {
    let shown = redact(&format!(
        "{BODY_SHORT}\n{BODY_NO_DIGIT}\n{OPENVPN_STATIC_END}\nafter"
    ));
    assert_eq!(
        shown,
        [HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE, "after"].join("\n")
    );
}

/// An OpenVPN tls-crypt-v2 server key's END line met before any BEGIN hides every line
/// above it and itself.
#[test]
fn a_tls_crypt_v2_server_key_end_with_no_begin_hides_every_line_before_it() {
    let shown = redact(&format!(
        "{BODY_NO_DIGIT}\n{BODY_SHORT}\n{TLS_CRYPT_SERVER_END}\nafter"
    ));
    assert_eq!(
        shown,
        [HIDDEN_LINE, HIDDEN_LINE, HIDDEN_LINE, "after"].join("\n")
    );
}

/// An OpenVPN tls-crypt-v2 client key whose BEGIN row lies in the 64 KiB before the tail
/// window and whose END row lies inside it hides every body row the window shows.
#[test]
fn a_tls_crypt_v2_client_key_opened_before_the_tail_window_hides_its_body_rows() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let rows = tail * 3 / 2 / (BODY_NO_DIGIT.len() + 1);
    let log = format!(
        "starting\n{TLS_CRYPT_CLIENT_BEGIN}\n{}{TLS_CRYPT_CLIENT_END}\nafter\n",
        body_rows(rows, 40)
    );
    let start = window_start(log.len());
    assert!(log.find(TLS_CRYPT_CLIENT_BEGIN).unwrap() + TLS_CRYPT_CLIENT_BEGIN.len() < start);
    assert!(log.find(TLS_CRYPT_CLIENT_END).unwrap() > start && start < tail);
    let (_root, lines) = reply_of(&log);
    assert_no_body(&lines, &[BODY_NO_DIGIT, &BODY_NO_DIGIT[..40]]);
    assert_eq!(lines.last().map(String::as_str), Some("after"));
    let body = &lines[..lines.len() - 1];
    assert!(!body.is_empty());
    assert!(body.iter().all(|line| line == HIDDEN_LINE), "{lines:?}");
}

/// A separator that only starts like an END line inside an open PEM key block leaves the
/// block open, so every row through the block's real END is hidden.
#[test]
fn a_separator_inside_a_key_block_leaves_it_open_to_its_end() {
    let shown = redact(&format!(
        "-----BEGIN RSA PRIVATE KEY-----\n{BODY_NO_DIGIT}\n{CHUNK_SEPARATOR}\n{BODY_SHORT}\n{BODY_NO_DIGIT}\n-----END RSA PRIVATE KEY-----\nafter"
    ));
    let hidden = [HIDDEN_LINE; 6];
    assert_eq!(shown, [&hidden[..], &["after"]].concat().join("\n"));
}

/// A PEM key block opened before the tail window, with a separator that only starts like an
/// END line also before the window, still hides every body row the window shows.
#[test]
fn a_separator_before_the_tail_window_leaves_an_opened_block_open() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let rows = tail * 3 / 2 / (BODY_NO_DIGIT.len() + 1);
    let log = format!(
        "starting\n{BEGIN}\n{BODY_NO_DIGIT}\n{CHUNK_SEPARATOR}\n{}",
        body_rows(rows, 40)
    );
    let start = window_start(log.len());
    assert!(log.find(CHUNK_SEPARATOR).unwrap() + CHUNK_SEPARATOR.len() < start && start < tail);
    assert!(!log.contains(END));
    let (_root, lines) = reply_of(&log);
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| line == HIDDEN_LINE), "{lines:?}");
}

/// A chunk separator that only starts like an END line, met before any BEGIN, hides nothing
/// above it and is shown itself.
#[test]
fn a_chunk_separator_before_any_begin_keeps_the_lines_above_it() {
    let text = format!("Read buoywatch.io/pricing\n{CHUNK_SEPARATOR}\nafter");
    assert_eq!(redact(&text), text);
}

/// A separator whose label holds `key` only inside a longer word, before the tail window and
/// with no END after it, opens no key block, so the window's rows stay shown.
#[test]
fn a_begin_label_holding_key_inside_a_word_before_the_tail_window_hides_nothing() {
    let tail = usize::try_from(RESEARCH_LOG_TAIL_BYTES).unwrap();
    let rows = plain_rows(tail * 3 / 2 / 40);
    let log = format!("starting\n-----BEGIN monkey notes-----\n{rows}");
    let start = window_start(log.len());
    assert!(log.find("-----BEGIN monkey").unwrap() < start && start < tail);
    let (_root, lines) = reply_of(&log);
    assert!(!lines.iter().any(|line| line == HIDDEN_LINE), "{lines:?}");
    assert_eq!(lines.last().map(String::as_str), rows.lines().next_back());
}
