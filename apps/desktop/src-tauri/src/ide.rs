use crate::bridge::{BridgeError, open};
use crazyagent::engine::owner::IdeTabServer;
use crazyagent::protocol::ProjectId;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use tauri::menu::{Menu, PredefinedMenuItem, Submenu};
use tauri::webview::{NewWindowResponse, WebviewBuilder};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Rect, Url, WebviewUrl, WebviewWindow,
};

/// The script every IDE webview runs before the workbench loads: it drops the control
/// characters WebKit can insert into a text field on an arrow key (tauri#10194).
pub const GUARD: &str = include_str!("ide_guard.js");

const TOKEN_HEX: usize = 64;
const TOKEN_FILE_BYTES_MAX: u64 = 256;
const WEBVIEW_HOST_CHARS: usize = 52;
const SPAN_MAX: f64 = 100_000.0;

fn refused(code: &'static str) -> BridgeError {
    BridgeError { code, cause: None }
}

/// The panel the IDE webview covers, in the main window's logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    /// Checks the panel the window sent: finite, inside a sane screen and at least one
    /// pixel each way. Input: the bounds. Output: them, or `ide_bounds_invalid`.
    pub fn checked(self) -> Result<Self, BridgeError> {
        let spans = [self.x, self.y, self.width, self.height];
        let sane = spans
            .iter()
            .all(|span| span.is_finite() && (0.0..=SPAN_MAX).contains(span))
            && self.width >= 1.0
            && self.height >= 1.0;
        if !sane {
            return Err(refused("ide_bounds_invalid"));
        }
        Ok(self)
    }

    fn rect(self) -> Rect {
        Rect {
            position: LogicalPosition::new(self.x, self.y).into(),
            size: LogicalSize::new(self.width, self.height).into(),
        }
    }
}

/// Where one project's IDE webview loads: the workbench URL with the session token and the
/// workspace, the server origin its navigation stays on, and the webview hosts' port.
pub struct Workbench {
    url: Url,
    server: Url,
    webview_port: u16,
}

/// Builds the workbench address from what the engine opened, reading the per-launch token
/// from its file so the token never travels through the engine socket. Input: the opened
/// server. Output: the workbench, or `ide_server_invalid` / `ide_token_unreadable`.
pub fn workbench(server: &IdeTabServer) -> Result<Workbench, BridgeError> {
    let invalid = || refused("ide_server_invalid");
    let origin = Url::parse(&server.url).map_err(|_| invalid())?;
    let loopback = origin.scheme() == "http"
        && origin.host_str() == Some("127.0.0.1")
        && origin.port().is_some()
        && origin.username().is_empty()
        && origin.password().is_none();
    let workspace = server.workspace.to_str().ok_or_else(invalid)?;
    if !loopback || server.webview_port == 0 || !server.workspace.is_absolute() {
        return Err(invalid());
    }
    let token = token(&server.token_path)?;
    let mut url = origin.clone();
    url.set_path("/");
    url.set_fragment(None);
    url.query_pairs_mut()
        .clear()
        .append_pair("tkn", &token)
        .append_pair("workspace", workspace);
    Ok(Workbench {
        url,
        server: origin,
        webview_port: server.webview_port,
    })
}

fn token(path: &Path) -> Result<String, BridgeError> {
    let unreadable = || refused("ide_token_unreadable");
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    let mut text = String::new();
    file.take(TOKEN_FILE_BYTES_MAX)
        .read_to_string(&mut text)
        .map_err(|_| unreadable())?;
    let token = text.trim();
    if token.len() != TOKEN_HEX || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(unreadable());
    }
    Ok(token.to_owned())
}

/// Whether the IDE webview may load a URL: the IDE server's own origin, or one webview host
/// `<52 lowercase base32hex characters>.localhost` on the webview port, both plain http
/// with no user name. Inputs: the URL, the server origin and the webview port. Output:
/// true when the load stays inside the IDE.
pub fn navigation_allowed(url: &Url, server: &Url, webview_port: u16) -> bool {
    let plain = url.scheme() == "http" && url.username().is_empty() && url.password().is_none();
    if !plain {
        return false;
    }
    if url.origin() == server.origin() {
        return true;
    }
    url.port() == Some(webview_port)
        && url
            .host_str()
            .and_then(|host| host.strip_suffix(".localhost"))
            .is_some_and(|label| {
                label.len() == WEBVIEW_HOST_CHARS
                    && label
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'v'))
            })
}

/// Hands a link the IDE may not load itself to the owner's browser, and refuses the load.
/// Input: the URL. Output: false, always.
fn outside(url: &Url) -> bool {
    if matches!(url.scheme(), "http" | "https") {
        let link = url.to_string();
        tauri::async_runtime::spawn(async move {
            let _ = open::open_link(&link).await;
        });
    }
    false
}

struct Loaded {
    label: String,
    url: Url,
}

#[derive(Default)]
struct Held {
    loaded: HashMap<String, Loaded>,
    shown: Option<String>,
    launches: u64,
}

/// The IDE webviews of the main window: each project's webview label and the workbench URL
/// it loaded, the project shown now, and a count that keeps a replaced webview's label
/// unique while the old one closes.
#[derive(Default)]
pub struct IdeViews(Mutex<Held>);

impl IdeViews {
    fn lock(&self) -> MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn failed(_: tauri::Error) -> BridgeError {
    refused("ide_webview_failed")
}

fn ide_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let name = app.package_info().name.clone();
    Menu::with_items(
        app,
        &[
            &Submenu::with_items(
                app,
                name,
                true,
                &[
                    &PredefinedMenuItem::about(app, None, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(app, None)?,
                    &PredefinedMenuItem::maximize(app, None)?,
                ],
            )?,
        ],
    )
}

fn restore_menu(app: &AppHandle) -> Result<(), BridgeError> {
    app.set_menu(Menu::default(app).map_err(failed)?)
        .map_err(failed)?;
    Ok(())
}

/// Shows a project's IDE over its panel: the loaded webview when its workbench is the same,
/// else a new child webview with the input guard, the navigation lock and outside links sent
/// to the browser. Any other project's IDE is hidden, and the app menu gives up Cmd+Z, W,
/// N, P, F and comma while the IDE shows. Inputs: the app, the main window, the views, the
/// project, its workbench and the panel. Output: Ok once shown.
pub fn show(
    app: &AppHandle,
    window: &WebviewWindow,
    views: &IdeViews,
    project: &ProjectId,
    workbench: Workbench,
    bounds: Bounds,
) -> Result<(), BridgeError> {
    let mut held = views.lock();
    if let Some(other) = held.shown.take().filter(|other| other != project.as_str())
        && let Some(webview) = held
            .loaded
            .get(&other)
            .and_then(|loaded| app.get_webview(&loaded.label))
    {
        webview.hide().map_err(failed)?;
    }
    let current = held
        .loaded
        .get(project.as_str())
        .filter(|loaded| loaded.url == workbench.url)
        .and_then(|loaded| app.get_webview(&loaded.label));
    let webview = match current {
        Some(webview) => {
            webview.set_bounds(bounds.rect()).map_err(failed)?;
            webview.show().map_err(failed)?;
            webview
        }
        None => {
            if let Some(stale) = held
                .loaded
                .remove(project.as_str())
                .and_then(|loaded| app.get_webview(&loaded.label))
            {
                stale.close().map_err(failed)?;
            }
            held.launches += 1;
            let hex: String = project
                .as_str()
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let label = format!("ide-{hex}-{}", held.launches);
            let server = workbench.server.clone();
            let port = workbench.webview_port;
            let builder = WebviewBuilder::new(&label, WebviewUrl::External(workbench.url.clone()))
                .initialization_script(GUARD)
                .on_navigation(move |next| navigation_allowed(next, &server, port) || outside(next))
                .on_new_window(|next, _| {
                    outside(&next);
                    NewWindowResponse::Deny
                });
            let webview = window
                .as_ref()
                .window()
                .add_child(
                    builder,
                    LogicalPosition::new(bounds.x, bounds.y),
                    LogicalSize::new(bounds.width, bounds.height),
                )
                .map_err(failed)?;
            held.loaded.insert(
                project.as_str().to_owned(),
                Loaded {
                    label,
                    url: workbench.url,
                },
            );
            webview
        }
    };
    held.shown = Some(project.as_str().to_owned());
    webview.set_focus().map_err(failed)?;
    app.set_menu(ide_menu(app).map_err(failed)?)
        .map_err(failed)?;
    Ok(())
}

/// Moves the shown IDE webview over its panel after the panel moved or resized. Inputs: the
/// app, the views, the project and the panel. Output: Ok, also when that IDE is not shown.
pub fn place(
    app: &AppHandle,
    views: &IdeViews,
    project: &ProjectId,
    bounds: Bounds,
) -> Result<(), BridgeError> {
    let held = views.lock();
    if held.shown.as_deref() != Some(project.as_str()) {
        return Ok(());
    }
    if let Some(webview) = held
        .loaded
        .get(project.as_str())
        .and_then(|loaded| app.get_webview(&loaded.label))
    {
        webview.set_bounds(bounds.rect()).map_err(failed)?;
    }
    Ok(())
}

/// Hides a project's IDE webview when its tab is left, keeping it loaded, and gives the
/// app its own menu back. Inputs: the app, the views and the project. Output: Ok.
pub fn hide(app: &AppHandle, views: &IdeViews, project: &ProjectId) -> Result<(), BridgeError> {
    let mut held = views.lock();
    if held.shown.as_deref() != Some(project.as_str()) {
        return Ok(());
    }
    held.shown = None;
    if let Some(webview) = held
        .loaded
        .get(project.as_str())
        .and_then(|loaded| app.get_webview(&loaded.label))
    {
        webview.hide().map_err(failed)?;
    }
    restore_menu(app)
}

/// Closes a project's IDE webview when the project page closes, and gives the app its own
/// menu back if that IDE was shown. Inputs: the app, the views and the project. Output: Ok.
pub fn close(app: &AppHandle, views: &IdeViews, project: &ProjectId) -> Result<(), BridgeError> {
    let mut held = views.lock();
    let shown = held.shown.as_deref() == Some(project.as_str());
    if shown {
        held.shown = None;
    }
    if let Some(webview) = held
        .loaded
        .remove(project.as_str())
        .and_then(|loaded| app.get_webview(&loaded.label))
    {
        webview.close().map_err(failed)?;
    }
    if shown {
        restore_menu(app)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ide_navigation_allows_only_the_two_loopback_origins() {
        let server = Url::parse("http://127.0.0.1:47911/").unwrap();
        let host = "0123456789abcdefghijklmnopqrstuv0123456789abcdefghij";
        assert_eq!(host.len(), WEBVIEW_HOST_CHARS);
        for allowed in [
            "http://127.0.0.1:47911/".to_owned(),
            "http://127.0.0.1:47911/?tkn=ab&workspace=%2Fdata%2Fide%2Fp%2Fcrazyagent.code-workspace".to_owned(),
            "http://127.0.0.1:47911/stable-2b94421b/static/out/vs/workbench/workbench.js".to_owned(),
            format!("http://{host}.localhost:47913/index.html?id=1"),
            format!("http://{host}.localhost:47913/service-worker.js"),
        ] {
            let url = Url::parse(&allowed).unwrap();
            assert!(navigation_allowed(&url, &server, 47913), "{allowed}");
        }
        for refused in [
            "https://127.0.0.1:47911/".to_owned(),
            "http://127.0.0.1:47912/".to_owned(),
            "http://localhost:47911/".to_owned(),
            "http://[::1]:47911/".to_owned(),
            "http://user@127.0.0.1:47911/".to_owned(),
            "http://user:pass@127.0.0.1:47911/".to_owned(),
            format!("http://{host}.localhost:47911/index.html"),
            format!("http://{host}.localhost:47914/index.html"),
            format!("https://{host}.localhost:47913/index.html"),
            "http://0123456789abcdefghijklmnopqrstuvwxyz0123456789abcd.localhost:47913/".to_owned(),
            "http://short.localhost:47913/".to_owned(),
            format!("http://{host}.localhost.example.com:47913/"),
            format!("http://a.{host}.localhost:47913/"),
            "https://open-vsx.org/".to_owned(),
            "https://github.com/VSCodium/vscodium".to_owned(),
            "file:///etc/passwd".to_owned(),
            "about:blank".to_owned(),
            "javascript:alert(1)".to_owned(),
            "data:text/html,hi".to_owned(),
        ] {
            let url = Url::parse(&refused).unwrap();
            assert!(!navigation_allowed(&url, &server, 47913), "{refused}");
        }
    }

    #[test]
    fn the_workbench_reads_the_token_file_and_refuses_anything_off_loopback() {
        let folder = tempfile::tempdir().unwrap();
        let token_path = folder.path().join("token");
        std::fs::write(&token_path, format!("{}\n", "ab".repeat(32))).unwrap();
        let workspace = folder.path().join("crazyagent.code-workspace");
        let opened = IdeTabServer {
            url: "http://127.0.0.1:47911/".into(),
            webview_port: 47913,
            token_path: token_path.clone(),
            workspace: workspace.clone(),
        };
        let built = workbench(&opened).unwrap_or_else(|error| panic!("{}", error.code));
        let pairs: Vec<(String, String)> = built.url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs,
            vec![
                ("tkn".to_owned(), "ab".repeat(32)),
                (
                    "workspace".to_owned(),
                    workspace.to_str().unwrap().to_owned()
                ),
            ]
        );
        assert_eq!(built.server.as_str(), "http://127.0.0.1:47911/");
        assert_eq!(built.webview_port, 47913);

        for url in [
            "http://localhost:47911/",
            "https://127.0.0.1:47911/",
            "http://10.0.0.2:47911/",
            "http://127.0.0.1/",
        ] {
            let off = IdeTabServer {
                url: url.into(),
                ..opened.clone()
            };
            assert_eq!(
                workbench(&off).err().map(|error| error.code),
                Some("ide_server_invalid"),
                "{url}"
            );
        }
        for text in [
            String::new(),
            "short".to_owned(),
            "zz".repeat(32),
            "ab".repeat(40),
        ] {
            std::fs::write(&token_path, &text).unwrap();
            assert_eq!(
                workbench(&opened).err().map(|error| error.code),
                Some("ide_token_unreadable"),
                "{text}"
            );
        }
        std::fs::remove_file(&token_path).unwrap();
        assert_eq!(
            workbench(&opened).err().map(|error| error.code),
            Some("ide_token_unreadable")
        );
    }

    #[test]
    fn panel_bounds_must_be_finite_on_screen_and_visible() {
        let fine = Bounds {
            x: 0.0,
            y: 81.0,
            width: 900.0,
            height: 601.0,
        };
        assert_eq!(fine.checked().map_err(|error| error.code), Ok(fine));
        for bad in [
            Bounds { width: 0.0, ..fine },
            Bounds {
                height: 0.5,
                ..fine
            },
            Bounds { x: -1.0, ..fine },
            Bounds {
                y: f64::NAN,
                ..fine
            },
            Bounds {
                width: f64::INFINITY,
                ..fine
            },
            Bounds {
                x: SPAN_MAX + 1.0,
                ..fine
            },
        ] {
            assert_eq!(
                bad.checked().map_err(|error| error.code),
                Err("ide_bounds_invalid"),
                "{bad:?}"
            );
        }
    }
}
