#[cfg(feature = "native-webdriver")]
use super::{Held, Native, with_native};
use super::{NativeEvent, SKIPPED};
use super::{screen, sink};
use crate::native_term::a11y::{self, Geometry};
#[cfg(feature = "native-webdriver")]
use crate::native_term::pane::GRID_MAX;
use crate::native_term::render::{GPU_FAULT, Shared, Surface};
use crate::native_term::screen::Frame;
use crate::native_term::view::TermView;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::Term;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSResponder, NSView, NSWindowOrderingMode};
use objc2_foundation::{NSPoint, NSRect, NSSize};
#[cfg(feature = "native-webdriver")]
use std::cell::Cell;
use std::ffi::c_void;
use tauri::ipc::Channel;

pub use crate::native_term::clip::Clip;

/// The proof that code runs on the main thread, which every AppKit call needs.
pub type Main = MainThreadMarker;

/// The page's webview as `with_webview` hands it over: the WKWebView pointer.
pub type Webview = *mut c_void;

/// The main thread's marker, or none on another thread. Output: the marker, if any.
pub fn main() -> Option<Main> {
    MainThreadMarker::new()
}

/// A view's AppKit and drawing parts: its NSView above the webview, the webview its place
/// is measured in, its surface and the backing scale key it was made at, its events channel
/// and the wall window's project a reopen needs.
pub struct View {
    ns: Retained<TermView>,
    webview: Retained<NSView>,
    surface: Option<Surface>,
    pub(super) scale: u32,
    pub(super) events: Channel<NativeEvent>,
    pub(super) project: Option<String>,
}

/// The webview a view opens over and the NSView that holds it, which the view joins.
pub struct Parent {
    webview: Retained<NSView>,
    host: Retained<NSView>,
}

/// The webview and its superview for one open. Input: the WKWebView pointer. Output: both,
/// or `native_terminal_webview_missing`.
pub fn parent(pointer: Webview) -> Result<Parent, &'static str> {
    assert!(!pointer.is_null());
    let webview = unsafe { Retained::retain(pointer.cast::<NSView>()) }
        .ok_or("native_terminal_webview_missing")?;
    let host = unsafe { webview.superview() }.ok_or("native_terminal_webview_missing")?;
    Ok(Parent { webview, host })
}

/// The backing scale of the window the webview shows in. Input: the parent. Output: the
/// scale.
pub fn parent_scale(parent: &Parent) -> f64 {
    backing(&parent.webview)
}

/// Makes a hidden view above its webview, labelled for VoiceOver, its events going to the
/// host's sink; macOS places it on its first layout, so the open rect is not read here.
/// Inputs: the marker, the parent, the page id, the scale key, the open rect, the label,
/// the events channel and the wall window's project. Output: the view; a macOS open never
/// fails here.
#[allow(clippy::too_many_arguments)]
pub fn open_view(
    mtm: Main,
    parent: Parent,
    page: u64,
    key: u32,
    _rect: [f64; 4],
    label: &str,
    events: Channel<NativeEvent>,
    project: Option<String>,
) -> Result<View, &'static str> {
    let Parent { webview, host } = parent;
    let ns = TermView::new(mtm, page, sink, screen);
    ns.set_label(label);
    ns.setHidden(true);
    host.addSubview_positioned_relativeTo(&ns, NSWindowOrderingMode::Above, Some(&webview));
    Ok(View {
        ns,
        webview,
        surface: None,
        scale: key,
        events,
        project,
    })
}

/// Makes the view its window's first responder; the app and its key window stay as they
/// are. Input: the view.
pub fn focus_view(view: &View) {
    let responder: &NSResponder = &view.ns;
    if let Some(window) = view.ns.window() {
        let _ = window.makeFirstResponder(Some(responder));
    }
}

/// Puts a view over its page place and shows it, or hides it, dropping marked text when a
/// shown view hides. Inputs: the view, the place in CSS pixels and whether it shows.
/// Output: the backing scale of a shown view, or none for a hidden one.
pub fn place_view(view: &mut View, rect: [f64; 4], shown: bool) -> Option<f64> {
    let [x, y, width, height] = rect;
    if !shown {
        if !view.ns.isHidden() {
            view.ns.cancel_marked();
        }
        view.ns.setHidden(true);
        return None;
    }
    let host = unsafe { view.ns.superview() };
    let css = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    view.ns
        .setFrame(view.webview.convertRect_toView(css, host.as_deref()));
    view.ns.setHidden(false);
    Some(backing(&view.ns))
}

/// Makes the view's surface on its NSView. Inputs: the view, the renderer at its scale and
/// the size in device pixels. Output: the surface, or none when it cannot be made.
fn surface(view: &View, shared: &mut Shared, pixels: (u32, u32)) -> Option<Surface> {
    let pointer = Retained::as_ptr(&view.ns) as *mut c_void;
    let made = unsafe { shared.surface(pointer, pixels.0, pixels.1) };
    made.ok()
}

/// Gives the view a new scale key: its surface, made on the old scale's renderer, goes,
/// and `fit` makes the next one. Inputs: the view and the key.
pub fn rescale(view: &mut View, key: u32) {
    view.surface = None;
    view.scale = key;
}

/// Sizes the view's surface to the pixels, making it on the renderer when the view has
/// none. Inputs: the view, the renderer at its scale and the size in device pixels.
/// Output: whether the view has a surface of that size.
pub fn fit(view: &mut View, shared: &mut Shared, pixels: (u32, u32)) -> bool {
    match view.surface.as_mut() {
        Some(surface) => shared.resize(surface, pixels.0, pixels.1).is_ok(),
        None => {
            view.surface = surface(view, shared, pixels);
            view.surface.is_some()
        }
    }
}

/// Draws a frame into the view's surface on the main thread. Inputs: the view, the
/// renderer at its scale and the frame. Output: ok, `native_terminal_gpu` when the GPU
/// refused the frame, or the skipped code when there is no surface or the draw skipped.
pub fn paint(view: &mut View, shared: &mut Shared, frame: &Frame) -> Result<(), &'static str> {
    let Some(surface) = view.surface.as_mut() else {
        return Err(SKIPPED);
    };
    match shared.draw(surface, frame) {
        Ok(()) => Ok(()),
        Err(code) if code == GPU_FAULT => Err(GPU_FAULT),
        Err(_) => Err(SKIPPED),
    }
}

/// Whether the view can present a frame now: always on macOS, where the draw is on the
/// main thread. Input: the view. Output: true.
pub fn ready(_view: &mut View) -> bool {
    true
}

/// Whether the view shows. Input: the view. Output: the answer.
pub fn shown(view: &View) -> bool {
    !view.ns.isHidden()
}

/// The backing scale of the window the view shows in now. Input: the view. Output: the
/// scale.
pub fn scale(view: &View) -> f64 {
    backing(&view.ns)
}

/// The input method's marked text the view holds, drawn over the cells. Input: the view.
/// Output: the text, or none.
pub fn marked(view: &View) -> Option<String> {
    view.ns.marked_text()
}

/// Hands VoiceOver and the input method what a frame showed. Inputs: the view, the cell
/// geometry and the terminal the frame came from.
pub fn drawn(view: &View, geometry: Geometry, term: &Term<VoidListener>) {
    view.ns
        .drawn(geometry, a11y::marks(term), a11y::text_key(term));
}

/// Takes a view's AppKit parts down in the order nothing can reach it after: marked text
/// dropped, its surface before its NSView. Input: the view. Output: its events channel.
pub fn dismiss_view(view: View) -> Channel<NativeEvent> {
    let View {
        ns,
        surface,
        events,
        ..
    } = view;
    ns.cancel_marked();
    drop(surface);
    ns.removeFromSuperview();
    events
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
