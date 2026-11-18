use crate::gpu::{Gpu, Timing};
use crate::probe::{log, typed};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The most queued render requests, and keys kept for the key row.
const QUEUE_MAX: usize = 256;
const KEYS_MAX: usize = 1024;

/// How long `start` waits for the render thread's GPU setup.
const SETUP_WAIT: Duration = Duration::from_secs(30);

/// One key's times in ms, as the window-thread row keeps them: send to submit, handler to
/// submit, glyph build, acquire.
pub type KeyTimes = (f64, f64, f64, f64);

/// Work for the render thread: a key the window thread took, or back-to-back frames.
enum Work {
    Key {
        sent: Option<Instant>,
        handled: Instant,
    },
    Frames {
        count: usize,
        budget: Duration,
        reply: SyncSender<Vec<Timing>>,
    },
}

static SENDER: OnceLock<SyncSender<Work>> = OnceLock::new();
static KEYS: Mutex<Vec<KeyTimes>> = Mutex::new(Vec::new());

/// Whether the render thread waits for the swap chain before it reads input (frame-ready
/// first) instead of building the frame and then waiting.
static READY_FIRST: AtomicBool = AtomicBool::new(false);

/// A key's send and handled instants, as the window thread hands it over.
type Key = (Option<Instant>, Instant);

/// Whether frames are drawn on the render thread instead of the window's thread.
pub fn active() -> bool {
    SENDER.get().is_some()
}

/// Starts the render thread, which makes its own GPU on the view and draws every frame
/// from then on, so a present never blocks the window's thread. The window thread's GPU
/// must be dropped first: one window takes one swap chain. Inputs: the view and module
/// handles, the size in pixels, the scale and whether to wait for the swap chain before
/// reading input. Output: Ok once the thread draws.
pub fn start(
    hwnd: isize,
    module: isize,
    size: (u32, u32),
    scale: f32,
    ready_first: bool,
) -> Result<(), String> {
    assert!(hwnd != 0, "the view exists before its render thread");
    assert!(SENDER.get().is_none(), "one render thread per process");
    READY_FIRST.store(ready_first, Ordering::SeqCst);
    let (send, receive) = mpsc::sync_channel(QUEUE_MAX);
    let (ready_send, ready) = mpsc::sync_channel(1);
    let started = Instant::now();
    std::thread::spawn(move || run(hwnd, module, size, scale, &receive, &ready_send));
    ready
        .recv_timeout(SETUP_WAIT)
        .map_err(|e| format!("render setup: {e}"))??;
    log(
        "summary_render_thread_setup_ms",
        &format!("{:.1}", started.elapsed().as_secs_f64() * 1000.0),
    );
    SENDER
        .set(send)
        .map_err(|_| "render thread already running".to_string())
}

/// Hands a key the window thread took to the render thread; never waits. Inputs: when it
/// was sent and when the window thread handled it. Output: false when the queue is full.
pub fn key(sent: Option<Instant>, handled: Instant) -> bool {
    SENDER
        .get()
        .is_some_and(|sender| sender.try_send(Work::Key { sent, handled }).is_ok())
}

/// Asks the render thread for frames back to back. Inputs: the count and the budget.
/// Output: the channel the timings arrive on, or None when no render thread runs.
pub fn frames(count: usize, budget: Duration) -> Option<Receiver<Vec<Timing>>> {
    let (reply, receive) = mpsc::sync_channel(1);
    let sender = SENDER.get()?;
    sender
        .send(Work::Frames {
            count,
            budget,
            reply,
        })
        .ok()?;
    Some(receive)
}

/// Takes the key times the render thread kept. Output: the times, oldest first.
pub fn take_keys() -> Vec<KeyTimes> {
    KEYS.lock()
        .map(|mut keys| std::mem::take(&mut *keys))
        .unwrap_or_default()
}

/// The render thread: sets up its GPU, then serves work until the window thread's sender
/// goes away. Keys queued together share one frame, as a host coalesces input.
fn run(
    hwnd: isize,
    module: isize,
    size: (u32, u32),
    scale: f32,
    receive: &Receiver<Work>,
    ready: &SyncSender<Result<(), String>>,
) {
    let mut gpu = match Gpu::new(hwnd, module, size.0, size.1, scale) {
        Ok(gpu) => gpu,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let mut next: Option<Work> = None;
    loop {
        let work = match next.take() {
            Some(work) => work,
            None => match receive.recv() {
                Ok(work) => work,
                Err(_) => return,
            },
        };
        match work {
            Work::Frames {
                count,
                budget,
                reply,
            } => {
                let _ = reply.send(draw_frames(&mut gpu, count, budget));
            }
            Work::Key { sent, handled } => {
                let mut keys = vec![(sent, handled)];
                let timing = if READY_FIRST.load(Ordering::SeqCst) {
                    gpu.draw_when_ready(|| {
                        next = drain(receive, &mut keys);
                        typed()
                    })
                } else {
                    next = drain(receive, &mut keys);
                    gpu.draw(true, &typed())
                };
                keep_keys(timing, &keys);
            }
        }
    }
}

/// Takes every key already queued, up to `QUEUE_MAX`, so they share one frame. Inputs: the
/// queue and the keys so far. Output: other work met in the queue, kept for after.
fn drain(receive: &Receiver<Work>, keys: &mut Vec<Key>) -> Option<Work> {
    assert!(!keys.is_empty());
    while keys.len() < QUEUE_MAX {
        match receive.try_recv() {
            Ok(Work::Key { sent, handled }) => keys.push((sent, handled)),
            Ok(other) => return Some(other),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return None,
        }
    }
    None
}

/// Keeps each timed key's times from the frame that drew them. Inputs: the frame's
/// timing or error and the keys' send and handled instants.
fn keep_keys(timing: Result<Timing, String>, keys: &[Key]) {
    assert!(!keys.is_empty() && keys.len() <= QUEUE_MAX);
    let timing = match timing {
        Ok(timing) => timing,
        Err(error) => {
            log("render_draw_error", &error);
            return;
        }
    };
    let Ok(mut kept) = KEYS.lock() else {
        return;
    };
    for &(sent, handled) in keys {
        if let Some(sent) = sent
            && kept.len() < KEYS_MAX
        {
            kept.push((
                (timing.submitted - sent).as_secs_f64() * 1000.0,
                (timing.submitted - handled).as_secs_f64() * 1000.0,
                timing.build_ms,
                timing.acquire_ms,
            ));
        }
    }
}

/// Draws text frames back to back until `count` or the budget. Inputs: the GPU, the count
/// and the budget. Output: their timings.
fn draw_frames(gpu: &mut Gpu, count: usize, budget: Duration) -> Vec<Timing> {
    assert!(count > 0);
    let started = Instant::now();
    let mut out = Vec::with_capacity(count);
    let ready_first = READY_FIRST.load(Ordering::SeqCst);
    while out.len() < count && started.elapsed() < budget {
        let drawn = if ready_first {
            gpu.draw_when_ready(typed)
        } else {
            gpu.draw(true, &typed())
        };
        match drawn {
            Ok(timing) => out.push(timing),
            Err(error) => {
                log("render_draw_error", &error);
                break;
            }
        }
    }
    assert!(out.len() <= count);
    out
}
