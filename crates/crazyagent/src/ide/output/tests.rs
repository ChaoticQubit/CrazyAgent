use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::time::{Duration, Instant};

fn token() -> String {
    "3f".repeat(crate::ide::token::TOKEN_BYTES)
}

fn scratch() -> tempfile::TempDir {
    let folder = tempfile::Builder::new()
        .prefix("ca-ide-log-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

fn written(lines: Vec<Vec<u8>>) -> (io::PipeReader, std::thread::JoinHandle<()>) {
    let (reader, mut writer) = crate::spawn_gate::guarded(io::pipe).unwrap();
    let feeding = std::thread::spawn(move || {
        for line in lines {
            writer.write_all(&line).unwrap();
        }
    });
    (reader, feeding)
}

#[test]
fn a_redacted_line_never_holds_the_token_or_any_tkn_value() {
    let token = token();
    for (line, expected) in [
        (
            format!("Web UI available at http://127.0.0.1:47931/?tkn={token}\n"),
            format!("Web UI available at http://127.0.0.1:47931/?tkn={REDACTED}\n"),
        ),
        (format!("{token}{token}"), format!("{REDACTED}{REDACTED}")),
        (format!("x{token}y"), format!("x{REDACTED}y")),
        (
            "cookie vscode-tkn=0123abcd; Path=/".to_owned(),
            format!("cookie vscode-tkn={REDACTED}; Path=/"),
        ),
        (
            "/?a=1&tkn=old%2Dvalue_9&b=2".to_owned(),
            format!("/?a=1&tkn={REDACTED}&b=2"),
        ),
        ("tkn=".to_owned(), "tkn=".to_owned()),
        ("tkn= spaced".to_owned(), "tkn= spaced".to_owned()),
        (
            "Extension host agent started.".to_owned(),
            "Extension host agent started.".to_owned(),
        ),
    ] {
        let redacted = redact(&line, &token);
        assert_eq!(redacted, expected, "{line}");
        assert!(!redacted.contains(&token));
    }
}

#[test]
fn copied_output_is_logged_redacted_through_a_real_pipe() {
    let token = token();
    let mut long = vec![b'x'; LINE_BYTES_MAX - 10];
    long.extend_from_slice(token.as_bytes());
    long.extend_from_slice(b" tail\n");
    let lines = vec![
        format!("Web UI available at http://127.0.0.1:47931/?tkn={token}\n").into_bytes(),
        b"Extension host agent listening on 47931\n".to_vec(),
        long,
        b"after the cut\n".to_vec(),
        vec![0xff, 0xfe, b'\n'],
        format!("last without newline {token}").into_bytes(),
    ];
    let (reader, feeding) = written(lines);
    let folder = scratch();
    let path = folder.path().join("server.log");
    let log = fs::File::create(&path).unwrap();
    let kept = copy(reader, log, &token);
    feeding.join().unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(kept, text.len() as u64);
    assert!(!text.contains(&token), "{text}");
    assert!(!text.contains(&token[..32]));
    assert_eq!(
        text,
        format!(
            "Web UI available at http://127.0.0.1:47931/?tkn={REDACTED}\nExtension host agent listening on 47931\n{CUT}after the cut\n\u{fffd}\u{fffd}\nlast without newline {REDACTED}"
        )
    );
}

#[test]
fn output_past_the_cap_is_drained_without_blocking_the_writer() {
    let token = token();
    let line = format!("{}\n", "y".repeat(1023)).into_bytes();
    let count = (LOG_BYTES_MAX as usize) / line.len() + 1024;
    let mut lines = vec![line; count];
    lines.push(format!("late {token}\n").into_bytes());
    let (reader, feeding) = written(lines);
    let folder = scratch();
    let path = folder.path().join("server.log");
    let kept = copy(reader, fs::File::create(&path).unwrap(), &token);
    feeding.join().unwrap();
    assert_eq!(kept, LOG_BYTES_MAX);
    assert_eq!(fs::metadata(&path).unwrap().len(), LOG_BYTES_MAX);
    assert!(!fs::read_to_string(&path).unwrap().contains(&token));
}

#[test]
fn a_started_log_is_private_new_and_redacted() {
    let token = token();
    let folder = scratch();
    let path = folder.path().join("server.log");
    fs::write(&path, format!("previous launch {token}\n")).unwrap();
    let mut writer = start(&path, &token).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    writer
        .write_all(format!("Web UI available at http://127.0.0.1:1/?tkn={token}\n").as_bytes())
        .unwrap();
    drop(writer);
    let expected = format!("Web UI available at http://127.0.0.1:1/?tkn={REDACTED}\n");
    let deadline = Instant::now() + Duration::from_secs(10);
    while fs::read_to_string(&path).unwrap() != expected {
        assert!(
            Instant::now() < deadline,
            "{}",
            fs::read_to_string(&path).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let linked = folder.path().join("linked.log");
    std::os::unix::fs::symlink(folder.path().join("target.log"), &linked).unwrap();
    assert!(start(&linked, &token).is_err());
    assert!(!folder.path().join("target.log").exists());
}
