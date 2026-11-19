use super::a11y::{self, Geometry};
use super::clip::Clip;
use super::codec;
use super::control::{Control, Event};
use super::ended::ended_final;
use super::pane::{Action, GRID_MAX, Input};
use super::render::{GPU_FAULT, Shared, Surface};
use super::screen;
use super::style::Style;
use super::table::{Direct, Entry, Outbox, Table, Told, VIEWS_MAX, changes};
use super::view::{MouseKind, TermView, ViewEvent};
use crate::bridge::BridgeError;
use crate::bridge::feed::{EngineFeed, Fed, FeedEvent, Opener, Opening, Wake};
use crate::bridge::terminal::{NATIVE_OVERFLOW, Reservation, ViewKey};
use alacritty_terminal::grid::Dimensions;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSResponder, NSView, NSWindowOrderingMode};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::{Cell, RefCell};
use std::collections::hash_map::Entry as Slot;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

/// The most engine feed events waiting for the main thread.
const INBOX_MAX: usize = 4096;

/// The most events one drain applies before it lets the main loop run again.
const DRAIN_MAX: usize = 512;

/// The most rounds the outermost host call spends on events AppKit sent meanwhile.
const SETTLE_ROUNDS: usize = 8;

/// The most host calls parked while a host call further up the main thread's stack holds
/// the state; that call's settle runs them.
const PARKED_MAX: usize = 64;

/// The most times work for the main loop is queued again while the park is full.
const LATER_TRIES: u8 = 8;

/// The cursor blink half period, as the xterm panel blinks.
const BLINK: Duration = Duration::from_millis(600);

/// The engine feed codes an open can answer with, kept as they are; any other is
/// `native_terminal_open_failed`.
const OPEN_CODES: [&str; 4] = [
    "terminal_size_bounds",
    "native_view_opened",
    "native_view_unbound",
    "terminal_state_failed",
];

const _: () = assert!(SETTLE_ROUNDS > 0 && DRAIN_MAX > 0 && DRAIN_MAX < INBOX_MAX);

/// Host work parked until the call holding the state settles.
type Parked = Box<dyn FnOnce(&mut Native)>;

thread_local! {
    static NATIVE: RefCell<Option<Native>> = const { RefCell::new(None) };
    static OUTBOX: RefCell<Outbox<ViewEvent>> = RefCell::new(Outbox::default());
    static PENDING: Cell<bool> = const { Cell::new(false) };
    static PARKED: RefCell<VecDeque<Parked>> = const { RefCell::new(VecDeque::new()) };
}

static APP: OnceLock<AppHandle> = OnceLock::new();
static SCHEDULED: AtomicBool = AtomicBool::new(false);
static BLINKING: AtomicBool = AtomicBool::new(false);

/// What a native view tells its panel, as `NativeEvent` in native-terminal.ts.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NativeEvent {
    Ready {
        cols: u16,
        rows: u16,
    },
    Menu {
        x: f64,
        y: f64,
    },
    Mouse {
        on: bool,
    },
    Behind {
        behind: bool,
    },
    Ended {
        code: String,
        #[serde(rename = "final")]
        last: bool,
    },
}

pub use super::view::LABEL_BYTES_MAX;

/// One open checked off the main thread: the calling window, the agent window name, the
/// screen reader label, the place the shared gate took, the tmux program and engine socket
/// when the direct path may draw it, the page place, the style, whether it opens covered,
/// whether its keys go through, the wall window's project and the events channel.
pub struct Open {
    pub window: String,
    pub name: String,
    pub label: String,
    pub place: Reservation,
    pub direct: Option<(PathBuf, PathBuf)>,
    pub rect: [f64; 4],
    pub style: Style,
    pub covered: bool,
    pub keys: bool,
    pub project: Option<String>,
    pub events: Channel<NativeEvent>,
}

/// One page call on an open view.
pub enum Command {
    Place([f64; 4]),
    Cover(bool),
    Focus,
    Copy(bool),
    Paste(String),
    Jump,
    Keys(bool),
    Close,
}

/// Why host work did not run: no view was ever opened, or a host call further up the main
/// thread's stack holds the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Missing,
    Busy,
}

/// A view's AppKit and drawing parts: its NSView above the webview, the webview its place
/// is measured in, its surface and the backing scale key it was made at, its events channel
/// and the wall window's project a reopen needs.
pub struct View {
    ns: Retained<TermView>,
    webview: Retained<NSView>,
    surface: Option<Surface>,
    scale: u32,
    events: Channel<NativeEvent>,
    project: Option<String>,
}

/// Everything the native terminals hold on the main thread.
pub struct Native {
    app: AppHandle,
    feed: EngineFeed,
    inbox: mpsc::Receiver<Fed>,
    control: Option<Control>,
    direct: Arc<Mutex<Direct<Event>>>,
    table: Table<View>,
    shared: HashMap<u32, Shared>,
    style: Option<Style>,
    shown: bool,
    clip: Clip,
}

fn scale_key(scale: f64) -> u32 {
    let key = (scale.clamp(1.0, 4.0) * 100.0).round() as u32;
    assert!((100..=400).contains(&key));
    key
}

#[cfg(feature = "native-webdriver")]
thread_local! {
    static TRIAL_SCALE: Cell<Option<f64>> = const { Cell::new(None) };
}

/// Test builds only: makes every native view read this backing scale in place of its
/// window's, or its window's own again with none, so a trial on a 1x display drives the
/// refit a move to a 2x display starts. Input: the scale, 1 to 4, or none. Output: none.
#[cfg(feature = "native-webdriver")]
pub fn trial_scale(scale: Option<f64>) {
    assert!(scale.is_none_or(|scale| (1.0..=4.0).contains(&scale)));
    TRIAL_SCALE.with(|trial| trial.set(scale));
    assert_eq!(TRIAL_SCALE.with(Cell::get), scale);
}

fn backing(view: &NSView) -> f64 {
    #[cfg(feature = "native-webdriver")]
    if let Some(scale) = TRIAL_SCALE.with(Cell::get) {
        return scale;
    }
    let scale = view
        .window()
        .map_or(2.0, |window| window.backingScaleFactor());
    assert!(scale.is_finite());
    scale
}

fn io_code(error: &std::io::Error) -> &'static str {
    let text = error.to_string();
    let code = OPEN_CODES
        .into_iter()
        .find(|code| *code == text)
        .unwrap_or("native_terminal_open_failed");
    assert!(!code.is_empty());
    code
}

/// Asks the main loop for one drain, coalesced: a burst of events costs one wake. Runs on
/// any thread and never touches the native state itself; on the main thread it goes through
/// the async runtime, as tauri runs a main-thread task inline there.
fn schedule() {
    if SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(app) = APP.get().cloned() else {
        SCHEDULED.store(false, Ordering::Release);
        return;
    };
    if MainThreadMarker::new().is_some() {
        tauri::async_runtime::spawn(async move {
            if app.run_on_main_thread(drain).is_err() {
                SCHEDULED.store(false, Ordering::Release);
            }
        });
        return;
    }
    if app.run_on_main_thread(drain).is_err() {
        SCHEDULED.store(false, Ordering::Release);
    }
}

fn drain() {
    SCHEDULED.store(false, Ordering::Release);
    if let Err((Held::Busy, _)) = with_native(Native::drain) {
        PENDING.with(|pending| pending.set(true));
    }
}

/// Runs work on the native state when it exists and nothing further up this thread holds
/// it, then applies what AppKit sent meanwhile. Input: the work. Output: its result, or why
/// it did not run with the work given back.
fn with_native<T, W: FnOnce(&mut Native) -> T>(work: W) -> Result<T, (Held, W)> {
    assert!(
        MainThreadMarker::new().is_some(),
        "native state is main-thread only"
    );
    NATIVE.with(|cell| {
        let Ok(mut held) = cell.try_borrow_mut() else {
            return Err((Held::Busy, work));
        };
        let Some(native) = held.as_mut() else {
            return Err((Held::Missing, work));
        };
        let out = work(native);
        native.settle();
        Ok(out)
    })
}

/// Runs work on the native state from the main loop after the tasks queued before it. It
/// goes through the async runtime, as tauri runs a main-thread task inline on the main
/// thread. Work that finds the state held is parked for the holder's settle, or with the park
/// full queued again, at most `tries` more times. Inputs: the app, the tries left, the work.
fn later(app: &AppHandle, tries: u8, work: impl FnOnce(&mut Native) + Send + 'static) {
    assert!(tries <= LATER_TRIES);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let again = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Err((Held::Busy, work)) = with_native(work) else {
                return;
            };
            let room = PARKED.with(|parked| parked.borrow().len() < PARKED_MAX);
            if room {
                PARKED.with(|parked| parked.borrow_mut().push_back(Box::new(work)));
            } else if tries > 0 {
                later(&again, tries - 1, work);
            }
        });
    });
}

/// The sink every TermView sends its events to: kept in order, applied by the outermost
/// host call, so an event AppKit sends from inside a host call never re-enters the state.
fn sink(page: u64, event: ViewEvent) {
    assert!(page > 0);
    OUTBOX.with(|outbox| outbox.borrow_mut().push(page, event));
    let _ = with_native(|_| ());
}

/// The visible grid VoiceOver reads, or none while the state is in use.
fn screen(page: u64) -> Option<a11y::Grid> {
    NATIVE.with(|cell| {
        let held = cell.try_borrow().ok()?;
        let entry = held.as_ref()?.table.get(page)?;
        entry.pane.term().map(a11y::visible)
    })
}

/// Runs work on the main thread and waits for it without a deadline: a main loop that
/// is gone drops the work and answers `native_terminal_main_busy`. Inputs: the app and the
/// work. Output: its result.
pub async fn on_main<T: Send + 'static>(
    app: &AppHandle,
    work: impl FnOnce(MainThreadMarker) -> T + Send + 'static,
) -> Result<T, &'static str> {
    let _ = APP.set(app.clone());
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            let _ = sender.send(work(mtm));
        }
    })
    .map_err(|_| "native_terminal_main_busy")?;
    receiver.await.map_err(|_| "native_terminal_main_busy")
}

/// Opens one view above the calling page's webview; on the main thread. Inputs: the app,
/// the marker, the WKWebView pointer and the checked open. Output: the page's view id, or
/// a plain code with the place given back.
pub fn open(
    app: &AppHandle,
    mtm: MainThreadMarker,
    pointer: *mut c_void,
    open: Open,
) -> Result<u64, &'static str> {
    assert!(!pointer.is_null());
    let _ = APP.set(app.clone());
    NATIVE.with(|cell| {
        if let Ok(mut held) = cell.try_borrow_mut()
            && held.is_none()
        {
            *held = Some(Native::started(app));
        }
    });
    let opened = with_native(|native| native.open(mtm, pointer, open));
    let page = opened.map_err(|_| "native_terminal_main_busy")??;
    assert!(page > 0);
    Ok(page)
}

/// Applies one page call to a view the calling window opened; on the main thread. A
/// close of a view that is gone is fine. Inputs: the window label, the view id and the
/// call. Output: ok, or a plain code.
pub fn command(window: &str, page: u64, call: Command) -> Result<(), &'static str> {
    let closing = matches!(call, Command::Close);
    match with_native(|native| native.command(window, page, call)) {
        Ok(result) => result,
        Err((Held::Missing, _)) if closing => Ok(()),
        Err((Held::Missing, _)) => Err("native_terminal_view_unknown"),
        Err((Held::Busy, _)) => Err("native_terminal_main_busy"),
    }
}

/// Takes out every view a window opened, telling no page, as that window's page reloads
/// or the window closes; their feeds and direct paths close with them. It runs on the main
/// thread after work queued before it, so a view the new page opens later stays. Inputs:
/// the app and the window label.
pub fn window_gone(app: &AppHandle, window: &str) {
    assert!(!window.is_empty());
    let window = window.to_owned();
    later(app, LATER_TRIES, move |native| {
        native.forget_window(&window)
    });
}

/// Sets how every view draws and every view opened later; on the main thread. With no view
/// open yet the next open brings its own style. Input: the checked style. Output: ok, or a
/// plain code with the old style kept.
pub fn style(style: Style) -> Result<(), &'static str> {
    match with_native(|native| native.restyle(style)) {
        Ok(result) => result,
        Err((Held::Missing, _)) => Ok(()),
        Err((Held::Busy, _)) => Err("native_terminal_main_busy"),
    }
}

/// What a native-webdriver trial reads of one view: its NSView, its visible rows, whether
/// the direct path draws it and the private pasteboard's text.
#[cfg(feature = "native-webdriver")]
pub struct Look {
    pub view: Retained<TermView>,
    pub rows: Vec<String>,
    pub direct: bool,
    pub pasteboard: Option<String>,
}

/// Reads one view a window opened for a native-webdriver trial; on the main thread. Inputs:
/// the window label and the view id. Output: the look, or a plain code.
#[cfg(feature = "native-webdriver")]
pub fn look(window: &str, page: u64) -> Result<Look, &'static str> {
    let looked = with_native(|native| {
        let Native { table, clip, .. } = native;
        let entry = table.owned(window, page)?;
        let rows = entry.pane.term().map_or_else(Vec::new, |term| {
            let grid = a11y::visible(term);
            let text = |row: &Vec<a11y::Glyph>| -> String {
                let cells = row.iter().filter(|glyph| !glyph.spacer);
                let line: String = cells.map(|glyph| glyph.text.as_str()).collect();
                line.trim_end().to_owned()
            };
            grid.rows.iter().map(text).collect()
        });
        assert!(rows.len() <= GRID_MAX);
        Ok(Look {
            view: entry.platform.ns.clone(),
            rows,
            direct: entry.direct,
            pasteboard: clip.read(),
        })
    });
    match looked {
        Ok(look) => look,
        Err((Held::Missing, _)) => Err("native_terminal_view_unknown"),
        Err((Held::Busy, _)) => Err("native_terminal_main_busy"),
    }
}

fn blink() {
    let more = with_native(|native| {
        native.shown = !native.shown;
        for (_, entry) in native.table.entries_mut() {
            entry.pane.dirty = true;
        }
        !native.table.is_empty() && native.style.as_ref().is_some_and(|s| s.blink)
    });
    if !matches!(more, Ok(true)) {
        BLINKING.store(false, Ordering::Release);
        let _ = with_native(|native| native.shown = true);
    }
}

fn start_blink() {
    if BLINKING.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = std::thread::Builder::new()
        .name("native-blink".into())
        .spawn(|| {
            while BLINKING.load(Ordering::Acquire) {
                std::thread::sleep(BLINK);
                let sent = APP
                    .get()
                    .is_some_and(|app| app.run_on_main_thread(blink).is_ok());
                if !sent {
                    BLINKING.store(false, Ordering::Release);
                }
            }
        });
    if started.is_err() {
        BLINKING.store(false, Ordering::Release);
    }
}

impl Native {
    fn started(app: &AppHandle) -> Native {
        let terminals = Arc::clone(&app.state::<crate::TerminalState>().0);
        let opening = app.clone();
        let opener: Opener = Arc::new(
            move |key: ViewKey, grid: (u16, u16), input_only: bool| -> Opening {
                let app = opening.clone();
                Box::pin(async move {
                    let state = app.state::<crate::NativeState>();
                    let mut bridge = crate::lock(&state).await?;
                    bridge
                        .open_terminal(key.agent, key.pane, Some(grid), input_only)
                        .await
                })
            },
        );
        let (sender, inbox) = mpsc::channel(INBOX_MAX);
        let wake: Wake = Arc::new(schedule);
        let native = Native {
            app: app.clone(),
            feed: EngineFeed::new(terminals, opener, sender, wake),
            inbox,
            control: None,
            direct: Arc::new(Mutex::new(Direct::default())),
            table: Table::default(),
            shared: HashMap::new(),
            style: None,
            shown: true,
            clip: Clip::new(),
        };
        assert!(native.table.is_empty() && native.control.is_none());
        native
    }

    /// Applies what AppKit sent while this call held the state: overflowed views end, the
    /// rest apply in order, then parked work and a drain that found the state busy run; then
    /// it draws.
    fn settle(&mut self) {
        for _ in 0..SETTLE_ROUNDS {
            let (events, over) = OUTBOX.with(|outbox| outbox.borrow_mut().take());
            let pending = PENDING.with(|pending| pending.replace(false));
            let parked: Vec<Parked> = PARKED.with(|parked| parked.borrow_mut().drain(..).collect());
            assert!(parked.len() <= PARKED_MAX);
            if events.is_empty() && over.is_empty() && parked.is_empty() && !pending {
                self.draw_dirty();
                return;
            }
            for page in over {
                self.end(page, NATIVE_OVERFLOW);
            }
            for (page, event) in events {
                self.on_view(page, event);
            }
            for work in parked {
                work(self);
            }
            if pending {
                self.drain();
            }
        }
        self.draw_dirty();
        schedule();
        assert!(self.table.pages().len() <= VIEWS_MAX);
    }

    /// Takes waiting engine feed and direct-path events, a bounded batch, and asks for
    /// another drain when more wait.
    fn drain(&mut self) {
        let mut taken = 0;
        while taken < DRAIN_MAX {
            let Ok(fed) = self.inbox.try_recv() else {
                break;
            };
            taken += 1;
            if let Some(event) = self.feed.take(fed) {
                self.on_feed(event);
            }
        }
        let (direct, lost) = self
            .direct
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(DRAIN_MAX);
        let more = taken == DRAIN_MAX || direct.len() == DRAIN_MAX;
        for event in direct {
            self.on_direct(event);
        }
        if lost {
            self.on_direct(Event::Closed);
        }
        if more {
            schedule();
        }
        assert!(taken <= DRAIN_MAX);
    }
}

impl Native {
    fn open(
        &mut self,
        mtm: MainThreadMarker,
        pointer: *mut c_void,
        open: Open,
    ) -> Result<u64, &'static str> {
        if self.table.full() {
            return Err("native_terminal_views_full");
        }
        let webview = unsafe { Retained::retain(pointer.cast::<NSView>()) }
            .ok_or("native_terminal_webview_missing")?;
        let host = unsafe { webview.superview() }.ok_or("native_terminal_webview_missing")?;
        if self.style.is_none() || self.table.is_empty() {
            self.restyle(open.style.clone())?;
        }
        let key = scale_key(backing(&webview));
        let grid = self.grid(key, open.rect)?;
        let direct = match &open.direct {
            Some((tmux, socket)) => self.ensure_control(tmux, socket),
            None => false,
        };
        let page = self.table.fresh();
        self.feed
            .bind(page, open.place, direct)
            .map_err(|error| error.code)?;
        if let Err(error) = self.feed.open(page, grid) {
            self.feed.close(page);
            return Err(io_code(&error));
        }
        let ns = TermView::new(mtm, page, sink, screen);
        ns.set_label(&open.label);
        ns.setHidden(true);
        host.addSubview_positioned_relativeTo(&ns, NSWindowOrderingMode::Above, Some(&webview));
        let platform = View {
            ns,
            webview,
            surface: None,
            scale: key,
            events: open.events,
            project: open.project,
        };
        let entry = Entry {
            window: open.window,
            name: open.name,
            pane: super::pane::Pane::new(open.keys),
            feed: page,
            direct,
            rect: open.rect,
            covered: open.covered,
            told: Told::default(),
            carry: 0.0,
            platform,
        };
        self.table.insert(page, entry);
        if self.style.as_ref().is_some_and(|style| style.blink) {
            start_blink();
        }
        assert!(self.table.page_of(page) == Some(page));
        Ok(page)
    }

    fn grid(&mut self, key: u32, rect: [f64; 4]) -> Result<(u16, u16), &'static str> {
        let style = self.style.as_ref().ok_or("native_terminal_style_invalid")?;
        let shared = shared_at(&mut self.shared, key, style)?;
        let (columns, lines) = codec::grid_for(rect[2], rect[3], shared.cell_points(), GRID_MAX);
        let grid = (
            u16::try_from(columns).unwrap_or(u16::MAX),
            u16::try_from(lines).unwrap_or(u16::MAX),
        );
        assert!(grid.0 >= 2 && grid.1 >= 2);
        assert!(usize::from(grid.0) <= GRID_MAX && usize::from(grid.1) <= GRID_MAX);
        Ok(grid)
    }

    fn ensure_control(&mut self, tmux: &std::path::Path, socket: &std::path::Path) -> bool {
        assert!(tmux.is_absolute());
        assert!(socket.is_absolute());
        if self.control.is_some() {
            return true;
        }
        let queue = Arc::clone(&self.direct);
        let sink = move |event: Event| {
            let kept = queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
            schedule();
            kept
        };
        self.control = Control::attach(tmux, socket, sink).ok();
        self.control.is_some()
    }

    /// Lets the direct path's client go: its drop detaches it and joins its reader, and
    /// what that reader left is forgotten, so no stale `Closed` reaches the next client.
    fn drop_control(&mut self) {
        self.control = None;
        self.direct
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        assert!(self.control.is_none());
    }

    fn restyle(&mut self, style: Style) -> Result<(), &'static str> {
        let refused = self
            .shared
            .values_mut()
            .map(|shared| shared.set_style(&style))
            .find_map(Result::err);
        if let Some(code) = refused {
            if let Some(old) = self.style.as_ref() {
                for shared in self.shared.values_mut() {
                    let _ = shared.set_style(old);
                }
            }
            return Err(code);
        }
        let blink = style.blink;
        self.style = Some(style);
        for page in self.table.pages() {
            self.layout(page);
        }
        if blink && !self.table.is_empty() {
            start_blink();
        }
        assert!(self.style.as_ref().is_some_and(|s| s.blink == blink));
        Ok(())
    }
}

fn shared_at<'a>(
    map: &'a mut HashMap<u32, Shared>,
    key: u32,
    style: &Style,
) -> Result<&'a mut Shared, &'static str> {
    assert!((100..=400).contains(&key));
    let shared = match map.entry(key) {
        Slot::Occupied(held) => held.into_mut(),
        Slot::Vacant(free) => free.insert(Shared::new(key as f32 / 100.0, style)?),
    };
    assert!(shared.cell_points().0 > 0.0);
    Ok(shared)
}

impl Native {
    fn command(&mut self, window: &str, page: u64, call: Command) -> Result<(), &'static str> {
        let closing = matches!(call, Command::Close);
        let entry = match self.table.owned(window, page) {
            Ok(entry) => entry,
            Err(_) if closing => return Ok(()),
            Err(code) => return Err(code),
        };
        let action = match call {
            Command::Place(rect) => {
                entry.rect = rect;
                Action::Nothing
            }
            Command::Cover(covered) => {
                entry.covered = covered;
                Action::Nothing
            }
            Command::Focus => {
                let responder: &NSResponder = &entry.platform.ns;
                if let Some(window) = entry.platform.ns.window() {
                    let _ = window.makeFirstResponder(Some(responder));
                }
                return Ok(());
            }
            Command::Copy(output) => {
                let text = entry.pane.copy(output)?;
                if !self.clip.write(&text) {
                    return Err("native_terminal_copy_failed");
                }
                return Ok(());
            }
            Command::Paste(text) => entry.pane.seed(&text)?,
            Command::Jump => {
                entry.pane.jump();
                Action::Local
            }
            Command::Keys(on) => {
                entry.pane.set_keys(on);
                return Ok(());
            }
            Command::Close => {
                self.dismiss(page);
                assert!(self.table.get(page).is_none());
                return Ok(());
            }
        };
        self.layout(page);
        self.act(page, action);
        self.tell(page);
        Ok(())
    }
}

impl Native {
    fn on_feed(&mut self, event: FeedEvent) {
        match event {
            FeedEvent::Attached { view, cols, rows } => self.attached(view, cols, rows),
            FeedEvent::Output { view, bytes } => {
                let Some(page) = self.table.page_of(view) else {
                    return;
                };
                if let Some(entry) = self.table.get_mut(page) {
                    entry.pane.on_output(&bytes);
                }
                self.tell(page);
            }
            FeedEvent::Ended { view, code } => {
                if let Some(page) = self.table.page_of(view) {
                    self.end(page, &code);
                }
            }
        }
    }

    fn attached(&mut self, feed: u64, cols: u16, rows: u16) {
        assert!(feed > 0);
        let Some(page) = self.table.page_of(feed) else {
            return;
        };
        let Some(entry) = self.table.get_mut(page) else {
            return;
        };
        if !entry.pane.on_attached(cols, rows) {
            self.end(page, "terminal_size_bounds");
            return;
        }
        let bound = match (entry.direct, self.control.as_mut()) {
            (true, Some(control)) => control.bind(page, &entry.name).is_ok(),
            (direct, _) => !direct,
        };
        entry.told = Told::default();
        let _ = entry
            .platform
            .events
            .send(NativeEvent::Ready { cols, rows });
        if !bound {
            self.reopen(page);
        }
        self.layout(page);
        assert!(self.table.get(page).is_none_or(|e| e.pane.ready()));
    }

    /// Applies one direct-path event. A refused view reopens engine-fed, and a closed
    /// client, including one that bound a view after it closed, reopens every direct view
    /// engine-fed, so no view waits for output from a client that is gone. Input: the event.
    fn on_direct(&mut self, event: Event) {
        match event {
            Event::Restored { view, bytes } => {
                if let Some(entry) = self.table.get_mut(view).filter(|e| e.direct) {
                    entry.pane.on_output(&bytes);
                }
                self.tell(view);
            }
            Event::Output { views, bytes } => {
                for page in views {
                    if let Some(entry) = self.table.get_mut(page).filter(|e| e.direct) {
                        entry.pane.on_output(&bytes);
                    }
                    self.tell(page);
                }
            }
            Event::Refused { view, .. } => self.reopen(view),
            Event::Closed => {
                self.drop_control();
                for page in self.table.direct_pages() {
                    self.reopen(page);
                }
                assert!(self.table.direct_pages().is_empty());
            }
        }
    }

    /// Drops a view whose direct path was refused to engine-fed output: its input-only
    /// session closes, and a full session opens under a fresh feed id through the shared
    /// gate again; the page keeps its id. Input: the page id.
    fn reopen(&mut self, page: u64) {
        let Some(entry) = self.table.get(page).filter(|e| e.direct) else {
            return;
        };
        let old = entry.feed;
        let (window, name) = (entry.window.clone(), entry.name.clone());
        let project = entry.platform.project.clone();
        self.feed.close(old);
        if let Some(control) = self.control.as_mut() {
            control.close(page);
        }
        let Some((_, new)) = self.table.refeed(page) else {
            return;
        };
        assert!(new > old);
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            let placed = crate::native_place(&app, &window, &name, project.as_deref()).await;
            later(&app, LATER_TRIES, move |native| {
                native.reopened(page, new, placed)
            });
        });
        assert!(
            self.table
                .get(page)
                .is_some_and(|e| !e.direct && e.feed == new)
        );
    }

    fn reopened(&mut self, page: u64, feed: u64, placed: Result<Reservation, BridgeError>) {
        let Some(entry) = self.table.get(page).filter(|e| e.feed == feed) else {
            return;
        };
        let (rect, key) = (entry.rect, entry.platform.scale);
        let place = match placed {
            Ok(place) => place,
            Err(error) => return self.end(page, error.code),
        };
        let grid = match self.grid(key, rect) {
            Ok(grid) => grid,
            Err(code) => return self.end(page, code),
        };
        if let Err(error) = self.feed.bind(feed, place, false) {
            return self.end(page, error.code);
        }
        if let Err(error) = self.feed.open(feed, grid) {
            self.end(page, io_code(&error));
        }
        assert!(self.table.get(page).is_none_or(|e| !e.direct));
    }

    /// Takes a view out in the order nothing can reach it after: its feed and direct path
    /// closed, marked text dropped, its surface before its NSView. Input: the page id.
    /// Output: its events channel, if it was open.
    fn dismiss(&mut self, page: u64) -> Option<Channel<NativeEvent>> {
        let entry = self.table.remove(page)?;
        self.feed.close(entry.feed);
        if let Some(control) = self.control.as_mut() {
            control.close(page);
        }
        let View {
            ns,
            surface,
            events,
            ..
        } = entry.platform;
        ns.cancel_marked();
        drop(surface);
        ns.removeFromSuperview();
        if self.table.is_empty() {
            self.drop_control();
            BLINKING.store(false, Ordering::Release);
        }
        assert!(self.table.page_of(entry.feed).is_none());
        assert!(self.table.get(page).is_none());
        Some(events)
    }

    fn end(&mut self, page: u64, code: &str) {
        assert!(!code.is_empty());
        if let Some(events) = self.dismiss(page) {
            let last = ended_final(code);
            let _ = events.send(NativeEvent::Ended {
                code: code.to_owned(),
                last,
            });
        }
        assert!(self.table.get(page).is_none());
    }

    fn forget_window(&mut self, window: &str) {
        for page in self.table.pages_of(window) {
            let _ = self.dismiss(page);
        }
        assert!(self.table.pages_of(window).is_empty());
    }
}

impl Native {
    fn on_view(&mut self, page: u64, event: ViewEvent) {
        assert!(page > 0);
        let Native {
            table,
            shared,
            clip,
            ..
        } = self;
        let Some(entry) = table.get_mut(page) else {
            return;
        };
        let action = match event {
            ViewEvent::Key(input) => entry.pane.apply(input),
            ViewEvent::Wheel {
                point,
                delta,
                precise,
            } => wheel(shared, entry, point, delta, precise),
            ViewEvent::Mouse {
                kind,
                point,
                option,
                clicks,
            } => {
                let cell = cell_of(shared, entry, point);
                let input = match kind {
                    MouseKind::Press => Input::Press {
                        cell,
                        option,
                        clicks,
                    },
                    MouseKind::Drag => Input::Drag { cell, option },
                    MouseKind::Release => Input::Release { cell, option },
                };
                entry.pane.apply(input)
            }
            ViewEvent::Menu { point } => {
                let (x, y) = (entry.rect[0] + point.0, entry.rect[1] + point.1);
                let _ = entry.platform.events.send(NativeEvent::Menu { x, y });
                Action::Nothing
            }
            ViewEvent::Copy => {
                if let Ok(text) = entry.pane.copy(false) {
                    let _ = clip.write(&text);
                }
                Action::Nothing
            }
            ViewEvent::Paste => match clip.read() {
                Some(text) => entry.pane.apply(Input::Paste { text }),
                None => Action::Nothing,
            },
            ViewEvent::SelectAll => {
                entry.pane.select_all();
                Action::Local
            }
            ViewEvent::Focus(_) | ViewEvent::Marked => {
                entry.pane.dirty = true;
                Action::Nothing
            }
        };
        self.act(page, action);
        self.tell(page);
        assert!(self.table.get(page).is_none_or(|e| e.feed > 0));
    }

    /// Sends what an input asks for through the engine feed. A refusal needs no answer
    /// here: a full typing queue ends the view through the feed's own `Ended`, and input
    /// before the attach or after an end has no session to reach. Inputs: the page id and
    /// the action.
    fn act(&mut self, page: u64, action: Action) {
        let Some(feed) = self.table.get(page).map(|entry| entry.feed) else {
            return;
        };
        assert!(feed > 0);
        let sent = match action {
            Action::Input { bytes, wheel } => self.feed.input(feed, &bytes, wheel),
            Action::Paste { text } => self.feed.paste(feed, text.as_bytes()),
            Action::Local | Action::Nothing => Ok(()),
        };
        match sent {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => schedule(),
            Err(_) => {}
        }
    }

    fn tell(&mut self, page: u64) {
        let Some(entry) = self.table.get_mut(page) else {
            return;
        };
        let now = Told {
            mouse: entry.pane.mouse_mode(),
            behind: entry.pane.behind(),
        };
        let (mouse, behind) = changes(entry.told, now);
        let events = &entry.platform.events;
        if let Some(on) = mouse {
            let _ = events.send(NativeEvent::Mouse { on });
        }
        if let Some(behind) = behind {
            let _ = events.send(NativeEvent::Behind { behind });
        }
        entry.told = now;
        assert_eq!(entry.told, now);
    }
}

fn cell_of(
    shared: &HashMap<u32, Shared>,
    entry: &Entry<View>,
    point: (f64, f64),
) -> (usize, usize) {
    let (Some(shared), Some(term)) = (shared.get(&entry.platform.scale), entry.pane.term()) else {
        return (0, 0);
    };
    let (columns, lines) = (term.columns(), term.screen_lines());
    let cell = codec::cell_at(point.0, point.1, shared.cell_points(), columns, lines);
    assert!(cell.0 < columns && cell.1 < lines);
    cell
}

fn wheel(
    shared: &HashMap<u32, Shared>,
    entry: &mut Entry<View>,
    point: (f64, f64),
    delta: f64,
    precise: bool,
) -> Action {
    let cell = cell_of(shared, entry, point);
    let line = shared
        .get(&entry.platform.scale)
        .map_or(17.0, |s| s.cell_points().1);
    assert!(line > 0.0 && delta.is_finite());
    let lines = if precise {
        let (lines, carry) = codec::wheel_lines(delta, entry.carry, line);
        entry.carry = carry;
        lines
    } else {
        delta.round().clamp(-1000.0, 1000.0) as i32
    };
    if lines == 0 {
        return Action::Nothing;
    }
    entry.pane.apply(Input::Wheel { lines, cell })
}

/// Draws one view's frame and hands VoiceOver and the input method what it showed. Inputs:
/// the view, its shared renderer, the style and the blink phase. Output: whether the frame
/// was drawn, a skipped frame leaving the view dirty for the next pass, or
/// `native_terminal_gpu` when the GPU refused the frame and the view must end.
fn draw_view(
    entry: &mut Entry<View>,
    shared: &mut Shared,
    style: &Style,
    shown: bool,
) -> Result<bool, &'static str> {
    let view = &mut entry.platform;
    let (Some(surface), Some(term)) = (view.surface.as_mut(), entry.pane.term()) else {
        return Ok(false);
    };
    let marked = view.ns.marked_text();
    let frame = screen::frame(term, &style.theme, shown, marked.as_deref());
    assert_eq!(frame.rows.len(), term.screen_lines());
    match shared.draw(surface, &frame) {
        Ok(()) => {}
        Err(code) if code == GPU_FAULT => return Err(GPU_FAULT),
        Err(_) => return Ok(false),
    }
    let scale = f64::from(view.scale) / 100.0;
    let (width, height) = shared.cell_points();
    assert!(scale >= 1.0 && width > 0.0 && height > 0.0);
    let geometry = Geometry {
        cell: (width * scale, height * scale),
        pad: 0.0,
        scale,
    };
    view.ns
        .drawn(geometry, a11y::marks(term), a11y::text_key(term));
    Ok(true)
}

impl Native {
    /// Puts a view over its page place, sizes its surface and the agent's window to the
    /// grid that fits, and shows it once its mirror is ready and nothing covers it; a
    /// surface that cannot be made ends the view. Input: the page id.
    fn layout(&mut self, page: u64) {
        let Native {
            table,
            shared,
            feed,
            style,
            ..
        } = self;
        let (Some(entry), Some(style)) = (table.get_mut(page), style.as_ref()) else {
            return;
        };
        assert!(entry.rect.iter().all(|side| side.is_finite()));
        let [x, y, width, height] = entry.rect;
        let shown = entry.pane.ready() && !entry.covered && width >= 1.0 && height >= 1.0;
        let view = &mut entry.platform;
        if !shown {
            if !view.ns.isHidden() {
                view.ns.cancel_marked();
            }
            view.ns.setHidden(true);
            return;
        }
        let host = unsafe { view.ns.superview() };
        let css = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
        view.ns
            .setFrame(view.webview.convertRect_toView(css, host.as_deref()));
        view.ns.setHidden(false);
        let scale = backing(&view.ns);
        let key = scale_key(scale);
        if view.scale != key {
            view.surface = None;
            view.scale = key;
        }
        let pixels = (
            (width * scale).round() as u32,
            (height * scale).round() as u32,
        );
        let Ok(shared) = shared_at(shared, key, style) else {
            return;
        };
        let made = match view.surface.as_mut() {
            Some(surface) => shared.resize(surface, pixels.0, pixels.1).is_ok(),
            None => {
                let pointer = Retained::as_ptr(&view.ns) as *mut c_void;
                let made = shared.surface(pointer, pixels.0, pixels.1);
                view.surface = made.ok();
                view.surface.is_some()
            }
        };
        let grid = codec::grid_for(width, height, shared.cell_points(), GRID_MAX);
        if let Some(size) = entry.pane.resize(grid) {
            let _ = feed.resize(entry.feed, size);
        }
        entry.pane.dirty = true;
        if !made {
            self.end(page, GPU_FAULT);
        }
        assert!(
            self.table
                .get(page)
                .is_none_or(|e| !e.platform.ns.isHidden())
        );
    }

    /// Draws every shown view whose screen changed, refits one whose backing scale changed,
    /// and hands VoiceOver and the input method what the frame showed.
    fn draw_dirty(&mut self) {
        let moved: Vec<u64> = self
            .table
            .pages()
            .into_iter()
            .filter(|page| {
                self.table.get(*page).is_some_and(|e| {
                    !e.platform.ns.isHidden()
                        && scale_key(backing(&e.platform.ns)) != e.platform.scale
                })
            })
            .collect();
        for page in moved {
            self.layout(page);
        }
        let Native {
            table,
            shared,
            style,
            shown,
            ..
        } = self;
        let Some(style) = style.as_ref() else {
            return;
        };
        let mut drawn = 0;
        let mut refused = Vec::new();
        for (page, entry) in table.entries_mut() {
            if !entry.pane.dirty || entry.platform.ns.isHidden() {
                continue;
            }
            let Some(shared) = shared.get_mut(&entry.platform.scale) else {
                continue;
            };
            match draw_view(entry, shared, style, *shown) {
                Ok(true) => {
                    entry.pane.dirty = false;
                    drawn += 1;
                }
                Ok(false) => {}
                Err(code) => refused.push((*page, code)),
            }
        }
        assert!(drawn + refused.len() <= self.table.pages().len());
        for (page, code) in refused {
            self.end(page, code);
            assert!(self.table.get(page).is_none());
        }
    }
}
