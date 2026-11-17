use super::{TOOL_NAME_BYTES_MAX, TOOLS_MAX};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{ChildStdin, ChildStdout};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::mpsc::{channel, sync_channel};
use std::time::Duration;
use std::time::Instant;

pub const CHECK_TIME: Duration = Duration::from_secs(10);
pub const LINE_BYTES_MAX: usize = 1024 * 1024;
pub const OUTPUT_BYTES_MAX: usize = 4 * 1024 * 1024;
const PAGES_MAX: u64 = 8;
#[cfg(unix)]
const ENV_KEPT: [&str; 6] = ["PATH", "HOME", "USER", "LOGNAME", "LANG", "TMPDIR"];
#[cfg(windows)]
pub(crate) const ENV_KEPT: [&str; 12] = [
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "USERPROFILE",
    "USERNAME",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
];
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

type Line = Result<Vec<u8>, &'static str>;

/// Starts a command MCP server as the engine's own user with no boundary, for trusted
/// commands such as the engine's own team tool, then checks it like [`check_command`].
/// Inputs: the absolute command, its arguments, secret environment values and the time
/// bound. Output: the tool names, or a plain rejection code.
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
        .current_dir(std::env::temp_dir());
    for name in ENV_KEPT {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
    for (name, value) in secrets {
        process.env(name, env_value(value)?);
    }
    check_command(process, limit, crate::sandbox::LineageCheck::Unneeded)
}

/// A secret environment value as Unix takes it: its bytes. Input: the bytes. Output: the
/// value, never refused here.
#[cfg(unix)]
pub(crate) fn env_value(value: &[u8]) -> Result<&OsStr, &'static str> {
    Ok(OsStr::from_bytes(value))
}

/// A secret environment value as Windows takes it: its text, as every stored secret is
/// already checked to be UTF-8. Input: the bytes. Output: the value, or
/// `server_secret_invalid` for bytes that are not UTF-8.
#[cfg(windows)]
pub(crate) fn env_value(value: &[u8]) -> Result<&OsStr, &'static str> {
    std::str::from_utf8(value)
        .map(OsStr::new)
        .map_err(|_| "server_secret_invalid")
}

/// The rejection code for a server command that did not start. Input: the start error.
/// Output: `server_command_not_found`, `server_command_not_runnable` or `server_cant_start`.
fn start_refused(error: std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "server_command_not_found",
        std::io::ErrorKind::PermissionDenied => "server_command_not_runnable",
        _ => "server_cant_start",
    }
}

/// Starts an already built MCP server command, such as one wrapped in the agent boundary,
/// runs `initialize` and `tools/list`, then stops its whole process group and, through the
/// warden a boundary command gets, every process left in its sandbox. Where `check` follows
/// its lineage, what it left outside the group is held and its code is the answer, in place
/// of the tool names (LINUX-KILL-GROUP r1 F1).
/// Inputs: the command with its arguments, environment and folder set, the time bound and
/// what the stop checks of the processes the server leaves outside its group. Output: the
/// tool names, or a plain rejection code, `server_cleanup_uncertain` when the warden did not
/// confirm the sandbox empty.
#[cfg(unix)]
pub fn check_command(
    mut process: Command,
    limit: Duration,
    check: crate::sandbox::LineageCheck,
) -> Result<Vec<String>, &'static str> {
    use crate::harnesses::leftovers::{
        self, Follower, HoldBy, LINEAGE_VARIABLE, Lineage, LineageToken, Unstopped,
    };
    use crate::sandbox::LineageCheck;
    let token = match check {
        LineageCheck::Unneeded => None,
        LineageCheck::FullAccess | LineageCheck::Sandboxed => {
            Some(LineageToken::new().ok_or("server_cant_start")?)
        }
    };
    if let Some(token) = &token {
        process.env(LINEAGE_VARIABLE, token.as_str());
    }
    process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let (mut child, warden) = crate::sandbox::warden::spawn(&mut process).map_err(start_refused)?;
    let pid = child.id();
    let (stdin, stdout) = match (child.stdin.take(), child.stdout.take()) {
        (Some(stdin), Some(stdout)) => (stdin, stdout),
        _ => {
            stop(pid);
            let _ = child.wait();
            crate::sandbox::warden::fire_reported(warden, "server_check")
                .map_err(|_| "server_cleanup_uncertain")?;
            return Err("server_cant_start");
        }
    };
    let follower = Follower::start(pid, token, check);
    assert_eq!(follower.is_some(), check != LineageCheck::Unneeded);
    let result = converse(stdin, stdout, limit);
    let lineage = follower.map(Follower::stop);
    stop(pid);
    let _ = child.kill();
    let _ = child.wait();
    let settled = match lineage.map(|lineage| lineage.and_then(Lineage::settle)) {
        None | Some(Ok(())) => Ok(()),
        Some(Err(unstopped)) => {
            let code = unstopped.code();
            if let Unstopped::Left(left) = unstopped {
                let by = HoldBy {
                    project: None,
                    who: "a plugin server check".to_owned(),
                };
                left.hold(None, (), by, leftovers::mode_of(None));
            }
            Err(code)
        }
    };
    crate::sandbox::warden::fire_reported(warden, "server_check")
        .map_err(|_| "server_cleanup_uncertain")?;
    settled?;
    result
}

/// Starts an already built MCP server command, such as one wrapped in the agent boundary,
/// suspended in a kill-on-close job of its own before it runs, runs `initialize` and
/// `tools/list`, then ends the job, so every process the server started ends with it and
/// none of them keeps the secret environment values. The boundary's own job ends the
/// agent's tree too, so a Windows check never follows a lineage.
/// Inputs: the command with its arguments, environment and folder set, the time bound and
/// the lineage check, always [`crate::sandbox::LineageCheck::Unneeded`] here. Output: the
/// tool names, or a plain rejection code.
#[cfg(windows)]
pub fn check_command(
    mut process: Command,
    limit: Duration,
    check: crate::sandbox::LineageCheck,
) -> Result<Vec<String>, &'static str> {
    use std::os::windows::process::CommandExt;
    assert_eq!(check, crate::sandbox::LineageCheck::Unneeded);
    process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = crate::spawn_gate::spawn(&mut process).map_err(start_refused)?;
    let tree = match crazyagent_win_sandbox::ProcessTree::hold(&child) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("server_check_tree_refused cause={error}");
            let _ = child.kill();
            let _ = child.wait();
            return Err("server_cant_start");
        }
    };
    let result = match (child.stdin.take(), child.stdout.take()) {
        (Some(stdin), Some(stdout)) => converse(stdin, stdout, limit),
        _ => Err("server_cant_start"),
    };
    drop(tree);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Reads the server's output on a thread of its own and talks to it until the time bound.
/// Inputs: the server's input and output and the bound. Output: the tool names, or a plain
/// rejection code.
fn converse(
    stdin: ChildStdin,
    stdout: ChildStdout,
    limit: Duration,
) -> Result<Vec<String>, &'static str> {
    let (send, receive) = sync_channel::<Line>(16);
    let reader = std::thread::Builder::new()
        .name("crazyagent-mcp-check".into())
        .spawn(move || read_lines(stdout, send));
    match reader {
        Ok(_) => talk(stdin, &receive, Instant::now() + limit),
        Err(_) => Err("server_cant_start"),
    }
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

/// Writes each queued message to the server's input until a write fails, on its own
/// thread, so a server that stops reading blocks only this writer and never the check's
/// deadline; stopping the server's process group ends a blocked write. Inputs: the input
/// and the queue. Output: none; the input closes when it returns.
fn write_lines(mut stdin: ChildStdin, lines: Receiver<Vec<u8>>) {
    for line in lines {
        if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
            return;
        }
    }
}

fn send(writer: &Sender<Vec<u8>>, message: &Value) -> Result<(), &'static str> {
    let mut line = serde_json::to_vec(message).map_err(|_| "server_cant_start")?;
    line.push(b'\n');
    writer.send(line).map_err(|_| "server_exited")
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
    stdin: ChildStdin,
    receive: &Receiver<Line>,
    deadline: Instant,
) -> Result<Vec<String>, &'static str> {
    let (writer, lines) = channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("crazyagent-mcp-write".into())
        .spawn(move || write_lines(stdin, lines))
        .map_err(|_| "server_cant_start")?;
    send(
        &writer,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "crazyagent", "version": env!("CARGO_PKG_VERSION")},
        }}),
    )?;
    answer(receive, 1, deadline)?;
    send(
        &writer,
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
            &writer,
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    const FIXTURE: &str = include_str!("../../tests/fixtures/mcp_fixture.py");

    /// Writes the MCP fixture as a new executable and runs it once in its `exit` mode, whose
    /// code 3 only the fixture returns, so its first-exec assessment is over before a check
    /// times it. Input: the folder. Output: the fixture path, already run once.
    fn fixture(root: &Path) -> PathBuf {
        let path = root.join("mcp_fixture.py");
        std::fs::write(&path, FIXTURE).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = crate::spawn_gate::first_run(
            Command::new(&path)
                .arg("exit")
                .env_clear()
                .env("PATH", "/usr/bin:/bin"),
        );
        assert_eq!(first.code(), Some(3));
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

    #[test]
    fn a_server_that_stops_reading_cannot_hold_the_check_past_its_time() {
        let root = tempfile::tempdir().unwrap();
        let server = fixture(root.path());
        let pidfile = root.path().join("deaf.pid");
        let (done, finished) = std::sync::mpsc::channel();
        let (command, path) = (server.to_str().unwrap().to_owned(), pidfile.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let checked = check_stdio(
                &command,
                &["deaf".into(), path.display().to_string()],
                &[],
                Duration::from_millis(1500),
            );
            let _ = done.send((checked, started.elapsed()));
        });
        let answer = finished.recv_timeout(Duration::from_secs(20));
        let Ok((checked, elapsed)) = answer else {
            panic!("a server that stopped reading held the check past its time");
        };
        assert_eq!(checked.unwrap_err(), "server_check_timeout");
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone = Instant::now() + Duration::from_secs(3);
        while alive(pid) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the deaf server must be stopped");
    }

    /// A server checked inside the agent boundary, as plugin checks run command servers,
    /// leaves a process in its own session and a `nohup` orphan; once the check times out,
    /// none of them is left.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_checked_server_in_the_agent_boundary_leaves_nothing_it_detached() {
        use crate::sandbox::warden::fixture::{DetachingRun, survivors};
        let run = DetachingRun::new();
        let (processes, checked) = std::thread::scope(|scope| {
            let check = scope.spawn(|| {
                check_command(
                    run.command(&run.profile),
                    Duration::from_millis(1500),
                    crate::sandbox::LineageCheck::Unneeded,
                )
            });
            (run.processes(), check.join().unwrap())
        });
        assert_eq!(checked.unwrap_err(), "server_check_timeout");
        assert_eq!(survivors(processes, Duration::from_secs(1)), []);
    }

    /// LINUX-KILL-GROUP r1 F1: a server checked in a boundary that leaves a process in a
    /// session of its own running, as the plain Linux Landlock helper's does, has its lineage
    /// followed, so what it left outside its group is held and the check answers with the
    /// sandboxed leftover code in place of what the server itself did. Its leader leaves the
    /// group the check starts it in and leads a session of its own, as the Linux helper's
    /// `own_session` makes a wrapped server, so the lineage admits its children.
    #[test]
    fn a_followed_server_check_names_what_the_server_left_outside_its_group() {
        let folder = tempfile::tempdir().unwrap();
        let mut command = Command::new("/usr/bin/perl");
        command
            .args(["-mPOSIX", "-e", "POSIX::setpgid(0, getpgrp(getppid())) or exit 1; POSIX::setsid() > 0 or exit 1; exec '/bin/sh', '-c', $ARGV[0] or exit 1"])
            .arg("/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, \">\", \"child.pid\") or exit 1; print $file \"$$\\n\"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 & exec /bin/sleep 30")
            .current_dir(folder.path());

        let checked = check_command(
            command,
            Duration::from_millis(1500),
            crate::sandbox::LineageCheck::Sandboxed,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let child = loop {
            if let Some(child) = std::fs::read_to_string(folder.path().join("child.pid"))
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
            {
                break child;
            }
            assert!(Instant::now() < deadline, "the stand-in never started");
            std::thread::sleep(Duration::from_millis(10));
        };
        if let Some(pid) = rustix::process::Pid::from_raw(child) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }

        assert_eq!(checked, Err(crate::harnesses::leftovers::SANDBOX_LEFT));
        assert_eq!(
            crate::plugins::check_problem(crate::harnesses::leftovers::SANDBOX_LEFT),
            "The server left processes running after its check. CrazyAgent ends them."
        );
    }
}

#[cfg(all(test, windows))]
pub(crate) mod windows_tests {
    use super::*;
    use std::path::PathBuf;

    /// The environment name a row sets to the file a grandchild holder writes its
    /// grandchild's process id to.
    const HOLDER_MARKER: &str = "CRAZYAGENT_TEST_GRANDCHILD_MARKER";

    /// The file whose presence in a holder's working folder asks it to write the id to
    /// `grandchild.pid` beside it, for a run whose environment a row cannot set.
    pub(crate) const HOLDER_REQUEST: &str = "grandchild.request";

    /// The test binary as a silent server that starts a grandchild, in place of a shell
    /// whose start time varies: the program and the arguments that run
    /// [`a_grandchild_holder_in_its_own_process`] alone. Output: the program, then its
    /// arguments.
    pub(crate) fn grandchild_holder() -> (PathBuf, [&'static str; 5]) {
        (
            std::env::current_exe().unwrap(),
            [
                "--exact",
                "plugins::check::windows_tests::a_grandchild_holder_in_its_own_process",
                "--ignored",
                "--test-threads",
                "1",
            ],
        )
    }

    /// Runs only in the process a row starts, when [`HOLDER_MARKER`] names a file or its
    /// working folder holds [`HOLDER_REQUEST`]: starts ping for two minutes with no streams,
    /// writes ping's process id to that file, then waits two minutes reading nothing, as a
    /// silent server. Run any other way it does nothing.
    #[test]
    #[ignore]
    fn a_grandchild_holder_in_its_own_process() {
        let requested = std::env::current_dir()
            .ok()
            .filter(|folder| folder.join(HOLDER_REQUEST).is_file())
            .map(|folder| folder.join("grandchild.pid"));
        let Some(marker) = std::env::var_os(HOLDER_MARKER)
            .map(PathBuf::from)
            .or(requested)
        else {
            return;
        };
        let ping = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("PING.EXE");
        let mut command = Command::new(ping);
        command
            .args(["-n", "120", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut grandchild = crate::spawn_gate::spawn(&mut command).unwrap();
        std::fs::write(&marker, grandchild.id().to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(120));
        let _ = grandchild.kill();
        let _ = grandchild.wait();
    }

    /// Whether a process id still runs, as `tasklist` lists it. Input: the id. Output: the
    /// answer.
    fn running(pid: u32) -> bool {
        let mut tasklist = Command::new("tasklist");
        tasklist.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
        let listed = crate::spawn_gate::collected(&mut tasklist).unwrap();
        String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{pid}\""))
    }

    #[test]
    fn a_missing_server_command_answers_with_a_plain_reason() {
        let folder = tempfile::tempdir().unwrap();
        let missing = folder.path().join("missing-server.exe");
        assert_eq!(
            check_stdio(missing.to_str().unwrap(), &[], &[], Duration::from_secs(5)).unwrap_err(),
            "server_command_not_found"
        );
    }

    /// A server that starts a grandchild and never answers times out, and the grandchild
    /// ends with the check's job. The server is the test binary
    /// ([`a_grandchild_holder_in_its_own_process`]), which writes the grandchild's id at
    /// once, so the row never waits on a shell's start.
    #[test]
    fn a_silent_server_times_out_and_every_process_it_started_ends_with_it() {
        let folder = tempfile::tempdir().unwrap();
        let marker = folder.path().join("grandchild.pid");
        let (holder, args) = grandchild_holder();
        let secrets = [(
            HOLDER_MARKER.to_owned(),
            marker.to_str().unwrap().as_bytes().to_vec(),
        )];
        let checked = check_stdio(
            holder.to_str().unwrap(),
            &args.map(String::from),
            &secrets,
            Duration::from_secs(20),
        );
        assert_eq!(checked.unwrap_err(), "server_check_timeout");
        let pid: u32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid > 4);
        let deadline = Instant::now() + Duration::from_secs(10);
        while running(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!running(pid), "the server's child {pid} outlived its check");
    }
}
