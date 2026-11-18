#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod probe;
#[cfg(windows)]
mod render;
#[cfg(windows)]
mod uia;

use std::time::{Duration, Instant};

/// The most threads and seconds a `--burn` load may ask for, so a stray load always ends.
const BURN_THREADS_MAX: usize = 256;
const BURN_SECONDS_MAX: u64 = 900;

/// Parses `THREADS SECONDS` for `--burn`. Input: the arguments after the flag.
/// Output: both values when each is a number in its range, else None.
fn burn_args(args: &[String]) -> Option<(usize, u64)> {
    let [threads, seconds] = args else {
        return None;
    };
    let threads: usize = threads.parse().ok()?;
    let seconds: u64 = seconds.parse().ok()?;
    let fits =
        (1..=BURN_THREADS_MAX).contains(&threads) && (1..=BURN_SECONDS_MAX).contains(&seconds);
    fits.then_some((threads, seconds))
}

/// Keeps `threads` threads busy for `seconds`: the loaded condition the perf rows run
/// under, so the worst case comes from the spike binary itself and needs no other tool.
/// Inputs: the thread count and the seconds, each inside its bound.
fn burn(threads: usize, seconds: u64) {
    assert!((1..=BURN_THREADS_MAX).contains(&threads));
    assert!((1..=BURN_SECONDS_MAX).contains(&seconds));
    let end = Instant::now() + Duration::from_secs(seconds);
    let workers: Vec<_> = (0..threads)
        .map(|seed| {
            std::thread::spawn(move || {
                let mut value = seed as u64 | 1;
                while Instant::now() < end {
                    for _ in 0..100_000 {
                        value = value
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                    }
                    std::hint::black_box(value);
                }
            })
        })
        .collect();
    assert_eq!(workers.len(), threads);
    for worker in workers {
        let _ = worker.join();
    }
}

/// Runs the Windows embedding spike, or with `--burn THREADS SECONDS` only the CPU load
/// for the worst-case cells. Output: the log at SPIKE_LOG; exit 0 when every row ran, 2 on
/// bad arguments or another platform, 3 on the watchdog, 4 when Tauri fails.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--burn") {
        let Some((threads, seconds)) = burn_args(&args[1..]) else {
            eprintln!(
                "usage: --burn THREADS SECONDS (1..={BURN_THREADS_MAX}, 1..={BURN_SECONDS_MAX})"
            );
            std::process::exit(2);
        };
        println!("burn threads={threads} seconds={seconds}");
        burn(threads, seconds);
        return;
    }
    run();
}

/// The spike's window and rows. Output: as `main`.
#[cfg(windows)]
fn run() {
    probe::main();
}

/// Refuses every other platform: the spike probes Win32 and WebView2 only.
/// Output: exit code 2.
#[cfg(not(windows))]
fn run() {
    eprintln!("native-windows-spike runs on Windows only");
    std::process::exit(2);
}
