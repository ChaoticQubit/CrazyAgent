use super::NativeEvent;
use crate::native_term::a11y::Geometry;
use crate::native_term::render::{Shared, Surface};
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::Term;
use tauri::ipc::Channel;

/// The main-thread proof of a system whose host has not landed: never made, so no
/// main-thread work runs here.
pub struct Main;

/// The page's webview as a host would take it: never handed over until the system's host
/// lands.
pub struct Webview;

/// The main thread's marker: always none, so main-thread work answers
/// `native_terminal_main_busy`. Output: none.
pub fn main() -> Option<Main> {
    None
}

/// A view's drawing parts, the same fields every host reads; none opens here.
pub struct View {
    pub(super) surface: Option<Surface>,
    pub(super) scale: u32,
    pub(super) events: Channel<NativeEvent>,
    pub(super) project: Option<String>,
}

/// The webview a view opens over: never made, as every open is refused first.
pub struct Parent;

/// Refuses an open before any feed is bound: this system's host has not landed. Input:
/// the webview. Output: `native_terminal_unsupported`.
pub fn parent(_pointer: Webview) -> Result<Parent, &'static str> {
    Err("native_terminal_unsupported")
}

/// The backing scale of the webview's window: 1, as nothing shows. Input: the parent.
/// Output: the scale.
pub fn parent_scale(_parent: &Parent) -> f64 {
    1.0
}

/// Makes a view with no drawing parts; never reached, as `parent` refuses first. Inputs:
/// as the macOS host takes them. Output: the view.
pub fn open_view(
    _mtm: Main,
    _parent: Parent,
    page: u64,
    key: u32,
    _label: &str,
    events: Channel<NativeEvent>,
    project: Option<String>,
) -> View {
    assert!(page > 0);
    View {
        surface: None,
        scale: key,
        events,
        project,
    }
}

/// Gives a view focus; no view exists to focus. Input: the view.
pub fn focus_view(_view: &View) {}

/// Places a view; no view shows here. Inputs: the view, the place and whether it shows.
/// Output: none, as for a hidden view.
pub fn place_view(_view: &mut View, _rect: [f64; 4], _shown: bool) -> Option<f64> {
    None
}

/// Makes a view's surface; none is made here. Inputs: the view, the renderer and the size.
/// Output: none.
pub fn surface(_view: &View, _shared: &mut Shared, _pixels: (u32, u32)) -> Option<Surface> {
    None
}

/// Whether a view shows: never here. Input: the view. Output: false.
pub fn shown(_view: &View) -> bool {
    false
}

/// The backing scale a view's window has now: 1, as nothing shows. Input: the view.
/// Output: the scale.
pub fn scale(_view: &View) -> f64 {
    1.0
}

/// A view's marked text: none here. Input: the view. Output: none.
pub fn marked(_view: &View) -> Option<String> {
    None
}

/// Hands assistive tools what a frame showed; nothing draws here. Inputs: the view, the
/// geometry and the terminal.
pub fn drawn(_view: &View, _geometry: Geometry, _term: &Term<VoidListener>) {}

/// Takes a view down. Input: the view. Output: its events channel.
pub fn dismiss_view(view: View) -> Channel<NativeEvent> {
    let View {
        surface, events, ..
    } = view;
    drop(surface);
    events
}

/// The clipboard of a system whose host has not landed: every write and read is refused.
pub struct Clip;

impl Clip {
    /// The refusing clipboard. Output: the clipboard.
    pub fn new() -> Clip {
        Clip
    }

    /// Refuses a copy. Input: the text. Output: false.
    pub fn write(&self, _text: &str) -> bool {
        false
    }

    /// Refuses a paste. Output: none.
    pub fn read(&self) -> Option<String> {
        None
    }
}
