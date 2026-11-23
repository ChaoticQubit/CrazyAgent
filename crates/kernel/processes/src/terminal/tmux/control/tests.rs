use super::*;
use std::sync::mpsc::channel;

const BOUND: Duration = Duration::from_secs(5);

/// A control client over a child that never reads its input, so a large enough write
/// blocks on the full pipe. Output: the client.
fn deaf_client() -> Control {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exec sleep 30"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the child starts");
    let input = child.stdin.take().expect("the child's input");
    Control {
        child: Mutex::new(Some(child)),
        input: Mutex::new(Some(input)),
        state: Arc::new(Mutex::new(State::default())),
        serial: CONTROL_SERIALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        listed: std::sync::atomic::AtomicUsize::new(0),
    }
}

/// Waits, without sleeping, until another thread holds the client's input. Input: the
/// client. Output: whether it was held within `BOUND`.
fn input_held(control: &Control) -> bool {
    let until = Instant::now() + BOUND;
    while Instant::now() < until {
        if control.input.try_lock().is_err() {
            return true;
        }
        std::thread::yield_now();
    }
    false
}

#[test]
fn a_stop_ends_a_client_whose_write_blocks_on_a_full_pipe() {
    let control = Arc::new(deaf_client());
    let pid = control.child.lock().unwrap().as_ref().unwrap().id();
    let writer = Arc::clone(&control);
    let written = std::thread::spawn(move || {
        let line = "a".repeat(1024 * 1024);
        writer.send(&line, Pending::Startup)
    });
    assert!(input_held(&control), "the write never took the input");
    let stopper = Arc::clone(&control);
    let (done, stopped) = channel();
    std::thread::spawn(move || {
        stopper.stop();
        let _ = done.send(());
    });
    assert!(
        stopped.recv_timeout(BOUND).is_ok(),
        "the stop waited on the blocked write"
    );
    assert!(control.child.lock().unwrap().is_none());
    assert!(written.join().expect("the writer ended").is_err());
    let alive = Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .status()
        .expect("kill runs");
    assert!(!alive.success(), "the child {pid} is still running");
}

#[test]
fn a_close_ends_a_client_whose_write_blocks_on_a_full_pipe() {
    let control = Arc::new(deaf_client());
    let writer = Arc::clone(&control);
    let written = std::thread::spawn(move || {
        let line = "a".repeat(1024 * 1024);
        writer.send(&line, Pending::Startup)
    });
    assert!(input_held(&control), "the write never took the input");
    let closer = Arc::clone(&control);
    let (done, closed) = channel();
    std::thread::spawn(move || {
        closer.close();
        let _ = done.send(());
    });
    assert!(
        closed.recv_timeout(BOUND).is_ok(),
        "the close waited on the blocked write"
    );
    assert!(control.child.lock().unwrap().is_none());
    assert!(written.join().expect("the writer ended").is_err());
}
