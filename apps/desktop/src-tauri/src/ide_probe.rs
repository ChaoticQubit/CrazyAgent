use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Manager, Webview};

const ANSWER_FILE: &str = "CRAZYAGENT_E2E_IDE_PROBE";
const PAGE_LOADED: Duration = Duration::from_secs(30);
const ANSWER_BACK: Duration = Duration::from_secs(5);
const FRAMES_FILE: &str = "ide-native.jsonl";
const FRAMES_EVERY: Duration = Duration::from_secs(5);
const FRAMES_ROWS: usize = 60;
const ASK: &str = r#"
try {
  window.__ideProbe = "asked";
  window.__TAURI_INTERNALS__.invoke("desktop_status").then(
    () => { window.__ideProbe = "answered"; },
    (failure) => { window.__ideProbe = `refused: ${failure}`; },
  );
} catch (failure) {
  window.__ideProbe = `threw: ${failure}`;
}
"#;
const READ: &str = "JSON.stringify({ origin: location.origin, answer: window.__ideProbe ?? null })";

static ASKED: Once = Once::new();

/// Test builds only: makes the first IDE page of this run call one native command as that page
/// would and writes the page's address and what it got back to the file named by the
/// environment, and logs the native frames beside it. Input: the IDE webview. Output: none;
/// without that variable it does nothing.
pub(crate) fn ask_once(webview: Webview) {
    let Some(file) = std::env::var_os(ANSWER_FILE).map(PathBuf::from) else {
        return;
    };
    ASKED.call_once(|| {
        let sampled = webview.clone();
        let frames = file.with_file_name(FRAMES_FILE);
        std::thread::spawn(move || ask(&webview, file));
        std::thread::spawn(move || sample(&sampled, &frames));
    });
}

fn sample(webview: &Webview, file: &Path) {
    for _ in 0..FRAMES_ROWS {
        if let Some(row) = frames(webview)
            && let Ok(mut out) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
        {
            let _ = writeln!(out, "{row}");
        }
        std::thread::sleep(FRAMES_EVERY);
    }
}

fn frames(webview: &Webview) -> Option<Value> {
    let window = webview.window();
    let main = webview.app_handle().get_webview("main")?;
    let inner = window.inner_size().ok()?;
    let outer = window.outer_size().ok()?;
    let inner_at = window.inner_position().ok()?;
    let outer_at = window.outer_position().ok()?;
    let at = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    Some(json!({
        "at": at.as_millis() as u64,
        "scale": window.scale_factor().ok()?,
        "inner": [inner.width, inner.height],
        "outer": [outer.width, outer.height],
        "inner_at": [inner_at.x, inner_at.y],
        "outer_at": [outer_at.x, outer_at.y],
        "main": frame(&main)?,
        "ide": frame(webview)?,
    }))
}

fn frame(webview: &Webview) -> Option<Value> {
    let at = webview.position().ok()?;
    let size = webview.size().ok()?;
    Some(json!({ "at": [at.x, at.y], "size": [size.width, size.height] }))
}

fn ask(webview: &Webview, file: PathBuf) {
    std::thread::sleep(PAGE_LOADED);
    if webview.eval(ASK).is_err() {
        return;
    }
    std::thread::sleep(ANSWER_BACK);
    let _ = webview.eval_with_callback(READ, move |answer| {
        let _ = std::fs::write(&file, answer);
    });
}
