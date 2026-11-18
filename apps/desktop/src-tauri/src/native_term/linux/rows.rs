use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a display row waits for one step: a window to map, a page to load, a frame
/// callback or an event.
pub const STEP_WAIT: Duration = Duration::from_secs(10);

/// How long a display row waits for the farm at a checkpoint: a screenshot or typed keys.
pub const FARM_WAIT: Duration = Duration::from_secs(60);

/// Runs GTK's pending events without blocking. Output: none.
pub fn pump() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

/// Runs GTK's events for a while. Input: how long. Output: none.
pub fn pump_for(wait: Duration) {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        pump();
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Runs GTK's events until a condition holds, failing the row after the step wait.
/// Inputs: what is awaited, for the failure, and the condition. Output: none.
pub fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < STEP_WAIT,
            "no {what} within the step wait"
        );
        pump();
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Milliseconds since a moment. Input: the moment. Output: the milliseconds.
pub fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// The folder the farm shares with a display row, from NATIVE_ROW_DIR. Output: it, or
/// none outside the farm.
pub fn row_dir() -> Option<PathBuf> {
    std::env::var_os("NATIVE_ROW_DIR").map(PathBuf::from)
}

/// Writes a step file for the farm and runs GTK's events until the farm's go file comes;
/// outside the farm it does nothing. Inputs: the step name and its JSON line. Output: none.
pub fn checkpoint(step: &str, line: &Value) {
    let Some(dir) = row_dir() else {
        return;
    };
    std::fs::write(dir.join(format!("{step}.json")), format!("{line}\n")).expect("step file");
    let go = dir.join(format!("go-{step}"));
    let started = Instant::now();
    while !go.exists() {
        assert!(
            started.elapsed() < FARM_WAIT,
            "the farm did not finish step {step}"
        );
        pump();
        std::thread::sleep(Duration::from_millis(5));
    }
}
