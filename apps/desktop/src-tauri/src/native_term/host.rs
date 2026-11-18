use super::clip::Clip;
use super::codec;
use super::control::{Control, Feed, Want, WantKind};
use super::pane::{self, Effect, Input, Named, Pane, Probe};
use super::render::{Shared, Surface};
use super::screen;
use super::view::{MouseKind, TermView};
use super::{HookEvent, HookInput, Rect, Report};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSView, NSWindowOrderingMode};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::ipc::Channel;

/// The most native views open at once across all windows, as many as a team wall shows.
const VIEWS_MAX: usize = 32;

/// The most feeds waiting for the main thread before the client counts as stuck.
const FEEDS_MAX: usize = 1 << 20;

/// How long a command from another thread waits for the main thread.
const MAIN_WAIT: Duration = Duration::from_secs(5);

thread_local! {
    static NATIVE: RefCell<Option<Native>> = const { RefCell::new(None) };
}

/// The app, kept so a drain that found the state in use can run again from the main loop.
static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// The owner's general pasteboard for real Cmd-C and Cmd-V, and the private one the test
/// hook copies to.
pub struct Clips {
    pub user: Clip,
    pub probe: Clip,
}

/// Feeds from the control client's threads waiting for the main thread, and whether a
/// drain is already scheduled, so a burst of output costs one main-thread wake.
struct Inbox {
    queue: Mutex<VecDeque<Feed>>,
    scheduled: AtomicBool,
}

/// One terminal panel drawn natively: its pane mirror, its NSView above the webview, the
/// webview its page rectangle is measured in, and the channel its timings go back on.
struct View {
    pane: Pane,
    ns: Retained<TermView>,
    webview: Retained<NSView>,
    window: String,
    surface: Option<Surface>,
    output: Channel<Report>,
    rect: Rect,
    covered: bool,
    wheel_carry: f64,
}

/// Everything the native terminals hold on the main thread.
struct Native {
    app: tauri::AppHandle,
    control: Option<Control>,
    inbox: Arc<Inbox>,
    views: HashMap<u64, View>,
    by_pane: HashMap<String, Vec<u64>>,
    next: u64,
    shared: Option<Shared>,
    clips: Clips,
}

/// Runs work on the main thread: at once when already on it, else through Tauri's main
/// loop, waiting at most `MAIN_WAIT`. Inputs: the app and the work. Output: its result, or
/// `native_terminal_main_busy`.
pub fn on_main<T: Send + 'static>(
    app: &tauri::AppHandle,
    work: impl FnOnce(MainThreadMarker) -> T + Send + 'static,
) -> Result<T, String> {
    if let Some(mtm) = MainThreadMarker::new() {
        return Ok(work(mtm));
    }
    assert!(MainThreadMarker::new().is_none());
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let _ = sender.send(work(mtm));
    })
    .map_err(|_| "native_terminal_main_busy".to_owned())?;
    receiver
        .recv_timeout(MAIN_WAIT)
        .map_err(|_| "native_terminal_main_busy".to_owned())
}

/// Runs work on the native state when it exists and is not already in use. Input: the
/// work. Output: its result, or none.
fn with_native<T>(work: impl FnOnce(&mut Native) -> T) -> Option<T> {
    assert!(MainThreadMarker::new().is_some());
    NATIVE.with(|cell| {
        let mut held = cell.try_borrow_mut().ok()?;
        held.as_mut().map(work)
    })
}

/// The engine socket the switch names, checked, and the engine root that holds it. Output:
/// (socket, root), or a plain code.
fn engine() -> Result<(PathBuf, PathBuf), String> {
    let raw = std::env::var_os(super::SOCKET_ENV).unwrap_or_default();
    let socket: PathBuf = super::engine_socket(&raw).map_err(str::to_owned)?;
    let root = socket
        .parent()
        .and_then(|terminal| terminal.parent())
        .ok_or("native_terminal_socket_invalid")?
        .to_path_buf();
    assert!(root.join("engine.db").is_file());
    assert!(socket.starts_with(&root));
    Ok((socket, root))
}

/// Starts the control client on the checked engine socket, with the tmux the engine uses.
/// Inputs: the app and the shared inbox. Output: the client, or a plain code.
fn connect(app: &tauri::AppHandle, inbox: &Arc<Inbox>) -> Result<Control, String> {
    let (socket, root) = engine()?;
    let root = root.as_path();
    let tmux = crazyagent::terminal::tmux::installed(root).ok_or("native_terminal_tmux_missing")?;
    assert!(tmux.is_absolute());
    assert_eq!(root.join("terminal").join("tmux.sock"), socket);
    let (app, inbox) = (app.clone(), Arc::clone(inbox));
    let sink = move |feed: Feed| {
        let mut queue = inbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queue.len() >= FEEDS_MAX {
            return false;
        }
        queue.push_back(feed);
        drop(queue);
        if !inbox.scheduled.swap(true, Ordering::AcqRel) {
            let _ = app.run_on_main_thread(drain);
        }
        true
    };
    Control::attach(&tmux, &socket, sink).map_err(|_| "native_terminal_attach_failed".to_owned())
}

/// Takes every waiting feed on the main thread and applies it, then draws what changed. When
/// the state is in use further up this thread's stack, the feeds stay queued, the drain stays
/// marked scheduled, and a helper thread queues it on the main loop again: Tauri runs a task
/// sent from the main thread at once, which would re-enter here while the state is held.
fn drain() {
    assert!(MainThreadMarker::new().is_some());
    let applied = with_native(|native| {
        native.inbox.scheduled.store(false, Ordering::Release);
        let feeds: Vec<Feed> = {
            let mut queue = native
                .inbox
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            queue.drain(..).collect()
        };
        for feed in feeds {
            native.feed(feed);
        }
        native.draw_dirty();
    });
    if applied.is_none()
        && let Some(app) = APP.get().cloned()
    {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1));
            assert!(MainThreadMarker::new().is_none());
            let _ = app.run_on_main_thread(drain);
        });
    }
}

/// Opens one native view for an engine-server agent window above the calling page's own
/// webview, and asks tmux for the pane. Inputs: the app, the window's label and its
/// WKWebView pointer, the checked window name, the page rectangle and the timing channel.
/// Output: the view id, or a plain code.
pub fn open(
    app: &tauri::AppHandle,
    window: (String, usize),
    name: String,
    rect: Rect,
    output: Channel<Report>,
) -> Result<u64, String> {
    assert!(rect.valid());
    assert!(super::window_name(&name).is_ok());
    let app_clone = app.clone();
    let _ = APP.set(app.clone());
    on_main(app, move |mtm| {
        NATIVE.with(|cell| {
            let mut held = cell
                .try_borrow_mut()
                .map_err(|_| "native_terminal_main_busy".to_owned())?;
            if held.is_none() {
                let inbox = Arc::new(Inbox {
                    queue: Mutex::new(VecDeque::new()),
                    scheduled: AtomicBool::new(false),
                });
                let clips = Clips {
                    user: Clip::new(false),
                    probe: Clip::new(true),
                };
                *held = Some(Native {
                    app: app_clone,
                    control: None,
                    inbox,
                    views: HashMap::new(),
                    by_pane: HashMap::new(),
                    next: 1,
                    shared: None,
                    clips,
                });
            }
            let native = held.as_mut().ok_or("native_terminal_main_busy")?;
            native.open(mtm, window, &name, rect, output)
        })
    })?
}

impl Native {
    fn open(
        &mut self,
        mtm: MainThreadMarker,
        window: (String, usize),
        name: &str,
        rect: Rect,
        output: Channel<Report>,
    ) -> Result<u64, String> {
        if self.views.len() >= VIEWS_MAX {
            return Err("native_terminal_views_full".into());
        }
        let (_, root) = engine()?;
        if super::own_server(&root, name) {
            return Err("native_terminal_own_server".into());
        }
        if self.control.is_none() {
            self.control = Some(connect(&self.app, &self.inbox)?);
        }
        let pointer = window.1 as *mut NSView;
        let webview =
            unsafe { Retained::retain(pointer) }.ok_or("native_terminal_webview_missing")?;
        let class = objc2::runtime::AnyClass::get(c"WKWebView");
        assert!(class.is_some_and(|class| webview.isKindOfClass(class)));
        let host = unsafe { webview.superview() }.ok_or("native_terminal_webview_missing")?;
        let id = self.next;
        self.next += 1;
        let ns = TermView::new(mtm, id);
        ns.setHidden(true);
        host.addSubview_positioned_relativeTo(&ns, NSWindowOrderingMode::Above, Some(&*webview));
        let mut pane = Pane::new(id, name);
        let control = self
            .control
            .as_mut()
            .ok_or("native_terminal_attach_failed")?;
        if pane.request(control).is_err() {
            ns.removeFromSuperview();
            return Err("native_terminal_attach_failed".into());
        }
        crazyagent::hop_trace::hop("native_open", 0, name);
        let view = View {
            pane,
            ns,
            webview,
            window: window.0,
            surface: None,
            output,
            rect,
            covered: false,
            wheel_carry: 0.0,
        };
        self.views.insert(id, view);
        assert!(self.views.len() <= VIEWS_MAX);
        assert!(self.next > id);
        Ok(id)
    }

    fn feed(&mut self, feed: Feed) {
        match feed {
            Feed::Output { pane, bytes, .. } => {
                crazyagent::hop_trace::hop("native_output", bytes.len(), &pane);
                let ids = self.by_pane.get(&pane).cloned().unwrap_or_default();
                assert!(ids.len() <= VIEWS_MAX);
                for id in ids {
                    if let Some(view) = self.views.get_mut(&id) {
                        view.pane.on_output(&bytes);
                    }
                }
            }
            Feed::Reply {
                want, lines, ok, ..
            } => self.reply(want, &lines, ok),
            Feed::Written { view, seq, .. } => {
                if let Some(view) = self.views.get_mut(&view) {
                    view.pane.on_written(seq);
                }
            }
            Feed::Closed => {
                self.control = None;
                for view in self.views.values() {
                    let _ = view.output.send(Report::Ended {
                        code: "native_terminal_tmux_closed".into(),
                    });
                }
                assert!(self.control.is_none());
            }
        }
    }

    fn reply(&mut self, want: Want, lines: &[Vec<u8>], ok: bool) {
        let Some(view) = self.views.get_mut(&want.view) else {
            return;
        };
        match want.kind {
            WantKind::Startup | WantKind::Command => {}
            WantKind::State => match view.pane.on_state(lines, ok) {
                Ok(pane) => {
                    assert!(codec::pane_id_valid(&pane));
                    let list = self.by_pane.entry(pane).or_default();
                    list.push(want.view);
                    assert!(list.len() <= VIEWS_MAX);
                }
                Err(code) => {
                    let _ = view.output.send(Report::Ended { code: code.into() });
                }
            },
            WantKind::Capture => {
                if view.pane.on_capture(lines, ok) {
                    let _ = view.output.send(Report::Ready);
                    self.layout(want.view);
                }
            }
            WantKind::Paste => view.pane.on_paste_reply(ok),
        }
    }

    /// Puts a view over its page rectangle, sizes its surface and the agent's window to the
    /// grid that fits, and shows it once its mirror is ready and nothing covers it. Input: the
    /// view id.
    fn layout(&mut self, id: u64) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let rect = view.rect;
        let shown = view.pane.ready && rect.w >= 1.0 && rect.h >= 1.0 && !view.covered;
        view.ns.setHidden(!shown);
        if !shown {
            return;
        }
        assert!(rect.valid() && rect.w >= 1.0 && rect.h >= 1.0);
        let css = NSRect::new(NSPoint::new(rect.x, rect.y), NSSize::new(rect.w, rect.h));
        let host = unsafe { view.ns.superview() };
        let frame = view.webview.convertRect_toView(css, host.as_deref());
        view.ns.setFrame(frame);
        let scale = view.ns.window().map_or(2.0, |w| w.backingScaleFactor()) as f32;
        let shared = self.shared.get_or_insert_with(|| Shared::new(scale));
        let pixels = (
            (rect.w * f64::from(scale)) as u32,
            (rect.h * f64::from(scale)) as u32,
        );
        match view.surface.as_mut() {
            Some(surface) => shared.resize(surface, pixels.0, pixels.1),
            None => {
                let pointer = Retained::as_ptr(&view.ns) as *mut c_void;
                match shared.surface(pointer, pixels.0, pixels.1) {
                    Ok(surface) => view.surface = Some(surface),
                    Err(error) => {
                        let _ = view.output.send(Report::Ended {
                            code: format!("native_terminal_gpu: {error}"),
                        });
                        return;
                    }
                }
            }
        }
        let cell = (f64::from(shared.cell.0), f64::from(shared.cell.1));
        let grid = codec::grid_for(
            pixels.0,
            pixels.1,
            cell,
            f64::from(shared.pad),
            pane::GRID_MAX,
        );
        if grid.0 >= 2
            && grid.1 >= 2
            && let Some(control) = self.control.as_mut()
        {
            let _ = view.pane.resize(control, grid);
        }
        view.pane.dirty = true;
        assert!(pixels.0 > 0 && pixels.1 > 0);
    }

    /// Draws every shown view whose screen changed and sends back the timings its frame
    /// finished.
    fn draw_dirty(&mut self) {
        let Some(shared) = self.shared.as_mut() else {
            return;
        };
        for view in self.views.values_mut() {
            if !view.pane.dirty || view.ns.isHidden() {
                continue;
            }
            let (Some(surface), Some(term)) = (view.surface.as_mut(), view.pane.term.as_ref())
            else {
                continue;
            };
            let frame = screen::frame(term, None);
            view.pane.dirty = false;
            assert!(!view.ns.isHidden());
            let Ok(timing) = shared.draw(surface, &frame) else {
                continue;
            };
            assert!(timing.build_ms >= 0.0 && timing.acquire_ms >= 0.0);
            let now = pane::wall_ms();
            let label = view.pane.pane_id().unwrap_or_default();
            crazyagent::hop_trace::hop("native_frame", 0, &label);
            for mut probe in view.pane.on_frame(now) {
                probe.build_ms = Some(timing.build_ms);
                probe.acquire_ms = Some(timing.acquire_ms);
                let _ = view.output.send(Report::Probe(probe));
            }
        }
    }

    fn cell_at(&self, id: u64, point: (f64, f64)) -> (usize, usize) {
        let (Some(view), Some(shared)) = (self.views.get(&id), self.shared.as_ref()) else {
            return (0, 0);
        };
        let Some(term) = view.pane.term.as_ref() else {
            return (0, 0);
        };
        use alacritty_terminal::grid::Dimensions;
        let scale = f64::from(shared.scale);
        assert!((1.0..=4.0).contains(&scale));
        let cell = (f64::from(shared.cell.0), f64::from(shared.cell.1));
        let at = codec::cell_at(
            point.0 * scale,
            point.1 * scale,
            cell,
            f64::from(shared.pad),
            term.columns(),
            term.screen_lines(),
        );
        assert!(at.0 < term.columns() && at.1 < term.screen_lines());
        at
    }

    /// Applies one input to a view and, when timed, tracks it until its frame. Inputs: the
    /// view id, the input and the probe to track, if any.
    fn apply(&mut self, id: u64, input: Input, probe: Option<Probe>) {
        assert!(self.views.len() <= VIEWS_MAX);
        let Some(control) = self.control.as_mut() else {
            return;
        };
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let seq = probe.as_ref().map(|p| p.seq);
        assert!(seq.is_none_or(|seq| seq > 0));
        if let Some(probe) = probe
            && let Some(dropped) = view.pane.track(probe)
        {
            let _ = view.output.send(Report::Probe(dropped));
        }
        let effect = view
            .pane
            .apply(control, input, &self.clips)
            .unwrap_or(Effect::Nothing);
        if let Effect::Sent(batch) = effect {
            crazyagent::hop_trace::hop("native_send", 0, &format!("{}-{batch}", view.pane.view));
        }
        if let Some(seq) = seq {
            view.pane.stamp(seq, &effect);
        }
        self.draw_dirty();
    }
}

/// The input a page key name makes: Enter, Backspace, Tab, Escape, the named keys, and
/// Control with one letter. Inputs: the `KeyboardEvent.key` and whether Control is held.
/// Output: the input, or none for keys the hook does not send.
pub fn page_key(key: &str, ctrl: bool) -> Option<Input> {
    let named = match key {
        "ArrowUp" => Some(Named::Up),
        "ArrowDown" => Some(Named::Down),
        "ArrowLeft" => Some(Named::Left),
        "ArrowRight" => Some(Named::Right),
        "Home" => Some(Named::Home),
        "End" => Some(Named::End),
        "PageUp" => Some(Named::PageUp),
        "PageDown" => Some(Named::PageDown),
        "Delete" => Some(Named::Delete),
        _ => None,
    };
    if let Some(key) = named {
        return Some(Input::Key(key));
    }
    let byte = match key {
        "Enter" => Some(b'\r'),
        "Backspace" => Some(0x7f),
        "Tab" => Some(b'\t'),
        "Escape" => Some(0x1b),
        _ if ctrl && key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() => {
            Some(key.as_bytes()[0].to_ascii_lowercase() & 0x1f)
        }
        _ => None,
    };
    assert!(byte.is_none_or(|b| b < 0x80));
    assert!(byte.is_none() || !key.is_empty());
    byte.map(|b| Input::Text(vec![b]))
}

/// Applies one test-hook event from the calling window's page to its view, timed from the
/// page's dispatch. Inputs: the app, the window label, the view id and the event. Output:
/// unit, or a plain code.
pub fn input(
    app: &tauri::AppHandle,
    window: String,
    id: u64,
    event: HookEvent,
) -> Result<(), String> {
    assert!(event.valid());
    if event
        .expect
        .as_ref()
        .is_some_and(|t| t.chars().count() > pane::EXPECT_CHARS_MAX)
    {
        return Err("native_terminal_expect_long".into());
    }
    on_main(app, move |_| {
        with_native(|native| {
            let owned = native.views.get(&id).is_some_and(|v| v.window == window);
            if !owned {
                return Err("native_terminal_view_unknown".to_owned());
            }
            let (kind, input) = native.hook_input(id, event.input);
            let bytes = match &input {
                Some(Input::Text(b)) => b.len(),
                Some(Input::Paste { text }) => text.len(),
                _ => 0,
            };
            assert!(bytes <= codec::COPY_BYTES_MAX);
            let probe = Probe {
                seq: event.seq,
                kind: kind.into(),
                t0: event.t0,
                bytes,
                expect: event.expect,
                ..Probe::default()
            };
            match input {
                Some(input) => native.apply(id, input, Some(probe)),
                None => {
                    let failed = Probe {
                        failed: true,
                        ..probe
                    };
                    if let Some(view) = native.views.get(&id) {
                        let _ = view.output.send(Report::Probe(failed));
                    }
                }
            }
            Ok(())
        })
        .unwrap_or_else(|| Err("native_terminal_main_busy".to_owned()))
    })?
}

impl Native {
    fn hook_input(&mut self, id: u64, input: HookInput) -> (&'static str, Option<Input>) {
        let scale = self.shared.as_ref().map_or(2.0, |s| f64::from(s.scale));
        let line_px = self.shared.as_ref().map_or(30.0, |s| f64::from(s.cell.1));
        assert!((1.0..=4.0).contains(&scale));
        assert!(line_px > 0.0);
        match input {
            HookInput::Text { text } => (
                "text",
                (!text.is_empty()).then(|| Input::Text(text.into_bytes())),
            ),
            HookInput::Key { key, ctrl } => ("key", page_key(&key, ctrl)),
            HookInput::Copy => ("copy", Some(Input::Copy { probe: true })),
            HookInput::Paste { text } => ("paste", Some(Input::Paste { text })),
            HookInput::Wheel { dy, mode, x, y } => {
                let cell = self.cell_at(id, (x, y));
                let Some(view) = self.views.get_mut(&id) else {
                    return ("wheel", None);
                };
                let lines = if mode == 0 {
                    let (lines, carry) = codec::wheel_lines(-dy * scale, view.wheel_carry, line_px);
                    view.wheel_carry = carry;
                    lines
                } else {
                    (-dy).round() as i32
                };
                ("wheel", Some(Input::Wheel { lines, cell }))
            }
            HookInput::Press { x, y, option } => (
                "press",
                Some(Input::Press {
                    cell: self.cell_at(id, (x, y)),
                    option,
                }),
            ),
            HookInput::Drag { x, y, option } => (
                "drag",
                Some(Input::Drag {
                    cell: self.cell_at(id, (x, y)),
                    option,
                }),
            ),
            HookInput::Release { x, y, option } => (
                "release",
                Some(Input::Release {
                    cell: self.cell_at(id, (x, y)),
                    option,
                }),
            ),
        }
    }
}

/// Moves a view to a new page rectangle. Inputs: the app, the window label, the view id
/// and the rectangle. Output: unit, or a plain code.
pub fn place(app: &tauri::AppHandle, window: String, id: u64, rect: Rect) -> Result<(), String> {
    assert!(rect.valid());
    on_main(app, move |_| {
        with_native(|native| {
            let Some(view) = native.views.get_mut(&id).filter(|v| v.window == window) else {
                return Err("native_terminal_view_unknown".to_owned());
            };
            view.rect = rect;
            assert!(view.rect == rect);
            native.layout(id);
            native.draw_dirty();
            Ok(())
        })
        .unwrap_or_else(|| Err("native_terminal_main_busy".to_owned()))
    })?
}

/// Hides or shows a view while a page overlay covers it. Inputs: the app, the window label,
/// the view id and whether it is covered. Output: unit, or a plain code.
pub fn cover(app: &tauri::AppHandle, window: String, id: u64, covered: bool) -> Result<(), String> {
    on_main(app, move |_| {
        with_native(|native| {
            let Some(view) = native.views.get_mut(&id).filter(|v| v.window == window) else {
                return Err("native_terminal_view_unknown".to_owned());
            };
            view.covered = covered;
            native.layout(id);
            native.draw_dirty();
            Ok(())
        })
        .unwrap_or_else(|| Err("native_terminal_main_busy".to_owned()))
    })?
}

/// Closes views: one by id, or every view of a window whose page reloaded. When none is
/// left the control client detaches. Inputs: the app, the window label and the view id, or
/// none for all of that window's views. Output: unit, or a plain code.
pub fn close(app: &tauri::AppHandle, window: String, id: Option<u64>) -> Result<(), String> {
    on_main(app, move |_| {
        with_native(|native| {
            let ids: Vec<u64> = native
                .views
                .iter()
                .filter(|(key, view)| view.window == window && id.is_none_or(|id| id == **key))
                .map(|(key, _)| *key)
                .collect();
            for key in &ids {
                if let Some(view) = native.views.remove(key) {
                    view.ns.removeFromSuperview();
                }
                for list in native.by_pane.values_mut() {
                    list.retain(|view| view != key);
                }
            }
            native.by_pane.retain(|_, list| !list.is_empty());
            if native.views.is_empty() {
                native.control = None;
            }
            assert!(
                native
                    .by_pane
                    .values()
                    .flatten()
                    .all(|key| !ids.contains(key))
            );
            assert!(native.views.keys().all(|key| !ids.contains(key)));
        })
        .ok_or_else(|| "native_terminal_main_busy".to_owned())
    })?
}

/// Applies a key or text the native view got from AppKit. Inputs: the view id, the input
/// and the event's timestamp.
pub fn on_native(id: u64, input: Input, _timestamp: f64) {
    with_native(|native| native.apply(id, input, None));
}

/// Copies the view's selection to the owner's pasteboard (Cmd-C or Edit > Copy). Inputs:
/// the view id and the event's timestamp.
pub fn on_copy(id: u64, _timestamp: f64) {
    with_native(|native| native.apply(id, Input::Copy { probe: false }, None));
}

/// Pastes the owner's pasteboard text into the view's pane (Cmd-V or Edit > Paste). Inputs:
/// the view id and the event's timestamp.
pub fn on_paste(id: u64, _timestamp: f64) {
    with_native(|native| {
        let Some(text) = native.clips.user.read() else {
            return;
        };
        native.apply(id, Input::Paste { text }, None);
    });
}

/// Applies a wheel event in view points, carrying fractions of a line for precise devices.
/// Inputs: the view id, the point, the delta (up is positive), whether it is in pixels and
/// the event's timestamp.
pub fn on_wheel(id: u64, point: (f64, f64), delta: f64, precise: bool, _timestamp: f64) {
    with_native(|native| {
        let cell = native.cell_at(id, point);
        let scale = native.shared.as_ref().map_or(2.0, |s| f64::from(s.scale));
        let line_px = native.shared.as_ref().map_or(30.0, |s| f64::from(s.cell.1));
        assert!(line_px > 0.0);
        let Some(view) = native.views.get_mut(&id) else {
            return;
        };
        let lines = if precise {
            let (lines, carry) = codec::wheel_lines(delta * scale, view.wheel_carry, line_px);
            view.wheel_carry = carry;
            lines
        } else {
            delta.round() as i32
        };
        native.apply(id, Input::Wheel { lines, cell }, None);
    });
}

/// Applies a mouse press, drag or release in view points. Inputs: the view id, the kind,
/// the point, whether Option is held and the event's timestamp.
pub fn on_mouse(id: u64, kind: MouseKind, point: (f64, f64), option: bool, _timestamp: f64) {
    with_native(|native| {
        let cell = native.cell_at(id, point);
        let input = match kind {
            MouseKind::Press => Input::Press { cell, option },
            MouseKind::Drag => Input::Drag { cell, option },
            MouseKind::Release => Input::Release { cell, option },
        };
        native.apply(id, input, None);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_keys_map_to_inputs() {
        assert!(matches!(page_key("Enter", false), Some(Input::Text(b)) if b == b"\r"));
        assert!(matches!(page_key("Backspace", false), Some(Input::Text(b)) if b == b"\x7f"));
        assert!(matches!(page_key("u", true), Some(Input::Text(b)) if b == b"\x15"));
        assert!(matches!(
            page_key("ArrowUp", false),
            Some(Input::Key(Named::Up))
        ));
        assert!(page_key("u", false).is_none());
        assert!(page_key("F5", false).is_none());
    }
}
