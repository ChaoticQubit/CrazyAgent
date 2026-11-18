use crate::gpu::Gpu;
use serde::Deserialize;
use std::cell::RefCell;
use std::io::Write;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};
use tauri::Manager;
use windows_sys::Win32::Foundation::{GlobalFree, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, EnumDisplayMonitors, GetDC, GetPixel, HDC, HMONITOR, ReleaseDC, ValidateRect};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetDpiForMonitor, GetDpiForWindow, GetThreadDpiAwarenessContext, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::Input::Ime::{
    CFS_POINT, COMPOSITIONFORM, CPS_COMPLETE, GCS_RESULTSTR, ImmGetCompositionStringW, ImmGetCompositionWindow, ImmGetContext,
    ImmIsIME, ImmNotifyIME, ImmReleaseContext, ImmSetCompositionStringW, ImmSetCompositionWindow, NI_COMPOSITIONSTR, SCS_SETSTR,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetFocus, GetKeyboardLayoutList, HKL, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, KLF_ACTIVATE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput, SetFocus,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, EnumChildWindows, GUITHREADINFO, GW_CHILD, GW_HWNDNEXT,
    GetClassNameW, GetClientRect, GetForegroundWindow, GetGUIThreadInfo, GetSystemMetrics, GetWindow, GetWindowRect,
    GetWindowThreadProcessId, HWND_TOP, IDC_ARROW, LoadCursorW, MA_ACTIVATE, RegisterClassW, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER,
    SetForegroundWindow, SetWindowPos, ShowWindow, WM_CHAR, WM_DPICHANGED_AFTERPARENT, WM_ERASEBKGND, WM_IME_CHAR,
    WM_IME_COMPOSITION, WM_IME_STARTCOMPOSITION, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_PAINT, WM_SETFOCUS,
    WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE, WindowFromPoint,
};

/// The window class of the native view, so a hit test names it apart from WebView2's.
const CLASS: &str = "CrazyNativeSpike";

/// How long one step waits for the window's thread before the row is logged as timed out.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the whole run may take before the watchdog ends it, so a stuck runner still
/// prints its log.
const WATCHDOG: Duration = Duration::from_secs(240);

/// Frames drawn back to back for the frame-time row, and keys for the key-to-frame row.
const FRAMES: usize = 300;
const KEYS: usize = 50;

/// Characters in the clipboard round trip, the size of a long copied selection.
const CLIP_CHARS: usize = 64 * 1024;

/// The popover's centre in page pixels (dist/index.html `#pop`), inside the terminal box.
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
static LAYOUT: Mutex<(Rect, Rect, f64)> = Mutex::new((Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }, Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }, 1.0));
static PARENT: AtomicIsize = AtomicIsize::new(0);
static CHILD: AtomicIsize = AtomicIsize::new(0);
static UI_THREAD: AtomicU32 = AtomicU32::new(0);
static CHARS: AtomicUsize = AtomicUsize::new(0);
static IME_STARTS: AtomicUsize = AtomicUsize::new(0);
static IME_COMPOSITIONS: AtomicUsize = AtomicUsize::new(0);
static IME_CHARS: AtomicUsize = AtomicUsize::new(0);
static IME_RESULT: Mutex<String> = Mutex::new(String::new());

/// Writes one log line, milliseconds since start then kind then detail, to the log file
/// and stdout, flushed at once so a killed run keeps every line. Inputs: kind and detail.
fn log(kind: &str, detail: &str) {
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
fn wide(text: &str) -> Vec<u16> {
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
fn on_main<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (send, receive) = mpsc::sync_channel(1);
    let app = APP.get()?;
    app.run_on_main_thread(move || {
        let _ = send.send(work());
    })
    .ok()?;
    receive.recv_timeout(STEP_TIMEOUT).ok()
}

/// Draws one frame on the window's thread when the view exists and nothing else holds its
/// state. Output: the frame's timing, or why none was drawn.
fn draw() -> Result<crate::gpu::Timing, String> {
    STATE.with(|state| {
        let mut state = state.try_borrow_mut().map_err(|_| "state busy".to_string())?;
        let State { gpu, typed, text, .. } = &mut *state;
        let gpu = gpu.as_mut().ok_or("no gpu")?;
        gpu.draw(*text, typed)
    })
}

/// Takes one typed character on the window's thread: appends it, draws at once, and when a
/// timed key is outstanding records event, handler and frame times. Input: the UTF-16 unit.
fn on_char(unit: u32) {
    let handled = Instant::now();
    CHARS.fetch_add(1, Ordering::SeqCst);
    let sent = SENT.lock().ok().and_then(|mut sent| sent.take());
    let drawn = STATE.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else {
            return Err("state busy".to_string());
        };
        if let Some(letter) = char::from_u32(unit) {
            state.typed.push(letter);
        }
        let State { gpu, typed, text, .. } = &mut *state;
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

/// The native view's window procedure: a click focuses it, characters and IME messages are
/// counted, paint draws a frame and every other message goes to the default procedure.
/// Inputs: the Win32 message. Output: its result.
unsafe extern "system" fn view_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => return MA_ACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            let before = unsafe { SetFocus(hwnd) };
            log("native_mouse_down", &format!("focus_was={}", class_of(before)));
            return 0;
        }
        WM_SETFOCUS => log("native_focus", "set"),
        WM_KILLFOCUS => log("native_focus", &format!("kill to={}", class_of(wparam as HWND))),
        WM_CHAR => {
            on_char(wparam as u32);
            return 0;
        }
        WM_IME_STARTCOMPOSITION => {
            IME_STARTS.fetch_add(1, Ordering::SeqCst);
        }
        WM_IME_COMPOSITION => {
            IME_COMPOSITIONS.fetch_add(1, Ordering::SeqCst);
            if (lparam as u32) & GCS_RESULTSTR != 0 {
                let context = unsafe { ImmGetContext(hwnd) };
                let mut units = [0u16; 64];
                let bytes = unsafe {
                    ImmGetCompositionStringW(context, GCS_RESULTSTR, units.as_mut_ptr().cast(), (units.len() * 2) as u32)
                };
                unsafe { ImmReleaseContext(hwnd, context) };
                if bytes > 0
                    && let Ok(mut result) = IME_RESULT.lock()
                {
                    *result = String::from_utf16_lossy(&units[..(bytes as usize / 2).min(units.len())]);
                }
            }
        }
        WM_IME_CHAR => {
            IME_CHARS.fetch_add(1, Ordering::SeqCst);
            return 0;
        }
        WM_PAINT => {
            if let Err(error) = draw() {
                log("paint_skipped", &error);
            }
            unsafe { ValidateRect(hwnd, std::ptr::null()) };
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_DPICHANGED_AFTERPARENT => log("native_dpi_changed", &unsafe { GetDpiForWindow(hwnd) }.to_string()),
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
    let mut point = POINT { x: (css.0 * scale).round() as i32, y: (css.1 * scale).round() as i32 };
    unsafe { ClientToScreen(PARENT.load(Ordering::SeqCst) as HWND, &mut point) };
    point
}

/// Creates the native view on first call, as the top child of the window above WebView2,
/// with its GPU; later calls move and size it without changing its z-order. Runs on the
/// window's thread. Inputs: the app and the terminal rectangle with its scale.
fn place_view(app: &tauri::AppHandle, term: Rect, scale: f64) {
    let (x, y, w, h) = client_rect(term, scale);
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    if !child.is_null() {
        unsafe { SetWindowPos(child, std::ptr::null_mut(), x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER) };
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
        log("placed", &format!("{x},{y} {w}x{h}"));
        return;
    }
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
    let child = unsafe {
        CreateWindowExW(0, class.as_ptr(), std::ptr::null(), WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS, x, y, w, h, parent,
            std::ptr::null_mut(), module, std::ptr::null())
    };
    if child.is_null() {
        log("error", "CreateWindowExW returned null");
        return;
    }
    CHILD.store(child as isize, Ordering::SeqCst);
    unsafe { SetWindowPos(child, HWND_TOP, x, y, w, h, SWP_NOACTIVATE) };
    log("child_created", &format!("{x},{y} {w}x{h} parent={}", class_of(parent)));
    match Gpu::new(child as isize, module as isize, w as u32, h as u32, scale as f32) {
        Ok(gpu) => {
            log("env_adapter_used", &gpu.adapter);
            for (index, adapter) in gpu.adapters.iter().enumerate() {
                log("env_adapter_seen", &format!("{index}: {adapter}"));
            }
            STATE.with(|state| state.borrow_mut().gpu = Some(gpu));
        }
        Err(error) => log("gpu_error", &error),
    }
    if let Err(error) = draw() {
        log("draw_error", &error);
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
    log("page_layout", &format!("term={term:?} field={field:?} dpr={dpr}"));
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
    format!("rgb={rgb:06X} raw={pixel:08X} green={} hit={} at={},{}", pixel == GREEN, class_of(hit), point.x, point.y)
}

/// The centre of the terminal box and of the field, in page pixels.
fn centres() -> ((f64, f64), (f64, f64)) {
    let (term, field, _) = LAYOUT.lock().map(|layout| *layout).unwrap_or_default();
    ((term.x + term.w / 2.0, term.y + term.h / 2.0), (field.x + field.w / 2.0, field.y + field.h / 2.0))
}

/// Clicks the left button at a page point through `SendInput`, in virtual-desktop
/// coordinates. Input: the page point. Output: how many events Windows took.
fn click(css: (f64, f64)) -> u32 {
    let scale = LAYOUT.lock().map(|layout| layout.2).unwrap_or(1.0);
    let point = screen_point(css, scale);
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
    let mouse = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, time: 0, dwExtraInfo: 0 },
        },
    };
    let inputs = [mouse(MOUSEEVENTF_MOVE), mouse(MOUSEEVENTF_LEFTDOWN), mouse(MOUSEEVENTF_LEFTUP)];
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) }
}

/// Types text through `SendInput` as Unicode key events, which reach the focused window as
/// WM_CHAR. Input: the text. Output: how many events Windows took.
fn type_text(text: &str) -> u32 {
    let key = |unit: u16, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let inputs: Vec<INPUT> = text
        .encode_utf16()
        .flat_map(|unit| [key(unit, KEYEVENTF_UNICODE), key(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)])
        .collect();
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) }
}

/// The window that has the keyboard focus on the window's thread, by class.
fn focus_class() -> String {
    let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    let thread = UI_THREAD.load(Ordering::SeqCst);
    if unsafe { GetGUIThreadInfo(thread, &mut info) } == 0 {
        return "unknown".into();
    }
    format!("{} active={}", class_of(info.hwndFocus), class_of(info.hwndActive))
}

/// p50, p95 and max of a set of milliseconds, with the count. Input: the values.
/// Output: the summary line.
fn percentiles(values: &mut [f64]) -> String {
    if values.is_empty() {
        return "n=0".into();
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| values[((values.len() - 1) as f64 * p).round() as usize];
    format!("n={} p50={:.2} p95={:.2} max={:.2}", values.len(), at(0.5), at(0.95), values[values.len() - 1])
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
        order.push(format!("{}[{},{} {}x{}]", class_of(child), rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top));
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
    let mut session = 0u32;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    log("env_session", &format!("process={session} console={}", unsafe { WTSGetActiveConsoleSessionId() }));
    let parent = PARENT.load(Ordering::SeqCst) as HWND;
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    log("env_dpi", &format!("parent={} child={} awareness={}", unsafe { GetDpiForWindow(parent) }, unsafe { GetDpiForWindow(child) }, unsafe {
        GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext())
    }));
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, rect: *mut RECT, found: LPARAM) -> windows_sys::core::BOOL {
        let found = unsafe { &mut *(found as *mut Vec<String>) };
        let (mut x, mut y) = (0u32, 0u32);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
        let rect = unsafe { *rect };
        found.push(format!("[{},{} {}x{} dpi={x}]", rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top));
        1
    }
    let mut monitors: Vec<String> = Vec::new();
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(each), &mut monitors as *mut Vec<String> as LPARAM) };
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
    unsafe { GetForegroundWindow() } == parent
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
    log("keys_abc_events", &format!("{} via=SendInput", type_text("abc")));
    std::thread::sleep(Duration::from_millis(300));
    log("keys_abc_to_native", &(CHARS.load(Ordering::SeqCst) - before).to_string());
    log("click_field_events", &click(field).to_string());
    std::thread::sleep(Duration::from_millis(300));
    log("focus_after_click_field", &focus_class());
    let before = CHARS.load(Ordering::SeqCst);
    log("keys_xyz_events", &format!("{} via=SendInput", type_text("xyz")));
    std::thread::sleep(Duration::from_millis(300));
    log("keys_xyz_to_native", &(CHARS.load(Ordering::SeqCst) - before).to_string());
    eval("window.reportField()");
    std::thread::sleep(Duration::from_millis(200));
    log("click_term_again_events", &click(term).to_string());
    std::thread::sleep(Duration::from_millis(300));
    log("focus_after_click_term_again", &focus_class());
    let before = CHARS.load(Ordering::SeqCst);
    type_text("q");
    std::thread::sleep(Duration::from_millis(300));
    log("key_q_to_native", &(CHARS.load(Ordering::SeqCst) - before).to_string());
    log("focus_get_focus_on_ui", &on_main(|| class_of(unsafe { GetFocus() })).unwrap_or_else(|| "timeout".into()));
}

/// The key-to-frame row: each key goes through `SendInput`, its WM_CHAR draws a frame at
/// once, and the time from the send to the frame's submit is kept.
fn key_rows() {
    on_main(|| STATE.with(|state| state.borrow_mut().text = true));
    on_main(|| STATE.with(|state| state.borrow_mut().keys.clear()));
    for _ in 0..KEYS {
        if let Ok(mut sent) = SENT.lock() {
            *sent = Some(Instant::now());
        }
        type_text("k");
        std::thread::sleep(Duration::from_millis(40));
    }
    let keys = on_main(|| STATE.with(|state| state.borrow().keys.clone())).unwrap_or_default();
    let column = |pick: fn(&(f64, f64, f64, f64)) -> f64| keys.iter().map(pick).collect::<Vec<f64>>();
    log("summary_key_send_to_submit_ms", &percentiles(&mut column(|key| key.0)));
    log("summary_key_handler_to_submit_ms", &percentiles(&mut column(|key| key.1)));
    log("summary_key_build_ms", &percentiles(&mut column(|key| key.2)));
    log("summary_key_acquire_ms", &percentiles(&mut column(|key| key.3)));
}

/// The frame-time row: `FRAMES` text frames drawn back to back on the window's thread,
/// with build, acquire and interval percentiles.
fn frame_rows() {
    let timings = on_main(|| {
        let mut out = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
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
    let mut builds: Vec<f64> = timings.iter().map(|timing| timing.build_ms).collect();
    let mut acquires: Vec<f64> = timings.iter().map(|timing| timing.acquire_ms).collect();
    let mut intervals: Vec<f64> =
        timings.windows(2).map(|pair| (pair[1].submitted - pair[0].submitted).as_secs_f64() * 1000.0).collect();
    log("summary_frame_build_ms", &percentiles(&mut builds));
    log("summary_frame_acquire_ms", &percentiles(&mut acquires));
    log("summary_frame_interval_ms", &percentiles(&mut intervals));
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
    let surface = on_main(|| STATE.with(|state| state.borrow().gpu.as_ref().map(Gpu::size))).flatten();
    log("dpi_view", &format!("child_dpi={} parent_dpi={} client={}x{} expected={w}x{h} surface={surface:?} dpr={scale}",
        unsafe { GetDpiForWindow(child) }, unsafe { GetDpiForWindow(parent) }, rect.right, rect.bottom));
}

/// The IME rows: the view has an input context, the composition window follows the
/// cursor, the installed layouts and which are IMEs, and with an IME a set composition
/// string reaches the view as composition and character messages.
fn ime_rows() {
    let report = on_main(|| {
        let child = CHILD.load(Ordering::SeqCst) as HWND;
        unsafe { SetFocus(child) };
        let context = unsafe { ImmGetContext(child) };
        let mut lines = vec![format!("context_null={}", context.is_null())];
        if !context.is_null() {
            let form = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x: 8, y: 400 }, rcArea: RECT::default() };
            let set = unsafe { ImmSetCompositionWindow(context, &form) };
            let mut read = COMPOSITIONFORM::default();
            let got = unsafe { ImmGetCompositionWindow(context, &mut read) };
            lines.push(format!("composition_window set={set} got={got} at={},{}", read.ptCurrentPos.x, read.ptCurrentPos.y));
        }
        let mut layouts = [std::ptr::null_mut(); 32];
        let count = unsafe { GetKeyboardLayoutList(layouts.len() as i32, layouts.as_mut_ptr()) }.max(0) as usize;
        let named: Vec<String> =
            layouts[..count].iter().map(|hkl: &HKL| format!("{:08X} ime={}", *hkl as usize & 0xFFFF_FFFF, unsafe { ImmIsIME(*hkl) })).collect();
        lines.push(format!("layouts={}", named.join(",")));
        let ime = layouts[..count].iter().copied().find(|hkl| unsafe { ImmIsIME(*hkl) } != 0);
        if let (Some(hkl), false) = (ime, context.is_null()) {
            unsafe { ActivateKeyboardLayout(hkl, KLF_ACTIVATE) };
            let text: Vec<u16> = "\u{304b}".encode_utf16().collect();
            let set = unsafe {
                ImmSetCompositionStringW(context, SCS_SETSTR, text.as_ptr().cast(), (text.len() * 2) as u32, std::ptr::null(), 0)
            };
            let done = unsafe { ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_COMPLETE, 0) };
            lines.push(format!("composition set={set} complete={done}"));
        } else {
            lines.push("no IME layout installed: composition not driven".into());
        }
        if !context.is_null() {
            unsafe { ImmReleaseContext(child, context) };
        }
        lines.join(" | ")
    });
    log("ime_setup", &report.unwrap_or_else(|| "timeout".into()));
    std::thread::sleep(Duration::from_millis(500));
    let result = IME_RESULT.lock().map(|result| result.clone()).unwrap_or_default();
    log("ime_messages", &format!("starts={} compositions={} ime_chars={} result={result:?}", IME_STARTS.load(Ordering::SeqCst),
        IME_COMPOSITIONS.load(Ordering::SeqCst), IME_CHARS.load(Ordering::SeqCst)));
}

/// The clipboard row: `CLIP_CHARS` characters written as CF_UNICODETEXT by the view's
/// window and read back equal, timed. Output: the row line.
fn clipboard() -> String {
    let child = CHILD.load(Ordering::SeqCst) as HWND;
    let text: Vec<u16> = (0..CLIP_CHARS).map(|index| u16::from(b'a' + (index % 26) as u8)).chain(std::iter::once(0)).collect();
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
    format!("placed={placed} equal={equal} chars={CLIP_CHARS} write_ms={written:.2} read_ms={:.2}", start.elapsed().as_secs_f64() * 1000.0)
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
    layering("");
    popover_rows();
    focus_rows();
    key_rows();
    frame_rows();
    dpi_rows();
    ime_rows();
    log("clipboard", &on_main(clipboard).unwrap_or_else(|| "timeout".into()));
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
        unsafe { SetWindowPos(child, HWND_TOP, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOMOVE) };
    });
    std::thread::sleep(Duration::from_millis(300));
    layering("_after_reraise");
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
