#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod probe;

/// Runs the Windows embedding spike: a native child window with a Direct3D 12 surface
/// above WebView2 in a Tauri window, and its probe rows. Output: the log at SPIKE_LOG;
/// exit 0 when every row ran, 3 on the watchdog, 4 when Tauri fails.
#[cfg(windows)]
fn main() {
    probe::main();
}

/// Refuses every other platform: the spike probes Win32 and WebView2 only.
/// Output: exit code 2.
#[cfg(not(windows))]
fn main() {
    eprintln!("native-windows-spike runs on Windows only");
    std::process::exit(2);
}
