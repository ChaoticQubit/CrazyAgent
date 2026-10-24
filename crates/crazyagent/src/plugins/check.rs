use super::{TOOL_NAME_BYTES_MAX, TOOLS_MAX};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

pub const CHECK_TIME: Duration = Duration::from_secs(10);
pub const LINE_BYTES_MAX: usize = 1024 * 1024;
pub const OUTPUT_BYTES_MAX: usize = 4 * 1024 * 1024;
const PAGES_MAX: u64 = 8;
const ENV_KEPT: [&str; 6] = ["PATH", "HOME", "USER", "LOGNAME", "LANG", "TMPDIR"];

type Line = Result<Vec<u8>, &'static str>;

/// Starts a command MCP server, runs `initialize` and `tools/list`, then stops its whole
/// process group. Inputs: the absolute command, its arguments, secret environment values
/// and the time bound. Output: the tool names, or a plain rejection code.
#[cfg(unix)]
pub fn check_stdio(
    command: &str,
    args: &[String],
    secrets: &[(String, Vec<u8>)],
    limit: Duration,
) -> Result<Vec<String>, &'static str> {
    let mut process = Command::new(command);
    process
        .args(args)
        .env_clear()
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    for name in ENV_KEPT {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
    for (name, value) in secrets {
        process.env(name, std::ffi::OsStr::from_bytes(value));
    }
    let mut child = process.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "server_command_not_found",
        std::io::ErrorKind::PermissionDenied => "server_command_not_runnable",
        _ => "server_cant_start",
    })?;
    let pid = child.id();
    let (stdin, stdout) = match (child.stdin.take(), child.stdout.take()) {
        (Some(stdin), Some(stdout)) => (stdin, stdout),
        _ => {
            stop(pid);
            let _ = child.wait();
            return Err("server_cant_start");
        }
    };
    let (send, receive) = sync_channel::<Line>(16);
    let reader = std::thread::Builder::new()
        .name("crazyagent-mcp-check".into())
        .spawn(move || read_lines(stdout, send));
    let result = match reader {
        Ok(_) => talk(stdin, &receive, Instant::now() + limit),
        Err(_) => Err("server_cant_start"),
    };
    stop(pid);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Refuses to start a command MCP server where its whole process group, which holds the
/// secret environment values, cannot be stopped. Takes the command, arguments, secrets
/// and time bound, all unused. Returns `server_check_unavailable`.
#[cfg(not(unix))]
pub fn check_stdio(
    _command: &str,
    _args: &[String],
    _secrets: &[(String, Vec<u8>)],
    _limit: Duration,
) -> Result<Vec<String>, &'static str> {
    Err("server_check_unavailable")
}

#[cfg(unix)]
fn stop(pid: u32) {
    if let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        assert!(pid.as_raw_nonzero().get() > 1);
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

fn read_lines(stdout: ChildStdout, send: SyncSender<Line>) {
    let mut reader = BufReader::new(stdout);
    let mut total = 0usize;
    loop {
        let mut line = Vec::new();
        match (&mut reader)
            .take(LINE_BYTES_MAX as u64 + 1)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => {
                let _ = send.send(Err("server_exited"));
                return;
            }
            Ok(read) => {
                total += read;
                if line.len() > LINE_BYTES_MAX || total > OUTPUT_BYTES_MAX {
                    let _ = send.send(Err("server_output_too_large"));
                    return;
                }
                if send.send(Ok(line)).is_err() {
                    return;
                }
            }
        }
    }
}

fn send(stdin: &mut ChildStdin, message: &Value) -> Result<(), &'static str> {
    let mut line = serde_json::to_vec(message).map_err(|_| "server_cant_start")?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .and_then(|()| stdin.flush())
        .map_err(|_| "server_exited")
}

fn answer(receive: &Receiver<Line>, id: u64, deadline: Instant) -> Result<Value, &'static str> {
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or("server_check_timeout")?;
        let line = match receive.recv_timeout(left) {
            Ok(line) => line?,
            Err(RecvTimeoutError::Timeout) => return Err("server_check_timeout"),
            Err(RecvTimeoutError::Disconnected) => return Err("server_exited"),
        };
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if message.get("method").is_some() || message.get("id").and_then(Value::as_u64) != Some(id)
        {
            continue;
        }
        if message.get("error").is_some() {
            return Err("server_refused");
        }
        return message
            .get("result")
            .filter(|result| result.is_object())
            .cloned()
            .ok_or("server_reply_invalid");
    }
}

fn talk(
    mut stdin: ChildStdin,
    receive: &Receiver<Line>,
    deadline: Instant,
) -> Result<Vec<String>, &'static str> {
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "crazyagent", "version": env!("CARGO_PKG_VERSION")},
        }}),
    )?;
    answer(receive, 1, deadline)?;
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for id in 2..2 + PAGES_MAX {
        let params = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        };
        send(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": params}),
        )?;
        let result = answer(receive, id, deadline)?;
        let listed = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or("server_reply_invalid")?;
        for tool in listed {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= TOOL_NAME_BYTES_MAX
                        && !name.chars().any(char::is_control)
                })
                .ok_or("server_reply_invalid")?;
            if tools.len() == TOOLS_MAX {
                return Err("server_reply_invalid");
            }
            tools.push(name.to_owned());
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert!(tools.len() <= TOOLS_MAX);
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    const FIXTURE: &str = include_str!("../../tests/fixtures/mcp_fixture.py");

    fn fixture(root: &Path) -> PathBuf {
        let path = root.join("mcp_fixture.py");
        std::fs::write(&path, FIXTURE).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn alive(pid: i32) -> bool {
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
    }

    #[test]
    fn a_real_stdio_server_lists_its_tools_across_pages_with_its_secret() {
        let root = tempfile::tempdir().unwrap();
        let server = fixture(root.path());
        let command = server.to_str().unwrap();
        assert_eq!(
            check_stdio(command, &[], &[], Duration::from_secs(10)).unwrap(),
            vec!["echo", "anonymous"]
        );
        let secret = vec![("FIXTURE_TOKEN".to_owned(), b"dummy-value".to_vec())];
        assert_eq!(
            check_stdio(
                command,
                &["normal".into()],
                &secret,
                Duration::from_secs(10)
            )
            .unwrap(),
            vec!["echo", "authorized"]
        );
    }

    #[test]
    fn servers_that_cannot_start_answer_with_a_plain_reason() {
        let root = tempfile::tempdir().unwrap();
        let server = fixture(root.path());
        let command = server.to_str().unwrap();
        assert_eq!(
            check_stdio("/nonexistent/mcp", &[], &[], Duration::from_secs(5)).unwrap_err(),
            "server_command_not_found"
        );
        let plain = root.path().join("plain.txt");
        std::fs::write(&plain, "x").unwrap();
        assert_eq!(
            check_stdio(plain.to_str().unwrap(), &[], &[], Duration::from_secs(5)).unwrap_err(),
            "server_command_not_runnable"
        );
        assert_eq!(
            check_stdio(command, &["exit".into()], &[], Duration::from_secs(5)).unwrap_err(),
            "server_exited"
        );
    }

    #[test]
    fn a_silent_server_times_out_and_its_whole_process_group_stops() {
        let root = tempfile::tempdir().unwrap();
        let server = fixture(root.path());
        let pidfile = root.path().join("child.pid");
        let started = Instant::now();
        assert_eq!(
            check_stdio(
                server.to_str().unwrap(),
                &["hang".into(), pidfile.display().to_string()],
                &[],
                Duration::from_millis(1500),
            )
            .unwrap_err(),
            "server_check_timeout"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone = Instant::now() + Duration::from_secs(3);
        while alive(pid) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the server's child process must stop too");
    }
}
