use crate::bridge::{BridgeError, open};
use crazyagent::engine::owner::IdeTabServer;
use crazyagent::protocol::ProjectId;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, PredefinedMenuItem, Submenu};
use tauri::webview::{NewWindowResponse, WebviewBuilder};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Rect, Runtime, Url, WebviewUrl, Window,
};

/// The script every IDE webview runs before the workbench loads: it drops the control
/// characters WebKit can insert into a text field on an arrow key (tauri#10194).
pub const GUARD: &str = include_str!("ide_guard.js");

const TOKEN_HEX: usize = 64;
const TOKEN_FILE_BYTES_MAX: u64 = 256;
const WEBVIEW_HOST_CHARS: usize = 52;
const SPAN_MAX: f64 = 100_000.0;
const LINK_GAP: Duration = Duration::from_secs(1);

static LINKS: Mutex<Option<Instant>> = Mutex::new(None);

fn refused(code: &'static str) -> BridgeError {
    BridgeError {
        code,
        cause: None,
        account: None,
    }
}

/// The panel the IDE webview covers, in the page's logical pixels, and the height of the page's
/// view.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub viewport: f64,
}

impl Bounds {
    /// Checks the panel the window sent: finite, inside a sane screen and at least one
    /// pixel each way. Input: the bounds. Output: them, or `ide_bounds_invalid`.
    pub fn checked(self) -> Result<Self, BridgeError> {
        let spans = [self.x, self.y, self.width, self.height, self.viewport];
        let sane = spans
            .iter()
            .all(|span| span.is_finite() && (0.0..=SPAN_MAX).contains(span))
            && self.width >= 1.0
            && self.height >= 1.0
            && self.viewport >= 1.0;
        if !sane {
            return Err(refused("ide_bounds_invalid"));
        }
        Ok(self)
    }

    fn below_inset(self, _content_height: f64) -> Self {
        self
    }

    /// Counts the panel from the top of the window's content view, where a child webview is
    /// placed, not from the top of the page. Input: the window. Output: the bounds to place.
    fn in_window<R: Runtime>(self, window: &Window<R>) -> Result<Self, BridgeError> {
        let scale = window.scale_factor().map_err(failed)?;
        let content = window.inner_size().map_err(failed)?;
        Ok(self.below_inset(content.to_logical::<f64>(scale).height))
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

/// Whether one more link may reach the owner's browser now: at most one per `LINK_GAP`,
/// so no page in the IDE can flood the browser with tabs. Inputs: the time of the last
/// link sent and now. Output: true when this link goes, which then becomes the last.
fn link_admitted(last: &Mutex<Option<Instant>>, now: Instant) -> bool {
    let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
    if last.is_some_and(|at| now.saturating_duration_since(at) < LINK_GAP) {
        return false;
    }
    *last = Some(now);
    true
}

/// Hands a link the IDE opened as a new window, which WebKit allows only on a click, to the
/// owner's browser, at most once a second. Input: the URL.
fn outside(url: &Url) {
    if matches!(url.scheme(), "http" | "https") && link_admitted(&LINKS, Instant::now()) {
        let link = url.to_string();
        tauri::async_runtime::spawn(async move {
            let _ = open::open_link(&link).await;
        });
    }
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
    opening: HashMap<String, u64>,
    asks: u64,
    engine: HashMap<String, Order>,
}

/// One project's place in the queue of engine opens and stops: the turn each holds while the
/// engine runs it, and the ticket of the project's newest open.
struct Order {
    turn: Arc<tokio::sync::Mutex<()>>,
    begun: u64,
}

/// A project page's close waiting to stop its IDE server: the project's turn and the last
/// ticket handed out when the page closed.
pub struct Closing {
    turn: Arc<tokio::sync::Mutex<()>>,
    epoch: u64,
}

impl Held {
    /// Takes an open's ticket when it is still the project's newest and no hide or close
    /// came since. Inputs: the project and the ticket. Output: true when it may show.
    fn admit(&mut self, project: &ProjectId, ticket: u64) -> bool {
        if self.opening.get(project.as_str()) != Some(&ticket) {
            return false;
        }
        self.opening.remove(project.as_str());
        true
    }

    /// Gives up the project's open still waiting for the engine. Input: the project.
    fn cancel(&mut self, project: &ProjectId) {
        self.opening.remove(project.as_str());
    }
}

/// The IDE webviews of the main window: each project's webview label and the workbench URL
/// it loaded, the project shown now, a count that keeps a replaced webview's label unique
/// while the old one closes, and the open of each project still waiting for the engine.
#[derive(Default)]
pub struct IdeViews(Mutex<Held>);

impl IdeViews {
    fn lock(&self) -> MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether any project's IDE webview is loaded, shown or hidden. Only a loaded IDE's
    /// extension listens for reveals, so without one the engine has no focus to hand over.
    /// Output: true while one is loaded.
    pub fn any_loaded(&self) -> bool {
        !self.lock().loaded.is_empty()
    }

    /// Starts one open of a project's IDE before the engine is asked, so a hide or close
    /// that comes first stops it from showing. Input: the project. Output: its ticket and
    /// the turn it holds while the engine opens the project's server, so a stop from a page
    /// close runs wholly before or wholly after it.
    pub fn begin(&self, project: &ProjectId) -> (u64, Arc<tokio::sync::Mutex<()>>) {
        let mut held = self.lock();
        held.asks += 1;
        let ticket = held.asks;
        held.opening.insert(project.as_str().to_owned(), ticket);
        let order = held
            .engine
            .entry(project.as_str().to_owned())
            .or_insert_with(|| Order {
                turn: Arc::default(),
                begun: ticket,
            });
        order.begun = ticket;
        (ticket, Arc::clone(&order.turn))
    }

    /// Whether this project's IDE webview is loaded, shown or hidden. Input: the project.
    /// Output: true while it is loaded.
    pub fn loaded(&self, project: &ProjectId) -> bool {
        self.lock().loaded.contains_key(project.as_str())
    }

    /// Notes a project page's close, before any await, when an open asked the engine for
    /// that project's server since the last close. Input: the project. Output: the close
    /// to stop the server with, or none when no open asked for one.
    pub fn closing(&self, project: &ProjectId) -> Option<Closing> {
        let held = self.lock();
        let order = held.engine.get(project.as_str())?;
        Some(Closing {
            turn: Arc::clone(&order.turn),
            epoch: held.asks,
        })
    }

    /// Runs a closed page's server stop in the project's turn, unless an open of that
    /// project began after the close and wants the server, then forgets the project's order
    /// once nothing else holds it. Inputs: the project, its close and the stop. Output: the
    /// stop's answer, or false when it was skipped.
    pub async fn stop_closed<F, S>(
        &self,
        project: &ProjectId,
        closing: Closing,
        stop: S,
    ) -> Result<bool, BridgeError>
    where
        S: FnOnce() -> F,
        F: std::future::Future<Output = Result<bool, BridgeError>>,
    {
        let turn = closing.turn.lock().await;
        let reopened = self
            .lock()
            .engine
            .get(project.as_str())
            .is_some_and(|order| order.begun > closing.epoch);
        let stopped = if reopened { Ok(false) } else { stop().await };
        drop(turn);
        let mut held = self.lock();
        let idle = held.engine.get(project.as_str()).is_some_and(|order| {
            Arc::ptr_eq(&order.turn, &closing.turn)
                && order.begun <= closing.epoch
                && Arc::strong_count(&order.turn) == 2
        });
        if idle {
            held.engine.remove(project.as_str());
        }
        stopped
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

fn restore_menu<R: Runtime>(app: &AppHandle<R>) -> Result<(), BridgeError> {
    app.set_menu(Menu::default(app).map_err(failed)?)
        .map_err(failed)?;
    Ok(())
}

/// Shows a project's IDE over its panel: the loaded webview when its workbench is the same,
/// else a new child webview with the input guard in every frame, a navigation lock that only
/// refuses, and links opened as new windows sent to the browser. Any other project's IDE is
/// hidden, and the app menu gives up Cmd+Z, W, N, P, F and comma before the IDE takes focus.
/// Inputs: the app, the main window, the views, the project, its workbench, the panel and
/// the open's ticket. Output: Ok once shown, or `ide_open_cancelled` when a hide or close or a
/// newer open came while the engine opened it.
pub fn show(
    app: &AppHandle,
    window: &Window,
    views: &IdeViews,
    project: &ProjectId,
    workbench: Workbench,
    bounds: Bounds,
    ticket: u64,
) -> Result<(), BridgeError> {
    let bounds = bounds.in_window(window)?;
    let mut held = views.lock();
    if !held.admit(project, ticket) {
        return Err(refused("ide_open_cancelled"));
    }
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
                .initialization_script_for_all_frames(GUARD)
                .on_navigation(move |next| navigation_allowed(next, &server, port))
                .on_new_window(|next, _| {
                    outside(&next);
                    NewWindowResponse::Deny
                });
            let webview = window
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
    if let Err(error) = ide_menu(app).and_then(|menu| app.set_menu(menu)) {
        held.shown = None;
        webview.hide().map_err(failed)?;
        return Err(failed(error));
    }
    #[cfg(feature = "native-webdriver")]
    crate::ide_probe::ask_once(webview.clone());
    webview.set_focus().map_err(failed)?;
    Ok(())
}

/// Moves the shown IDE webview over its panel after the panel moved or resized. Inputs: the
/// app, the views, the project and the panel. Output: Ok, also when that IDE is not shown.
pub fn place<R: Runtime>(
    app: &AppHandle<R>,
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
        let bounds = bounds.in_window(&webview.window())?;
        webview.set_bounds(bounds.rect()).map_err(failed)?;
    }
    Ok(())
}

/// Hides a project's IDE webview when its tab is left or an overlay covers it, keeping it
/// loaded, stops an open of it still waiting for the engine, and gives the app its own menu
/// back. Inputs: the app, the views and the project. Output: Ok.
pub fn hide<R: Runtime>(
    app: &AppHandle<R>,
    views: &IdeViews,
    project: &ProjectId,
) -> Result<(), BridgeError> {
    let mut held = views.lock();
    held.cancel(project);
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

/// Closes a project's IDE webview when the project page closes, stops an open of it still
/// waiting for the engine, and gives the app its own menu back if that IDE was shown.
/// Inputs: the app, the views and the project. Output: Ok.
pub fn close<R: Runtime>(
    app: &AppHandle<R>,
    views: &IdeViews,
    project: &ProjectId,
) -> Result<(), BridgeError> {
    let mut held = views.lock();
    held.cancel(project);
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
    fn a_panel_is_counted_from_the_content_view_and_not_from_the_page() {
        let panel = Bounds {
            x: 244.0,
            y: 144.0,
            width: 1012.0,
            height: 678.0,
            viewport: 838.0,
        };
        assert_eq!(
            panel.below_inset(870.0),
            Bounds { y: 176.0, ..panel },
            "the page starts 32 below the top of a content view that reaches under the title bar"
        );
        assert_eq!(
            panel.below_inset(838.0),
            panel,
            "a page that fills the content view is already counted from its top"
        );
        assert_eq!(
            panel.below_inset(800.0),
            panel,
            "a page caught mid-resize taller than the content view moves nothing"
        );
    }

    #[test]
    fn panel_bounds_must_be_finite_on_screen_and_visible() {
        let fine = Bounds {
            x: 0.0,
            y: 81.0,
            width: 900.0,
            height: 601.0,
            viewport: 720.0,
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
            Bounds {
                viewport: 0.0,
                ..fine
            },
            Bounds {
                viewport: f64::NAN,
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

    #[test]
    fn an_open_the_tab_gave_up_on_is_refused_and_only_the_newest_open_shows() {
        let views = IdeViews::default();
        let alpha = ProjectId::new("alpha").unwrap();
        let beta = ProjectId::new("beta").unwrap();
        let (first, _) = views.begin(&alpha);
        let (second, _) = views.begin(&alpha);
        assert!(second > first);
        assert!(
            !views.lock().admit(&alpha, first),
            "a replaced open never shows"
        );
        assert!(views.lock().admit(&alpha, second));
        assert!(!views.lock().admit(&alpha, second), "a ticket shows once");
        let (hidden, _) = views.begin(&alpha);
        views.lock().cancel(&alpha);
        assert!(
            !views.lock().admit(&alpha, hidden),
            "hide or close before the engine answered"
        );
        let (other, _) = views.begin(&beta);
        views.lock().cancel(&alpha);
        assert!(
            views.lock().admit(&beta, other),
            "cancelling one project leaves the other"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_closed_page_stops_its_server_in_turn_unless_a_newer_open_wants_it() {
        use crate::bridge::NativeBridge;
        use crazyagent::engine::bootstrap::EngineService;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicBool, Ordering};

        let root = tempfile::Builder::new()
            .prefix("ca-dide-stop-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &root.path().join("g"),
            &root.path().join("p"),
            &stopped,
        )
        .await
        .unwrap();
        let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
        bridge.connect().await.unwrap();
        let bridge = Arc::new(bridge);
        let views = Arc::new(IdeViews::default());
        let alpha = ProjectId::new("alpha").unwrap();
        assert!(
            views.closing(&alpha).is_none(),
            "a page that never asked for a server stops nothing"
        );

        let (_, open) = views.begin(&alpha);
        let opening = open.lock().await;
        let closing = views.closing(&alpha).unwrap();
        let asked = Arc::new(AtomicBool::new(false));
        let stop = tokio::spawn({
            let (views, bridge, asked, alpha) = (
                Arc::clone(&views),
                Arc::clone(&bridge),
                Arc::clone(&asked),
                alpha.clone(),
            );
            async move {
                views
                    .stop_closed(&alpha, closing, || async {
                        asked.store(true, Ordering::SeqCst);
                        let call = bridge.ide_stop(alpha.clone())?;
                        call.await
                    })
                    .await
                    .map_err(|error| error.code)
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!stop.is_finished(), "the stop waits while the open runs");
        assert!(
            !asked.load(Ordering::SeqCst),
            "the engine is not asked to stop before the open ends"
        );
        drop(opening);
        drop(open);
        assert_eq!(
            stop.await.unwrap(),
            Ok(false),
            "then the engine is asked, and with no server running it stopped none"
        );
        assert!(asked.load(Ordering::SeqCst));
        assert!(
            views.closing(&alpha).is_none(),
            "a stopped project is forgotten"
        );

        views.begin(&alpha);
        let closing = views.closing(&alpha).unwrap();
        views.begin(&alpha);
        let asked = AtomicBool::new(false);
        assert_eq!(
            views
                .stop_closed(&alpha, closing, || async {
                    asked.store(true, Ordering::SeqCst);
                    let call = bridge.ide_stop(alpha.clone())?;
                    call.await
                })
                .await
                .map_err(|error| error.code),
            Ok(false),
            "an open that began after the close keeps its server"
        );
        assert!(
            !asked.load(Ordering::SeqCst),
            "and the engine is never asked to stop it"
        );
        assert!(
            views.closing(&alpha).is_some(),
            "the newer open's order stays"
        );

        let closing = views.closing(&alpha).unwrap();
        let detached = NativeBridge::new(Some(root.path().join("e.sock")));
        assert_eq!(
            views
                .stop_closed(&alpha, closing, || async {
                    let call = detached.ide_stop(alpha.clone())?;
                    call.await
                })
                .await
                .map_err(|error| error.code),
            Err("not_connected"),
            "a refused stop reaches the caller"
        );
        assert!(views.closing(&alpha).is_none());
        service.shutdown().await.unwrap();
    }

    #[test]
    fn links_reach_the_browser_at_most_once_a_second() {
        let last = Mutex::new(None);
        let now = Instant::now();
        assert!(link_admitted(&last, now));
        assert!(!link_admitted(&last, now + Duration::from_millis(500)));
        assert!(!link_admitted(&last, now + Duration::from_millis(999)));
        assert!(link_admitted(&last, now + LINK_GAP));
        assert!(!link_admitted(&last, now + LINK_GAP));
    }

    #[test]
    fn an_ide_counts_as_loaded_from_its_first_show_until_it_closes() {
        let views = IdeViews::default();
        assert!(!views.any_loaded(), "no IDE webview yet");
        views.lock().loaded.insert(
            "alpha".to_owned(),
            Loaded {
                label: "ide-616c706861-1".to_owned(),
                url: Url::parse("http://127.0.0.1:41000/").unwrap(),
            },
        );
        assert!(views.any_loaded(), "a hidden IDE still listens for reveals");
        views.lock().loaded.remove("alpha");
        assert!(!views.any_loaded());
    }
}
