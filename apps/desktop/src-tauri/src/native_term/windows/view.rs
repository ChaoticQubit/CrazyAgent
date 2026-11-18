use std::cell::Cell;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT, ScreenToClient};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
    GetWindowRect, HWND_TOP, IDC_IBEAM, LoadCursorW, MA_ACTIVATE, RegisterClassExW, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_DESTROY,
    WM_DPICHANGED_AFTERPARENT, WM_ERASEBKGND, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEACTIVATE,
    WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WNDCLASSEXW, WS_CHILD, WS_CLIPSIBLINGS,
    WS_EX_NOPARENTNOTIFY,
};

use super::super::event::{Sink, ViewEvent};
use super::super::screen::Frame;
use super::super::style::Style;
use super::board::Geometry;
use super::draw::{self, BOARD, DROP_WAIT};
use super::place::{self, SCALE_KEYS};

/// The plain code for a parent window no child view can be made in.
const UNAVAILABLE: &str = "native_terminal_surface_unavailable";

/// The scale key a view uses while its window's DPI cannot be read.
const KEY_DEFAULT: u32 = *SCALE_KEYS.start();

static CLASS: OnceLock<Option<Vec<u16>>> = OnceLock::new();

fn module() -> HINSTANCE {
    unsafe { GetModuleHandleW(ptr::null()) }
}

/// The view window class, registered once: no background brush and no redraw on resize,
/// so only the render thread's swap chain paints it. Output: the class name, or none.
fn class() -> Option<&'static [u16]> {
    let name = CLASS.get_or_init(|| {
        let name: Vec<u16> = "CrazyAgentNativeTerm\0".encode_utf16().collect();
        assert_eq!(name.last(), Some(&0), "a nul-terminated class name");
        let class = WNDCLASSEXW {
            cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).expect("a class size"),
            lpfnWndProc: Some(window_proc),
            hInstance: module(),
            hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_IBEAM) },
            lpszClassName: name.as_ptr(),
            ..Default::default()
        };
        let atom = unsafe { RegisterClassExW(&class) };
        (atom != 0).then_some(name)
    });
    name.as_deref()
}

/// One view's window-thread state, reached from its window procedure.
struct Inner {
    id: u64,
    sink: Sink,
    parent: HWND,
    webview: HWND,
    page: Cell<Option<(f64, f64, f64, f64)>>,
    covered: Cell<bool>,
    key: Cell<u32>,
    placed: Cell<Option<Geometry>>,
    shown: Cell<bool>,
    alive: Cell<bool>,
}

/// One terminal view's native child window over the webview on Windows. Its window thread
/// only places, hides and focuses it and posts frames; the render thread draws it.
pub struct View {
    hwnd: HWND,
    inner: Box<Inner>,
}

impl View {
    /// Makes a hidden child window over the webview for one terminal view and hands it to
    /// the render thread. Inputs: the parent window, the webview window in it (or null), the
    /// view id, the host's event function and the style. Output: the view, or a plain code.
    pub fn new(
        parent: HWND,
        webview: HWND,
        id: u64,
        sink: Sink,
        style: &Style,
    ) -> Result<View, &'static str> {
        draw::start(style)?;
        let class = class().ok_or(UNAVAILABLE)?;
        let style = WS_CHILD | WS_CLIPSIBLINGS;
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOPARENTNOTIFY,
                class.as_ptr(),
                ptr::null(),
                style,
                0,
                0,
                1,
                1,
                parent,
                ptr::null_mut(),
                module(),
                ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(UNAVAILABLE);
        }
        let key = place::scale_key(unsafe { GetDpiForWindow(hwnd) }).unwrap_or(KEY_DEFAULT);
        assert!(SCALE_KEYS.contains(&key));
        let inner = Box::new(Inner {
            id,
            sink,
            parent,
            webview,
            page: Cell::new(None),
            covered: Cell::new(false),
            key: Cell::new(key),
            placed: Cell::new(None),
            shown: Cell::new(false),
            alive: Cell::new(true),
        });
        let geometry = Geometry {
            size: (1, 1),
            scale_key: key,
        };
        if let Err(code) = BOARD.add(id, hwnd as isize, geometry) {
            unsafe { DestroyWindow(hwnd) };
            return Err(code);
        }
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr::from_ref(&*inner) as isize) };
        Ok(View { hwnd, inner })
    }

    /// The view's own window, for the host's focus and hit tests.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Places the view over the page's terminal box and shows it, or hides it for none.
    /// Input: the box in CSS pixels. Output: ok, or `native_terminal_rect_invalid`, which
    /// leaves the view hidden.
    pub fn place(&self, page: Option<(f64, f64, f64, f64)>) -> Result<(), &'static str> {
        self.inner.page.set(page);
        self.inner.apply(self.hwnd)
    }

    /// Hides the view while a page popover, menu or dialog covers its box, and shows it
    /// again after; a box that cannot be placed keeps it hidden, as `place` reported.
    /// Input: whether it is covered.
    pub fn cover(&self, covered: bool) {
        self.inner.covered.set(covered);
        let _ = self.inner.apply(self.hwnd);
    }

    /// Gives the view keyboard focus.
    pub fn focus(&self) {
        unsafe { SetFocus(self.hwnd) };
    }

    /// Hands the render thread the view's newest screen. Input: the frame. Output: whether
    /// a live view took it.
    pub fn feed(&self, frame: Frame) -> bool {
        BOARD.post_frame(self.inner.id, frame)
    }
}

impl Inner {
    fn hide(&self, hwnd: HWND) {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
        if self.shown.replace(false) {
            BOARD.post_shown(self.id, false);
        }
    }

    /// The webview's origin in the parent's client area, in device pixels: zero with no
    /// webview window, none when Windows cannot say.
    fn offset(&self) -> Option<(i32, i32)> {
        if self.webview.is_null() {
            return Some((0, 0));
        }
        let mut frame = RECT::default();
        if unsafe { GetWindowRect(self.webview, &mut frame) } == 0 {
            return None;
        }
        let mut origin = POINT {
            x: frame.left,
            y: frame.top,
        };
        if unsafe { ScreenToClient(self.parent, &mut origin) } == 0 {
            return None;
        }
        Some((origin.x, origin.y))
    }

    /// Moves the view to its page box at its scale, on top of the webview, and shows it,
    /// or hides it with no box, covered, or a box that cannot be placed. Output: ok, or why
    /// the box cannot be placed.
    fn apply(&self, hwnd: HWND) -> Result<(), &'static str> {
        let placed = self.move_to_box(hwnd);
        if placed != Ok(true) {
            self.hide(hwnd);
        }
        placed.map(|_| ())
    }

    /// Posts the render thread only what changed: the size and scale, and showing.
    /// Output: whether the view shows, or why its box cannot be placed.
    fn move_to_box(&self, hwnd: HWND) -> Result<bool, &'static str> {
        let Some(page) = self.page.get() else {
            return Ok(false);
        };
        let offset = self.offset().ok_or(place::RECT_INVALID)?;
        let key = self.key.get();
        assert!(SCALE_KEYS.contains(&key), "a view's scale key");
        let rect = place::rect(page, key, offset)?;
        let width = i32::try_from(rect.w).expect("a bounded width");
        let height = i32::try_from(rect.h).expect("a bounded height");
        let shown = !self.covered.get();
        let flags = SWP_NOACTIVATE | if shown { SWP_SHOWWINDOW } else { 0 };
        unsafe { SetWindowPos(hwnd, HWND_TOP, rect.x, rect.y, width, height, flags) };
        let geometry = Geometry {
            size: (rect.w, rect.h),
            scale_key: key,
        };
        if self.placed.replace(Some(geometry)) != Some(geometry) {
            BOARD.post_geometry(self.id, geometry);
        }
        if shown && !self.shown.replace(true) {
            BOARD.post_shown(self.id, true);
        }
        Ok(shown)
    }

    /// Reads the window's DPI again after its parent moved to a display of another scale
    /// and places the box at it; an unreadable DPI keeps the scale it had.
    fn rescale(&self, hwnd: HWND) {
        let read = place::scale_key(unsafe { GetDpiForWindow(hwnd) });
        self.key.set(read.unwrap_or(self.key.get()));
        assert!(SCALE_KEYS.contains(&self.key.get()), "a view's scale key");
        let _ = self.apply(hwnd);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let held = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Inner;
    let Some(inner) = (unsafe { held.as_ref() }) else {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_MOUSEACTIVATE => MA_ACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            unsafe { SetFocus(hwnd) };
            0
        }
        WM_SETFOCUS => {
            (inner.sink)(inner.id, ViewEvent::Focus(true));
            0
        }
        WM_KILLFOCUS => {
            (inner.sink)(inner.id, ViewEvent::Focus(false));
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut paint) };
            unsafe { EndPaint(hwnd, &paint) };
            0
        }
        WM_DPICHANGED_AFTERPARENT => {
            inner.rescale(hwnd);
            0
        }
        WM_DESTROY => {
            BOARD.remove(inner.id, DROP_WAIT);
            0
        }
        WM_NCDESTROY => {
            inner.alive.set(false);
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

impl Drop for View {
    /// Takes the view from the render thread, waiting bounded for its surface to drop, then
    /// destroys its window. A surface still held past the bound keeps the window, hidden and
    /// cut off from this view, since the surface must not outlive it.
    fn drop(&mut self) {
        let gone = BOARD.remove(self.inner.id, DROP_WAIT);
        if !self.inner.alive.get() {
            return;
        }
        if gone {
            unsafe { DestroyWindow(self.hwnd) };
        } else {
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
        }
    }
}
