use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;
use tauri::Webview;

const ANSWER_FILE: &str = "CRAZYAGENT_E2E_IDE_PROBE";
const PAGE_LOADED: Duration = Duration::from_secs(30);
const ANSWER_BACK: Duration = Duration::from_secs(5);
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
/// would, and writes the page's address and what it got back to the file named by the
/// environment. Input: the IDE webview. Output: none; without that variable it does nothing.
pub(crate) fn ask_once(webview: Webview) {
    let Some(file) = std::env::var_os(ANSWER_FILE).map(PathBuf::from) else {
        return;
    };
    ASKED.call_once(|| {
        std::thread::spawn(move || ask(&webview, file));
    });
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
