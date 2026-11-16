use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime, Window};

const LEVELS_FILE: &str = "CRAZYAGENT_E2E_WINDOW_LEVELS";
const LOOK_EVERY: Duration = Duration::from_millis(500);
const LOOKS_MAX: usize = 3_600;

/// Test builds only: keeps every app window above other apps' windows and on every desktop
/// Space, so the owner's windows never cover the app and stop its WebKit pages drawing. With
/// the environment naming a file, writes there whether each window floats, once at start and
/// again whenever that changes. Input: none. Output: the plugin.
pub(crate) fn plugin<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("e2e-window")
        .setup(|app, _| {
            if let Some(file) = std::env::var_os(LEVELS_FILE).map(PathBuf::from) {
                let app = app.clone();
                std::thread::spawn(move || watch(&app, &file));
            }
            Ok(())
        })
        .on_window_ready(|window| float(&window))
        .build()
}

fn float<R: Runtime>(window: &Window<R>) {
    if let Err(error) = window.set_always_on_top(true) {
        eprintln!("e2e window {} did not float: {error}", window.label());
    }
    if let Err(error) = window.set_visible_on_all_workspaces(true) {
        eprintln!(
            "e2e window {} did not join every Space: {error}",
            window.label()
        );
    }
}

fn watch<R: Runtime>(app: &AppHandle<R>, file: &Path) {
    let mut written: Option<Vec<(String, Option<bool>)>> = None;
    for _ in 0..LOOKS_MAX {
        let mut levels: Vec<(String, Option<bool>)> = app
            .windows()
            .into_iter()
            .map(|(label, window)| (label, window.is_always_on_top().ok()))
            .collect();
        levels.sort();
        if written.as_ref() != Some(&levels) {
            append(file, &levels);
            written = Some(levels);
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

fn append(file: &Path, levels: &[(String, Option<bool>)]) {
    let windows: Vec<Value> = levels
        .iter()
        .map(|(label, on_top)| json!({ "label": label, "on_top": on_top }))
        .collect();
    if let Ok(mut out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
    {
        let _ = writeln!(out, "{}", json!({ "windows": windows }));
    }
}
