use super::{NativeEvent, SKIPPED, sink};
use crate::native_term::a11y::{self, Geometry};
use crate::native_term::render::{GPU_FAULT, Shared};
use crate::native_term::screen::Frame;
use crate::native_term::style::Style;
use crate::native_term::windows::{View as Child, scale_key};
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::Term;
use std::marker::PhantomData;
use std::sync::OnceLock;
use std::thread::{self, ThreadId};
use tauri::ipc::Channel;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetParent, IsChild};

pub use crate::native_term::windows::Clip;

/// The class wry gives the window each webview's WebView2 sits in, and the room read for a
/// class name: one unit past it, so a longer name never matches.
const WEBVIEW_CLASS: &str = "WRY_WEBVIEW";
const CLASS_UNITS: usize = 32;
const MISSING: &str = "native_terminal_webview_missing";

const _: () = assert!(WEBVIEW_CLASS.len() < CLASS_UNITS);

/// The thread that made the app's windows, recorded once by the app's setup.
static MAIN: OnceLock<ThreadId> = OnceLock::new();

/// The proof that code runs on the thread that made the app's windows, which every window
/// call here needs; it never leaves that thread.
pub struct Main(PhantomData<*const ()>);

/// The page's webview as the host is handed it: its WebView2 controller's parent window,
/// wry's `WRY_WEBVIEW` container, as a handle value.
pub type Webview = isize;

/// Records the calling thread as the one that makes the app's windows, from the app's
/// setup, which runs on it; a mark from another thread later is a bug.
pub fn mark_main() {
    mark_on(&MAIN, thread::current().id());
}

/// Records a thread in a mark once; the marked thread may mark again. Inputs: the mark and
/// the calling thread.
fn mark_on(mark: &OnceLock<ThreadId>, current: ThreadId) {
    let _ = mark.set(current);
}

/// The main thread's marker, or none on another thread or before the app's setup recorded
/// its thread. Output: the marker, if any.
pub fn main() -> Option<Main> {
    main_on(MAIN.get().copied(), thread::current().id())
}

fn main_on(marked: Option<ThreadId>, current: ThreadId) -> Option<Main> {
    (marked == Some(current)).then_some(Main(PhantomData))
}

/// Test builds only: the marker for a row's own thread, which makes the row's windows.
/// Output: the marker.
#[cfg(test)]
pub fn trial_main() -> Main {
    Main(PhantomData)
}

/// A view's window and its drawing hand-off: the child window over the webview, whose
/// render thread draws the frames `paint` posts, and the fields every host reads.
pub struct View {
    child: Child,
    pub(super) scale: u32,
    pub(super) events: Channel<NativeEvent>,
    pub(super) project: Option<String>,
}

/// The webview's window and the Tauri window that holds it, which the view joins.
pub struct Parent {
    window: HWND,
    webview: HWND,
}

/// The webview's container and the window it is a child of. Input: the handle the host was
/// handed. Output: both, or `native_terminal_webview_missing` when the handle is not a
/// `WRY_WEBVIEW` child window.
pub fn parent(pointer: Webview) -> Result<Parent, &'static str> {
    let webview = pointer as HWND;
    if webview.is_null() || !is_webview(webview) {
        return Err(MISSING);
    }
    let window = unsafe { GetParent(webview) };
    if window.is_null() || unsafe { IsChild(window, webview) } == 0 {
        return Err(MISSING);
    }
    Ok(Parent { window, webview })
}

/// Whether a window's class is wry's webview container. Input: the window. Output: the
/// answer, false when Windows cannot read the class.
fn is_webview(hwnd: HWND) -> bool {
    let mut name = [0_u16; CLASS_UNITS];
    let room = i32::try_from(CLASS_UNITS).expect("a class name room");
    let read = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), room) };
    let Some(read) = usize::try_from(read)
        .ok()
        .filter(|read| *read < CLASS_UNITS)
    else {
        return false;
    };
    name[..read]
        .iter()
        .copied()
        .eq(WEBVIEW_CLASS.encode_utf16())
}

/// The backing scale of the Tauri window, from its DPI; 1 when Windows cannot read it.
/// Input: the parent. Output: the scale.
pub fn parent_scale(parent: &Parent) -> f64 {
    let key = scale_key(unsafe { GetDpiForWindow(parent.window) }).unwrap_or(100);
    f64::from(key) / 100.0
}

/// Makes a hidden child window over the webview, its events going to the host's sink, and
/// hands it to the render thread, started on the first open in the host's style. Inputs:
/// the marker, the parent, the page id, the scale key, the label, the style, the events
/// channel and the wall window's project. Output: the view, or a plain code.
#[allow(clippy::too_many_arguments)]
pub fn open_view(
    _mtm: Main,
    parent: Parent,
    page: u64,
    key: u32,
    _label: &str,
    style: &Style,
    events: Channel<NativeEvent>,
    project: Option<String>,
) -> Result<View, &'static str> {
    let child = Child::new(parent.window, parent.webview, page, sink, style)?;
    Ok(View {
        child,
        scale: key,
        events,
        project,
    })
}

/// Gives the view keyboard focus; a faulted view takes none. Input: the view.
pub fn focus_view(view: &View) {
    view.child.focus();
}

/// Puts a view over its page place and shows it, or hides it. Inputs: the view, the place
/// in CSS pixels and whether it shows. Output: the backing scale of a view asked to show,
/// faulted ones included so `fit` ends them, or none for a hidden or unplaceable one.
pub fn place_view(view: &mut View, rect: [f64; 4], shown: bool) -> Option<f64> {
    let [x, y, width, height] = rect;
    let placed = view.child.place(shown.then_some((x, y, width, height)));
    (shown && placed.is_ok()).then(|| view.child.scale())
}

/// Gives the view a new scale key; the render thread refits its surface from the geometry
/// the view posts. Inputs: the view and the key.
pub fn rescale(view: &mut View, key: u32) {
    view.scale = key;
}

/// Whether the view can take frames at this size: the render thread sizes its surface from
/// the posted geometry, so only a fault refuses. Inputs: the view, the renderer and the
/// size in device pixels. Output: the answer.
pub fn fit(view: &mut View, _shared: &mut Shared, _pixels: (u32, u32)) -> bool {
    !view.child.faulted()
}

/// Posts a frame to the render thread. Inputs: the view, the renderer and the frame.
/// Output: ok, `native_terminal_gpu` once the render thread faulted the view, or the
/// skipped code when the board did not take the frame.
pub fn paint(view: &mut View, _shared: &mut Shared, frame: Frame) -> Result<(), &'static str> {
    if view.child.faulted() {
        return Err(GPU_FAULT);
    }
    if view.child.feed(frame) {
        Ok(())
    } else {
        Err(SKIPPED)
    }
}

/// Whether the view can take a frame now: always, as the render thread keeps only the
/// newest. Input: the view. Output: true.
pub fn ready(_view: &mut View) -> bool {
    true
}

/// Whether the view shows. Input: the view. Output: the answer.
pub fn shown(view: &View) -> bool {
    view.child.shown()
}

/// The backing scale of the view's window now. Input: the view. Output: the scale.
pub fn scale(view: &View) -> f64 {
    view.child.scale()
}

/// The input method's marked text the view holds, drawn over the cells. Input: the view.
/// Output: the text, or none.
pub fn marked(view: &View) -> Option<String> {
    view.child.marked_text()
}

/// Hands UI Automation what a frame showed; it reads the cell size the render thread
/// posted. Inputs: the view, the cell geometry and the terminal the frame came from.
pub fn drawn(view: &View, _geometry: Geometry, term: &Term<VoidListener>) {
    view.child.access(a11y::visible(term), a11y::text_key(term));
}

/// Takes a view down: marked text dropped, then its window, after its surface. Input: the
/// view. Output: its events channel.
pub fn dismiss_view(view: View) -> Channel<NativeEvent> {
    let View { child, events, .. } = view;
    child.cancel_marked();
    drop(child);
    events
}

/// Test builds only: the cell size the render thread posted for the view. Input: the view.
/// Output: the cell, or none before the render thread made it.
#[cfg(test)]
pub fn trial_cell(view: &View) -> Option<(f64, f64)> {
    view.child.cell()
}

#[cfg(test)]
mod tests {
    use super::super::{Command, command};
    use super::{main, main_on, mark_main, mark_on};
    use std::sync::OnceLock;
    use std::thread;

    /// The only row that marks the process's main thread; host calls run on it, before any
    /// view opened, as a page's call on a view that is not there.
    #[test]
    fn the_setup_marks_its_thread_and_host_calls_there_answer_for_a_missing_view() {
        mark_main();
        mark_main();
        assert!(main().is_some(), "the marked thread");
        let elsewhere = thread::spawn(|| main().is_none()).join();
        assert!(elsewhere.expect("another thread"), "another thread");
        let focus = command("main", 1, Command::Focus);
        assert_eq!(focus, Err("native_terminal_view_unknown"));
        assert_eq!(command("main", 1, Command::Close), Ok(()));
    }

    #[test]
    fn a_mark_holds_its_first_thread_and_takes_that_thread_again() {
        let mark = OnceLock::new();
        let here = thread::current().id();
        mark_on(&mark, here);
        mark_on(&mark, here);
        assert_eq!(mark.get(), Some(&here));
        assert!(main_on(mark.get().copied(), here).is_some());
    }

    #[test]
    #[should_panic(expected = "marked once")]
    fn a_mark_from_a_second_thread_is_a_bug() {
        let mark = OnceLock::new();
        let there = thread::spawn(|| thread::current().id())
            .join()
            .expect("another thread");
        mark_on(&mark, there);
        mark_on(&mark, thread::current().id());
    }

    #[test]
    fn main_answers_only_on_the_recorded_thread() {
        let here = thread::current().id();
        let there = thread::spawn(|| thread::current().id())
            .join()
            .expect("another thread");
        assert!(main_on(Some(here), here).is_some(), "the recorded thread");
        assert!(main_on(Some(there), here).is_none(), "another thread");
        assert!(
            main_on(None, here).is_none(),
            "before the setup records one"
        );
    }
}
