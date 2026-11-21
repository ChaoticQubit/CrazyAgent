use super::process::ProcessShared;
use super::{CodexHostOutput, CodexHostProcess};
use crate::routing::ResourceBudget;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// The environment name that tells [`codex_host_stand_in`] which harness to stand in for.
const STAND_IN: &str = "CODEX_STAND_IN";

/// The environment name of the file a `holder` stand-in writes its grandchild's id to.
const STAND_IN_PID: &str = "CODEX_STAND_IN_PID";

/// The line a stand-in writes before what it stands in for writes, so a row reads past what
/// the test harness itself prints first.
const MARK: &[u8] = b"<codex-stand-in>\n";

/// The line a `turn` stand-in answers its turn request with.
const TURN_STARTED: &[u8] = b"{\"method\":\"turn/started\"}\n";

/// How many bytes a `flood-then-count` stand-in writes before it reads its input.
const FLOOD_BYTES: usize = 2 * 1024 * 1024;

/// The frame a row sends a `flood-then-count` stand-in: the owner's largest master frame.
const FRAME_BYTES: usize = crate::master_frame_limit::BYTES_MAX;

/// The test binary as a stand-in Codex harness: the arguments that run
/// [`codex_host_stand_in`] alone. Output: the arguments.
const STAND_IN_ARGS: [&str; 6] = [
    "--exact",
    "engine::codex_host::process_windows_tests::codex_host_stand_in",
    "--ignored",
    "--nocapture",
    "--test-threads",
    "1",
];

/// A command that runs this test binary as the `mode` stand-in, with only the system root in
/// its environment. Inputs: the temporary root and the mode. Output: the command.
fn stand_in_command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(STAND_IN_ARGS)
        .current_dir(root)
        .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
        .env(STAND_IN, mode);
    command
}

/// Starts the `mode` stand-in under custody with a 300 s clock and room for 4 MiB of output.
/// Inputs: the temporary root and the mode. Output: the running process.
async fn stand_in(root: &Path, mode: &str) -> CodexHostProcess {
    let limits = ResourceBudget {
        output_bytes: 4 * 1024 * 1024,
        wall_time_ms: 300_000,
        ..ResourceBudget::unit()
    };
    CodexHostProcess::spawn(stand_in_command(root, mode), &limits)
        .await
        .unwrap()
}

/// Reads a process's stdout until it ends, within `limit`. Inputs: the process and the
/// bound. Output: every byte it wrote.
async fn read_to_exit(process: &mut CodexHostProcess, limit: Duration) -> Vec<u8> {
    let mut stdout = Vec::new();
    tokio::time::timeout(limit, async {
        while let CodexHostOutput::Stdout(bytes) = process.next_output().await.unwrap() {
            stdout.extend(bytes);
        }
    })
    .await
    .expect("the stand-in never ended");
    stdout
}

/// What a stand-in wrote after its [`MARK`]. Input: its whole stdout. Output: the payload.
fn payload(stdout: &[u8]) -> &[u8] {
    let at = stdout
        .windows(MARK.len())
        .position(|window| window == MARK)
        .expect("the stand-in wrote no mark");
    &stdout[at + MARK.len()..]
}

/// Whether a process id still runs, as `tasklist` lists it. Input: the id. Output: the
/// answer.
fn running(pid: u32) -> bool {
    let mut tasklist = Command::new("tasklist");
    tasklist.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
    let listed = crate::spawn_gate::collected(&mut tasklist).unwrap();
    String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{pid}\""))
}

/// Waits up to 10 s for a process id to stop running. Input: the id. Output: whether it
/// stopped.
fn ends(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while running(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    !running(pid)
}

/// Sends a `turn` stand-in its turn and reads up to its answer line. Input: the process.
/// Output: none.
async fn start_quiet_turn(process: &mut CodexHostProcess) {
    process
        .send(b"{\"method\":\"turn/start\"}\n")
        .await
        .unwrap();
    let mut answer = Vec::new();
    while !answer.ends_with(TURN_STARTED) {
        let output = tokio::time::timeout(Duration::from_secs(10), process.next_output())
            .await
            .expect("the stand-in never answered its turn")
            .unwrap();
        let CodexHostOutput::Stdout(bytes) = output else {
            panic!("the stand-in ended before its turn went quiet");
        };
        answer.extend(bytes);
    }
}

/// Counts the passes of a process's custody worker and of its reader while the reader waits
/// one second for output that never comes, after waiting `settle`. Inputs: the process and
/// the settle time. Output: the custody and reader passes.
async fn passes_while_quiet(process: &mut CodexHostProcess, settle: Duration) -> (u64, u64) {
    tokio::time::sleep(settle).await;
    let shared = process.shared();
    let custody = shared.custody_passes.load(Ordering::SeqCst);
    let reader = shared.host_passes.load(Ordering::SeqCst);
    let quiet = tokio::time::timeout(Duration::from_secs(1), process.next_output()).await;
    assert!(quiet.is_err(), "the stand-in was not quiet");
    (
        shared.custody_passes.load(Ordering::SeqCst) - custody,
        shared.host_passes.load(Ordering::SeqCst) - reader,
    )
}

/// Waits until a process's custody worker makes its next pass, at most 2 s. Inputs: the
/// shared state and the pass count to move past. Output: how long it took.
async fn next_custody_pass(shared: &ProcessShared, before: u64) -> Duration {
    let started = Instant::now();
    while shared.custody_passes.load(Ordering::SeqCst) == before
        && started.elapsed() < Duration::from_secs(2)
    {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    started.elapsed()
}

/// V244-WIN-MASTER S1: a Windows Codex host takes its input through its pipe and gives back
/// what the harness wrote, in order, then reports the harness's exit once it is reaped; a
/// second close returns the same status. It used to be refused as `codex_host_unavailable`
/// once its child ran, as std has no non-blocking pipe on Windows.
#[tokio::test]
async fn a_windows_codex_host_passes_its_input_to_its_output_and_is_reaped() {
    let root = tempfile::tempdir().unwrap();
    let mut process = stand_in(root.path(), "echo").await;
    assert!(process.pid() > 4);
    process.send(b"unit inert pipe bytes\n").await.unwrap();
    process.close_stdin();
    let stdout = read_to_exit(&mut process, Duration::from_secs(20)).await;
    assert_eq!(payload(&stdout), b"unit inert pipe bytes\n");
    let status = process.exit_status().expect("reaped status");
    assert!(status.success());
    assert_eq!(process.observed_bytes(), (stdout.len() as u64, 0));
    assert_eq!(process.close().await.unwrap(), status);
    assert_eq!(process.close().await.unwrap(), status);
    assert!(process.settled());
}

/// A reader that stalls while a Windows harness writes 565,600 bytes, more than the 128
/// chunks the output queue holds, ends no run (OB-5e): what the queue cannot hold stays in
/// the pipe, and every line arrives once, in order.
#[tokio::test]
async fn a_stalled_reader_holds_a_windows_harness_output_back_and_loses_none_of_it() {
    let root = tempfile::tempdir().unwrap();
    let mut process = stand_in(root.path(), "flood").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let stdout = read_to_exit(&mut process, Duration::from_secs(30)).await;
    let text = std::str::from_utf8(payload(&stdout)).unwrap();
    assert_eq!(text.len(), 5_600 * 101);
    assert_eq!(text.lines().count(), 5_600);
    for (index, line) in text.lines().enumerate() {
        assert_eq!(line, format!("{:0100}", index + 1));
    }
    assert!(process.exit_status().expect("reaped status").success());
    process.close().await.unwrap();
    assert!(process.settled());
}

/// Starts the `flood-then-count` stand-in under custody with no clock, as a master with no
/// time limit runs, with `output_bytes` of room and frames up to [`FRAME_BYTES`], then waits
/// for it to fill its output. Inputs: the root and the bound. Output: the running process.
async fn flood_then_count(root: &Path, output_bytes: u64) -> CodexHostProcess {
    let limits = ResourceBudget {
        output_bytes,
        wall_time_ms: 60_000,
        ..ResourceBudget::unit()
    };
    let mut process = CodexHostProcess::spawn_guarded(
        stand_in_command(root, "flood-then-count"),
        &limits,
        None,
        None,
        crate::sandbox::LineageCheck::Unneeded,
    )
    .await
    .unwrap();
    process.limit_input(FRAME_BYTES);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    process
}

/// One frame of [`FRAME_BYTES`] bytes, ending in a newline. Output: the frame.
fn largest_frame() -> Vec<u8> {
    let mut frame = vec![b'f'; FRAME_BYTES - 1];
    frame.push(b'\n');
    frame
}

/// CodeRabbit CR-1354: a Windows harness with no clock that writes 2 MiB before it reads its
/// input still takes a 1 MiB frame, larger than its pipe. The host waits on the frame without
/// reading, so custody used to stop reading at a full queue, the harness blocked on its
/// output and the frame never drained.
#[tokio::test]
async fn a_windows_harness_that_floods_its_output_before_it_reads_still_takes_a_frame_larger_than_its_pipe()
 {
    let root = tempfile::tempdir().unwrap();
    let mut process = flood_then_count(root.path(), 4 * 1024 * 1024).await;
    let sent = tokio::time::timeout(Duration::from_secs(20), process.send(&largest_frame())).await;
    assert!(
        sent.is_ok(),
        "the frame waited 20 s on a harness blocked on its unread output"
    );
    sent.unwrap().unwrap();
    process.close_stdin();
    let stdout = read_to_exit(&mut process, Duration::from_secs(30)).await;
    let written = payload(&stdout);
    let count = format!("{FRAME_BYTES}\n");
    assert_eq!(written.len(), FLOOD_BYTES + count.len());
    assert!(written[..FLOOD_BYTES].iter().all(|byte| *byte == b'w'));
    assert_eq!(&written[FLOOD_BYTES..], count.as_bytes());
    assert!(process.exit_status().expect("reaped status").success());
    process.close().await.unwrap();
}

/// CodeRabbit CR-1354: while a frame waits on a Windows harness with no clock, custody reads
/// its output past the full queue only up to the run's output bound. A harness that writes
/// 2 MiB past a 1 MiB bound ends the run with `codex_host_output_limit`, the frame is refused,
/// and the queue never held more than the bound.
#[tokio::test]
async fn the_output_bound_ends_a_windows_run_that_floods_past_it_while_a_frame_waits() {
    const BOUND: u64 = 1024 * 1024;
    let root = tempfile::tempdir().unwrap();
    let mut process = flood_then_count(root.path(), BOUND).await;
    let pid = process.pid();
    let sent = tokio::time::timeout(Duration::from_secs(20), process.send(&largest_frame())).await;
    assert!(
        sent.is_ok(),
        "the frame waited 20 s on a harness blocked on its unread output"
    );
    assert!(
        sent.unwrap().is_err(),
        "a refused frame was reported as written"
    );
    let error = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match process.next_output().await {
                Ok(CodexHostOutput::Stdout(_)) => {}
                Ok(CodexHostOutput::Exited) => panic!("the bound was reported as an exit"),
                Err(error) => break error,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(error.0, "codex_host_output_limit");
    let queued: usize = process
        .shared()
        .lock()
        .unwrap()
        .output
        .iter()
        .map(|output| match output {
            CodexHostOutput::Stdout(bytes) => bytes.len(),
            CodexHostOutput::Exited => 0,
        })
        .sum();
    assert!(
        queued as u64 <= BOUND,
        "{queued} bytes queued past the bound"
    );
    tokio::time::timeout(Duration::from_secs(10), process.close())
        .await
        .unwrap()
        .unwrap();
    assert!(ends(pid), "the stand-in {pid} outlived its close");
}

/// A quiet Windows harness meets its wall clock: the reader gets `codex_host_deadline`, and
/// the harness has ended by the time the close returns, within its bound.
#[tokio::test]
async fn a_quiet_windows_codex_host_meets_its_deadline_and_ends_by_its_close() {
    let root = tempfile::tempdir().unwrap();
    let limits = ResourceBudget {
        output_bytes: 4096,
        wall_time_ms: 1500,
        ..ResourceBudget::unit()
    };
    let started = Instant::now();
    let mut process = CodexHostProcess::spawn(stand_in_command(root.path(), "quiet"), &limits)
        .await
        .unwrap();
    let pid = process.pid();
    let error = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match process.next_output().await {
                Ok(CodexHostOutput::Stdout(_)) => {}
                Ok(CodexHostOutput::Exited) => panic!("the quiet stand-in exited on its own"),
                Err(error) => break error,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(error.0, "codex_host_deadline");
    let status = tokio::time::timeout(Duration::from_secs(10), process.close())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert!(ends(pid), "the closed stand-in {pid} still runs");
    assert!(started.elapsed() < Duration::from_secs(15));
}

/// Stderr counts toward a Windows run's output bound: a harness that writes past it on stderr
/// alone ends the run with `codex_host_output_limit`, and the close still ends it.
#[tokio::test]
async fn windows_stderr_past_the_output_bound_ends_the_run_with_the_limit_code() {
    let root = tempfile::tempdir().unwrap();
    let limits = ResourceBudget {
        output_bytes: 256,
        wall_time_ms: 30_000,
        ..ResourceBudget::unit()
    };
    let mut process = CodexHostProcess::spawn(stand_in_command(root.path(), "stderr"), &limits)
        .await
        .unwrap();
    process.close_stdin();
    let error = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match process.next_output().await {
                Ok(CodexHostOutput::Stdout(_)) => {}
                Ok(CodexHostOutput::Exited) => panic!("the bound was reported as an exit"),
                Err(error) => break error,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(error.0, "codex_host_output_limit");
    let (stdout, stderr) = process.observed_bytes();
    assert!(stdout < 256, "{stdout} stdout bytes");
    assert!(stderr > 0 && stdout + stderr > 256, "{stdout} + {stderr}");
    let pid = process.pid();
    tokio::time::timeout(Duration::from_secs(10), process.close())
        .await
        .unwrap()
        .unwrap();
    assert!(ends(pid), "the stand-in {pid} outlived its close");
}

/// Closing a Windows Codex host ends the harness and every process it started, which the
/// engine holds in a kill-on-close job of the harness's own; the close is clean, with no
/// cleanup in doubt, each of three times.
#[tokio::test]
async fn closing_a_windows_codex_host_ends_every_process_it_started() {
    for round in 0..3 {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("grandchild.pid");
        let mut command = stand_in_command(root.path(), "holder");
        command.env(STAND_IN_PID, &marker);
        let limits = ResourceBudget {
            output_bytes: 4096,
            wall_time_ms: 60_000,
            ..ResourceBudget::unit()
        };
        let mut process = CodexHostProcess::spawn(command, &limits).await.unwrap();
        let pid = process.pid();
        let deadline = Instant::now() + Duration::from_secs(20);
        let grandchild: u32 = loop {
            if let Ok(text) = std::fs::read_to_string(&marker)
                && let Ok(id) = text.trim().parse()
            {
                break id;
            }
            assert!(Instant::now() < deadline, "round {round}: no grandchild");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(grandchild > 4 && running(grandchild));
        let status = tokio::time::timeout(Duration::from_secs(10), process.close())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success());
        assert!(process.settled(), "round {round}: cleanup in doubt");
        assert_eq!(process.operation_result().map_err(|error| error.0), Ok(()));
        assert!(
            ends(pid),
            "round {round}: the stand-in {pid} outlived its close"
        );
        assert!(
            ends(grandchild),
            "round {round}: the grandchild {grandchild} outlived its close"
        );
    }
}

/// A quiet Windows turn costs a few wakes a second: custody waits for its pipes and the host
/// instead of a 10 ms tick.
#[tokio::test]
async fn a_quiet_windows_codex_turn_wakes_custody_and_its_reader_a_few_times_a_second() {
    let root = tempfile::tempdir().unwrap();
    let mut process = stand_in(root.path(), "turn").await;
    start_quiet_turn(&mut process).await;
    let (custody, reader) = passes_while_quiet(&mut process, Duration::from_millis(300)).await;
    assert!(custody <= 10, "{custody} custody passes in a quiet second");
    assert!(reader <= 10, "{reader} reader passes in a quiet second");
    let status = process.close().await.unwrap();
    assert!(!status.success());
}

/// Closing a quiet Windows turn reaches its custody at once, not at its next wait bound.
#[tokio::test]
async fn closing_a_quiet_windows_codex_turn_wakes_its_custody_at_once() {
    let root = tempfile::tempdir().unwrap();
    let mut process = stand_in(root.path(), "turn").await;
    start_quiet_turn(&mut process).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let shared = process.shared();
    let before = shared.custody_passes.load(Ordering::SeqCst);
    let (closed, took) = tokio::join!(process.close(), next_custody_pass(&shared, before));
    assert!(!closed.unwrap().success());
    assert!(
        took < Duration::from_millis(50),
        "custody saw the stop after {took:?}"
    );
}

/// A Windows harness whose output fills the queue is waited for without spinning, and the
/// reader making room has custody read again at once.
#[tokio::test]
async fn a_windows_harness_whose_output_fills_the_queue_is_waited_for_without_spinning() {
    let root = tempfile::tempdir().unwrap();
    let mut process = stand_in(root.path(), "yes").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let shared = process.shared();
    let before = shared.custody_passes.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let passes = shared.custody_passes.load(Ordering::SeqCst) - before;
    assert!(
        passes <= 10,
        "{passes} custody passes in a second with the queue full"
    );
    let before = shared.custody_passes.load(Ordering::SeqCst);
    let started = Instant::now();
    let output = process.next_output().await.unwrap();
    assert!(matches!(output, CodexHostOutput::Stdout(_)));
    let took = started.elapsed() + next_custody_pass(&shared, before).await;
    assert!(
        took < Duration::from_millis(50),
        "custody read again {took:?} after the reader made room"
    );
    let status = process.close().await.unwrap();
    assert!(!status.success());
}

/// Runs only in a process a row starts with [`STAND_IN`] set, as the harness that mode names,
/// writing [`MARK`] first; run any other way it does nothing. Modes: `echo` copies its input
/// to its output and exits 0; `flood` writes 5,600 numbered lines of 101 bytes and exits 0;
/// `quiet` waits two minutes; `stderr` writes 1,024 bytes to stderr, then waits; `holder`
/// starts ping for two minutes, writes its id to the [`STAND_IN_PID`] file, then waits;
/// `turn` answers one input line with [`TURN_STARTED`], then waits; `yes` writes lines until
/// it is ended; `flood-then-count` writes [`FLOOD_BYTES`] bytes, then reads its input to the
/// end and writes how many bytes it read.
#[test]
#[ignore]
fn codex_host_stand_in() {
    let Ok(mode) = std::env::var(STAND_IN) else {
        return;
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(MARK).unwrap();
    stdout.flush().unwrap();
    let wait = || std::thread::sleep(Duration::from_secs(120));
    match mode.as_str() {
        "echo" => {
            let mut input = Vec::new();
            std::io::stdin().lock().read_to_end(&mut input).unwrap();
            stdout.write_all(&input).unwrap();
        }
        "flood" => {
            for line in 1..=5_600 {
                writeln!(stdout, "{line:0100}").unwrap();
            }
        }
        "quiet" => wait(),
        "stderr" => {
            let mut stderr = std::io::stderr().lock();
            stderr.write_all(&[b'e'; 1024]).unwrap();
            stderr.flush().unwrap();
            wait();
        }
        "holder" => {
            let marker = PathBuf::from(std::env::var_os(STAND_IN_PID).unwrap());
            let ping = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32")
                .join("PING.EXE");
            let mut command = Command::new(ping);
            command
                .args(["-n", "120", "127.0.0.1"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[expect(
                clippy::zombie_processes,
                reason = "the holder leaves its grandchild running for the host to end"
            )]
            let grandchild = crate::spawn_gate::spawn(&mut command).unwrap();
            std::fs::write(&marker, grandchild.id().to_string()).unwrap();
            wait();
        }
        "turn" => {
            let mut turn = String::new();
            std::io::stdin().lock().read_line(&mut turn).unwrap();
            stdout.write_all(TURN_STARTED).unwrap();
            stdout.flush().unwrap();
            wait();
        }
        "flood-then-count" => {
            let block = [b'w'; 4096];
            for _ in 0..FLOOD_BYTES / block.len() {
                stdout.write_all(&block).unwrap();
            }
            stdout.flush().unwrap();
            let mut input = Vec::new();
            std::io::stdin().lock().read_to_end(&mut input).unwrap();
            writeln!(stdout, "{}", input.len()).unwrap();
        }
        "yes" => loop {
            if stdout.write_all(b"y\n").is_err() {
                break;
            }
        },
        other => panic!("no stand-in mode {other}"),
    }
    let _ = stdout.flush();
    std::process::exit(0);
}
