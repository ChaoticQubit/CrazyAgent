use super::custodian::{CAUSE_BYTES_MAX, CustodianCause, CustodianChild};
use super::entry_bootstrap::read_bootstrap;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::terminal::windows::ConptyHost;
use crate::terminal::{AgentId, Host};
use crazyagent_win_sandbox::test_files::inheritable;
use std::fs::File;
use std::io::ErrorKind;
use std::io::Write;
use std::os::windows::io::OwnedHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const NOT_PIPE: &str = "native_auth_bootstrap_input_not_pipe";
const TIMEOUT: &str = "native_auth_bootstrap_timeout";
const TRUNCATED: &str = "native_auth_bootstrap_truncated";
const EXCESS: &str = "native_auth_bootstrap_excess";
const UNAVAILABLE: &str = "native_auth_host_unavailable";
const TOO_LONG: &str = "native_auth_host_launch_too_long";
const DIGEST_INVALID: &str = "native_auth_host_executable_digest_invalid";
const UNSAFE: &str = "native_auth_host_executable_unsafe";
const USAGE: &str = "usage: crazyagent native-auth-custodian RUNTIME_DIR";
const ARGUMENTS_PAST_BOUND: usize = 60;
const BOOTSTRAP_BOUND: Duration = Duration::from_secs(11);
const CAUSE_BOUND: Duration = Duration::from_secs(20);
const RACE_SPAWNS: usize = 200;
const RACE_PANES_MAX: usize = 32;
const WAITER: &str = "ping -n 60 127.0.0.1 >NUL";
const WAITER_WITH_CHILD: &str = "start /b ping -n 60 127.0.0.1 >NUL & ping -n 60 127.0.0.1 >NUL";
const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TREE_END_BOUND: Duration = Duration::from_secs(2);
const FIND_BOUND: Duration = Duration::from_secs(20);
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The product binary the farm built for these rows, named by
/// `CRAZYAGENT_TEST_NATIVE_AUTH_HOST` as the Unix custodian rows name it.
fn executable() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_TEST_NATIVE_AUTH_HOST")
        .map(PathBuf::from)
        .expect("the farm supplies the product custodian executable path");
    assert!(path.is_absolute());
    path
}

/// Starts the custodian with the arguments after `native-auth-custodian` and the given
/// standard input, its error stream piped and its output dropped. Its runtime folder is the
/// temp folder: every outcome these rows check comes before the custodian opens it.
fn custodian(args: &[&str], stdin: Stdio) -> Child {
    crate::spawn_gate::spawn(
        Command::new(executable())
            .arg("native-auth-custodian")
            .arg(std::env::temp_dir())
            .args(args)
            .stdin(stdin)
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

/// Waits for a custodian, at most `bound`, and returns its error line and exit code; a
/// custodian still running at the bound is killed and fails the row.
fn ended(mut child: Child, bound: Duration) -> (String, Option<i32>, Duration) {
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > bound {
            child.kill().unwrap();
            panic!("the custodian ran past {bound:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let elapsed = started.elapsed();
    let output = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
        elapsed,
    )
}

/// A pipe's read end as an owned file, and its write end.
fn pipe() -> (File, std::io::PipeWriter) {
    let (reader, writer) = std::io::pipe().unwrap();
    (File::from(OwnedHandle::from(reader)), writer)
}

/// Reads a bootstrap from `input` on a runtime of its own, with the given deadline.
fn bootstrap(input: File, deadline: Instant) -> Result<[u8; 32], String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(read_bootstrap(input, deadline))
        .map_err(|error| error.0)
}

/// A custodian whose standard input is a file or NUL is refused before it reads anything,
/// the twin of the Unix FIFO check (rv-sec2 #728: `FILE_TYPE_PIPE`).
#[test]
fn a_custodian_with_a_file_or_nul_on_stdin_is_refused_as_not_a_pipe() {
    let folder = tempfile::tempdir().unwrap();
    let token = folder.path().join("token");
    std::fs::write(&token, [7; 32]).unwrap();
    let from_file = custodian(&[], Stdio::from(File::open(&token).unwrap()));
    assert_eq!(ended(from_file, BOOTSTRAP_BOUND).0, format!("{NOT_PIPE}\n"));
    let from_nul = custodian(&[], Stdio::from(File::open("NUL").unwrap()));
    let (line, code, _) = ended(from_nul, BOOTSTRAP_BOUND);
    assert_eq!(line, format!("{NOT_PIPE}\n"));
    assert_eq!(code, Some(1));
}

/// The argument checks run before the input, as on Unix: a word count past the bound, a
/// relative harness, a bad digest and a wrong shape are each refused with their own code.
#[test]
fn a_custodian_with_bad_arguments_is_refused_before_its_input() {
    let many: Vec<String> = (0..ARGUMENTS_PAST_BOUND).map(|n| n.to_string()).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        ended(custodian(&many, Stdio::null()), BOOTSTRAP_BOUND).0,
        format!("{TOO_LONG}\n")
    );
    let relative = ["--codex-executable", "codex.exe", "--codex-sha256", DIGEST];
    assert_eq!(
        ended(custodian(&relative, Stdio::null()), BOOTSTRAP_BOUND).0,
        format!("{UNSAFE}\n")
    );
    let bad_digest = [
        "--claude-executable",
        r"C:\Program Files\claude.exe",
        "--claude-sha256",
        "ZZ",
    ];
    assert_eq!(
        ended(custodian(&bad_digest, Stdio::null()), BOOTSTRAP_BOUND).0,
        format!("{DIGEST_INVALID}\n")
    );
    assert_eq!(
        ended(custodian(&["extra"], Stdio::null()), BOOTSTRAP_BOUND).0,
        format!("{USAGE}\n")
    );
}

/// A custodian on a pipe reads its bootstrap before it answers: closed and empty is
/// truncated, a whole token then EOF reaches the engine, which still answers unavailable
/// until the gate flips in N4.
#[test]
fn a_custodian_on_a_pipe_reads_its_bootstrap_before_it_answers() {
    let mut empty = custodian(&[], Stdio::piped());
    drop(empty.stdin.take());
    assert_eq!(ended(empty, BOOTSTRAP_BOUND).0, format!("{TRUNCATED}\n"));
    let mut whole = custodian(&[], Stdio::piped());
    whole.stdin.take().unwrap().write_all(&[7; 32]).unwrap();
    assert_eq!(ended(whole, BOOTSTRAP_BOUND).0, format!("{UNAVAILABLE}\n"));
}

/// A custodian whose pipe stays open and empty ends itself at the bootstrap bound, so the
/// timeout ends the process, not only the read (rv-sec2 #728, PLAN-0541 row 2).
#[test]
fn a_custodian_whose_pipe_stays_open_and_empty_exits_at_the_bootstrap_bound() {
    let mut open = custodian(&[], Stdio::piped());
    let held = open.stdin.take().unwrap();
    let (line, code, elapsed) = ended(open, BOOTSTRAP_BOUND);
    drop(held);
    assert_eq!(line, format!("{TIMEOUT}\n"));
    assert_eq!(code, Some(1));
    assert!(elapsed >= Duration::from_secs(10), "{elapsed:?}");
}

/// The Windows bootstrap read takes exactly 32 bytes then EOF; fewer is truncated, more is
/// excess, and an open, empty pipe times out at the deadline, within a second of it.
#[test]
fn the_windows_bootstrap_read_takes_32_bytes_and_refuses_the_rest() {
    let soon = || Instant::now() + Duration::from_secs(5);
    let (input, mut writer) = pipe();
    writer.write_all(&[9; 32]).unwrap();
    drop(writer);
    assert_eq!(bootstrap(input, soon()), Ok([9; 32]));
    for (bytes, refusal) in [(31, TRUNCATED), (33, EXCESS), (0, TRUNCATED)] {
        let (input, mut writer) = pipe();
        writer.write_all(&vec![9; bytes]).unwrap();
        drop(writer);
        assert_eq!(bootstrap(input, soon()), Err(refusal.to_owned()), "{bytes}");
    }
    let (input, writer) = pipe();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(2);
    assert_eq!(bootstrap(input, deadline), Err(TIMEOUT.to_owned()));
    let elapsed = started.elapsed();
    drop(writer);
    assert!(
        elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(3),
        "{elapsed:?}"
    );
}

/// Starts a stand-in custodian that writes the bytes a PowerShell expression makes, raw,
/// on its error stream and exits 3, and reads its cause as dispatch does, on every poll and
/// once after it exited. Input: the expression. Output: the cause.
fn cause_of(bytes: &str) -> CustodianCause {
    let script = format!(
        "$b = [Text.Encoding]::ASCII.GetBytes({bytes}); \
         $e = [Console]::OpenStandardError(); $e.Write($b, 0, $b.Length); $e.Flush(); exit 3"
    );
    let mut child = crate::spawn_gate::spawn(
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let mut cause = CustodianCause::new(child.stderr.take().unwrap()).unwrap();
    let started = Instant::now();
    let status = loop {
        cause.read().unwrap();
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < CAUSE_BOUND, "the writer blocked");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(3));
    cause.read().unwrap();
    cause
}

/// The Windows cause reader reads as the Unix one does: one line of a code is the code, a
/// 129-byte line or a second line is none, and a custodian that writes 1 MB never blocks,
/// with 130 bytes kept.
#[test]
fn the_cause_reader_drains_a_large_error_stream_and_keeps_one_code() {
    assert_eq!(cause_of("'some_code' + [char]10").code(), Some("some_code"));
    let past = format!("('a' * {}) + [char]10", CAUSE_BYTES_MAX + 1);
    assert_eq!(cause_of(&past).code(), None);
    assert_eq!(cause_of("'one' + [char]10 + 'two' + [char]10").code(), None);
    let large = cause_of("'x' * 1048576");
    assert_eq!(large.text.len(), CAUSE_BYTES_MAX + 2);
    assert_eq!(large.code(), None);
}

/// The system's `cmd.exe`, found through `SystemRoot`.
fn cmd() -> PathBuf {
    let root = std::env::var_os("SystemRoot").expect("Windows sets SystemRoot");
    let cmd = PathBuf::from(root).join("System32").join("cmd.exe");
    assert!(cmd.is_absolute());
    cmd
}

/// Starts a std child that waits a minute, its stdio all named, through the spawn gate, as
/// every engine spawn starts: std starts it with handle inheritance on.
fn std_waiter() -> Child {
    crate::spawn_gate::spawn(
        Command::new(cmd())
            .args(["/c", WAITER])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap()
}

/// Opens a ConPTY pane for `name` through the engine's real terminal path, its command
/// waiting a minute.
fn pane(host: &ConptyHost, name: &str) -> AgentId {
    let agent = AgentId::new(name).unwrap();
    let command = InteractiveCommand {
        argv: vec![
            cmd().to_string_lossy().into_owned(),
            "/c".to_owned(),
            WAITER.to_owned(),
        ],
        cwd: cmd().parent().unwrap().to_path_buf(),
        env: Vec::new(),
        transcript: Transcript::Unavailable,
    };
    host.spawn(&agent, &command).unwrap();
    agent
}

/// Whether anyone but this process holds a pipe's read end: the row drops its own read end
/// first, so a one-byte write succeeds only into a pipe another process holds open.
fn held_elsewhere(reader: std::io::PipeReader, writer: &mut std::io::PipeWriter) -> bool {
    drop(reader);
    match writer.write_all(b"x") {
        Ok(()) => true,
        Err(error) => {
            assert_eq!(error.kind(), ErrorKind::BrokenPipe, "{error}");
            false
        }
    }
}

/// rv-sec2 #728 row 4, in order: a handle the engine holds inheritable while a ConPTY pane
/// starts reaches neither the pane's child nor the conhost `CreatePseudoConsole` starts, so
/// a custodian's token pipe never does. The positive control, a std child, which starts
/// with handle inheritance on, is seen holding the same kind of handle, or the check would
/// prove nothing.
#[test]
fn a_conpty_pane_and_its_conhost_get_no_inheritable_handle_of_the_engine() {
    let (reader, mut writer) = std::io::pipe().unwrap();
    inheritable(&reader).unwrap();
    let mut control = std_waiter();
    assert!(control.try_wait().unwrap().is_none());
    let control_held = held_elsewhere(reader, &mut writer);
    control.kill().unwrap();
    control.wait().unwrap();
    assert!(
        control_held,
        "the std child must hold the inheritable read end"
    );
    let (reader, mut writer) = std::io::pipe().unwrap();
    inheritable(&reader).unwrap();
    let host = ConptyHost::new();
    let agent = pane(&host, "row-four");
    assert!(host.alive().unwrap().contains(&agent));
    let pane_held = held_elsewhere(reader, &mut writer);
    assert!(host.alive().unwrap().contains(&agent));
    host.kill(&agent).unwrap();
    assert!(
        !pane_held,
        "a ConPTY pane or its conhost holds the engine's handle"
    );
}

/// rv-sec2 #728 row 4, the race: ConPTY panes keep opening while a stand-in custodian, with
/// a piped stdin as the custodian's token pipe, starts and ends 200 times. After each ends,
/// a write to its stdin must fail while every pane and its conhost still runs, so no pane
/// start took the pipe std makes inheritable during the custodian's spawn. The control, the
/// stand-in still running, takes the write.
#[test]
fn a_custodian_token_pipe_reaches_no_conpty_pane_started_at_the_same_time() {
    let mut alive = crate::spawn_gate::spawn(
        Command::new(cmd())
            .args(["/c", WAITER])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    alive.stdin.take().unwrap().write_all(b"x").unwrap();
    alive.kill().unwrap();
    alive.wait().unwrap();
    let host = std::sync::Arc::new(ConptyHost::new());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opener = {
        let (host, stop) = (std::sync::Arc::clone(&host), std::sync::Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut opened = Vec::new();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) && opened.len() < RACE_PANES_MAX
            {
                opened.push(pane(&host, &format!("race-{}", opened.len())));
            }
            opened
        })
    };
    let mut held = 0;
    for _ in 0..RACE_SPAWNS {
        let mut child = crate::spawn_gate::spawn(
            Command::new(cmd())
                .args(["/c", "exit 0"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let mut input = child.stdin.take().unwrap();
        child.wait().unwrap();
        match input.write_all(b"x") {
            Ok(()) => held += 1,
            Err(error) => assert_eq!(error.kind(), ErrorKind::BrokenPipe, "{error}"),
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let opened = opener.join().unwrap();
    let running = host.alive().unwrap();
    for agent in &opened {
        assert!(running.contains(agent), "{agent:?} ended before the checks");
        host.kill(agent).unwrap();
    }
    assert!(!opened.is_empty());
    assert_eq!(
        held, 0,
        "{held} of {RACE_SPAWNS} token pipes reached a pane"
    );
}

/// Records what a custodian does with no `SystemRoot` in its environment, for PLAN-0541's
/// design 2, written to the file the farm names, never asserted.
#[test]
#[ignore = "farm probe, run with --ignored"]
fn what_a_custodian_does_with_no_system_root() {
    let out = std::env::var_os("CRAZYAGENT_FARM_PROBE_OUT").unwrap();
    let folder = tempfile::tempdir().unwrap();
    let mut child = crate::spawn_gate::spawn(
        Command::new(executable())
            .arg("native-auth-custodian")
            .arg(folder.path())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    drop(child.stdin.take());
    let (line, code, elapsed) = ended(child, BOOTSTRAP_BOUND);
    std::fs::write(
        out,
        format!("no SystemRoot: code={code:?} elapsed={elapsed:?} stderr={line:?}\n"),
    )
    .unwrap();
}

/// A stand-in custodian whose first act starts a grandchild `ping.exe`, both waiting a minute,
/// its stdio all named.
fn waiter_with_grandchild() -> Command {
    let mut command = Command::new(cmd());
    command
        .args(["/c", WAITER_WITH_CHILD])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Whether a process id runs the named image now, so a reused id never reads as the process
/// a row started. Inputs: the id and the image, such as `PING.EXE`. Output: the answer.
fn runs(pid: u32, image: &str) -> bool {
    let listed = crate::spawn_gate::collected(
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .stdin(Stdio::null()),
    )
    .unwrap();
    String::from_utf8_lossy(&listed.stdout)
        .to_ascii_lowercase()
        .contains(&format!("\"{}\"", image.to_ascii_lowercase()))
}

/// The id of the `ping.exe` a process started, waited for at most [`FIND_BOUND`]. Input:
/// the parent's id. Output: the grandchild's id.
fn ping_child_of(parent: u32) -> u32 {
    let started = Instant::now();
    let query = format!(
        "Get-CimInstance Win32_Process -Filter \"ParentProcessId={parent} AND Name='PING.EXE'\" \
         | Select-Object -First 1 -ExpandProperty ProcessId"
    );
    loop {
        let found = crate::spawn_gate::collected(
            Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &query])
                .stdin(Stdio::null()),
        )
        .unwrap();
        if let Ok(pid) = String::from_utf8_lossy(&found.stdout).trim().parse() {
            return pid;
        }
        assert!(started.elapsed() < FIND_BOUND, "no grandchild of {parent}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Ends every `ping.exe` a process started, the process itself gone or not, so a control
/// row leaves no waiter behind. Input: the parent's id. Output: none.
fn end_ping_children_of(parent: u32) {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"ParentProcessId={parent} AND Name='PING.EXE'\" \
         | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}"
    );
    let _ = crate::spawn_gate::collected(
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null()),
    );
}

/// Waits until none of the processes runs, at most `bound`. Inputs: the ids with their
/// images, and the bound. Output: whether all ended within it.
fn all_end_within(processes: &[(u32, &str)], bound: Duration) -> bool {
    let started = Instant::now();
    while processes.iter().any(|(pid, image)| runs(*pid, image)) {
        if started.elapsed() > bound {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// PLAN-0541 row 3: a custodian started suspended is in its job before it runs, so the
/// grandchild its first act starts is in it too, and dropping the custodian ends both
/// within two seconds. The control, the same stand-in with no job, leaves its grandchild
/// running after its own kill, or the row would prove nothing.
#[test]
fn the_custodian_job_holds_its_tree_from_the_start_and_ends_it_on_drop() {
    let mut loose = crate::spawn_gate::spawn(&mut waiter_with_grandchild()).unwrap();
    let parent_of_loose = loose.id();
    let orphan = ping_child_of(parent_of_loose);
    loose.kill().unwrap();
    loose.wait().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let survived = runs(orphan, "PING.EXE");
    end_ping_children_of(parent_of_loose);
    assert!(
        survived,
        "a grandchild outside any job must outlive its parent's kill"
    );
    let mut command = waiter_with_grandchild();
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let custodian = CustodianChild::start(&mut command).unwrap();
    let parent = custodian.id();
    let grandchild = ping_child_of(parent);
    assert!(runs(parent, "cmd.exe") && runs(grandchild, "PING.EXE"));
    drop(custodian);
    assert!(all_end_within(
        &[(parent, "cmd.exe"), (grandchild, "PING.EXE")],
        TREE_END_BOUND
    ));
}

/// PLAN-0541 row 3: a custodian whose job cannot be made is killed, never left suspended or
/// running outside a job.
#[test]
fn a_custodian_whose_hold_fails_is_killed() {
    let mut command = waiter_with_grandchild();
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let child = crate::spawn_gate::spawn(&mut command).unwrap();
    let pid = child.id();
    assert!(runs(pid, "cmd.exe"));
    let held = CustodianChild::held(child, |_| {
        Err(crazyagent_win_sandbox::BoundaryError::Refused(
            "a row's forced hold failure",
        ))
    });
    assert!(held.is_err());
    let ended = all_end_within(&[(pid, "cmd.exe")], TREE_END_BOUND);
    if !ended {
        let _ = crate::spawn_gate::collected(
            Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .stdin(Stdio::null()),
        );
    }
    assert!(ended, "the custodian outlived its failed hold");
}
