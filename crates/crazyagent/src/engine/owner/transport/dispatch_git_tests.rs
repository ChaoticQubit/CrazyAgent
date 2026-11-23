use super::*;

/// A start held past the read's timeout returns the timeout's error at the timeout, while
/// the start still waits, instead of blocking the runtime until the start goes through.
#[test]
fn a_start_held_past_the_timeout_returns_the_timeout_error() {
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (holding, entered) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        kernel_processes::spawn_gate::guarded(|| {
            holding.send(()).unwrap();
            let _ = held.recv_timeout(Duration::from_secs(5));
        });
    });
    entered.recv().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let command = Command::new("/usr/bin/true");
    let started = Instant::now();
    let outcome = runtime.block_on(spawn_within(command, Duration::from_millis(200)));
    let waited = started.elapsed();
    release.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(
        outcome.err().map(|error| error.0),
        Some("dispatch_git_head_outcome_unknown".to_owned())
    );
    assert!(waited < Duration::from_secs(4), "{waited:?}");
}
