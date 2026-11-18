use crate::gpu::Gpu;
use serde::Deserialize;
use std::cell::RefCell;
use std::io::Write;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};
use tauri::Manager;
use windows_sys::Win32::Foundation::{
    FILETIME, GlobalFree, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, EnumDisplayMonitors, GetDC, GetPixel, HDC, HMONITOR, ReleaseDC, ValidateRect,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSGetActiveConsoleSessionId,
};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, GetSystemTimes,
};
use windows_sys::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetDpiForMonitor, GetDpiForWindow,
    GetThreadDpiAwarenessContext, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::Input::Ime::{
    CFS_POINT, COMPOSITIONFORM, CPS_CANCEL, CPS_COMPLETE, GCS_COMPSTR, GCS_CURSORPOS,
    GCS_RESULTSTR, HIMC, ImmGetCompositionStringW, ImmGetCompositionWindow, ImmGetContext,
    ImmIsIME, ImmNotifyIME, ImmReleaseContext, ImmSetCompositionStringW, ImmSetCompositionWindow,
    NI_COMPOSITIONSTR, SCS_SETSTR,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetFocus, GetKeyState, GetKeyboardLayoutList, HKL, INPUT, INPUT_0,
    INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, KLF_ACTIVATE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput, SetFocus, VK_CONTROL, VK_MENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, EnumChildWindows, GUITHREADINFO,
    GW_CHILD, GW_HWNDNEXT, GetClassNameW, GetClientRect, GetForegroundWindow, GetGUIThreadInfo,
    GetSystemMetrics, GetWindow, GetWindowRect, GetWindowThreadProcessId, HWND_TOP, IDC_ARROW,
    LoadCursorW, MA_ACTIVATE, RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow, WM_CHAR, WM_DEADCHAR,
    WM_DPICHANGED_AFTERPARENT, WM_ERASEBKGND, WM_IME_CHAR, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_NOTIFY, WM_IME_REQUEST, WM_IME_STARTCOMPOSITION, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_PAINT, WM_SETFOCUS, WM_SYSCHAR,
    WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
    WindowFromPoint,
};

/// The window class of the native view, so a hit test names it apart from WebView2's.
const CLASS: &str = "CrazyNativeSpike";

/// How long one step waits for the window's thread before the row is logged as timed out.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the frame-time row waits for its frames: the software adapter on a hosted
/// runner took over 10 s for them (farm run 44245995942).
const FRAMES_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the frame loop may draw before it stops short and reports how many frames it
/// drew, inside `FRAMES_TIMEOUT`, so a loaded runner never leaves the window's thread busy
/// past the row's wait.
const FRAMES_BUDGET: Duration = Duration::from_secs(45);

/// How long the key row waits for one key's character before it sends the next, so a
/// send time is never paired with an earlier key's frame.
const KEY_WAIT: Duration = Duration::from_secs(1);

/// How long the whole run may take before the watchdog ends it, so a stuck runner still
/// prints its log.
const WATCHDOG: Duration = Duration::from_secs(420);

/// The most key, character and IME messages kept between two reads by a row.
const NOTES_MAX: usize = 512;

/// The font files the font row looks for: the monospace candidates and the fallbacks.
const FONT_CANDIDATES: [&str; 5] = [
    "CascadiaMono.ttf",
    "CascadiaCode.ttf",
    "consola.ttf",
    "seguiemj.ttf",
    "seguisym.ttf",
];

/// Text frames per font in the font row, and how long they may take.
const FONT_FRAMES: usize = 60;
const FONT_BUDGET: Duration = Duration::from_secs(20);

/// Frames drawn back to back for the frame-time row, and keys for the key-to-frame row.
const FRAMES: usize = 300;
const KEYS: usize = 50;

/// Characters in the clipboard round trip, the size of a long copied selection.
const CLIP_CHARS: usize = 64 * 1024;

/// The popover's centre in page pixels (page/index.html `#pop`), inside the terminal box.
const POP_CENTER_CSS: (f64, f64) = (530.0, 280.0);

/// The view's clear colour as a screen COLORREF (0x00BBGGRR): pure green.
const GREEN: u32 = 0x0000_FF00;

/// A rectangle in page pixels, as the page reports it.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// The view's state, owned by the window's thread: the GPU, the typed text, counters the
/// rows read, and the last page layout.
struct State {
    gpu: Option<Gpu>,
    typed: String,
    text: bool,
    keys: Vec<(f64, f64, f64, f64)>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State { gpu: None, typed: String::new(), text: false, keys: Vec::new() }) };
}

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static DRIVE: OnceLock<()> = OnceLock::new();
static SENT: Mutex<Option<Instant>> = Mutex::new(None);
static LAYOUT: Mutex<(Rect, Rect, f64)> = Mutex::new((
    Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    },
    Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    },
    1.0,
));
static PARENT: AtomicIsize = AtomicIsize::new(0);
static CHILD: AtomicIsize = AtomicIsize::new(0);
static UI_THREAD: AtomicU32 = AtomicU32::new(0);
static CHARS: AtomicUsize = AtomicUsize::new(0);
static IME_STARTS: AtomicUsize = AtomicUsize::new(0);
static IME_COMPOSITIONS: AtomicUsize = AtomicUsize::new(0);
static IME_CHARS: AtomicUsize = AtomicUsize::new(0);
static IME_RESULT: Mutex<String> = Mutex::new(String::new());
static TYPED: Mutex<String> = Mutex::new(String::new());
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Writes one log line, milliseconds since start then kind then detail, to the log file
/// and stdout, flushed at once so a killed run keeps every line. Inputs: kind and detail.
pub(crate) fn log(kind: &str, detail: &str) {
    let ms = START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0;
    let line = format!("{ms:.1} {kind} {detail}\n");
    print!("{line}");
    let _ = std::io::stdout().flush();
    if let Ok(mut file) = LOG.lock()
        && let Some(file) = file.as_mut()
    {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// UTF-16 with a trailing zero, for a Win32 wide string. Input: the text. Output: the units.
pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The window class name of a window, or "none" for a null one. Input: the window.
/// Output: the class name.
fn class_of(hwnd: HWND) -> String {
    if hwnd.is_null() {
        return "none".into();
    }
    let mut buffer = [0u16; 128];
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

/// Runs a closure on the window's thread and waits for its result up to `STEP_TIMEOUT`.
/// Input: the closure. Output: its result, or None when the app is gone or it timed out.
pub(crate) fn on_main<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    on_main_within(STEP_TIMEOUT, work)
}

/// The native view's window, null before it exists.
pub(crate) fn child() -> HWND {
    CHILD.load(Ordering::SeqCst) as HWND
}

/// The id of the window's thread, zero before the view exists.
#[cfg(feature = "uia")]
pub(crate) fn ui_thread() -> u32 {
    UI_THREAD.load(Ordering::SeqCst)
}

/// The text typed into the view so far, for readers off the window's thread.
pub(crate) fn typed() -> String {
    TYPED.lock().map(|typed| typed.clone()).unwrap_or_default()
}

/// Keeps one key, character or IME message for the row that reads it; past `NOTES_MAX`
/// the rest are dropped. Input: the entry.
pub(crate) fn note(entry: String) {
    if let Ok(mut notes) = NOTES.lock()
        && notes.len() < NOTES_MAX
    {
        notes.push(entry);
    }
}

/// Takes every kept message, oldest first, and starts a new list. Output: the messages.
pub(crate) fn take_notes() -> Vec<String> {
    NOTES
        .lock()
        .map(|mut notes| std::mem::take(&mut *notes))
        .unwrap_or_default()
}

/// Clicks the terminal box, which focuses the view, and waits for the focus to land.
pub(crate) fn click_term() {
    let (term, _) = centres();
    click(term);
    std::thread::sleep(Duration::from_millis(300));
}

/// Runs a closure on the window's thread and waits for its result up to `wait`.
/// Inputs: the wait and the closure. Output: its result, or None when the app is gone or
/// it timed out.
pub(crate) fn on_main_within<T: Send + 'static>(
    wait: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (send, receive) = mpsc::sync_channel(1);
    let app = APP.get()?;
    app.run_on_main_thread(move || {
        let _ = send.send(work());
    })
    .ok()?;
    receive.recv_timeout(wait).ok()
}

/// Draws one frame on the window's thread when the view exists and nothing else holds its
/// state. Output: the frame's timing, or why none was drawn.
fn draw() -> Result<crate::gpu::Timing, String> {
    STATE.with(|state| {
        let mut state = state
            .try_borrow_mut()
            .map_err(|_| "state busy".to_string())?;
        let State {
            gpu, typed, text, ..
        } = &mut *state;
        let gpu = gpu.as_mut().ok_or("no gpu")?;
        gpu.draw(*text, typed)
    })
}

/// Takes one typed character on the window's thread: appends it and, with a render thread,
/// hands it over without waiting; otherwise draws at once. When a timed key is outstanding
/// it records event, handler and frame times. Input: the UTF-16 unit.
fn on_char(unit: u32) {
    let handled = Instant::now();
    CHARS.fetch_add(1, Ordering::SeqCst);
    let sent = SENT.lock().ok().and_then(|mut sent| sent.take());
    if crate::render::active() {
        if let Some(letter) = char::from_u32(unit)
            && let Ok(mut typed) = TYPED.lock()
        {
            typed.push(letter);
        }
        if !crate::render::key(sent, handled) {
            log("render_queue_full", "");
        }
        return;
    }
    let drawn = STATE.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else {
            return Err("state busy".to_string());
        };
        if let Some(letter) = char::from_u32(unit) {
            state.typed.push(letter);
            if let Ok(mut typed) = TYPED.lock() {
                typed.push(letter);
            }
        }
        let State {
            gpu, typed, text, ..
        } = &mut *state;
        let gpu = gpu.as_mut().ok_or("no gpu")?;
        gpu.draw(*text, typed)
    });
    match (sent, drawn) {
        (Some(sent), Ok(timing)) => STATE.with(|state| {
            if let Ok(mut state) = state.try_borrow_mut() {
                state.keys.push((
                    (timing.submitted - sent).as_secs_f64() * 1000.0,
                    (timing.submitted - handled).as_secs_f64() * 1000.0,
                    timing.build_ms,
                    timing.acquire_ms,
                ));
            }
        }),
        (_, Err(error)) => log("draw_error", &error),
        _ => {}
    }
}

/// One IME string of the view's context. Inputs: the context and which string.
/// Output: the string, empty when the IME has none.
fn ime_string(context: HIMC, kind: u32) -> String {
    let mut units = [0u16; 256];
    let bytes = unsafe {
        ImmGetCompositionStringW(
            context,
            kind,
            units.as_mut_ptr().cast(),
            (units.len() * 2) as u32,
        )
    };
    if bytes <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&units[..(bytes as usize / 2).min(units.len())])
}

/// Keeps what a WM_IME_COMPOSITION carries: the marked string, its cursor and the result.
/// Inputs: the view's window and the message's flags.
fn on_composition(hwnd: HWND, lparam: LPARAM) {
    IME_COMPOSITIONS.fetch_add(1, Ordering::SeqCst);
    let flags = lparam as u32;
    let context = unsafe { ImmGetContext(hwnd) };
    if context.is_null() {
        note(format!("ime_comp:{flags:X} context_null"));
        return;
    }
    let mut entry = format!("ime_comp:{flags:X}");
    if flags & GCS_COMPSTR != 0 {
        entry += &format!(" comp={:?}", ime_string(context, GCS_COMPSTR));
    }
    if flags & GCS_CURSORPOS != 0 {
        let cursor =
            unsafe { ImmGetCompositionStringW(context, GCS_CURSORPOS, std::ptr::null_mut(), 0) };
        entry += &format!(" cursor={cursor}");
    }
    if flags & GCS_RESULTSTR != 0 {
        let result = ime_string(context, GCS_RESULTSTR);
        entry += &format!(" result={result:?}");
        if let Ok(mut kept) = IME_RESULT.lock() {
            *kept = result;
        }
    }
    unsafe { ImmReleaseContext(hwnd, context) };
    note(entry);
}

/// Keeps a key or character message with its code and the Ctrl and Alt state Windows had
/// for it. Inputs: the message and its code.
fn note_key(message: u32, wparam: WPARAM) {
    let kind = match message {
        WM_KEYDOWN => "kd",
        WM_SYSKEYDOWN => "skd",
        WM_SYSKEYUP => "sku",
        WM_CHAR => "ch",
        WM_DEADCHAR => "dc",
        WM_SYSCHAR => "sc",
        WM_SYSDEADCHAR => "sdc",
        _ => "other",
    };
    let ctrl = unsafe { GetKeyState(i32::from(VK_CONTROL)) } < 0;
    let alt = unsafe { GetKeyState(i32::from(VK_MENU)) } < 0;
    note(format!(
        "{kind}:{:04X}{}{}",
        wparam & 0xFFFF,
        if ctrl { "+c" } else { "" },
        if alt { "+a" } else { "" }
    ));
}

/// The native view's window procedure: a click focuses it, key, character and IME
/// messages are kept for the rows, paint draws a frame, and with the `uia` feature the
/// root object request gets the view's provider; the rest go to the default procedure.
/// Inputs: the Win32 message. Output: its result.
unsafe extern "system" fn view_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => return MA_ACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            let before = unsafe { SetFocus(hwnd) };
            log(
                "native_mouse_down",
                &format!("focus_was={}", class_of(before)),
            );
            return 0;
        }
        WM_SETFOCUS => log("native_focus", "set"),
        WM_KILLFOCUS => log(
            "native_focus",
            &format!("kill to={}", class_of(wparam as HWND)),
        ),
        WM_KEYDOWN => note_key(message, wparam),
        WM_SYSKEYDOWN | WM_SYSKEYUP | WM_DEADCHAR | WM_SYSCHAR | WM_SYSDEADCHAR => {
            note_key(message, wparam);
            return 0;
        }
        WM_CHAR => {
            note_key(message, wparam);
            on_char(wparam as u32);
            return 0;
        }
        WM_IME_STARTCOMPOSITION => {
            IME_STARTS.fetch_add(1, Ordering::SeqCst);
            note("ime_start".into());
        }
        WM_IME_COMPOSITION => on_composition(hwnd, lparam),
        WM_IME_ENDCOMPOSITION => note("ime_end".into()),
        WM_IME_NOTIFY => note(format!("ime_ntf:{wparam:X}")),
        WM_IME_REQUEST => {
            if let Some(result) = crate::keys::answer_request(wparam, lparam) {
                return result;
            }
        }
        WM_IME_CHAR => {
            IME_CHARS.fetch_add(1, Ordering::SeqCst);
            note(format!("ime_ch:{:04X}", wparam & 0xFFFF));
            return 0;
        }
        #[cfg(feature = "uia")]
        windows_sys::Win32::UI::WindowsAndMessaging::WM_GETOBJECT => {
            if let Some(result) = crate::uia::answer(hwnd as isize, wparam, lparam) {
                return result;
            }
        }
        WM_PAINT => {
            if let Err(error) = draw() {
                log("paint_skipped", &error);
            }
            unsafe { ValidateRect(hwnd, std::ptr::null()) };
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_DPICHANGED_AFTERPARENT => log(
            "native_dpi_changed",
            &unsafe { GetDpiForWindow(hwnd) }.to_string(),
        ),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// A page rectangle in the parent's client pixels. Inputs: the rectangle and the scale.
/// Output: x, y, width and height, each at least one pixel wide.
fn client_rect(rect: Rect, scale: f64) -> (i32, i32, i32, i32) {
    let x = (rect.x * scale).round() as i32;
    let y = (rect.y * scale).round() as i32;
    let w = ((rect.w * scale).round() as i32).max(1);
    let h = ((rect.h * scale).round() as i32).max(1);
    (x, y, w, h)
}

/// A page point on the screen, through the parent's client origin. Inputs: the page point
/// and the scale. Output: the screen point.
fn screen_point(css: (f64, f64), scale: f64) -> POINT {
    let mut point = POINT {
        x: (css.0 * scale).round() as i32,
        y: (css.1 * scale).round() as i32,
    };
    unsafe { ClientToScreen(PARENT.load(Ordering::SeqCst) as HWND, &mut point) };
    point
}

/// Creates the native view on first call, as the top child of the window above WebView2,
/// with its GPU; later calls move and size it without changing its z-order. Runs on the
/// window's thread and logs how long it took. Inputs: the app and the terminal rectangle
/// with its scale.
fn place_view(app: &tauri::AppHandle, term: Rect, scale: f64) {
    let started = Instant::now();
    let (x, y, w, h) = client_rect(term, scale);
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    if child.is_null() {
        create_view(app, (x, y, w, h), scale);
        return;
    }
    unsafe {
        SetWindowPos(
            child,
            std::ptr::null_mut(),
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    STATE.with(|state| {
        if let Ok(mut state) = state.try_borrow_mut()
            && let Some(gpu) = state.gpu.as_mut()
        {
            gpu.resize(w as u32, h as u32);
        }
    });
    if let Err(error) = draw() {
        log("draw_error", &error);
    }
    let took = started.elapsed().as_secs_f64() * 1000.0;
    log("placed", &format!("{x},{y} {w}x{h} place_ms={took:.2}"));
}

/// Registers the view's class and creates the view at a client rectangle of the window.
/// Inputs: the parent window and the rectangle. Output: the view, or null on failure.
fn create_child(parent: HWND, (x, y, w, h): (i32, i32, i32, i32)) -> HWND {
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = wide(CLASS);
    let registered = unsafe {
        RegisterClassW(&WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(view_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        })
    };
    log("class_registered", &registered.to_string());
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            x,
            y,
            w,
            h,
            parent,
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        )
    }
}

/// Creates the view over the terminal box with its GPU and draws its first frame, logging
/// the surface setup and first-frame times, then starts the rows. Runs on the window's
/// thread. Inputs: the app, the client rectangle and the scale.
fn create_view(app: &tauri::AppHandle, (x, y, w, h): (i32, i32, i32, i32), scale: f64) {
    let Some(window) = app.get_webview_window("main") else {
        log("error", "no main window");
        return;
    };
    let parent = match window.hwnd() {
        Ok(hwnd) => hwnd.0 as HWND,
        Err(error) => {
            log("error", &format!("hwnd: {error}"));
            return;
        }
    };
    PARENT.store(parent as isize, Ordering::SeqCst);
    UI_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    let child = create_child(parent, (x, y, w, h));
    if child.is_null() {
        log("error", "CreateWindowExW returned null");
        return;
    }
    CHILD.store(child as isize, Ordering::SeqCst);
    unsafe { SetWindowPos(child, HWND_TOP, x, y, w, h, SWP_NOACTIVATE) };
    log(
        "child_created",
        &format!("{x},{y} {w}x{h} parent={}", class_of(parent)),
    );
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let setup = Instant::now();
    match Gpu::new(
        child as isize,
        module as isize,
        w as u32,
        h as u32,
        scale as f32,
    ) {
        Ok(gpu) => {
            log(
                "summary_surface_setup_ms",
                &format!("{:.1}", setup.elapsed().as_secs_f64() * 1000.0),
            );
            log("env_adapter_used", &gpu.adapter);
            for (index, adapter) in gpu.adapters.iter().enumerate() {
                log("env_adapter_seen", &format!("{index}: {adapter}"));
            }
            STATE.with(|state| state.borrow_mut().gpu = Some(gpu));
        }
        Err(error) => log("gpu_error", &error),
    }
    let first = Instant::now();
    match draw() {
        Ok(_) => log(
            "summary_first_frame_ms",
            &format!("{:.1}", first.elapsed().as_secs_f64() * 1000.0),
        ),
        Err(error) => log("draw_error", &error),
    }
    if DRIVE.set(()).is_ok() {
        std::thread::spawn(drive);
    }
}

/// The page's layout report: places the native view over the terminal box on the window's
/// thread; a report with a non-positive or non-finite pixel ratio or size is refused.
/// Inputs: the terminal and field rectangles and the page's device pixel ratio.
#[tauri::command]
fn place(app: tauri::AppHandle, term: Rect, field: Rect, dpr: f64) {
    let sane = |value: f64| value.is_finite() && (0.0..=100_000.0).contains(&value);
    if !(sane(dpr) && dpr > 0.0 && sane(term.w) && sane(term.h)) {
        log("page_layout_refused", &format!("term={term:?} dpr={dpr}"));
        return;
    }
    if let Ok(mut layout) = LAYOUT.lock() {
        *layout = (term, field, dpr);
    }
    log(
        "page_layout",
        &format!("term={term:?} field={field:?} dpr={dpr}"),
    );
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || place_view(&handle, term, dpr)) {
        log("error", &format!("run_on_main_thread: {error}"));
    }
}

/// A page line for the log. Inputs: kind and detail.
#[tauri::command]
fn report(kind: String, detail: String) {
    log(&kind, &detail);
}

/// Hides the native view while a page popover covers it and shows it again after, since
/// a child window always draws above the page. Input: whether the popover is open.
#[tauri::command]
fn popover(open: bool) {
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    unsafe { ShowWindow(child, if open { SW_HIDE } else { SW_SHOWNA }) };
    log("native_hidden", &open.to_string());
}

/// The screen pixel at a page point and the window under it. Input: the page point.
/// Output: "rgb=RRGGBB green=bool hit=class".
fn look(css: (f64, f64)) -> String {
    let scale = LAYOUT.lock().map(|layout| layout.2).unwrap_or(1.0);
    let point = screen_point(css, scale);
    let screen: HDC = unsafe { GetDC(std::ptr::null_mut()) };
    let pixel = unsafe { GetPixel(screen, point.x, point.y) };
    unsafe { ReleaseDC(std::ptr::null_mut(), screen) };
    let hit = unsafe { WindowFromPoint(point) };
    let rgb = ((pixel & 0xFF) << 16) | (pixel & 0xFF00) | ((pixel >> 16) & 0xFF);
    format!(
        "rgb={rgb:06X} raw={pixel:08X} green={} hit={} at={},{}",
        pixel == GREEN,
        class_of(hit),
        point.x,
        point.y
    )
}

/// The centre of the terminal box and of the field, in page pixels.
fn centres() -> ((f64, f64), (f64, f64)) {
    let (term, field, _) = LAYOUT.lock().map(|layout| *layout).unwrap_or_default();
    (
        (term.x + term.w / 2.0, term.y + term.h / 2.0),
        (field.x + field.w / 2.0, field.y + field.h / 2.0),
    )
}

/// Clicks the left button at a page point. Input: the page point.
/// Output: how many events Windows took.
fn click(css: (f64, f64)) -> u32 {
    let scale = LAYOUT.lock().map(|layout| layout.2).unwrap_or(1.0);
    click_screen(screen_point(css, scale))
}

/// Clicks the left button at a screen point through `SendInput`, in virtual-desktop
/// coordinates. Input: the screen point. Output: how many events Windows took.
pub(crate) fn click_screen(point: POINT) -> u32 {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let dx = ((point.x - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
    let dy = ((point.y - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
    let mouse = |flags: u32| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        mouse(MOUSEEVENTF_MOVE),
        mouse(MOUSEEVENTF_LEFTDOWN),
        mouse(MOUSEEVENTF_LEFTUP),
    ];
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    }
}

/// Types text through `SendInput` as Unicode key events, which reach the focused window as
/// WM_CHAR. Input: the text. Output: how many events Windows took.
fn type_text(text: &str) -> u32 {
    let key = |unit: u16, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs: Vec<INPUT> = text
        .encode_utf16()
        .flat_map(|unit| {
            [
                key(unit, KEYEVENTF_UNICODE),
                key(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect();
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    }
}

/// The window that has the keyboard focus on the window's thread, by class.
fn focus_class() -> String {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let thread = UI_THREAD.load(Ordering::SeqCst);
    if unsafe { GetGUIThreadInfo(thread, &mut info) } == 0 {
        return "unknown".into();
    }
    format!(
        "{} active={}",
        class_of(info.hwndFocus),
        class_of(info.hwndActive)
    )
}

/// p50, p95 and max of a set of milliseconds, with the count. Input: the values.
/// Output: the summary line.
fn percentiles(values: &mut [f64]) -> String {
    if values.is_empty() {
        return "n=0".into();
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| values[((values.len() - 1) as f64 * p).round() as usize];
    format!(
        "n={} p50={:.2} p95={:.2} max={:.2}",
        values.len(),
        at(0.5),
        at(0.95),
        values[values.len() - 1]
    )
}

/// Runs a script in the page. Input: the script.
fn eval(script: &str) {
    if let Some(window) = APP.get().and_then(|app| app.get_webview_window("main")) {
        let _ = window.eval(script);
    }
}

/// Every child window of the parent in z-order, front first, with class and rectangle,
/// and the tree EnumChildWindows walks. Output: the order line.
fn tree() -> String {
    let parent = PARENT.load(Ordering::SeqCst) as HWND;
    let mut order = Vec::new();
    let mut child = unsafe { GetWindow(parent, GW_CHILD) };
    while !child.is_null() && order.len() < 32 {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(child, &mut rect) };
        order.push(format!(
            "{}[{},{} {}x{}]",
            class_of(child),
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top
        ));
        child = unsafe { GetWindow(child, GW_HWNDNEXT) };
    }
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> windows_sys::core::BOOL {
        let found = unsafe { &mut *(found as *mut Vec<String>) };
        found.push(class_of(hwnd));
        i32::from(found.len() < 64)
    }
    let mut all: Vec<String> = Vec::new();
    unsafe { EnumChildWindows(parent, Some(each), &mut all as *mut Vec<String> as LPARAM) };
    format!("front_to_back={} all={}", order.join(" > "), all.join(","))
}

/// Logs the run's environment: sessions, the foreground window, DPI and awareness, the
/// monitors, and the parent's child window tree.
fn environment() {
    let cores = std::thread::available_parallelism().map_or(0, usize::from);
    log("env_cores", &cores.to_string());
    let mut session = 0u32;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    log(
        "env_session",
        &format!("process={session} console={}", unsafe {
            WTSGetActiveConsoleSessionId()
        }),
    );
    let parent = PARENT.load(Ordering::SeqCst) as HWND;
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    log(
        "env_dpi",
        &format!(
            "parent={} child={} awareness={}",
            unsafe { GetDpiForWindow(parent) },
            unsafe { GetDpiForWindow(child) },
            unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) }
        ),
    );
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _: HDC,
        rect: *mut RECT,
        found: LPARAM,
    ) -> windows_sys::core::BOOL {
        let found = unsafe { &mut *(found as *mut Vec<String>) };
        let (mut x, mut y) = (0u32, 0u32);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
        let rect = unsafe { *rect };
        found.push(format!(
            "[{},{} {}x{} dpi={x}]",
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top
        ));
        1
    }
    let mut monitors: Vec<String> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(each),
            &mut monitors as *mut Vec<String> as LPARAM,
        )
    };
    log("env_monitors", &monitors.join(" "));
    log("env_tree", &tree());
}

/// Brings the app's window to the foreground, attaching to the foreground thread's input
/// when a plain request is refused. Output: whether the window is in front afterwards.
fn foreground() -> bool {
    let parent = PARENT.load(Ordering::SeqCst) as HWND;
    if unsafe { SetForegroundWindow(parent) } != 0 && unsafe { GetForegroundWindow() } == parent {
        return true;
    }
    let front = unsafe { GetForegroundWindow() };
    let theirs = unsafe { GetWindowThreadProcessId(front, std::ptr::null_mut()) };
    let ours = unsafe { GetCurrentThreadId() };
    unsafe { AttachThreadInput(ours, theirs, 1) };
    unsafe { SetForegroundWindow(parent) };
    unsafe { AttachThreadInput(ours, theirs, 0) };
    let now = unsafe { GetForegroundWindow() };
    now == parent
}

/// The z-order and pixel rows: the native view over the terminal box, the page field, and
/// a page popover with and without the hide-on-overlap rule. Input: the row's suffix.
fn layering(suffix: &str) {
    let (term, field) = centres();
    log(&format!("look_term_center{suffix}"), &look(term));
    log(&format!("look_field_center{suffix}"), &look(field));
}

/// The popover rows: a page popover over the view stays under it unless the view hides.
fn popover_rows() {
    eval("window.showPop(false)");
    std::thread::sleep(Duration::from_millis(400));
    log("look_popover_not_hidden", &look(POP_CENTER_CSS));
    eval("window.hidePop(); window.showPop(true)");
    std::thread::sleep(Duration::from_millis(400));
    log("look_popover_view_hidden", &look(POP_CENTER_CSS));
    eval("window.hidePop()");
    std::thread::sleep(Duration::from_millis(400));
    log("look_popover_closed", &look(POP_CENTER_CSS));
}

/// The focus rows: a click on the view focuses it and keys go to it; a click on the page
/// field moves focus to WebView2 and keys go to the page; a click back returns focus.
fn focus_rows() {
    log("foreground", &foreground().to_string());
    let (term, field) = centres();
    log("click_term_events", &click(term).to_string());
    std::thread::sleep(Duration::from_millis(300));
    log("focus_after_click_term", &focus_class());
    let before = CHARS.load(Ordering::SeqCst);
    log(
        "keys_abc_events",
        &format!("{} via=SendInput", type_text("abc")),
    );
    std::thread::sleep(Duration::from_millis(300));
    log(
        "keys_abc_to_native",
        &(CHARS.load(Ordering::SeqCst) - before).to_string(),
    );
    log("click_field_events", &click(field).to_string());
    std::thread::sleep(Duration::from_millis(300));
    log("focus_after_click_field", &focus_class());
    let before = CHARS.load(Ordering::SeqCst);
    log(
        "keys_xyz_events",
        &format!("{} via=SendInput", type_text("xyz")),
    );
    std::thread::sleep(Duration::from_millis(300));
    log(
        "keys_xyz_to_native",
        &(CHARS.load(Ordering::SeqCst) - before).to_string(),
    );
    eval("window.reportField()");
    std::thread::sleep(Duration::from_millis(200));
    log("click_term_again_events", &click(term).to_string());
    std::thread::sleep(Duration::from_millis(300));
    log("focus_after_click_term_again", &focus_class());
    let before = CHARS.load(Ordering::SeqCst);
    type_text("q");
    std::thread::sleep(Duration::from_millis(300));
    log(
        "key_q_to_native",
        &(CHARS.load(Ordering::SeqCst) - before).to_string(),
    );
    log(
        "focus_get_focus_on_ui",
        &on_main(|| class_of(unsafe { GetFocus() })).unwrap_or_else(|| "timeout".into()),
    );
}

/// The key-to-frame row: each key goes through `SendInput`, its WM_CHAR draws a frame at
/// once, and the time from the send to the frame's submit is kept. The next key waits for
/// this key's character (up to `KEY_WAIT`), so a loaded runner never pairs a send with an
/// earlier key's frame; keys whose character never came are counted.
fn key_rows() {
    on_main(|| STATE.with(|state| state.borrow_mut().text = true));
    on_main(|| STATE.with(|state| state.borrow_mut().keys.clear()));
    let mut lost = 0usize;
    for _ in 0..KEYS {
        let before = CHARS.load(Ordering::SeqCst);
        if let Ok(mut sent) = SENT.lock() {
            *sent = Some(Instant::now());
        }
        type_text("k");
        let waited = Instant::now();
        while CHARS.load(Ordering::SeqCst) == before && waited.elapsed() < KEY_WAIT {
            std::thread::sleep(Duration::from_millis(2));
        }
        lost += usize::from(CHARS.load(Ordering::SeqCst) == before);
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(lost <= KEYS);
    log("summary_key_lost", &format!("{lost} of {KEYS}"));
    let keys = if crate::render::active() {
        let drained = crate::render::frames(1, FRAMES_BUDGET)
            .is_some_and(|reply| reply.recv_timeout(FRAMES_TIMEOUT).is_ok());
        log("render_keys_drained", &drained.to_string());
        crate::render::take_keys()
    } else {
        on_main(|| STATE.with(|state| state.borrow().keys.clone())).unwrap_or_default()
    };
    let column =
        |pick: fn(&(f64, f64, f64, f64)) -> f64| keys.iter().map(pick).collect::<Vec<f64>>();
    log(
        "summary_key_send_to_handler_ms",
        &percentiles(&mut column(|key| key.0 - key.1)),
    );
    log(
        "summary_key_send_to_submit_ms",
        &percentiles(&mut column(|key| key.0)),
    );
    log(
        "summary_key_handler_to_submit_ms",
        &percentiles(&mut column(|key| key.1)),
    );
    log(
        "summary_key_build_ms",
        &percentiles(&mut column(|key| key.2)),
    );
    log(
        "summary_key_acquire_ms",
        &percentiles(&mut column(|key| key.3)),
    );
}

/// Logs build, acquire and interval percentiles of back-to-back frames and their wall
/// time. Inputs: the timings and when the row started.
fn frame_summary(timings: &[crate::gpu::Timing], start: Instant) {
    let mut builds: Vec<f64> = timings.iter().map(|timing| timing.build_ms).collect();
    let mut acquires: Vec<f64> = timings.iter().map(|timing| timing.acquire_ms).collect();
    let mut intervals: Vec<f64> = timings
        .windows(2)
        .map(|pair| (pair[1].submitted - pair[0].submitted).as_secs_f64() * 1000.0)
        .collect();
    log("summary_frame_build_ms", &percentiles(&mut builds));
    log("summary_frame_acquire_ms", &percentiles(&mut acquires));
    log("summary_frame_interval_ms", &percentiles(&mut intervals));
    log(
        "summary_frames_wall_ms",
        &format!(
            "{:.1} n={}",
            start.elapsed().as_secs_f64() * 1000.0,
            timings.len()
        ),
    );
}

/// The frame-time row on the render thread: while it draws `FRAMES` frames back to back,
/// the window's thread is pinged every 20 ms, so the row shows whether presents leave it
/// free.
fn thread_frame_rows() {
    let start = Instant::now();
    let Some(reply) = crate::render::frames(FRAMES, FRAMES_BUDGET) else {
        log("summary_frames", "no render thread");
        return;
    };
    let mut pings = Vec::new();
    let timings = loop {
        match reply.recv_timeout(Duration::from_millis(20)) {
            Ok(timings) => break Some(timings),
            Err(mpsc::RecvTimeoutError::Timeout) if start.elapsed() < FRAMES_TIMEOUT => {
                let asked = Instant::now();
                if on_main(|| ()).is_some() {
                    pings.push(asked.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Err(_) => break None,
        }
    };
    log("summary_ui_ping_ms", &percentiles(&mut pings));
    match timings {
        Some(timings) => frame_summary(&timings, start),
        None => log("summary_frames", "timeout"),
    }
}

/// Moves drawing to a render thread for the perf rows: drops the window thread's GPU, then
/// starts the render thread on the same view. Output: whether it runs.
fn start_render_thread() -> bool {
    let size = on_main(|| STATE.with(|state| state.borrow_mut().gpu.take().map(|gpu| gpu.size())))
        .flatten();
    let Some(size) = size else {
        log("render_thread", "no gpu to replace");
        return false;
    };
    let scale = LAYOUT.lock().map(|layout| layout.2).unwrap_or(1.0) as f32;
    let module = unsafe { GetModuleHandleW(std::ptr::null()) } as isize;
    match crate::render::start(CHILD.load(Ordering::SeqCst), module, size, scale) {
        Ok(()) => {
            log("render_thread", "running");
            true
        }
        Err(error) => {
            log("render_thread", &format!("error {error}"));
            false
        }
    }
}

/// The frame-time row: `FRAMES` text frames drawn back to back on the window's thread (or
/// on the render thread when one runs), with build, acquire and interval percentiles.
fn frame_rows() {
    if crate::render::active() {
        thread_frame_rows();
        return;
    }
    let start = Instant::now();
    let timings = on_main_within(FRAMES_TIMEOUT, || {
        let mut out = Vec::with_capacity(FRAMES);
        let started = Instant::now();
        while out.len() < FRAMES && started.elapsed() < FRAMES_BUDGET {
            match draw() {
                Ok(timing) => out.push(timing),
                Err(error) => {
                    log("draw_error", &error);
                    break;
                }
            }
        }
        out
    });
    let Some(timings) = timings else {
        log("summary_frames", "timeout");
        return;
    };
    frame_summary(&timings, start);
}

/// The DPI row: the view's DPI against the parent's and its client size against the
/// page box at the page's pixel ratio.
fn dpi_rows() {
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    let parent = PARENT.load(Ordering::SeqCst) as HWND;
    let mut rect = RECT::default();
    unsafe { GetClientRect(child, &mut rect) };
    let (term, _, scale) = LAYOUT.lock().map(|layout| *layout).unwrap_or_default();
    let (_, _, w, h) = client_rect(term, scale);
    let surface =
        on_main(|| STATE.with(|state| state.borrow().gpu.as_ref().map(Gpu::size))).flatten();
    log(
        "dpi_view",
        &format!(
            "child_dpi={} parent_dpi={} client={}x{} expected={w}x{h} surface={surface:?} dpr={scale}",
            unsafe { GetDpiForWindow(child) },
            unsafe { GetDpiForWindow(parent) },
            rect.right,
            rect.bottom
        ),
    );
}

/// The IME setup, on the window's thread: the view has an input context, the composition
/// window follows the cursor, the installed layouts and which report as IMEs, and the
/// first of those is activated. Output: the row line.
fn ime_setup() -> String {
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    unsafe { SetFocus(child) };
    let context = unsafe { ImmGetContext(child) };
    let mut lines = vec![format!("context_null={}", context.is_null())];
    if !context.is_null() {
        let form = COMPOSITIONFORM {
            dwStyle: CFS_POINT,
            ptCurrentPos: POINT { x: 8, y: 400 },
            rcArea: RECT::default(),
        };
        let set = unsafe { ImmSetCompositionWindow(context, &form) };
        let mut read = COMPOSITIONFORM::default();
        let got = unsafe { ImmGetCompositionWindow(context, &mut read) };
        lines.push(format!(
            "composition_window set={set} got={got} at={},{}",
            read.ptCurrentPos.x, read.ptCurrentPos.y
        ));
        unsafe { ImmReleaseContext(child, context) };
    }
    let mut layouts: [HKL; 32] = [std::ptr::null_mut(); 32];
    let count = unsafe { GetKeyboardLayoutList(layouts.len() as i32, layouts.as_mut_ptr()) }.max(0)
        as usize;
    assert!(count <= layouts.len());
    let named: Vec<String> = layouts[..count]
        .iter()
        .map(|hkl: &HKL| {
            format!("{:08X} ime={}", *hkl as usize & 0xFFFF_FFFF, unsafe {
                ImmIsIME(*hkl)
            })
        })
        .collect();
    lines.push(format!("layouts={}", named.join(",")));
    match layouts[..count]
        .iter()
        .copied()
        .find(|hkl| unsafe { ImmIsIME(*hkl) } != 0)
    {
        Some(hkl) => {
            unsafe { ActivateKeyboardLayout(hkl, KLF_ACTIVATE) };
            lines.push(format!("activated={:08X}", hkl as usize & 0xFFFF_FFFF));
        }
        None => lines.push("no IME layout installed".into()),
    }
    lines.join(" | ")
}

/// Sets the view's marked text as an IME would, on the window's thread.
/// Input: the marked text. Output: whether the IME took it.
fn set_marked(text: &str) -> String {
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    let context = unsafe { ImmGetContext(child) };
    if context.is_null() {
        return "context_null".into();
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    let set = unsafe {
        ImmSetCompositionStringW(
            context,
            SCS_SETSTR,
            units.as_ptr().cast(),
            (units.len() * 2) as u32,
            std::ptr::null(),
            0,
        )
    };
    unsafe { ImmReleaseContext(child, context) };
    format!("set={set}")
}

/// Completes or cancels the view's composition, on the window's thread.
/// Input: CPS_COMPLETE or CPS_CANCEL. Output: whether the IME took it.
fn finish_marked(action: u32) -> String {
    assert!(action == CPS_COMPLETE || action == CPS_CANCEL);
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    let context = unsafe { ImmGetContext(child) };
    if context.is_null() {
        return "context_null".into();
    }
    let done = unsafe { ImmNotifyIME(context, NI_COMPOSITIONSTR, action, 0) };
    unsafe { ImmReleaseContext(child, context) };
    format!("notified={done}")
}

/// Runs one IME step on the window's thread and logs what reached the view.
/// Inputs: the row name and the step.
fn ime_step(row: &str, step: fn() -> String) {
    let done = on_main(step).unwrap_or_else(|| "timeout".into());
    std::thread::sleep(Duration::from_millis(300));
    log(row, &format!("{done} got={}", take_notes().join(" ")));
}

/// The IME rows: setup, then marked text "か", then "かな" with its cursor, the commit and
/// its result, then marked text again and a cancel that must leave no result.
fn ime_rows() {
    log(
        "ime_setup",
        &on_main(ime_setup).unwrap_or_else(|| "timeout".into()),
    );
    take_notes();
    ime_step("ime_marked_ka", || set_marked("\u{304b}"));
    ime_step("ime_marked_kana", || set_marked("\u{304b}\u{306a}"));
    ime_step("ime_commit", || finish_marked(CPS_COMPLETE));
    ime_step("ime_marked_again", || set_marked("\u{304b}"));
    ime_step("ime_cancel", || finish_marked(CPS_CANCEL));
    let result = IME_RESULT
        .lock()
        .map(|result| result.clone())
        .unwrap_or_default();
    log(
        "ime_messages",
        &format!(
            "starts={} compositions={} ime_chars={} result={result:?}",
            IME_STARTS.load(Ordering::SeqCst),
            IME_COMPOSITIONS.load(Ordering::SeqCst),
            IME_CHARS.load(Ordering::SeqCst)
        ),
    );
}

/// The font row: which candidate files the fonts folder holds, then for each monospace
/// candidate present its family, the first text frame (cold glyphs) and the warm build
/// times; Consolas is drawn again after.
fn font_rows() {
    let dir = match crate::gpu::fonts_dir() {
        Ok(dir) => dir,
        Err(error) => {
            log("fonts_dir", &format!("error {error}"));
            return;
        }
    };
    log("fonts_dir", &dir.display().to_string());
    for name in FONT_CANDIDATES {
        let bytes = std::fs::metadata(dir.join(name)).map(|meta| meta.len());
        log("font_candidate", &format!("{name} bytes={bytes:?}"));
    }
    for name in ["CascadiaMono.ttf", "consola.ttf"] {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let line = on_main_within(FRAMES_TIMEOUT, move || font_frames(&path));
        log(
            "font_build_ms",
            &format!("{name} {}", line.unwrap_or_else(|| "timeout".into())),
        );
    }
}

/// Switches the view to one font file and draws text frames with it, on the window's
/// thread. Input: the file. Output: the row line.
fn font_frames(path: &std::path::Path) -> String {
    STATE.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else {
            return "state busy".to_string();
        };
        let State { gpu, typed, .. } = &mut *state;
        let Some(gpu) = gpu.as_mut() else {
            return "no gpu".to_string();
        };
        let family = match gpu.use_font(path) {
            Ok(family) => family,
            Err(error) => return format!("load error {error}"),
        };
        let started = Instant::now();
        let mut builds = Vec::with_capacity(FONT_FRAMES);
        while builds.len() < FONT_FRAMES && started.elapsed() < FONT_BUDGET {
            match gpu.draw(true, typed) {
                Ok(timing) => builds.push(timing.build_ms),
                Err(error) => return format!("family={family} draw error {error}"),
            }
        }
        let first = builds.first().copied().unwrap_or(f64::NAN);
        let warm = percentiles(
            builds
                .get(1..)
                .map(<[f64]>::to_vec)
                .unwrap_or_default()
                .as_mut_slice(),
        );
        format!("family={family:?} first={first:.2} warm {warm}")
    })
}

/// The accessibility row: an out-of-process UI Automation reader asks the view for its
/// text. Without the `uia` feature there is no provider and no text pattern (the red);
/// with it, the text equals the drawn screen and a selection is refused (the green).
fn uia_rows() {
    log("uia_provider_built", &cfg!(feature = "uia").to_string());
    on_main(|| STATE.with(|state| state.borrow_mut().text = true));
    on_main(|| {
        let _ = draw();
    });
    let expected: Vec<u16> = crate::gpu::screen_text(&typed()).encode_utf16().collect();
    let tail =
        String::from_utf16_lossy(&expected[expected.len().saturating_sub(24)..]).replace('\n', "|");
    log(
        "uia_expected",
        &format!("chars={} tail={tail}", expected.len()),
    );
    let lines = match crate::uia::read(CHILD.load(Ordering::SeqCst)) {
        Ok(lines) => lines,
        Err(error) => {
            log("uia_reader", &format!("error {error}"));
            return;
        }
    };
    for line in &lines {
        log("uia_reader", line);
    }
    let has = |wanted: &str| lines.iter().any(|line| line.as_str() == wanted);
    let pattern = has("reader_text_pattern=True");
    let equal = has(&format!("reader_text_chars={}", expected.len()))
        && has(&format!("reader_text_tail={tail}"));
    let refused = lines
        .iter()
        .any(|line| line.starts_with("reader_select=refused"));
    log(
        "uia_verdict",
        &format!("text_pattern={pattern} text_equal={equal} select_refused={refused}"),
    );
}

/// The clipboard row: `CLIP_CHARS` characters written as CF_UNICODETEXT by the view's
/// window and read back equal, timed. Output: the row line.
fn clipboard() -> String {
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    let text: Vec<u16> = (0..CLIP_CHARS)
        .map(|index| u16::from(b'a' + (index % 26) as u8))
        .chain(std::iter::once(0))
        .collect();
    let start = Instant::now();
    if unsafe { OpenClipboard(child) } == 0 {
        return "open refused".into();
    }
    unsafe { EmptyClipboard() };
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, text.len() * 2) };
    let locked = unsafe { GlobalLock(memory) } as *mut u16;
    if locked.is_null() {
        unsafe { CloseClipboard() };
        unsafe { GlobalFree(memory) };
        return "lock refused".into();
    }
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), locked, text.len()) };
    unsafe { GlobalUnlock(memory) };
    let placed = !unsafe { SetClipboardData(u32::from(CF_UNICODETEXT), memory) }.is_null();
    if !placed {
        unsafe { GlobalFree(memory) };
    }
    unsafe { CloseClipboard() };
    let written = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    if unsafe { OpenClipboard(child) } == 0 {
        return format!("placed={placed} write_ms={written:.2} reopen refused");
    }
    let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT)) };
    let mut equal = false;
    if !handle.is_null() {
        let read = unsafe { GlobalLock(handle) } as *const u16;
        if !read.is_null() {
            let back = unsafe { std::slice::from_raw_parts(read, text.len()) };
            equal = back == text.as_slice();
            unsafe { GlobalUnlock(handle) };
        }
    }
    unsafe { CloseClipboard() };
    format!(
        "placed={placed} equal={equal} chars={CLIP_CHARS} write_ms={written:.2} read_ms={:.2}",
        start.elapsed().as_secs_f64() * 1000.0
    )
}

/// The machine's idle and total CPU time so far, in 100 ns units, summed over cores.
/// Output: (idle, total), or zeros when Windows refuses.
fn cpu_times() -> (u64, u64) {
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return (0, 0);
    }
    let value =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    let total = value(kernel) + value(user);
    (value(idle), total)
}

/// The share of CPU time busy between two samples, so a loaded cell shows its load.
/// Inputs: the samples. Output: the percentage line.
fn busy_pct(before: (u64, u64), after: (u64, u64)) -> String {
    let idle = after.0.saturating_sub(before.0);
    let total = after.1.saturating_sub(before.1);
    if total == 0 {
        return "unknown".into();
    }
    format!(
        "{:.1}",
        100.0 * total.saturating_sub(idle) as f64 / total as f64
    )
}

/// Which rows a run takes, from `SPIKE_ROWS`: `base` (every deterministic row, the default),
/// `perf` (keys and frames only, for the quiet and loaded cells), `uia` (the reader) or
/// `lang` (real IMEs and the emoji panel, after the language step).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rows {
    Base,
    Perf,
    Uia,
    Lang,
}

/// The rows `SPIKE_ROWS` asks for. Output: the set, or None for an unknown name.
fn rows() -> Option<Rows> {
    match std::env::var("SPIKE_ROWS").as_deref() {
        Err(_) | Ok("base") => Some(Rows::Base),
        Ok("perf") => Some(Rows::Perf),
        Ok("uia") => Some(Rows::Uia),
        Ok("lang") => Some(Rows::Lang),
        Ok(_) => None,
    }
}

/// Every deterministic row: layering, focus, keys, frames, DPI, IME, layouts, clipboard,
/// fonts, the accessibility reader, then resize, reload and re-raise.
fn base_rows() {
    layering("");
    popover_rows();
    focus_rows();
    key_rows();
    frame_rows();
    dpi_rows();
    ime_rows();
    crate::keys::dead_key_rows();
    crate::keys::altgr_rows();
    log(
        "clipboard",
        &on_main(clipboard).unwrap_or_else(|| "timeout".into()),
    );
    font_rows();
    uia_rows();
    if let Some(window) = APP.get().and_then(|app| app.get_webview_window("main")) {
        let _ = window.set_size(tauri::PhysicalSize::new(1400u32, 900u32));
    }
    std::thread::sleep(Duration::from_millis(1200));
    log("env_tree_after_resize", &tree());
    layering("_after_resize");
    eval("location.reload()");
    std::thread::sleep(Duration::from_millis(2500));
    log("env_tree_after_reload", &tree());
    layering("_after_reload");
    on_main(|| {
        let child = CHILD.load(Ordering::SeqCst) as HWND;
        unsafe {
            SetWindowPos(
                child,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOMOVE,
            )
        };
    });
    std::thread::sleep(Duration::from_millis(300));
    layering("_after_reraise");
}

/// The run's rows in order, then the exit. Runs on its own thread; each window step goes
/// through `on_main`, input through `SendInput`.
fn drive() {
    std::thread::sleep(Duration::from_millis(1500));
    on_main(|| {
        let _ = draw();
    });
    std::thread::sleep(Duration::from_millis(300));
    environment();
    let Some(rows) = rows() else {
        log("rows_refused", "SPIKE_ROWS is not base, perf, uia or lang");
        std::process::exit(5);
    };
    log("rows", &format!("{rows:?}"));
    match rows {
        Rows::Base => base_rows(),
        Rows::Perf => {
            log("foreground", &foreground().to_string());
            click_term();
            let thread = std::env::var("SPIKE_RENDER").as_deref() == Ok("thread");
            log("env_render", if thread { "thread" } else { "window" });
            if thread && !start_render_thread() {
                log("done", "render thread refused");
                std::process::exit(6);
            }
            let before = cpu_times();
            key_rows();
            frame_rows();
            log("env_cpu_busy_pct", &busy_pct(before, cpu_times()));
        }
        Rows::Uia => uia_rows(),
        Rows::Lang => {
            log("foreground", &foreground().to_string());
            crate::keys::candidate_rows();
            crate::keys::real_ime_rows();
            crate::keys::emoji_rows();
        }
    }
    log("done", "");
    std::process::exit(0);
}

/// Starts the spike: the log file from `SPIKE_LOG` (default spike.log), the watchdog, and
/// the Tauri app whose page reports its layout to `place`.
pub fn main() {
    START.get_or_init(Instant::now);
    let path = std::env::var("SPIKE_LOG").unwrap_or_else(|_| "spike.log".into());
    if let Ok(mut file) = LOG.lock() {
        *file = std::fs::File::create(&path).ok();
    }
    log("start", &path);
    let condition: String = std::env::var("SPIKE_CONDITION")
        .unwrap_or_else(|_| "unset".into())
        .chars()
        .take(32)
        .collect();
    log("env_condition", &condition);
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        log("watchdog_exit", &WATCHDOG.as_secs().to_string());
        std::process::exit(3);
    });
    let built = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![place, report, popover])
        .setup(|app| {
            let _ = APP.set(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(error) = built {
        log("error", &format!("tauri: {error}"));
        std::process::exit(4);
    }
}
