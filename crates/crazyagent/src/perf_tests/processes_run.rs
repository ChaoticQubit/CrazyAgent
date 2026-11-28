use crate::perf_kit::{Row, SAMPLES, time};
use crate::setup::run::run_bounded;
use std::hint::black_box;
use std::process::Command;
use std::time::Duration;

const FAST: usize = 1000;
const ONE: usize = 1;
const LIMIT: Duration = Duration::from_secs(30);
const CONTROL_TEXT: &str = "a control text of one hundred and twenty bytes that both twins copy and format the same way, so a row shows the binary!.";

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "processes",
        name,
        batch,
    }
}

/// A command that exits at once without output: `/usr/bin/true`, or `cmd /C exit 0` on
/// Windows. Output: the command.
fn exit_now() -> Command {
    #[cfg(unix)]
    let command = Command::new("/usr/bin/true");
    #[cfg(windows)]
    let mut command = Command::new(cmd());
    #[cfg(windows)]
    command.args(["/C", "exit 0"]);
    command
}

/// A command that prints one short report and exits: `/bin/echo x`, or `cmd /C echo x` on
/// Windows. Output: the command.
fn report() -> Command {
    #[cfg(unix)]
    let mut command = Command::new("/bin/echo");
    #[cfg(windows)]
    let mut command = Command::new(cmd());
    #[cfg(windows)]
    command.args(["/C", "echo"]);
    command.arg("x");
    command
}

/// The Windows command interpreter under the system root. Output: its path.
#[cfg(windows)]
fn cmd() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").expect("the SystemRoot variable is set");
    std::path::PathBuf::from(root)
        .join("System32")
        .join("cmd.exe")
}

/// Times `run_bounded` from the spawn to the reaped exit: a command that exits at once, one
/// that sleeps 20 ms (Unix), and one whose report is kept, then a control row both twins
/// run the same way. Output: none; prints one PERF line per row.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn run_calls() {
    time(&row("run.run_bounded.exit_now", ONE), SAMPLES, || {
        black_box(run_bounded(&mut exit_now(), LIMIT, false).unwrap());
    });
    #[cfg(unix)]
    time(&row("run.run_bounded.exit_20ms", ONE), SAMPLES, || {
        black_box(run_bounded(Command::new("/bin/sleep").arg("0.02"), LIMIT, false).unwrap());
    });
    time(&row("run.run_bounded.capture", ONE), SAMPLES, || {
        let kept = run_bounded(&mut report(), LIMIT, true).unwrap();
        assert!(black_box(kept).starts_with(b"x"));
    });
    assert_eq!(CONTROL_TEXT.len(), 120);
    time(&row("run.control.format", FAST), SAMPLES, || {
        black_box(format!("{}: {}", black_box("run"), black_box(CONTROL_TEXT)));
    });
}
