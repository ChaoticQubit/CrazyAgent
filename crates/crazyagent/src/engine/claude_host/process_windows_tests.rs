use super::super::{ClaudeHostLimits, Phase, Status};
use super::{Channels, start};
use crate::adapters::claude::ClaudeProtocol;
use crate::adapters::{NativeAction, NativeHarness};
use crate::engine::codex_host::pump::Signal;
use crate::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
use crate::sandbox::LineageCheck;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// The environment name of the file the holder stand-in writes its own process id to.
const HOLDER_PID: &str = "CLAUDE_STAND_IN_PID";

/// The libtest arguments that run only [`claude_stand_in_holder`] in this test binary.
const HOLDER_ARGS: &str = "--exact engine::claude_host::process::windows_tests::claude_stand_in_holder --ignored --nocapture --test-threads 1";

/// What a row keeps of a custody run: its status, its stop, its wake, its pass count and the
/// harness id, plus the ends custody would otherwise see closed.
struct Watched {
    status: watch::Receiver<Status>,
    stop: Arc<AtomicBool>,
    doubt: Arc<AtomicBool>,
    signal: Arc<Signal>,
    passes: Arc<AtomicU64>,
    group: Arc<AtomicU32>,
    _turns: std::sync::mpsc::SyncSender<super::super::Turn>,
    _events: mpsc::Receiver<crate::adapters::events::ObservedEvent>,
}

/// The channels a custody run takes and what the row keeps of them. Output: both.
fn channels() -> (Channels, Watched) {
    let (turns, input) = std::sync::mpsc::sync_channel(1);
    let (output, events) = mpsc::channel(128);
    let (report, status) = watch::channel(Status {
        phase: Phase::Starting,
        fault: None,
    });
    let stop = Arc::new(AtomicBool::new(false));
    let doubt = Arc::new(AtomicBool::new(false));
    let group = Arc::new(AtomicU32::new(0));
    let signal = Arc::new(Signal::new());
    let passes = Arc::new(AtomicU64::new(0));
    let channels = Channels {
        input,
        output,
        report,
        stop: Arc::clone(&stop),
        doubt: Arc::clone(&doubt),
        group: Arc::clone(&group),
        diagnostic: Arc::new(Mutex::new(None)),
        woken: Arc::clone(&signal),
        passes: Arc::clone(&passes),
    };
    let watched = Watched {
        status,
        stop,
        doubt,
        signal,
        passes,
        group,
        _turns: turns,
        _events: events,
    };
    (channels, watched)
}

/// A Claude Code protocol for a launch, as the host makes one. Output: the protocol.
fn protocol() -> ClaudeProtocol {
    ClaudeProtocol::new(
        NativeAction::Launch {
            session_id: SessionId::new("claude-windows-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: CredentialRef::new("claude-windows-profile").unwrap(),
            worktree_lease: RequestId::new("claude-windows-lease").unwrap(),
        },
        CredentialRef::new("claude-windows-account").unwrap(),
    )
    .unwrap()
}

/// Custody limits with room for every row's output unless a row narrows them. Inputs: the
/// wall clock and both output bounds. Output: the limits.
fn limits(
    run_time: Option<Duration>,
    stdout_bytes: usize,
    stderr_bytes: usize,
) -> ClaudeHostLimits {
    ClaudeHostLimits {
        run_time,
        cleanup_time: Duration::from_secs(2),
        stdout_bytes,
        stderr_bytes,
    }
}

/// The System32 folder. Output: its path.
fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32")
}

/// A cmd.exe stand-in that runs `script` as written, so its stdout carries only what the
/// script writes. Input: the script. Output: the command.
fn cmd(script: &str) -> Command {
    let mut command = Command::new(system32().join("cmd.exe"));
    command.args(["/d", "/c"]).raw_arg(script);
    command
}

/// Runs one custody of `command` with no profile, as a turn-less Windows host does, until it
/// retires. Inputs: the command and the limits. Output: what the row kept, the custody
/// retired.
fn run_to_retired(command: Command, limits: ClaudeHostLimits) -> Watched {
    let (channels, watched) = channels();
    start(
        command,
        LineageCheck::Unneeded,
        (None, ProjectId::new("claude-windows-project").unwrap()),
        protocol(),
        limits,
        false,
        channels,
    );
    assert!(watched.status.borrow().phase == Phase::Retired);
    watched
}

/// The fault a retired custody reported, as its plain code. Input: what the row kept.
/// Output: the code, or none.
fn fault(watched: &Watched) -> Option<String> {
    watched.status.borrow().fault.map(|fault| fault.error().0)
}

/// Whether a process with this id runs, by tasklist. Input: the id. Output: the answer.
fn running(pid: u32) -> bool {
    let mut command = Command::new(system32().join("tasklist.exe"));
    command.args(["/NH", "/FO", "CSV", "/FI", &format!("PID eq {pid}")]);
    let listed = crate::spawn_gate::collected(&mut command).unwrap();
    String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{pid}\""))
}

/// Waits up to `bound` for a process to end. Inputs: the id and the bound. Output: whether
/// it ended.
fn ends(pid: u32, bound: Duration) -> bool {
    let until = Instant::now() + bound;
    while Instant::now() < until {
        if !running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !running(pid)
}

/// Writes `bytes` to a new file in `folder`. Inputs: the folder, a name and the bytes.
/// Output: the file's path.
fn fixture(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let file = folder.join(name);
    std::fs::write(&file, bytes).unwrap();
    file
}

/// V244-WIN-MASTER S4: a Windows Claude Code harness started under custody with no turn is
/// pumped, reaped and retired: one that exits 0 before any turn ends the run with its exit
/// code, as on Unix, every time. Its pipes end a moment before its exit can be read, which
/// farm run 44724702953 read as `claude_host_disconnected` in each of 3 tries, so the row
/// runs ten custodies.
#[test]
fn a_windows_claude_harness_that_exits_before_a_turn_is_reaped_and_retired() {
    for run in 0..10 {
        let watched = run_to_retired(cmd("exit 0"), limits(None, 65_536, 65_536));
        assert_eq!(
            fault(&watched).as_deref(),
            Some("claude_host_native_exit_0"),
            "run {run}"
        );
        assert!(watched.group.load(Ordering::Acquire) > 1);
        assert!(!watched.doubt.load(Ordering::Acquire));
    }
}

/// Stdout past the custody bound ends a Windows run with `claude_host_stdout_limit`.
#[test]
fn windows_stdout_past_its_bound_ends_the_run_with_the_stdout_limit() {
    let folder = tempfile::tempdir().unwrap();
    let out = fixture(folder.path(), "out.txt", &[b'x'; 4096]);
    let script = format!("type {}", out.display());
    let watched = run_to_retired(cmd(&script), limits(None, 1024, 65_536));
    assert_eq!(fault(&watched).as_deref(), Some("claude_host_stdout_limit"));
}

/// Stderr past the custody bound ends a Windows run with `claude_host_stderr_limit`.
#[test]
fn windows_stderr_past_its_bound_ends_the_run_with_the_stderr_limit() {
    let folder = tempfile::tempdir().unwrap();
    let err = fixture(folder.path(), "err.txt", &[b'e'; 2048]);
    let script = format!("type {} 1>&2", err.display());
    let watched = run_to_retired(cmd(&script), limits(None, 65_536, 512));
    assert_eq!(fault(&watched).as_deref(), Some("claude_host_stderr_limit"));
}

/// A quiet Windows harness meets the custody's wall clock: the run ends with
/// `claude_host_deadline` within its cleanup time, and the harness ends with it.
#[test]
fn a_quiet_windows_claude_harness_meets_its_deadline_and_ends() {
    let started = Instant::now();
    let watched = run_to_retired(
        cmd("ping -n 30 127.0.0.1 >nul"),
        limits(Some(Duration::from_millis(300)), 65_536, 65_536),
    );
    assert_eq!(fault(&watched).as_deref(), Some("claude_host_deadline"));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(ends(
        watched.group.load(Ordering::Acquire),
        Duration::from_secs(1)
    ));
}

/// Stopping a Windows custody ends the harness and the process it started, which outlives
/// the harness's own exit unless the engine's tree ends it, with no fault and nothing in
/// doubt.
#[test]
fn stopping_a_windows_claude_custody_ends_every_process_it_started() {
    let folder = tempfile::tempdir().unwrap();
    let marker = folder.path().join("holder.pid");
    let exe = std::env::current_exe().unwrap();
    let mut command = cmd(&format!("{} {HOLDER_ARGS} >nul 2>nul", exe.display()));
    command.env(HOLDER_PID, &marker);
    let (channels, watched) = channels();
    let custody = std::thread::spawn(move || {
        start(
            command,
            LineageCheck::Unneeded,
            (None, ProjectId::new("claude-windows-project").unwrap()),
            protocol(),
            limits(None, 65_536, 65_536),
            false,
            channels,
        );
    });
    let until = Instant::now() + Duration::from_secs(30);
    let grandchild = loop {
        if let Some(pid) = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            break pid;
        }
        assert!(Instant::now() < until, "the holder never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(running(grandchild));
    assert!(watched.status.borrow().phase == Phase::Running);
    watched.stop.store(true, Ordering::Release);
    watched.signal.raise();
    custody.join().unwrap();
    assert!(watched.status.borrow().phase == Phase::Retired);
    assert_eq!(fault(&watched), None);
    assert!(!watched.doubt.load(Ordering::Acquire));
    assert!(ends(grandchild, Duration::from_secs(1)));
    assert!(ends(
        watched.group.load(Ordering::Acquire),
        Duration::from_secs(1)
    ));
}

/// A quiet Windows custody waits on its wake instead of spinning, a few passes a second, and
/// a stop with a raised wake reaches its loop at once.
#[test]
fn a_quiet_windows_claude_custody_waits_on_its_wake_and_a_stop_reaches_it_at_once() {
    let (channels, watched) = channels();
    let custody = std::thread::spawn(move || {
        start(
            cmd("ping -n 30 127.0.0.1 >nul"),
            LineageCheck::Unneeded,
            (None, ProjectId::new("claude-windows-project").unwrap()),
            protocol(),
            limits(None, 65_536, 65_536),
            false,
            channels,
        );
    });
    let until = Instant::now() + Duration::from_secs(10);
    while watched.status.borrow().phase != Phase::Running {
        assert!(Instant::now() < until, "the custody never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = watched.passes.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(1));
    let quiet = watched.passes.load(Ordering::SeqCst) - before;
    assert!(quiet <= 10, "{quiet} passes in a quiet second");
    let stopped = watched.passes.load(Ordering::SeqCst);
    let asked = Instant::now();
    watched.stop.store(true, Ordering::Release);
    watched.signal.raise();
    while watched.passes.load(Ordering::SeqCst) == stopped {
        assert!(
            asked.elapsed() < Duration::from_secs(1),
            "the stop never woke the loop"
        );
        std::thread::yield_now();
    }
    assert!(
        asked.elapsed() < Duration::from_millis(50),
        "{:?}",
        asked.elapsed()
    );
    custody.join().unwrap();
    assert!(watched.status.borrow().phase == Phase::Retired);
    assert_eq!(fault(&watched), None);
}

/// The holder a stopping row's harness starts: writes its own process id to the file
/// [`HOLDER_PID`] names, then waits two minutes, longer than any row. Run only by those
/// rows, through cmd.exe.
#[test]
#[ignore = "a stand-in the Windows custody rows start, not a row"]
fn claude_stand_in_holder() {
    let marker = PathBuf::from(std::env::var_os(HOLDER_PID).unwrap());
    std::fs::write(&marker, std::process::id().to_string()).unwrap();
    std::thread::sleep(Duration::from_secs(120));
}
