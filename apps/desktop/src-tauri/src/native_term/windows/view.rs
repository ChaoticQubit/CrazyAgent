use std::cell::{Cell, RefCell};
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, PAINTSTRUCT, ScreenToClient,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM, CPS_CANCEL, GCS_COMPSTR, GCS_CURSORPOS,
    GCS_RESULTSTR, HIMC, IMECHARPOSITION, IMR_QUERYCHARPOSITION, ISC_SHOWUICOMPOSITIONWINDOW,
    ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME, ImmReleaseContext,
    ImmSetCandidateWindow, ImmSetCompositionWindow, NI_COMPOSITIONSTR,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, GetKeyboardLayout, SetFocus, ToUnicodeEx, VK_CONTROL, VK_LWIN, VK_MENU,
    VK_PROCESSKEY, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
    GetWindowRect, HWND_TOP, IDC_IBEAM, ISMEX_REPLIED, ISMEX_SEND, InSendMessageEx, LoadCursorW,
    MA_ACTIVATE, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW, RegisterClassExW, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_APP, WM_CHAR,
    WM_DEADCHAR, WM_DESTROY, WM_DPICHANGED_AFTERPARENT, WM_ERASEBKGND, WM_GETOBJECT,
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_REQUEST, WM_IME_SETCONTEXT,
    WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEACTIVATE,
    WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WNDCLASSEXW,
    WS_CHILD, WS_CLIPSIBLINGS, WS_EX_NOPARENTNOTIFY,
};

use super::super::a11y::Grid;
use super::super::event::{Sink, ViewEvent};
use super::super::ime::Ime;
use super::super::ime::compose::{self, Read};
use super::super::screen::{Frame, Preedit};
use super::super::style::Style;
use super::board::Geometry;
use super::draw::{self, BOARD, DROP_WAIT};
use super::keys::{self, Mods, Next, Typed};
use super::place::{self, SCALE_KEYS};
use super::uia::{Access, Root};

/// The plain code for a parent window no child view can be made in.
const UNAVAILABLE: &str = "native_terminal_surface_unavailable";

/// The scale key a view uses while its window's DPI cannot be read.
const KEY_DEFAULT: u32 = *SCALE_KEYS.start();

/// ToUnicodeEx's flag that leaves the keyboard state, a pending dead key included, as it
/// was (Windows 10 1607 and later), so reading a chord's character never eats a dead key.
const KEEP_KEYBOARD_STATE: u32 = 0x4;

static CLASS: OnceLock<Option<Vec<u16>>> = OnceLock::new();

/// The render thread's private message to a view's window: the view faulted.
const FAULTED: u32 = WM_APP + 1;

/// Tells a view's window that the render thread faulted it, so its window thread hides it
/// and hands a held focus to the page before the host ends it. Input: the window handle.
/// A refused post (a full queue) still leaves `active`'s fault check and the host's end.
pub(super) fn post_faulted(handle: isize) {
    let _ = unsafe { PostMessageW(handle as HWND, FAULTED, 0, 0) };
}

/// A composition trial's answers: per string kind, the count IMM32 reports and the units the
/// IME holds.
#[cfg(test)]
type Answers = Vec<(u32, i32, Vec<u16>)>;

#[cfg(test)]
thread_local! {
    static TRIAL_KEY: Cell<Option<u32>> = const { Cell::new(None) };
    static TRIAL_COMPOSITION: RefCell<Option<Answers>> = const { RefCell::new(None) };
}

/// Test builds only: makes this thread's views read these answers in place of
/// ImmGetCompositionStringW, as an IME gives them; a kind they lack reads as
/// IMM_ERROR_NODATA. Input: the answers, or none to read the real input context.
#[cfg(test)]
pub fn trial_composition(answers: Option<Answers>) {
    let set = answers.is_some();
    TRIAL_COMPOSITION.with(|trial| *trial.borrow_mut() = answers);
    assert_eq!(
        TRIAL_COMPOSITION.with(|trial| trial.borrow().is_some()),
        set
    );
}

/// Test builds only: the trial's answer to one read, copying at most the buffer's units
/// and reporting the count it was given, true or not. Inputs: the kind, the buffer (null
/// to measure) and its bytes. Output: the reported count, or none without a trial.
#[cfg(test)]
fn trial_read(kind: u32, buffer: *mut u16, bytes: u32) -> Option<i32> {
    TRIAL_COMPOSITION.with(|trial| {
        let trial = trial.borrow();
        let answers = trial.as_ref()?;
        let Some((_, reported, held)) = answers.iter().find(|(asked, _, _)| *asked == kind) else {
            return Some(-1);
        };
        if !buffer.is_null() {
            let room = usize::try_from(bytes / 2).expect("a u32 fits a usize");
            let count = room.min(held.len());
            assert!(count <= room);
            unsafe { ptr::copy_nonoverlapping(held.as_ptr(), buffer, count) };
        }
        Some(*reported)
    })
}

/// Test builds only: makes this thread's views read this scale key in place of their
/// window's DPI, as a move to a display of another scale does. Input: the key, or none.
#[cfg(test)]
pub fn trial_key(key: Option<u32>) {
    assert!(key.is_none_or(|key| SCALE_KEYS.contains(&key)));
    TRIAL_KEY.with(|trial| trial.set(key));
    assert_eq!(TRIAL_KEY.with(Cell::get), key);
}

/// The scale key for a view's window: its DPI's, or none when Windows cannot read it.
fn read_key(hwnd: HWND) -> Option<u32> {
    #[cfg(test)]
    if let Some(key) = TRIAL_KEY.with(Cell::get) {
        return Some(key);
    }
    place::scale_key(unsafe { GetDpiForWindow(hwnd) })
}

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
    focused: Cell<bool>,
    swallow: Cell<bool>,
    held: Cell<Option<u16>>,
    cursor: Cell<Option<(usize, usize)>>,
    preedit: Cell<Option<Preedit>>,
    ime: RefCell<Ime>,
    alive: Cell<bool>,
    root: Root,
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
        let key = read_key(hwnd).unwrap_or(KEY_DEFAULT);
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
            focused: Cell::new(false),
            swallow: Cell::new(false),
            held: Cell::new(None),
            cursor: Cell::new(None),
            preedit: Cell::new(None),
            ime: RefCell::new(Ime::default()),
            alive: Cell::new(true),
            root: Root::new(Access::new(id, key)),
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

    /// Hands the render thread the view's newest screen, keeping its cursor cell and its
    /// drawn marked text's cells for the IME. Input: the frame. Output: whether a live view
    /// took it.
    pub fn feed(&self, frame: Frame) -> bool {
        let cursor = frame
            .cursor
            .as_ref()
            .map(|cursor| (cursor.row, cursor.column));
        let cursor = cursor.or_else(|| frame.preedit.map(|drawn| (drawn.row, drawn.column)));
        self.inner.cursor.set(cursor);
        self.inner.preedit.set(frame.preedit);
        BOARD.post_frame(self.inner.id, frame)
    }

    /// Whether the render thread gave up on the view's GPU work for good, for H5b's host,
    /// which ends it with `native_terminal_gpu`, as macOS ends a view whose frame the GPU
    /// refused. Output: the answer.
    pub fn faulted(&self) -> bool {
        BOARD.faulted(self.inner.id)
    }

    /// The marked text the next frame draws, for the host's `screen::frame`, as the macOS
    /// view's. Output: the text, or none when not composing.
    pub fn marked_text(&self) -> Option<String> {
        let ime = self.inner.ime.borrow();
        ime.composing().then(|| ime.marked().to_owned())
    }

    /// Drops any marked text with nothing sent, as the host does on hide and close.
    pub fn cancel_marked(&self) {
        self.inner.cancel_marked(self.hwnd);
    }

    /// Hands UI Automation the screen a frame drew and raises its text and selection events
    /// for what changed, as the macOS view's `drawn` posts VoiceOver's. Inputs: the visible
    /// grid (`a11y::visible`) and `a11y::text_key` of the same terminal.
    pub fn access(&self, grid: Grid, text_key: u64) {
        let changed = self.inner.root.access().post(grid, text_key);
        self.inner.root.raise(changed);
    }
}

impl Inner {
    fn hide(&self, hwnd: HWND) {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
        if self.shown.replace(false) {
            self.cancel_marked(hwnd);
            BOARD.post_shown(self.id, false);
        }
    }

    /// Whether keys and text reach the sink: the view is live, not faulted, shown, uncovered
    /// and holds keyboard focus. The fault is read here too, since a key can come before
    /// the render thread's `FAULTED` message.
    fn active(&self) -> bool {
        let open = self.alive.get() && !BOARD.faulted(self.id);
        open && self.shown.get() && !self.covered.get() && self.focused.get()
    }

    /// A faulted view fails closed before the host ends it: a held focus goes to the page
    /// and the child hides, which `move_to_box` keeps from now on, so the owner neither types
    /// into a dead pane nor reads its last frame as live.
    fn fail_closed(&self, hwnd: HWND) {
        self.give_focus_back(hwnd);
        let _ = self.apply(hwnd);
    }

    /// Hands keyboard focus to the page when this view holds it: its webview window, or the
    /// parent with none. When Windows refuses that, focus goes to no window, so keys are
    /// dropped rather than sent to a pane that is gone. Input: the view's window.
    fn give_focus_back(&self, hwnd: HWND) {
        if unsafe { GetFocus() } != hwnd {
            return;
        }
        let page = if self.webview.is_null() {
            self.parent
        } else {
            self.webview
        };
        if unsafe { SetFocus(page) }.is_null() {
            unsafe { SetFocus(ptr::null_mut()) };
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
        let shown = !self.covered.get() && !BOARD.faulted(self.id);
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
        self.key.set(read_key(hwnd).unwrap_or(self.key.get()));
        assert!(SCALE_KEYS.contains(&self.key.get()), "a view's scale key");
        self.root.access().rescale(self.key.get());
        let _ = self.apply(hwnd);
    }
}

impl Inner {
    /// A key-down on an active view sends the key's event by `keys::down` and marks the
    /// character message queued behind it to be dropped when the key sent on the key-down;
    /// a key the IME takes, or an inactive view's, goes to the default procedure. Inputs:
    /// the message. Output: its result.
    fn key_down(&self, hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        self.swallow.set(false);
        let vk = u16::try_from(wparam).ok().filter(|&vk| vk != VK_PROCESSKEY);
        let Some(vk) = vk.filter(|_| self.active()) else {
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        };
        let mods = held_mods();
        let chord = mods.control || mods.alt;
        let bare = chord
            .then(|| shift_only_char(vk, lparam, mods.shift))
            .flatten();
        let down = keys::down(vk, mods, queued(hwnd), bare);
        self.swallow.set(down.swallow);
        match down.event {
            Some(event) => (self.sink)(self.id, event),
            None if down.swallow => {}
            None => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
        0
    }

    /// A character message: dropped when its key-down sent already; a system character
    /// (Alt+Space) goes to the default procedure; on an active view one character, a
    /// surrogate pair joined, goes to the sink as text. Inputs: the message. Output: its
    /// result.
    fn character(&self, hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if self.swallow.replace(false) {
            return 0;
        }
        if message == WM_SYSCHAR {
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        let held = self.held.take();
        let unit = u16::try_from(wparam).ok().filter(|_| self.active());
        let Some(unit) = unit else {
            return 0;
        };
        match keys::typed(unit, held) {
            Typed::Text(input) => (self.sink)(self.id, ViewEvent::Key(input)),
            Typed::Held(high) => self.held.set(Some(high)),
            Typed::Dropped => {}
        }
        0
    }

    /// Puts the IME's composition window at the view's cursor cell and its candidate list
    /// clear of the caret's cell inside the drawn marked text, from the cell size the render
    /// thread posted; with no cursor, cell or input context the IME keeps its own placement.
    fn place_ime(&self, hwnd: HWND) {
        let (Some(cursor), Some(cell)) = (self.cursor.get(), BOARD.cell(self.id)) else {
            return;
        };
        let before = {
            let ime = self.ime.borrow();
            ime.selected_range()
                .map_or(0, |(at, _)| ime.cells_before(at))
        };
        let caret = compose::char_cell(cursor, self.preedit.get(), before);
        let key = self.key.get();
        let spots = place::ime_spot(cursor, cell, key).zip(place::ime_spot(caret, cell, key));
        let Some((spot, at)) = spots else {
            return;
        };
        let context = unsafe { ImmGetContext(hwnd) };
        if context.is_null() {
            return;
        }
        assert_eq!(
            spot.point,
            (spot.cell.0, spot.cell.1),
            "the point is the cell's corner"
        );
        assert_eq!(
            at.point,
            (at.cell.0, at.cell.1),
            "the point is the cell's corner"
        );
        let composition = COMPOSITIONFORM {
            dwStyle: CFS_POINT,
            ptCurrentPos: POINT {
                x: spot.point.0,
                y: spot.point.1,
            },
            rcArea: RECT::default(),
        };
        let (left, top, right, bottom) = at.cell;
        let candidate = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE,
            ptCurrentPos: POINT { x: left, y: top },
            rcArea: RECT {
                left,
                top,
                right,
                bottom,
            },
        };
        unsafe { ImmSetCompositionWindow(context, &composition) };
        unsafe { ImmSetCandidateWindow(context, &candidate) };
        unsafe { ImmReleaseContext(hwnd, context) };
    }

    /// A WM_IME_COMPOSITION: reads what its flags name from the input context (a refused
    /// read leaves that part as it was), applies it to the marked text, sends a commit's
    /// inputs, asks for a redraw when the marked text changed and moves the candidate list
    /// to the caret. Inputs: the window and the message's flags.
    fn composition(&self, hwnd: HWND, flags: LPARAM) {
        let reads = compose::reads(flags as u32);
        let context = unsafe { ImmGetContext(hwnd) };
        if context.is_null() {
            return;
        }
        let string = |kind, cap| composition_string(context, kind, cap);
        let result = reads
            .result
            .then(|| string(GCS_RESULTSTR, compose::RESULT_READ_UNITS));
        let marked = reads
            .marked
            .then(|| string(GCS_COMPSTR, compose::MARKED_READ_UNITS));
        let caret = reads
            .caret
            .then(|| unsafe { composition_read(context, GCS_CURSORPOS, ptr::null_mut(), 0) });
        unsafe { ImmReleaseContext(hwnd, context) };
        let read = Read {
            result: result.flatten(),
            marked: marked.flatten(),
            caret,
        };
        let done = compose::compose(&mut self.ime.borrow_mut(), self.active(), read);
        for input in done.inputs {
            (self.sink)(self.id, ViewEvent::Key(input));
        }
        if done.redraw {
            (self.sink)(self.id, ViewEvent::Marked);
        }
        self.place_ime(hwnd);
    }

    /// A WM_IME_ENDCOMPOSITION: any marked text left goes with nothing sent, and the host
    /// hears to redraw.
    fn end_composition(&self) {
        if self.ime.borrow_mut().cancel() {
            (self.sink)(self.id, ViewEvent::Marked);
        }
        assert!(!self.ime.borrow().composing());
    }

    /// Drops any marked text with nothing sent (hidden, focus lost, clicked, destroyed):
    /// the state clears first, so a composition message the cancel sends back finds none,
    /// then the IME's composition is cancelled and, when there was marked text, the host
    /// hears to redraw. Input: the view's window.
    fn cancel_marked(&self, hwnd: HWND) {
        let was = self.ime.borrow_mut().cancel();
        assert!(!self.ime.borrow().composing());
        cancel_composition(hwnd);
        if was {
            (self.sink)(self.id, ViewEvent::Marked);
        }
    }

    /// An IME message. The IME's own composition window stays hidden, as the view draws the
    /// marked text; a composition never reaches the default procedure, so no WM_IME_CHAR
    /// follows a commit; a request other than a character's position goes to the default
    /// procedure. Inputs: the message. Output: its result.
    fn ime_message(&self, hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match message {
            WM_IME_SETCONTEXT => {
                let inline = lparam & !(ISC_SHOWUICOMPOSITIONWINDOW as LPARAM);
                unsafe { DefWindowProcW(hwnd, message, wparam, inline) }
            }
            WM_IME_STARTCOMPOSITION => {
                self.place_ime(hwnd);
                0
            }
            WM_IME_COMPOSITION => {
                self.composition(hwnd, lparam);
                0
            }
            WM_IME_ENDCOMPOSITION => {
                self.end_composition();
                0
            }
            WM_IME_REQUEST if wparam == IMR_QUERYCHARPOSITION as WPARAM => {
                self.char_position(hwnd, lparam)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    /// IMR_QUERYCHARPOSITION: writes character N's cell (its screen point, the line height
    /// and the view's screen rectangle) into the IME's IMECHARPOSITION once its address, its
    /// dwSize and its dwCharPos pass; anything refused answers 0 and writes nothing. Inputs:
    /// the window and the struct's address. Output: 1 when answered.
    fn char_position(&self, hwnd: HWND, lparam: LPARAM) -> LRESULT {
        let asked = lparam as *mut IMECHARPOSITION;
        if asked.is_null() {
            return 0;
        }
        let size = unsafe { (&raw const (*asked).dwSize).read_unaligned() };
        if !compose::fits(size) {
            return 0;
        }
        let offset = unsafe { (&raw const (*asked).dwCharPos).read_unaligned() };
        let before = {
            let ime = self.ime.borrow();
            let Some(offset) = compose::char_offset(offset, &ime) else {
                return 0;
            };
            ime.cells_before(offset)
        };
        let at = self
            .cursor
            .get()
            .zip(BOARD.cell(self.id))
            .and_then(|(cursor, cell)| {
                let char_cell = compose::char_cell(cursor, self.preedit.get(), before);
                place::ime_spot(char_cell, cell, self.key.get())
            });
        let Some(at) = at else {
            return 0;
        };
        let mut point = POINT {
            x: at.point.0,
            y: at.point.1,
        };
        let mut window = RECT::default();
        let placed = unsafe { ClientToScreen(hwnd, &mut point) } != 0
            && unsafe { GetWindowRect(hwnd, &mut window) } != 0;
        if !placed {
            return 0;
        }
        let height = u32::try_from(at.cell.3 - at.cell.1).expect("a spot's cell has height");
        assert!(height > 0);
        unsafe { (&raw mut (*asked).pt).write_unaligned(point) };
        unsafe { (&raw mut (*asked).cLineHeight).write_unaligned(height) };
        unsafe { (&raw mut (*asked).rcDocument).write_unaligned(window) };
        1
    }

    /// Focus leaves the view: keys stop, any composition is cancelled and the host hears.
    fn lose_focus(&self, hwnd: HWND) {
        self.focused.set(false);
        self.root.access().focus(false);
        self.swallow.set(false);
        self.held.set(None);
        self.cancel_marked(hwnd);
        (self.sink)(self.id, ViewEvent::Focus(false));
    }
}

/// The modifier keys held now, as the key message's own thread state reads them.
fn held_mods() -> Mods {
    let down = |vk: u16| unsafe { GetKeyState(i32::from(vk)) } < 0;
    Mods {
        shift: down(VK_SHIFT),
        control: down(VK_CONTROL),
        alt: down(VK_MENU),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

/// The character message TranslateMessage queued behind the key-down being handled, read
/// without removing it. Input: the view's window. Output: what is queued.
fn queued(hwnd: HWND) -> Next {
    let mut message = MSG::default();
    let found = unsafe { PeekMessageW(&mut message, hwnd, WM_CHAR, WM_SYSDEADCHAR, PM_NOREMOVE) };
    if found == 0 {
        return Next::Nothing;
    }
    match message.message {
        WM_CHAR | WM_SYSCHAR => u16::try_from(message.wParam).map_or(Next::Nothing, Next::Char),
        WM_DEADCHAR | WM_SYSDEADCHAR => Next::Dead,
        _ => Next::Nothing,
    }
}

/// The character a key types on the thread's layout with Shift alone, or nothing, for a
/// Ctrl or Alt chord's own character; the keyboard state is left as it was. Inputs: the VK,
/// the key message's lparam (its scan code) and Shift. Output: the character, or none.
fn shift_only_char(vk: u16, lparam: LPARAM, shift: bool) -> Option<char> {
    let mut state = [0u8; 256];
    state[usize::from(VK_SHIFT)] = if shift { 0x80 } else { 0 };
    let scan = u32::try_from((lparam >> 16) & 0xff).ok()?;
    let mut buffer = [0u16; 4];
    let layout = unsafe { GetKeyboardLayout(0) };
    let written = unsafe {
        ToUnicodeEx(
            u32::from(vk),
            scan,
            state.as_ptr(),
            buffer.as_mut_ptr(),
            4,
            KEEP_KEYBOARD_STATE,
            layout,
        )
    };
    if written != 1 {
        return None;
    }
    char::from_u32(u32::from(buffer[0]))
}

/// ImmGetCompositionStringW, or in test builds the trial's answer. Inputs: the context, the
/// kind, a buffer of at least `bytes` (null to measure) and its bytes. Output: the count or
/// caret IMM32 reports.
unsafe fn composition_read(context: HIMC, kind: u32, buffer: *mut u16, bytes: u32) -> i32 {
    #[cfg(test)]
    if let Some(answer) = trial_read(kind, buffer, bytes) {
        return answer;
    }
    unsafe { ImmGetCompositionStringW(context, kind, buffer.cast(), bytes) }
}

/// One composition string of an input context, sized by IMM32's measure but held to a cap
/// before any allocation, its copy count never trusted past the buffer. Inputs: the
/// context, the kind (GCS_COMPSTR or GCS_RESULTSTR) and the cap in UTF-16 units. Output:
/// the text, or none when the IME's answer is refused.
fn composition_string(context: HIMC, kind: u32, cap: usize) -> Option<String> {
    let measured = unsafe { composition_read(context, kind, ptr::null_mut(), 0) };
    let wanted = compose::wanted_units(measured, cap)?;
    if wanted == 0 {
        return Some(String::new());
    }
    let mut buffer = vec![0u16; wanted];
    let bytes = u32::try_from(wanted * 2).ok()?;
    let copied = unsafe { composition_read(context, kind, buffer.as_mut_ptr(), bytes) };
    let copied = compose::copied_units(copied, buffer.len())?;
    assert!(copied <= wanted);
    assert!(wanted <= cap);
    Some(compose::text_of(&buffer[..copied]))
}

/// Cancels the IME composition on a view that hides, loses focus, is clicked or goes, so no
/// commit lands after it. Input: the view's window.
fn cancel_composition(hwnd: HWND) {
    let context = unsafe { ImmGetContext(hwnd) };
    if context.is_null() {
        return;
    }
    unsafe { ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_CANCEL, 0) };
    unsafe { ImmReleaseContext(hwnd, context) };
}

/// Whether this thread is handling a message another thread sent and still waits on, where
/// UI Automation refuses to disconnect a provider. Output: true inside such a send.
fn in_send() -> bool {
    let flags = unsafe { InSendMessageEx(ptr::null()) };
    flags & ISMEX_SEND != 0 && flags & ISMEX_REPLIED == 0
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
            inner.cancel_marked(hwnd);
            unsafe { SetFocus(hwnd) };
            0
        }
        WM_SETFOCUS => {
            inner.focused.set(true);
            inner.root.access().focus(true);
            (inner.sink)(inner.id, ViewEvent::Focus(true));
            0
        }
        WM_KILLFOCUS => {
            inner.lose_focus(hwnd);
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => inner.key_down(hwnd, message, wparam, lparam),
        WM_CHAR | WM_SYSCHAR => inner.character(hwnd, message, wparam, lparam),
        WM_IME_SETCONTEXT
        | WM_IME_STARTCOMPOSITION
        | WM_IME_COMPOSITION
        | WM_IME_ENDCOMPOSITION
        | WM_IME_REQUEST => inner.ime_message(hwnd, message, wparam, lparam),
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
        FAULTED => {
            inner.fail_closed(hwnd);
            0
        }
        WM_GETOBJECT => inner
            .root
            .answer(hwnd as isize, wparam, lparam)
            .unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }),
        WM_DESTROY => {
            inner.cancel_marked(hwnd);
            inner.root.end(hwnd as isize, in_send());
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
    /// Hands a held focus to the page, takes the view from the render thread, waiting bounded
    /// for its surface to drop, then destroys its window. A surface still held past the bound
    /// keeps the window, hidden and cut off from this view, since the surface must not
    /// outlive it.
    fn drop(&mut self) {
        if self.inner.alive.get() {
            self.inner.give_focus_back(self.hwnd);
            self.inner.root.end(self.hwnd as isize, in_send());
        }
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
