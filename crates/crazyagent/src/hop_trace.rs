use crate::terminal::TerminalRequest;
use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The variable naming the file the hop trace appends to (DEC-202 terminal latency research);
/// when it is unset nothing is traced.
pub const HOP_TRACE_ENV: &str = "CRAZYAGENT_HOP_TRACE";

/// The most characters one hop's label carries into a trace line.
const LABEL_CHARS_MAX: usize = 32;

static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Appends one line `<wall clock ms> <pid> <hop> <bytes> <label>` to the file that
/// `CRAZYAGENT_HOP_TRACE` names, and does nothing without it. It never writes typed bytes,
/// only their count. Takes the hop name (no spaces), the bytes the hop carries and a short
/// label such as a pane id. Returns nothing; a failed write drops the line.
pub fn hop(name: &str, bytes: usize, label: &str) {
    let sink = SINK.get_or_init(|| {
        let path = std::env::var_os(HOP_TRACE_ENV)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Mutex::new(file))
    });
    let Some(sink) = sink else {
        return;
    };
    assert!(!name.is_empty());
    assert!(!name.contains(char::is_whitespace));
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64() * 1000.0);
    let label: String = label
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(LABEL_CHARS_MAX)
        .collect();
    if let Ok(mut file) = sink.lock() {
        let _ = writeln!(
            file,
            "{ms:.3} {} {name} {bytes} {label}",
            std::process::id()
        );
    }
}

/// Traces one owner terminal request at a hop: its kind as the label and the bytes it types,
/// never the bytes themselves. Takes the hop name and the request. Returns nothing.
pub fn request(name: &str, request: &TerminalRequest) {
    let (kind, bytes) = match request {
        TerminalRequest::Input { bytes } => ("input", bytes.len()),
        TerminalRequest::Scroll { bytes } => ("scroll", bytes.len()),
        TerminalRequest::Prompt { text } => ("prompt", text.len()),
        TerminalRequest::Resize { .. } => ("resize", 0),
        TerminalRequest::Attach { .. }
        | TerminalRequest::AttachPane { .. }
        | TerminalRequest::AttachSized { .. } => ("attach", 0),
        TerminalRequest::Detach => ("detach", 0),
    };
    hop(name, bytes, kind);
}
