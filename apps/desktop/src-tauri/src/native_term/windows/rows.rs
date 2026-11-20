use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationEventHandler, IUIAutomationEventHandler_Impl, IUIAutomationTextPattern,
    IUIAutomationTextRange, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
    TextUnit_Line, TreeScope_Element, UIA_EVENT_ID, UIA_Text_TextChangedEventId, UIA_TextPatternId,
};
use windows_core::{Ref, implement};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CLR_INVALID, ClientToScreen, CreateSolidBrush, GetDC, GetPixel, ReleaseDC, ScreenToClient,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows_sys::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM, GCS_COMPSTR, GCS_CURSORPOS,
    GCS_RESULTSTR, IMECHARPOSITION, IMR_QUERYCHARPOSITION, ImmGetCandidateWindow,
    ImmGetCompositionWindow, ImmGetContext, ImmReleaseContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetCapture, GetDoubleClickTime, GetFocus, GetKeyboardLayout, INPUT,
    INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, KLF_ACTIVATE, LoadKeyboardLayoutW, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyExW, ReleaseCapture,
    SendInput, SetCapture, SetFocus, VK_A, VK_APPS, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_DOWN,
    VK_E, VK_END, VK_ESCAPE, VK_F5, VK_F10, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT,
    VK_OEM_7, VK_PRIOR, VK_Q, VK_RETURN, VK_RIGHT, VK_RMENU, VK_SHIFT, VK_TAB, VK_UP, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, ES_MULTILINE, GW_CHILD,
    GWLP_WNDPROC, GetSystemMetrics, GetWindow, GetWindowRect, IsWindow, IsWindowVisible, MSG,
    PM_NOREMOVE, PM_QS_SENDMESSAGE, PM_REMOVE, PeekMessageW, PostMessageW, RegisterClassExW,
    SM_CXSCREEN, SM_CYSCREEN, SPI_GETWHEELSCROLLLINES, SW_MINIMIZE, SW_RESTORE, SendMessageW,
    SetWindowLongPtrW, ShowWindow, SystemParametersInfoW, TranslateMessage, WM_CHAR,
    WM_CONTEXTMENU, WM_DPICHANGED_AFTERPARENT, WM_IME_CHAR, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_REQUEST, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS,
    WM_LBUTTONDOWN, WM_MOUSEWHEEL, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use super::super::a11y;
use super::super::event::{MouseKind, ViewEvent};
use super::super::host::windows as platform;
use super::super::host::{Command, command};
use super::super::pane::{CONTROL, Input, Named, Pane};
use super::super::render::{GPU_FAULT, Shared};
use super::super::screen::{self, Frame};
use super::super::style::{Style, Theme, font_for};
use super::draw;
use super::mouse;
use super::place;
use super::view::{self, View};
use std::cell::RefCell;
use tauri::ipc::Channel;

/// The longest a farm step waits for the render thread, on a loaded software adapter.
const FARM_WAIT: Duration = Duration::from_secs(60);

/// The longest a teardown may take: well under the render thread's drop bound, so a pass
/// means the surface went, not that the wait ran out.
const TEARDOWN_MAX: Duration = Duration::from_secs(1);

/// The stand-in parent's colour, then the stand-in webview's (COLORREF is 0x00BBGGRR).
const RED: COLORREF = 0x0000_00ff;
const GREEN: COLORREF = 0x0000_ff00;

/// The stand-in webview's colour as the screen reads it.
const WEBVIEW_RGB: (u8, u8, u8) = (0, 255, 0);

/// The page box the row places, in CSS pixels, and the stand-in webview's origin in the
/// parent's client area.
const PAGE: (f64, f64, f64, f64) = (50.0, 40.0, 400.0, 300.0);
const WEBVIEW_AT: (i32, i32) = (10, 20);

static SEEN: Mutex<Vec<(u64, ViewEvent)>> = Mutex::new(Vec::new());

fn sink(id: u64, event: ViewEvent) {
    SEEN.lock().expect("the seen events").push((id, event));
}

fn take_seen() -> Vec<(u64, ViewEvent)> {
    std::mem::take(&mut *SEEN.lock().expect("the seen events"))
}

/// Runs this thread's window messages until a condition holds, failing past the bound.
/// Inputs: what is awaited, the bound and the condition.
fn pump_until(what: &str, wait: Duration, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < wait, "{what} within {wait:?}");
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe { TranslateMessage(&message) };
            unsafe { DispatchMessageW(&message) };
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn pump_for(wait: Duration) {
    let until = Instant::now() + wait;
    pump_until("the pump", wait + Duration::from_secs(1), || {
        Instant::now() >= until
    });
}

fn stand_in_class(name: &'static str, color: COLORREF) -> Vec<u16> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let class = WNDCLASSEXW {
        cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).expect("a class size"),
        lpfnWndProc: Some(DefWindowProcW),
        hInstance: unsafe { GetModuleHandleW(ptr::null()) },
        hbrBackground: unsafe { CreateSolidBrush(color) },
        lpszClassName: wide.as_ptr(),
        ..Default::default()
    };
    assert_ne!(unsafe { RegisterClassExW(&class) }, 0, "the {name} class");
    wide
}

/// A topmost red parent with a green stand-in webview child, as the Tauri window and
/// WRY_WEBVIEW sit. Output: the parent and the webview.
fn stand_ins() -> (HWND, HWND) {
    let parent_class = stand_in_class("NativeRowsParent", RED);
    let webview_class = stand_in_class("WRY_WEBVIEW", GREEN);
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let window = |extended, class: &[u16], style, at: (i32, i32), size: (i32, i32), up| unsafe {
        let (x, y, w, h) = (at.0, at.1, size.0, size.1);
        let none = ptr::null_mut();
        CreateWindowExW(
            extended,
            class.as_ptr(),
            ptr::null(),
            style,
            x,
            y,
            w,
            h,
            up,
            none,
            module,
            ptr::null(),
        )
    };
    let parent_style = WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN;
    let parent = window(
        WS_EX_TOPMOST,
        &parent_class,
        parent_style,
        (0, 0),
        (800, 600),
        ptr::null_mut(),
    );
    assert!(!parent.is_null(), "the stand-in parent");
    let webview_style = WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS;
    let webview = window(
        0,
        &webview_class,
        webview_style,
        WEBVIEW_AT,
        (700, 500),
        parent,
    );
    assert!(!webview.is_null(), "the stand-in webview");
    pump_for(Duration::from_millis(300));
    (parent, webview)
}

fn style() -> Style {
    let (family, source) = font_for("JetBrains Mono").expect("the built-in font");
    Style {
        family,
        source,
        font_px: 13.0,
        blink: false,
        theme: Theme::default(),
    }
}

fn fed_frame(theme: &Theme, text: &[u8]) -> Frame {
    let mut pane = Pane::new(true);
    assert!(pane.on_attached(40, 12));
    assert!(pane.on_output(text));
    screen::frame(pane.term().expect("an attached pane"), theme, true, None)
}

fn on_screen(parent: HWND, client: (i32, i32)) -> POINT {
    let mut point = POINT {
        x: client.0,
        y: client.1,
    };
    assert_ne!(unsafe { ClientToScreen(parent, &mut point) }, 0);
    point
}

fn pixel(at: POINT) -> (u8, u8, u8) {
    let screen = unsafe { GetDC(ptr::null_mut()) };
    assert!(!screen.is_null(), "the screen device context");
    let color = unsafe { GetPixel(screen, at.x, at.y) };
    unsafe { ReleaseDC(ptr::null_mut(), screen) };
    assert_ne!(color, CLR_INVALID, "a screen pixel at {},{}", at.x, at.y);
    let [r, g, b, _] = color.to_le_bytes();
    (r, g, b)
}

fn near(seen: (u8, u8, u8), want: (u8, u8, u8)) -> bool {
    let close = |a: u8, b: u8| a.abs_diff(b) <= 8;
    close(seen.0, want.0) && close(seen.1, want.1) && close(seen.2, want.2)
}

fn assert_color(at: POINT, want: (u8, u8, u8), what: &str) {
    let seen = pixel(at);
    assert!(
        near(seen, want),
        "{what}: {seen:?} at {},{}, want {want:?}",
        at.x,
        at.y
    );
}

fn background() -> (u8, u8, u8) {
    let theme = Theme::default();
    (theme.background.r, theme.background.g, theme.background.b)
}

/// The view's rectangle in the parent's client area, whether it is visible and whether it
/// is the parent's top child.
fn placement(parent: HWND, view: HWND) -> ((i32, i32, i32, i32), bool, bool) {
    let mut frame = RECT::default();
    assert_ne!(unsafe { GetWindowRect(view, &mut frame) }, 0);
    let mut origin = POINT {
        x: frame.left,
        y: frame.top,
    };
    assert_ne!(unsafe { ScreenToClient(parent, &mut origin) }, 0);
    let size = (frame.right - frame.left, frame.bottom - frame.top);
    let visible = unsafe { IsWindowVisible(view) } != 0;
    let top = unsafe { GetWindow(parent, GW_CHILD) } == view;
    ((origin.x, origin.y, size.0, size.1), visible, top)
}

fn presents() -> u64 {
    draw::counts().0
}

fn await_present(what: &str, after: u64) {
    pump_until(what, FARM_WAIT, || presents() > after);
    pump_for(Duration::from_millis(300));
}

/// Samples the first text row and counts samples that differ from the background: the
/// fed text drawn. Input: the view's origin on screen. Output: the count.
fn text_samples(origin: POINT) -> usize {
    let mut drawn = 0;
    for y in (3..14).step_by(4) {
        for x in (0..120).step_by(4) {
            let at = POINT {
                x: origin.x + x,
                y: origin.y + y,
            };
            drawn += usize::from(!near(pixel(at), background()));
        }
    }
    drawn
}

/// One mouse input at a screen point: an absolute move with the button or wheel flags and
/// the wheel's delta. Inputs: the point, the flags and the delta. Output: the input.
fn pointer(at: POINT, flags: u32, delta: i32) -> INPUT {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(2);
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(2);
    let (dx, dy) = (at.x * 65_535 / (width - 1), at.y * 65_535 / (height - 1));
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: delta as u32,
                dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn click(at: POINT) {
    let inputs = [
        pointer(at, 0, 0),
        pointer(at, MOUSEEVENTF_LEFTDOWN, 0),
        pointer(at, MOUSEEVENTF_LEFTUP, 0),
    ];
    let size = i32::try_from(size_of::<INPUT>()).expect("an input size");
    let sent = unsafe { SendInput(3, inputs.as_ptr(), size) };
    assert_eq!(sent, 3, "the click went in");
}

fn placed_shown_and_drawn(parent: HWND, view: &View) -> POINT {
    let before = presents();
    assert!(view.feed(fed_frame(&Theme::default(), b"native rows H1\r\n")));
    view.place(Some(PAGE)).expect("a placed box");
    let want = (WEBVIEW_AT.0 + 50, WEBVIEW_AT.1 + 40, 400, 300);
    assert_eq!(
        placement(parent, view.hwnd()),
        (want, true, true),
        "over the webview"
    );
    await_present("the first present", before);
    let origin = on_screen(parent, (want.0, want.1));
    let centre = on_screen(parent, (want.0 + 200, want.1 + 200));
    assert_color(centre, background(), "the view's background shows");
    let drawn = text_samples(origin);
    assert!(
        drawn >= 5,
        "the fed text is drawn: {drawn} samples off the background"
    );
    let beside = on_screen(parent, (WEBVIEW_AT.0 + 20, WEBVIEW_AT.1 + 20));
    assert_color(beside, WEBVIEW_RGB, "the webview shows beside the view");
    let idle = presents();
    pump_for(Duration::from_millis(500));
    assert_eq!(presents(), idle, "no present while nothing changed");
    println!("h1 row: placed {want:?}, text samples {drawn}, presents {idle}");
    centre
}

fn covered_and_shown_again(view: &View, centre: POINT) {
    view.place(None).expect("the host hides a covered view");
    assert_eq!(
        unsafe { IsWindowVisible(view.hwnd()) },
        0,
        "a covered view hides"
    );
    let hidden = presents();
    assert!(view.feed(fed_frame(&Theme::default(), b"while covered\r\n")));
    pump_for(Duration::from_millis(500));
    assert_eq!(presents(), hidden, "a covered view never presents");
    assert_color(centre, WEBVIEW_RGB, "the webview shows where the view was");
    view.place(Some(PAGE))
        .expect("the host places an uncovered view again");
    assert_ne!(
        unsafe { IsWindowVisible(view.hwnd()) },
        0,
        "uncovered, it shows again"
    );
    await_present("showing redraws", hidden);
    assert_color(centre, background(), "the view draws again");
}

fn focus_follows_clicks(view: &View, webview: HWND, centre: POINT) {
    take_seen();
    view.focus();
    pump_for(Duration::from_millis(100));
    assert_eq!(
        unsafe { GetFocus() },
        view.hwnd(),
        "focus() gives the view focus"
    );
    unsafe { SetFocus(webview) };
    pump_for(Duration::from_millis(100));
    clicked_into(view, centre);
    let seen = take_seen();
    assert!(seen.iter().all(|(id, _)| *id == 1), "{seen:?}");
    let (focus, clicked): (Vec<ViewEvent>, Vec<ViewEvent>) = seen
        .into_iter()
        .map(|(_, event)| event)
        .partition(|event| matches!(event, ViewEvent::Focus(_)));
    let want = [
        ViewEvent::Focus(true),
        ViewEvent::Focus(false),
        ViewEvent::Focus(true),
    ];
    assert_eq!(
        focus, want,
        "focus(), the webview's focus, then the click's"
    );
    one_click(&clicked, view_point(view, centre));
}

/// A move to a 150 percent display: the view takes the new device rectangle and the render
/// thread makes its surface again on the 1.5x shared state, then presents.
fn rescaled_on_a_dpi_change(parent: HWND, view: &View) {
    let before = presents();
    view::trial_key(Some(150));
    unsafe { SendMessageW(view.hwnd(), WM_DPICHANGED_AFTERPARENT, 0, 0) };
    view::trial_key(None);
    let want = (WEBVIEW_AT.0 + 75, WEBVIEW_AT.1 + 60, 600, 450);
    assert_eq!(
        placement(parent, view.hwnd()),
        (want, true, true),
        "at 1.5x"
    );
    await_present("the present at 1.5x", before);
    let centre = on_screen(parent, (want.0 + 300, want.1 + 300));
    assert_color(centre, background(), "the view draws at 1.5x");
    assert_eq!(
        draw::counts().1,
        0,
        "the surface was made again without a failure"
    );
    println!("h1 row: 1.5x placed {want:?}, presents {}", presents());
}

/// Views wider than wgpu's default 8192 texture side (#569 sec F1, gate 1 of the v243
/// merge): 6000 CSS pixels at 1.5x is 9000 device pixels, within the adapter's own side, and
/// 13000 is 19500, past it, so the surface holds to that side. The render thread must not
/// panic in configure: each wide view presents, then the box presents again.
fn held_to_the_device_limit(parent: HWND, view: &View) {
    for (css, device) in [(6000.0, 9000), (13000.0, 19500)] {
        let before = presents();
        view.place(Some((0.0, 0.0, css, 60.0))).expect("a wide box");
        let want = (WEBVIEW_AT.0, WEBVIEW_AT.1, device, 90);
        assert_eq!(
            placement(parent, view.hwnd()),
            (want, true, true),
            "{device} px wide"
        );
        await_present("the present past the default texture limit", before);
    }
    let wide = presents();
    view.place(Some(PAGE)).expect("the box again");
    await_present("the render thread presents after the wide views", wide);
    assert_eq!(draw::counts().1, 0, "no failure past the texture limit");
    println!(
        "h1 row: 9000 and 19500 px views presented, then {} presents",
        presents()
    );
}

fn parent_destroys_a_live_view(parent: HWND, webview: HWND) {
    let second = View::new(parent, webview, 2, sink, &style()).expect("a second view");
    let before = presents();
    assert!(second.feed(fed_frame(&Theme::default(), b"second\r\n")));
    second
        .place(Some((420.0, 40.0, 200.0, 100.0)))
        .expect("a second box");
    await_present("the second view's present", before);
    let hwnd = second.hwnd();
    let started = Instant::now();
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the parent goes first");
    let took = started.elapsed();
    assert!(
        took < TEARDOWN_MAX,
        "the surface went within {took:?}, not at the bound"
    );
    assert_eq!(unsafe { IsWindow(hwnd) }, 0);
    assert!(
        !second.feed(fed_frame(&Theme::default(), b"late\r\n")),
        "off the board"
    );
    drop(second);
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_view_sits_over_the_webview_draws_a_fed_screen_hides_and_drops_its_surface_first() {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, 1, sink, &style()).expect("a view");
    assert_eq!(
        unsafe { IsWindowVisible(view.hwnd()) },
        0,
        "hidden until placed"
    );
    let centre = placed_shown_and_drawn(parent, &view);
    covered_and_shown_again(&view, centre);
    focus_follows_clicks(&view, webview, centre);
    rescaled_on_a_dpi_change(parent, &view);
    held_to_the_device_limit(parent, &view);
    let hwnd = view.hwnd();
    let started = Instant::now();
    drop(view);
    let took = started.elapsed();
    assert!(
        took < TEARDOWN_MAX,
        "the surface went within {took:?}, not at the bound"
    );
    assert_eq!(
        unsafe { IsWindow(hwnd) },
        0,
        "dropped after its surface, the window goes"
    );
    parent_destroys_a_live_view(parent, webview);
    assert_eq!(
        draw::counts().1,
        0,
        "no surface setup, swap-chain wait or draw failed"
    );
}

/// The layouts the key rows load, by their KLID.
/// A minimized parent and an empty page box hide a view without ending it: restored and
/// in its box again, it draws and has not faulted. Inputs: the parent and the view.
fn survives_a_minimized_parent_and_an_empty_box(parent: HWND, view: &View) {
    unsafe { ShowWindow(parent, SW_MINIMIZE) };
    assert!(view.feed(fed_frame(&Theme::default(), b"minimized\r\n")));
    pump_for(Duration::from_millis(500));
    let before = presents();
    unsafe { ShowWindow(parent, SW_RESTORE) };
    assert!(view.feed(fed_frame(&Theme::default(), b"restored\r\n")));
    await_present("the present after the restore", before);
    assert!(!view.faulted(), "a minimized parent does not end the view");
    let empty = view.place(Some((50.0, 40.0, 0.0, 0.0)));
    assert_eq!(
        empty,
        Err(place::RECT_INVALID),
        "an empty box cannot be placed"
    );
    assert_eq!(
        unsafe { IsWindowVisible(view.hwnd()) },
        0,
        "an empty box hides the view"
    );
    pump_for(Duration::from_millis(300));
    let hidden = presents();
    view.place(Some(PAGE)).expect("the box again");
    await_present("the present in the box again", hidden);
    assert!(!view.faulted(), "an empty box does not end the view");
    println!(
        "h5a row: minimized, restored and an empty box, then presents {}",
        presents()
    );
}

/// A refused resize faults the view, as macOS ends one, and nothing draws or remakes it
/// after. Input: the view, id 1.
fn faults_on_a_refused_resize_with_no_remake(view: &View) {
    assert!(!view.faulted(), "a drawn view has not faulted");
    draw::trial_refuse_resize(1);
    view.place(Some((50.0, 40.0, 420.0, 300.0)))
        .expect("a wider box");
    pump_until("the refused resize faults the view", FARM_WAIT, || {
        view.faulted()
    });
    pump_for(Duration::from_millis(300));
    let faulted = presents();
    let later = fed_frame(&Theme::default(), b"after the fault\r\n");
    assert!(view.feed(later), "a faulted view still takes posts");
    view.place(Some(PAGE)).expect("the box again");
    pump_for(Duration::from_millis(500));
    assert_eq!(
        presents(),
        faulted,
        "a faulted view is never drawn or remade"
    );
    println!("h5a row: a refused resize faulted the view, presents stay {faulted}");
}

/// Drops a view and checks its surface went within the bound. Inputs: the view and what
/// it is.
fn dropped_within_the_bound(view: View, what: &str) {
    let started = Instant::now();
    drop(view);
    let took = started.elapsed();
    assert!(
        took < TEARDOWN_MAX,
        "{what}: the surface went within {took:?}"
    );
}

/// A device lost between a view's swap-chain wait and its draw faults that view and, at
/// once, a hidden view still on that device, and a view opened after the loss draws on a
/// new device. Inputs: the parent and the webview.
fn faults_on_a_loss_after_the_wait_and_a_new_view_draws(parent: HWND, webview: HWND) {
    let lost = View::new(parent, webview, 2, sink, &style()).expect("a second view");
    let before = presents();
    assert!(lost.feed(fed_frame(&Theme::default(), b"before the loss\r\n")));
    lost.place(Some(PAGE)).expect("a box");
    await_present("the present before the loss", before);
    let hidden = View::new(parent, webview, 4, sink, &style()).expect("a view to hide");
    let shown = presents();
    assert!(hidden.feed(fed_frame(&Theme::default(), b"hidden after\r\n")));
    hidden
        .place(Some((420.0, 160.0, 100.0, 60.0)))
        .expect("a fourth box");
    await_present("the fourth view's present", shown);
    hidden.place(None).expect("no box hides it");
    draw::trial_lose_after_wait(2);
    assert!(lost.feed(fed_frame(&Theme::default(), b"after the loss\r\n")));
    pump_until("the loss faults the view", FARM_WAIT, || lost.faulted());
    let made = draw::trial_shared_made();
    let after = View::new(parent, webview, 3, sink, &style()).expect("a view after the loss");
    let fresh = presents();
    assert!(after.feed(fed_frame(&Theme::default(), b"a new device\r\n")));
    after
        .place(Some((420.0, 40.0, 200.0, 100.0)))
        .expect("a third box");
    await_present("a view after the loss draws on a new device", fresh);
    assert_eq!(
        draw::trial_shared_made(),
        made,
        "the new device came into the kept shared state, not new shared state"
    );
    assert!(!after.faulted(), "the view after the loss has not faulted");
    assert!(lost.faulted(), "the lost view stays faulted");
    assert!(
        hidden.faulted(),
        "the hidden view on the lost device faulted with it"
    );
    dropped_within_the_bound(hidden, "the hidden view");
    dropped_within_the_bound(lost, "the lost view");
    dropped_within_the_bound(after, "the view after the loss");
    println!(
        "h5a row: a loss faulted views 2 and 4, view 3 drew on a new device, presents {}",
        presents()
    );
}

/// Draws a view once, then removes the D3D12 device right before its next present and
/// waits until the seam has removed it, so `Present` itself is the first call to find the
/// loss. Inputs: the parent and the webview. Output: the view, id 1.
fn removed_inside_present(parent: HWND, webview: HWND) -> View {
    let view = View::new(parent, webview, 1, sink, &style()).expect("a view");
    let before = presents();
    assert!(view.feed(fed_frame(&Theme::default(), b"before the removal\r\n")));
    view.place(Some(PAGE)).expect("a box");
    await_present("the present before the removal", before);
    assert_eq!(
        draw::trial_removals(),
        0,
        "no removal before the seam is armed"
    );
    draw::trial_remove_at_present(1);
    assert!(view.feed(fed_frame(&Theme::default(), b"after the removal\r\n")));
    pump_until("the seam removed the device", FARM_WAIT, || {
        draw::trial_removals() == 1
    });
    view
}

#[test]
#[ignore = "needs a Windows desktop and a process of its own, since a removed D3D12 device \
            stays removed for the process: the farm runs it alone"]
fn a_device_removed_inside_present_faults_the_view() {
    let (parent, webview) = stand_ins();
    let view = removed_inside_present(parent, webview);
    pump_until("the removal faults the view", FARM_WAIT, || view.faulted());
    let (presents, failures) = draw::counts();
    println!("device-lost row: view 1 faulted, presents {presents}, failures {failures}");
    dropped_within_the_bound(view, "the faulted view");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the parent goes last");
}

#[test]
#[ignore = "needs a Windows desktop and a process of its own, since a removed D3D12 device \
            stays removed for the process: the farm runs it alone"]
fn the_render_thread_answers_a_view_placed_after_a_removal_inside_present() {
    let (parent, webview) = stand_ins();
    let removed = removed_inside_present(parent, webview);
    let before = presents();
    let later = View::new(parent, webview, 2, sink, &style()).expect("a later view");
    assert!(later.feed(fed_frame(&Theme::default(), b"a later view\r\n")));
    later
        .place(Some((420.0, 40.0, 200.0, 100.0)))
        .expect("a second box");
    pump_until(
        "the render thread answers the later view",
        FARM_WAIT,
        || later.faulted() || presents() > before,
    );
    let replaced = if later.faulted() { "no" } else { "yes" };
    println!("device-lost row: a replacement device after the removal came up: {replaced}");
    dropped_within_the_bound(later, "the later view");
    dropped_within_the_bound(removed, "the removed view");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the parent goes last");
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_gpu_refusal_or_loss_faults_a_view_for_good_and_a_minimized_parent_does_not() {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, 1, sink, &style()).expect("a view");
    placed_shown_and_drawn(parent, &view);
    survives_a_minimized_parent_and_an_empty_box(parent, &view);
    faults_on_a_refused_resize_with_no_remake(&view);
    dropped_within_the_bound(view, "the faulted view");
    faults_on_a_loss_after_the_wait_and_a_new_view_draws(parent, webview);
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the parent goes last");
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_failed_wait_faults_its_own_view_never_another_at_its_scale() {
    let (parent, webview) = stand_ins();
    let failing = View::new(parent, webview, 1, sink, &style()).expect("a view");
    let other = View::new(parent, webview, 2, sink, &style()).expect("a second view");
    let before = presents();
    assert!(failing.feed(fed_frame(&Theme::default(), b"one\r\n")));
    failing
        .place(Some((50.0, 40.0, 200.0, 100.0)))
        .expect("a box");
    assert!(other.feed(fed_frame(&Theme::default(), b"two\r\n")));
    other
        .place(Some((300.0, 40.0, 200.0, 100.0)))
        .expect("a second box");
    pump_until("both views present", FARM_WAIT, || presents() >= before + 2);
    draw::trial_fail_waits(Some(1));
    assert!(failing.feed(fed_frame(&Theme::default(), b"a failed wait\r\n")));
    pump_for(Duration::from_millis(500));
    draw::trial_fail_waits(None);
    let drawn = presents();
    assert!(other.feed(fed_frame(&Theme::default(), b"after it\r\n")));
    pump_until("the other view draws or faults", FARM_WAIT, || {
        presents() > drawn || other.faulted()
    });
    assert!(
        !other.faulted(),
        "another view's failed wait never faults this one"
    );
    assert!(
        failing.faulted(),
        "a failed wait whose reconfigure recorded an error faults its own view"
    );
    println!(
        "h5a row: view 1's failed wait faulted view 1, view 2 drew, presents {}",
        presents()
    );
    dropped_within_the_bound(failing, "the failed view");
    dropped_within_the_bound(other, "the other view");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the parent goes last");
}

const GERMAN: &str = "00000407";
const US_INTERNATIONAL: &str = "00020409";

/// How long a key row waits for its first event, then how long it keeps pumping to catch a
/// second one.
const KEY_WAIT: Duration = Duration::from_secs(5);
const KEY_TAIL: Duration = Duration::from_millis(250);

/// A layout's id as the wide string LoadKeyboardLayoutW takes.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// One keyboard input, by VK with its scan code in the thread's layout or, with
/// KEYEVENTF_UNICODE, by UTF-16 unit. Right Alt and the navigation keys go as extended.
fn keyboard(vk: u16, unit: u16, flags: u32) -> INPUT {
    let layout = unsafe { GetKeyboardLayout(0) };
    let scan = if vk == 0 {
        unit
    } else {
        let scan = unsafe { MapVirtualKeyExW(u32::from(vk), MAPVK_VK_TO_VSC, layout) };
        u16::try_from(scan).expect("a scan code")
    };
    let navigation = [
        VK_RMENU, VK_UP, VK_DOWN, VK_LEFT, VK_RIGHT, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_INSERT,
        VK_DELETE,
    ];
    let extended = if navigation.contains(&vk) {
        KEYEVENTF_EXTENDEDKEY
    } else {
        0
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags | extended,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    let count = u32::try_from(inputs.len()).expect("a few inputs");
    let size = i32::try_from(size_of::<INPUT>()).expect("an input size");
    let sent = unsafe { SendInput(count, inputs.as_ptr(), size) };
    assert_eq!(sent, count, "the keys went in");
}

/// The held keys pressed in order, the key tapped, then the held keys released.
fn chord(held: &[u16], key: u16) -> Vec<INPUT> {
    let down = |vk| keyboard(vk, 0, 0);
    let up = |vk| keyboard(vk, 0, KEYEVENTF_KEYUP);
    let mut inputs: Vec<INPUT> = held.iter().map(|&vk| down(vk)).collect();
    inputs.extend([down(key), up(key)]);
    inputs.extend(held.iter().rev().map(|&vk| up(vk)));
    assert_eq!(inputs.len(), held.len() * 2 + 2);
    inputs
}

/// The typing events a view sent for some input: everything but focus changes, after the
/// first one came and the tail passed with no second one missed.
fn typed_events(what: &str, inputs: &[INPUT]) -> Vec<ViewEvent> {
    take_seen();
    send(inputs);
    let typing = |seen: &[(u64, ViewEvent)]| {
        seen.iter()
            .any(|(_, event)| !matches!(event, ViewEvent::Focus(_)))
    };
    pump_until(what, KEY_WAIT, || {
        typing(&SEEN.lock().expect("the seen events"))
    });
    pump_for(KEY_TAIL);
    let seen = take_seen();
    assert!(seen.iter().all(|(id, _)| *id == 3), "{what}: {seen:?}");
    seen.into_iter()
        .map(|(_, event)| event)
        .filter(|event| !matches!(event, ViewEvent::Focus(_)))
        .collect()
}

fn text(bytes: &[u8]) -> ViewEvent {
    ViewEvent::Key(Input::Text(bytes.to_vec()))
}

/// Each key, as the panel sends it, reaches the sink exactly once: no key-down and
/// character message both sending.
fn keys_send_once() {
    let named = |key, mods| ViewEvent::Key(Input::Key { key, mods });
    let rows = [
        ("a", chord(&[], VK_A), text(b"a")),
        ("Enter", chord(&[], VK_RETURN), text(b"\r")),
        ("Shift+Enter", chord(&[VK_SHIFT], VK_RETURN), text(b"\n")),
        ("Backspace", chord(&[], VK_BACK), text(b"\x7f")),
        ("Tab", chord(&[], VK_TAB), text(b"\t")),
        ("Shift+Tab", chord(&[VK_SHIFT], VK_TAB), text(b"\x1b[Z")),
        ("Escape", chord(&[], VK_ESCAPE), text(b"\x1b")),
        ("Ctrl+A", chord(&[VK_CONTROL], VK_A), text(b"\x01")),
        ("Alt+A", chord(&[VK_MENU], VK_A), text(b"\x1ba")),
        (
            "Ctrl+Up",
            chord(&[VK_CONTROL], VK_UP),
            named(Named::Up, CONTROL),
        ),
        ("F5", chord(&[], VK_F5), named(Named::F(5), 0)),
        (
            "Ctrl+C",
            chord(&[VK_CONTROL], VK_C),
            ViewEvent::CopyKey(Input::Text(vec![0x03])),
        ),
        ("Ctrl+V", chord(&[VK_CONTROL], VK_V), ViewEvent::Paste),
    ];
    for (what, inputs, want) in rows {
        assert_eq!(typed_events(what, &inputs), [want], "{what}");
    }
    println!("h2 row: 13 keys, one event each");
}

/// Types on a loaded layout, then puts the thread's layout back. Inputs: the layout, what
/// is typed, the inputs and the text wanted.
fn on_layout(layout: &str, what: &str, inputs: impl FnOnce() -> Vec<INPUT>, want: &str) {
    let before = unsafe { GetKeyboardLayout(0) };
    let loaded = unsafe { LoadKeyboardLayoutW(wide(layout).as_ptr(), KLF_ACTIVATE) };
    assert!(!loaded.is_null(), "{what}: layout {layout} loads");
    let events = typed_events(what, &inputs());
    unsafe { ActivateKeyboardLayout(before, 0) };
    assert_eq!(events, [text(want.as_bytes())], "{what}");
}

/// AltGr on a German layout types its character, never Meta, and a US-International dead
/// key joins the next letter; each once.
fn layouts_type_their_characters() {
    on_layout(GERMAN, "AltGr+Q", || chord(&[VK_RMENU], VK_Q), "@");
    let dead = || [chord(&[], VK_OEM_7), chord(&[], VK_E)].concat();
    on_layout(US_INTERNATIONAL, "' then e", dead, "é");
    println!("h2 row: AltGr+Q typed @, ' then e typed é");
}

/// Text that comes without a layout, as KEYEVENTF_UNICODE packets (an emoji is a surrogate
/// pair) and as an IME's WM_IME_CHAR, types once.
fn unicode_and_ime_text_types_once(view: &View) {
    let unit = |unit, up: bool| {
        let release = if up { KEYEVENTF_KEYUP } else { 0 };
        keyboard(0, unit, KEYEVENTF_UNICODE | release)
    };
    let e_acute = [unit(0xE9, false), unit(0xE9, true)];
    assert_eq!(typed_events("a packet é", &e_acute), [text("é".as_bytes())]);
    let emoji = [
        unit(0xD83D, false),
        unit(0xD83D, true),
        unit(0xDE0A, false),
        unit(0xDE0A, true),
    ];
    assert_eq!(
        typed_events("a packet pair", &emoji),
        [text("😊".as_bytes())]
    );
    take_seen();
    unsafe { SendMessageW(view.hwnd(), WM_IME_CHAR, 0x304B, 0) };
    pump_for(KEY_TAIL);
    let seen: Vec<ViewEvent> = take_seen().into_iter().map(|(_, event)| event).collect();
    assert_eq!(seen, [text("か".as_bytes())], "an IME commit types once");
    println!("h2 row: packets é and an emoji, IME か, once each");
}

/// At a composition's start the view puts the IME's composition window at its cursor
/// cell, from the cell size the render thread posted.
fn ime_window_at_the_cursor_cell(view: &View) {
    let before = presents();
    assert!(view.feed(fed_frame(&Theme::default(), b"ime> ")));
    await_present("the ime frame", before);
    unsafe { SendMessageW(view.hwnd(), WM_IME_STARTCOMPOSITION, 0, 0) };
    let context = unsafe { ImmGetContext(view.hwnd()) };
    assert!(!context.is_null(), "the view has an input context");
    let mut form = COMPOSITIONFORM::default();
    let read = unsafe { ImmGetCompositionWindow(context, &mut form) };
    unsafe { ImmReleaseContext(view.hwnd(), context) };
    unsafe { SendMessageW(view.hwnd(), WM_IME_ENDCOMPOSITION, 0, 0) };
    assert_ne!(read, 0, "the composition window reads back");
    let cell = draw::BOARD
        .cell(3)
        .expect("the render thread posted the cell");
    let want = place::ime_spot((0, 5), cell, 100).expect("a spot");
    let seen = (form.dwStyle, form.ptCurrentPos.x, form.ptCurrentPos.y);
    assert_eq!(
        seen,
        (CFS_POINT, want.point.0, want.point.1),
        "at the cursor cell"
    );
    assert!(want.point.0 > 0, "after the prompt, not at the origin");
    println!("h2 row: IME point {:?}, cell {cell:?}", want.point);
}

/// A covered view types nothing, and a view without focus drops a character or key-down
/// sent to it straight.
fn inactive_views_type_nothing(view: &View, webview: HWND, centre: POINT) {
    let typing = || {
        take_seen()
            .into_iter()
            .filter(|(_, event)| !matches!(event, ViewEvent::Focus(_)))
            .count()
    };
    view.place(None).expect("the host hides a covered view");
    take_seen();
    send(&chord(&[], VK_A));
    pump_for(KEY_TAIL);
    assert_eq!(typing(), 0, "a covered view types nothing");
    view.place(Some(PAGE))
        .expect("the host places an uncovered view again");
    clicked_into(view, centre);
    unsafe { SetFocus(webview) };
    pump_for(KEY_TAIL);
    take_seen();
    unsafe { PostMessageW(view.hwnd(), WM_CHAR, usize::from(b'a'), 0) };
    unsafe { PostMessageW(view.hwnd(), WM_KEYDOWN, usize::from(VK_UP), 0) };
    pump_for(KEY_TAIL);
    assert_eq!(typing(), 0, "a view without focus types nothing");
    println!("h2 row: covered and unfocused views typed nothing");
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn each_key_reaches_the_sink_once_as_the_panel_sends_it_and_nothing_while_inactive() {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, 3, sink, &style()).expect("a view");
    let centre = placed_shown_and_drawn(parent, &view);
    clicked_into(&view, centre);
    keys_send_once();
    layouts_type_their_characters();
    unicode_and_ime_text_types_once(&view);
    ime_window_at_the_cursor_cell(&view);
    inactive_views_type_nothing(&view, webview, centre);
    drop(view);
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// The view ids of the rv-sec3 N1 rows: a faulted focused view, then a focused view ended.
const FAULTED_FOCUSED: u64 = 6;
const ENDED_FOCUSED: u64 = 7;

/// The stand-in page's key-downs, counted once a row hands it `page_proc`.
static PAGE_KEYS: AtomicUsize = AtomicUsize::new(0);

/// The stand-in webview's window procedure in the N1 rows: it counts key-downs, so a row
/// sees a key land in the page. Output: the default procedure's answer.
unsafe extern "system" fn page_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_KEYDOWN {
        PAGE_KEYS.fetch_add(1, Ordering::SeqCst);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Clicks into a drawn view and waits until it holds focus and the click's release has
/// reached the sink, so no late release lands in a later step's events. Inputs: the view
/// and its centre on the screen.
fn clicked_into(view: &View, centre: POINT) {
    let releases = || {
        let seen = SEEN.lock().expect("the seen events");
        let release = |event: &ViewEvent| {
            matches!(
                event,
                ViewEvent::Mouse {
                    kind: MouseKind::Release,
                    ..
                }
            )
        };
        seen.iter().filter(|(_, event)| release(event)).count()
    };
    let before = releases();
    click(centre);
    pump_until("the click's focus and release", FARM_WAIT, || {
        let focused = unsafe { GetFocus() } == view.hwnd();
        focused && releases() > before
    });
}

/// Checks that events are one left click at a point: a single press, then its release.
/// Inputs: the events and the point.
fn one_click(events: &[ViewEvent], at: (f64, f64)) {
    assert_eq!(events.len(), 2, "one press and its release: {events:?}");
    assert_eq!(mouse_event(&events[0], MouseKind::Press, at), (false, 1));
    assert!(!mouse_event(&events[1], MouseKind::Release, at).0);
}

/// Faults a focused view by a refused resize and waits for the board's fault while this
/// thread runs only sent messages, so any posted word of the fault stays queued. Inputs:
/// the view and its id.
fn faulted_unheard(view: &View, id: u64) {
    draw::trial_refuse_resize(id);
    view.place(Some((50.0, 40.0, 420.0, 300.0)))
        .expect("a wider box");
    let started = Instant::now();
    while !view.faulted() {
        assert!(
            started.elapsed() < FARM_WAIT,
            "the refused resize faults the view within {FARM_WAIT:?}"
        );
        let mut message = MSG::default();
        let flags = PM_NOREMOVE | PM_QS_SENDMESSAGE;
        unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, flags) };
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        unsafe { GetFocus() },
        view.hwnd(),
        "the faulted view still holds focus"
    );
}

/// How many faults the sink heard for a view, taking all it heard. Input: the view id.
/// Output: the count.
fn faults_heard(id: u64) -> usize {
    take_seen()
        .into_iter()
        .filter(|(seen, event)| *seen == id && *event == ViewEvent::Faulted)
        .count()
}

/// What the sink heard besides focus changes, taken. Output: the count.
fn typed() -> usize {
    take_seen()
        .into_iter()
        .filter(|(_, event)| !matches!(event, ViewEvent::Focus(_)))
        .count()
}

/// The page takes the next real key and the pane hears nothing. Input: the webview, which
/// holds focus.
fn page_takes_the_next_key(webview: HWND) {
    let proc = page_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    unsafe { SetWindowLongPtrW(webview, GWLP_WNDPROC, proc as usize as isize) };
    PAGE_KEYS.store(0, Ordering::SeqCst);
    take_seen();
    send(&chord(&[], VK_A));
    pump_until("a key in the page", FARM_WAIT, || {
        PAGE_KEYS.load(Ordering::SeqCst) > 0
    });
    pump_for(KEY_TAIL);
    assert_eq!(typed(), 0, "the key went to the page, not the pane");
}

/// rv-sec3 N1: a focused view the render thread faults types nothing at once, before its
/// window hears, then hides and hands focus to the page, where the next key lands; placing
/// it again keeps it hidden, and its end leaves focus in the page.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_faulted_focused_view_types_nothing_hides_and_the_page_takes_the_next_key() {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, FAULTED_FOCUSED, sink, &style()).expect("a view");
    let centre = placed_shown_and_drawn(parent, &view);
    clicked_into(&view, centre);
    faulted_unheard(&view, FAULTED_FOCUSED);
    take_seen();
    unsafe { SendMessageW(view.hwnd(), WM_KEYDOWN, usize::from(VK_UP), 0) };
    unsafe { SendMessageW(view.hwnd(), WM_CHAR, usize::from(b'a'), 0) };
    assert_eq!(
        typed(),
        0,
        "a faulted view types nothing before its window hears"
    );
    pump_until("the faulted view hides", FARM_WAIT, || unsafe {
        IsWindowVisible(view.hwnd()) == 0
    });
    assert_eq!(
        unsafe { GetFocus() },
        webview,
        "the page holds focus after the fault"
    );
    assert_eq!(
        faults_heard(FAULTED_FOCUSED),
        1,
        "the window tells the host of the fault once"
    );
    view.focus();
    assert_eq!(
        unsafe { GetFocus() },
        webview,
        "a page's focus call gives a faulted view no keys"
    );
    page_takes_the_next_key(webview);
    view.place(Some(PAGE)).expect("the box again");
    assert_eq!(
        unsafe { IsWindowVisible(view.hwnd()) },
        0,
        "a faulted view placed again stays hidden"
    );
    dropped_within_the_bound(view, "the faulted view");
    assert_eq!(
        unsafe { GetFocus() },
        webview,
        "the page keeps focus after the end"
    );
    println!("n1 row: a faulted focused view typed nothing, hid, and the page took the key");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// rv-sec3 N1: ending a focused live view hands focus to the page first, so the next key
/// goes to the page, not to a window that is gone or to the parent.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn ending_a_focused_view_hands_focus_to_the_page() {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, ENDED_FOCUSED, sink, &style()).expect("a view");
    let centre = placed_shown_and_drawn(parent, &view);
    clicked_into(&view, centre);
    dropped_within_the_bound(view, "the focused view");
    assert_eq!(
        unsafe { GetFocus() },
        webview,
        "the page holds focus after the end"
    );
    page_takes_the_next_key(webview);
    println!("n1 row: an ended focused view left focus in the page, which took the key");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// The id of the composition row's view, its cell size read from the board by it.
const IME_VIEW: u64 = 5;

/// One trial answer true to its text: the kind, its byte count and its UTF-16 units.
fn told(kind: u32, text: &str) -> (u32, i32, Vec<u16>) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let bytes = i32::try_from(units.len() * 2).expect("a short trial string");
    assert_eq!(usize::try_from(bytes).ok(), Some(units.len() * 2));
    (kind, bytes, units)
}

/// What the view sent since the last take, focus changes left out.
fn sent() -> Vec<ViewEvent> {
    take_seen()
        .into_iter()
        .map(|(_, event)| event)
        .filter(|event| !matches!(event, ViewEvent::Focus(_)))
        .collect()
}

/// Sends one WM_IME_COMPOSITION with these flags, the view reading the trial's answers in
/// place of an IME's. Inputs: the view, the flags and the answers. Output: what it sent.
fn composed(view: &View, flags: u32, answers: Vec<(u32, i32, Vec<u16>)>) -> Vec<ViewEvent> {
    let flags = isize::try_from(flags).expect("composition flags fit an LPARAM");
    view::trial_composition(Some(answers));
    take_seen();
    let handled = unsafe { SendMessageW(view.hwnd(), WM_IME_COMPOSITION, 0, flags) };
    view::trial_composition(None);
    assert_eq!(handled, 0, "the view handles a composition itself");
    sent()
}

/// A screen of `text` with `marked` drawn at its cursor, as the host builds it from
/// `marked_text`. Output: the frame.
fn marked_frame(text: &[u8], marked: &str) -> Frame {
    let mut pane = Pane::new(true);
    assert!(pane.on_attached(40, 12));
    assert!(pane.on_output(text));
    let term = pane.term().expect("an attached pane");
    screen::frame(term, &Theme::default(), true, Some(marked))
}

/// An IMECHARPOSITION with marks in every field the view writes, so a refused query can be
/// seen to write nothing. Inputs: its dwSize and dwCharPos. Output: the struct.
fn char_query(size: u32, offset: u32) -> IMECHARPOSITION {
    let mark = RECT {
        left: -7,
        top: -7,
        right: -7,
        bottom: -7,
    };
    IMECHARPOSITION {
        dwSize: size,
        dwCharPos: offset,
        pt: POINT { x: -7, y: -7 },
        cLineHeight: 7,
        rcDocument: mark,
    }
}

/// Sends IMR_QUERYCHARPOSITION with this struct. Inputs: the view and the struct. Output:
/// the view's answer, the struct as left.
fn asked_where(view: &View, mut asked: IMECHARPOSITION) -> (isize, IMECHARPOSITION) {
    let at = ptr::from_mut(&mut asked) as isize;
    let wparam = IMR_QUERYCHARPOSITION as usize;
    let answer = unsafe { SendMessageW(view.hwnd(), WM_IME_REQUEST, wparam, at) };
    (answer, asked)
}

/// The fields a query answer writes: the point, the line height and the document's sides.
fn written(asked: &IMECHARPOSITION) -> ((i32, i32), u32, (i32, i32, i32, i32)) {
    let side = asked.rcDocument;
    let sides = (side.left, side.top, side.right, side.bottom);
    ((asked.pt.x, asked.pt.y), asked.cLineHeight, sides)
}

/// The spot of a cell of the composition row's view, at its posted cell size.
fn cell_spot(cell: (usize, usize)) -> place::Spot {
    let size = draw::BOARD
        .cell(IME_VIEW)
        .expect("the render thread posted the cell");
    place::ime_spot(cell, size, 100).expect("a spot")
}

/// Counts the pixels down the middle of each of these cells of the view's screen that
/// differ from the background. Inputs: the view, the row and the columns. Output: the count.
fn drawn_down_cells(view: &View, row: usize, columns: std::ops::Range<usize>) -> usize {
    let mut window = RECT::default();
    assert_ne!(unsafe { GetWindowRect(view.hwnd(), &mut window) }, 0);
    let mut drawn = 0;
    for column in columns {
        let (left, top, right, bottom) = cell_spot((row, column)).cell;
        assert!(bottom > top);
        let x = window.left + (left + right) / 2;
        for y in top..bottom {
            let at = POINT {
                x,
                y: window.top + y,
            };
            drawn += usize::from(!near(pixel(at), background()));
        }
    }
    drawn
}

/// Marked text is the view's own: a composition string marks it with one redraw, its cells
/// are drawn, the drawn marked text gives each character's screen cell to
/// IMR_QUERYCHARPOSITION and puts the candidate list clear of the caret's cell. Input: the
/// focused view.
fn marks_and_answers_where_its_characters_are(view: &View) {
    let caret = (GCS_CURSORPOS, 1, Vec::new());
    let answers = vec![told(GCS_COMPSTR, "かな"), caret];
    let seen = composed(view, GCS_COMPSTR | GCS_CURSORPOS, answers);
    assert_eq!(seen, [ViewEvent::Marked], "marking asks for one redraw");
    assert_eq!(view.marked_text().as_deref(), Some("かな"));
    let before = presents();
    let frame = marked_frame(b"ime> ", "かな");
    let drawn = frame.preedit.expect("the marked text drawn");
    assert_eq!(
        (drawn.column, drawn.cells),
        (5, 4),
        "after the prompt, two wide cells"
    );
    assert!(view.feed(frame));
    await_present("the marked frame", before);
    let span = drawn.column..drawn.column + drawn.cells;
    let marked = drawn_down_cells(view, drawn.row, span.clone());
    assert!(marked > 0, "the marked cells are drawn");
    let blank = drawn_down_cells(view, drawn.row + 2, span);
    assert_eq!(blank, 0, "the cells two rows down are background");
    let (answer, asked) = asked_where(view, char_query(36, 1));
    assert_eq!(answer, 1, "the second character's cell is answered");
    let want = cell_spot((drawn.row, drawn.column + 2));
    let mut window = RECT::default();
    assert_ne!(unsafe { GetWindowRect(view.hwnd(), &mut window) }, 0);
    let point = POINT {
        x: window.left + want.point.0,
        y: window.top + want.point.1,
    };
    let height = u32::try_from(want.cell.3 - want.cell.1).expect("a cell height");
    let sides = (window.left, window.top, window.right, window.bottom);
    assert_eq!(written(&asked), ((point.x, point.y), height, sides));
    let moved = composed(view, GCS_CURSORPOS, vec![(GCS_CURSORPOS, 2, Vec::new())]);
    assert_eq!(
        moved,
        [ViewEvent::Marked],
        "a caret alone moves with one redraw"
    );
    let context = unsafe { ImmGetContext(view.hwnd()) };
    assert!(!context.is_null(), "the view has an input context");
    let mut form = CANDIDATEFORM::default();
    let read = unsafe { ImmGetCandidateWindow(context, 0, &mut form) };
    unsafe { ImmReleaseContext(view.hwnd(), context) };
    assert_ne!(read, 0, "the candidate form reads back");
    let at = cell_spot((drawn.row, drawn.column + 4)).cell;
    let area = (
        form.rcArea.left,
        form.rcArea.top,
        form.rcArea.right,
        form.rcArea.bottom,
    );
    assert_eq!(
        (form.dwStyle, area),
        (CFS_EXCLUDE, at),
        "clear of the caret's cell"
    );
    let at = written(&asked);
    println!("h2b row: marked かな, {marked} pixels drawn, char 1 at {at:?}, candidate {area:?}");
}

/// Answers the view cannot trust change nothing: a null, short or past-the-end char query
/// answers 0 and writes nothing, and a composition string or result whose count is an
/// error, odd, or a lie past its buffer is refused. Input: the view, marking "かな".
fn refuses_what_it_cannot_trust(view: &View) {
    let wparam = IMR_QUERYCHARPOSITION as usize;
    let null = unsafe { SendMessageW(view.hwnd(), WM_IME_REQUEST, wparam, 0) };
    assert_eq!(null, 0, "a null query is refused");
    for (size, offset) in [(35, 1), (36, 3), (36, u32::MAX)] {
        let (answer, asked) = asked_where(view, char_query(size, offset));
        assert_eq!(answer, 0, "query size {size} at {offset} is refused");
        assert_eq!(
            written(&asked),
            written(&char_query(size, offset)),
            "and writes nothing"
        );
    }
    let held: Vec<u16> = "ご".encode_utf16().collect();
    for bytes in [-1, 3, 1 << 30] {
        let seen = composed(view, GCS_COMPSTR, vec![(GCS_COMPSTR, bytes, held.clone())]);
        assert_eq!(seen, [], "a string count of {bytes} is refused");
        assert_eq!(
            view.marked_text().as_deref(),
            Some("かな"),
            "the marked text stays"
        );
    }
    let lie = (GCS_RESULTSTR, i32::MAX - 1, held);
    assert_eq!(
        composed(view, GCS_RESULTSTR, vec![lie]),
        [],
        "a lying result commits nothing"
    );
    println!("h2b row: bad queries and counts refused");
}

/// A result commits once, as typed text through the same caps, before the composition
/// ends; controls and ESC in it are text, and no WM_IME_CHAR follows. Input: the view,
/// marking "かな".
fn commits_once_as_typed_text(view: &View) {
    let answers = vec![told(GCS_RESULTSTR, "日本"), told(GCS_COMPSTR, "")];
    let seen = composed(view, GCS_RESULTSTR | GCS_COMPSTR, answers);
    assert_eq!(seen, [text("日本".as_bytes()), ViewEvent::Marked]);
    assert_eq!(view.marked_text(), None, "the composition ended");
    let controls = vec![told(GCS_RESULTSTR, "a\u{1b}[2J\u{3}")];
    let seen = composed(view, GCS_RESULTSTR, controls);
    assert_eq!(
        seen,
        [text(b"a\x1b[2J\x03")],
        "controls and ESC as typed text"
    );
    pump_for(KEY_TAIL);
    assert_eq!(sent(), [], "no WM_IME_CHAR follows a commit");
    println!("h2b row: 日本 and a control commit once each");
}

/// The end of a composition, a click, a focus loss and a hide each drop the marked text
/// with nothing sent. Inputs: the view, the webview and the view's centre on screen.
fn compositions_end_and_cancel_with_nothing_sent(view: &View, webview: HWND, centre: POINT) {
    let mark = || {
        let seen = composed(view, GCS_COMPSTR, vec![told(GCS_COMPSTR, "か")]);
        assert_eq!(seen, [ViewEvent::Marked]);
    };
    mark();
    unsafe { SendMessageW(view.hwnd(), WM_IME_ENDCOMPOSITION, 0, 0) };
    assert_eq!(
        sent(),
        [ViewEvent::Marked],
        "the end clears the marked text"
    );
    assert_eq!(view.marked_text(), None);
    past_a_double_click();
    mark();
    clicked_into(view, centre);
    pump_for(KEY_TAIL);
    let clicked = sent();
    let (cancel, pressed) = clicked.split_first().expect("the click sent the cancel");
    assert_eq!(
        *cancel,
        ViewEvent::Marked,
        "a click cancels the marked text"
    );
    one_click(pressed, view_point(view, centre));
    assert_eq!(view.marked_text(), None);
    mark();
    unsafe { SetFocus(webview) };
    pump_for(KEY_TAIL);
    assert_eq!(
        sent(),
        [ViewEvent::Marked],
        "a focus loss cancels the marked text"
    );
    assert_eq!(view.marked_text(), None);
    clicked_into(view, centre);
    take_seen();
    mark();
    view.place(None).expect("hidden");
    pump_for(KEY_TAIL);
    assert_eq!(
        sent(),
        [ViewEvent::Marked],
        "a hide cancels the marked text"
    );
    assert_eq!(view.marked_text(), None);
    view.place(Some(PAGE)).expect("shown again");
    clicked_into(view, centre);
    println!("h2b row: end, click, focus loss and hide each cancelled once");
}

/// A covered view and a view without focus commit nothing and keep no marked text (the
/// keys-off gate covers IME commits). Inputs: the view, the webview and the view's centre.
fn inactive_views_compose_nothing(view: &View, webview: HWND, centre: POINT) {
    let both = || vec![told(GCS_RESULTSTR, "x"), told(GCS_COMPSTR, "かな")];
    let seen = composed(view, GCS_COMPSTR, vec![told(GCS_COMPSTR, "か")]);
    assert_eq!(seen, [ViewEvent::Marked]);
    view.place(None).expect("the host hides a covered view");
    let seen = composed(view, GCS_RESULTSTR | GCS_COMPSTR, both());
    let typed = |seen: &[ViewEvent]| seen.iter().any(|event| matches!(event, ViewEvent::Key(_)));
    assert!(!typed(&seen), "a covered view commits nothing: {seen:?}");
    assert_eq!(
        view.marked_text(),
        None,
        "a covered view keeps no marked text"
    );
    view.place(Some(PAGE))
        .expect("the host places an uncovered view again");
    unsafe { SetFocus(webview) };
    pump_for(KEY_TAIL);
    take_seen();
    let seen = composed(view, GCS_RESULTSTR | GCS_COMPSTR, both());
    assert!(
        !typed(&seen),
        "a view without focus commits nothing: {seen:?}"
    );
    assert_eq!(
        view.marked_text(),
        None,
        "a view without focus keeps no marked text"
    );
    clicked_into(view, centre);
    println!("h2b row: covered and unfocused views composed nothing");
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn the_ime_marks_inline_commits_once_refuses_bad_answers_and_nothing_while_inactive() {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, IME_VIEW, sink, &style()).expect("a view");
    let centre = placed_shown_and_drawn(parent, &view);
    clicked_into(&view, centre);
    marks_and_answers_where_its_characters_are(&view);
    refuses_what_it_cannot_trust(&view);
    commits_once_as_typed_text(&view);
    compositions_end_and_cancel_with_nothing_sent(&view, webview, centre);
    inactive_views_compose_nothing(&view, webview, centre);
    drop(view);
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// The PowerShell reader: Windows PowerShell's managed UI Automation client. `text` reads
/// the element and its text; the `probe_` modes read one kind of range each.
const READER: &str = include_str!("reader.ps1");

/// How long a reader may run: Windows PowerShell's start and its assembly loads take a few
/// seconds on a hosted runner, and a hung reader must not hold the run.
const READER_WAIT: Duration = Duration::from_secs(60);

/// Windows PowerShell, by its absolute path, as an agent's wrapped command needs it.
fn powershell() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    std::path::PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// The reader command for a view. Inputs: the script, the view's window and the mode.
fn reader_command(script: &std::path::Path, hwnd: HWND, mode: &str) -> std::process::Command {
    let mut command = std::process::Command::new(powershell());
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg((hwnd as isize).to_string())
        .arg(mode);
    command
}

/// Runs a reader to its end while this thread pumps the view's messages, handing each line
/// to `on_line` as it arrives. Inputs: what reads, the command and the line callback.
/// Output: every line the reader printed, and its exit code.
fn read_view(
    what: &str,
    mut command: std::process::Command,
    mut on_line: impl FnMut(&str),
) -> (Vec<String>, Option<i32>) {
    use std::io::BufRead;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = crazyagent::spawn_gate::spawn(&mut command)
        .unwrap_or_else(|error| panic!("{what} starts: {error}"));
    let stdout = child.stdout.take().expect("piped");
    let (send, lines) = std::sync::mpsc::channel();
    let reading = thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let started = Instant::now();
    loop {
        while let Ok(line) = lines.try_recv() {
            on_line(&line);
            seen.push(line);
        }
        if reading.is_finished() && child.try_wait().ok().flatten().is_some() {
            break;
        }
        if started.elapsed() > READER_WAIT {
            let _ = child.kill();
            panic!("{what} ended within {READER_WAIT:?}: {seen:?}");
        }
        pump_for(Duration::from_millis(20));
    }
    seen.extend(lines.try_iter());
    let output = child.wait_with_output().expect("the reader's end");
    let errors = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code();
    println!("{what}: {seen:?} rc {code:?} stderr {}", errors.trim());
    (seen, code)
}

/// The value of the reader line with this key. Inputs: the lines and the key. Output: the
/// value, or none.
fn value<'a>(lines: &'a [String], key: &str) -> Option<&'a str> {
    let key = format!("{key}=");
    lines
        .iter()
        .find_map(|line| line.strip_prefix(key.as_str()))
}

/// Feeds a view one screen, drawn and posted to UI Automation as the host does after each
/// frame. Inputs: the view and the bytes the terminal took.
fn fed_and_posted(view: &View, text: &[u8]) {
    let mut pane = Pane::new(true);
    assert!(pane.on_attached(40, 12));
    assert!(pane.on_output(text));
    let term = pane.term().expect("an attached pane");
    assert!(view.feed(screen::frame(term, &Theme::default(), true, None)));
    view.access(a11y::visible(term), a11y::text_key(term));
}

/// A plain canonical folder: the verbatim prefix Windows' canonical form adds dropped, as
/// the boundary takes paths. Input: the folder. Output: its plain form.
fn plain_canonical(path: &std::path::Path) -> std::path::PathBuf {
    let full = std::fs::canonicalize(path).expect("a canonical folder");
    let text = full.to_str().expect("a UTF-8 folder");
    std::path::PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
}

/// A UI Automation client a row starts: Windows PowerShell's managed client, or this test
/// binary's compiled IUIAutomation reader, the COM client API screen readers use.
#[derive(Clone, Copy, Debug)]
enum Client {
    PowerShell,
    Compiled,
}

/// Runs a client's text read inside an agent's AppContainer, through the product's own
/// Windows agent launch: a project sandbox with an open network, the widest capabilities
/// an agent gets, installed with the boundary helper beside this test binary, then its
/// wrapped command, the reader copied into the project. Inputs: the view's window and the
/// client. Output: the reader's lines.
fn read_from_an_agent_container(hwnd: HWND, client: Client) -> Vec<String> {
    use crazyagent::sandbox::{
        InstalledSandbox, LinuxStrict, LinuxSupervisor, ProjectSandbox, SandboxNetwork,
    };
    let temp = tempfile::tempdir().expect("a temporary folder");
    let root = plain_canonical(temp.path());
    let (project, home, tmp, profiles) = (
        root.join("project"),
        root.join("agent").join("home"),
        root.join("agent").join("tmp"),
        root.join("profiles"),
    );
    for folder in [&project, &home, &tmp, &profiles] {
        std::fs::create_dir_all(folder).expect("a sandbox folder");
    }
    let mut harness = match client {
        Client::PowerShell => {
            let script = project.join("reader.ps1");
            std::fs::write(&script, READER).expect("the reader script");
            reader_command(&script, hwnd, "text")
        }
        Client::Compiled => {
            let binary = project.join("compiled-reader.exe");
            let this = std::env::current_exe().expect("this test binary");
            std::fs::copy(this, &binary).expect("this test binary copied into the project");
            compiled_command(&binary, hwnd, "text")
        }
    };
    let sandbox =
        ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Open).expect("a policy");
    let profile = profiles.join("agent.sb");
    let installed =
        InstalledSandbox::install(sandbox, &profile, LinuxSupervisor::On, LinuxStrict::Off);
    let installed = installed.expect("the boundary helper beside the test binary");
    harness.current_dir(&project);
    let command = installed.wrap(&harness).expect("an agent command");
    let what = format!("h3 row: agent container reader {client:?}");
    let (lines, code) = read_view(&what, command, |_| {});
    assert_eq!(
        code,
        Some(0),
        "the {client:?} container reader ran to its end: {lines:?}"
    );
    lines
}

/// The wall: a screen reader outside any container reads the view's text, and each client,
/// started in an agent's AppContainer, must reach the view's own window in this process and
/// read nothing of it. Inputs: the view, showing "ab" and "cd", and the reader script.
fn the_wall(view: &View, script: &std::path::Path) {
    let (shown, code) = read_view(
        "h3 row: screen reader text",
        reader_command(script, view.hwnd(), "text"),
        |_| {},
    );
    assert_eq!(code, Some(0), "the screen reader ran to its end: {shown:?}");
    found_the_view(&shown, view.hwnd());
    assert_eq!(value(&shown, "reader_control"), Some(DOCUMENT));
    assert_eq!(value(&shown, "reader_name"), Some("Terminal"));
    assert_eq!(value(&shown, "reader_text_pattern"), Some("True"));
    assert!(value(&shown, "reader_text").is_some_and(|text| text.starts_with("ab|cd|")));
    for client in [Client::PowerShell, Client::Compiled] {
        let contained = read_from_an_agent_container(view.hwnd(), client);
        assert!(
            value(&contained, "reader_stage") == Some("lookup"),
            "the {client:?} container reader never reached the UIA lookup: {contained:?}"
        );
        found_the_view(&contained, view.hwnd());
        contained_read_nothing(&contained);
        println!("h3 row: agent container read nothing {client:?}: {contained:?}");
    }
}

/// A view placed, drawn and showing "ab" and "cd", with the reader script written beside
/// it. Inputs: the parent and the webview stand-in. Output: the view and the folder holding
/// the script.
fn shown_for_readers(parent: HWND, webview: HWND) -> (View, tempfile::TempDir) {
    let view = View::new(parent, webview, 1, sink, &style()).expect("a view");
    placed_shown_and_drawn(parent, &view);
    fed_and_posted(&view, b"ab\r\ncd");
    let temp = tempfile::tempdir().expect("a temporary folder");
    std::fs::write(temp.path().join("reader.ps1"), READER).expect("the reader script");
    assert!(!view.hwnd().is_null());
    (view, temp)
}

/// A screen reader, the compiled IUIAutomation client, reads the view's second line, the
/// empty selection at the cursor, the line's bounds inside the window, a refused Select and
/// the text after a new screen, and counts text-changed events. Input: the view, showing
/// "ab" and "cd".
fn read_in_full(view: &View) {
    let mut posted = false;
    let this = std::env::current_exe().expect("this test binary");
    let (lines, _) = read_view(
        "h3 row: screen reader",
        compiled_command(&this, view.hwnd(), "full"),
        |line| {
            if line == "reader_listening" && !posted {
                posted = true;
                fed_and_posted(view, b"ab\r\ncd\r\nef");
            }
        },
    );
    found_the_view(&lines, view.hwnd());
    assert_eq!(value(&lines, "reader_control"), Some(DOCUMENT));
    assert_eq!(value(&lines, "reader_name"), Some("Terminal"));
    assert_eq!(value(&lines, "reader_text_pattern"), Some("True"));
    assert!(value(&lines, "reader_text").is_some_and(|text| text.starts_with("ab|cd|")));
    assert_eq!(value(&lines, "reader_line_moved"), Some("1"));
    assert_eq!(value(&lines, "reader_line"), Some("cd|"));
    assert_eq!(value(&lines, "reader_selection"), Some("1:0"));
    assert!(value(&lines, "reader_select").is_some_and(|said| said.starts_with("refused")));
    let mut window = RECT::default();
    assert_ne!(unsafe { GetWindowRect(view.hwnd(), &mut window) }, 0);
    let rects = value(&lines, "reader_rects").expect("the line's bounds");
    let sides: Vec<i32> = rects
        .split([',', ';'])
        .filter_map(|side| side.parse().ok())
        .collect();
    assert_eq!(
        sides.len(),
        4,
        "one rectangle for the line's two cells: {rects}"
    );
    assert!(
        sides[0] >= window.left && sides[1] >= window.top,
        "{rects} inside the window at {}, {}",
        window.left,
        window.top
    );
    assert!(sides[0] + sides[2] <= window.right && sides[1] + sides[3] <= window.bottom);
    assert!(value(&lines, "reader_text_after").is_some_and(|text| text.starts_with("ab|cd|ef|")));
    let events = value(&lines, "reader_text_changed").unwrap_or("none");
    println!("h3 row: text changed events {events}");
}

/// The UIA control type id of a Document, the type the view's provider answers; the system's
/// own proxy for a bare window answers Pane.
const DOCUMENT: &str = "50030";

/// Checks a reader's element is the view itself: its window handle, as UI Automation gives
/// it (32 bits), and this process. Inputs: the reader's lines and the view's window.
fn found_the_view(lines: &[String], hwnd: HWND) {
    let handle = (hwnd as isize as i32).to_string();
    assert_eq!(
        value(lines, "reader_hwnd"),
        Some(handle.as_str()),
        "the reader found the view's window: {lines:?}"
    );
    let process = std::process::id().to_string();
    assert_eq!(
        value(lines, "reader_process"),
        Some(process.as_str()),
        "the reader found this process's window: {lines:?}"
    );
}

/// Checks a reader in an agent's AppContainer reached none of the view's provider: not its
/// Document type, its name, its Text pattern or its text, only the system's window proxy,
/// whose answer of no Text pattern it must print. Input: the lines of a reader that ran to
/// its end on the view's own window.
fn contained_read_nothing(contained: &[String]) {
    let read = "an agent's AppContainer read the view";
    assert!(
        value(contained, "reader_control").is_none_or(|control| control != DOCUMENT),
        "{read}: its Document type, {contained:?}"
    );
    assert!(
        value(contained, "reader_name").is_none_or(|name| name != "Terminal"),
        "{read}: its name, {contained:?}"
    );
    assert_eq!(
        value(contained, "reader_text_pattern"),
        Some("False"),
        "{read}: its Text pattern, or the reader never asked: {contained:?}"
    );
    assert!(
        value(contained, "reader_text").is_none(),
        "{read}: its text, {contained:?}"
    );
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn an_agent_container_reaches_the_view_and_reads_nothing_of_it() {
    let (parent, webview) = stand_ins();
    let (view, temp) = shown_for_readers(parent, webview);
    the_wall(&view, &temp.path().join("reader.ps1"));
    drop(view);
    assert_eq!(
        draw::counts().1,
        0,
        "no surface setup, swap-chain wait or draw failed"
    );
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_screen_reader_reads_lines_the_selection_bounds_and_events() {
    let (parent, webview) = stand_ins();
    let (view, _temp) = shown_for_readers(parent, webview);
    read_in_full(&view);
    drop(view);
    assert_eq!(
        draw::counts().1,
        0,
        "no surface setup, swap-chain wait or draw failed"
    );
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn each_kind_of_range_is_read_by_its_own_reader_and_recorded() {
    let (parent, webview) = stand_ins();
    let (view, temp) = shown_for_readers(parent, webview);
    let script = temp.path().join("reader.ps1");
    for probe in ["probe_visible", "probe_empty", "probe_selection"] {
        let what = format!("h3 row: {probe}");
        let (lines, _) = read_view(&what, reader_command(&script, view.hwnd(), probe), |_| {});
        assert_eq!(
            value(&lines, "reader_stage"),
            Some("lookup"),
            "{what} reached its lookup: {lines:?}"
        );
    }
    let this = std::env::current_exe().expect("this test binary");
    let command = compiled_command(&this, view.hwnd(), "probe");
    let (lines, _) = read_view("h3 row: compiled reader", command, |_| {});
    assert_eq!(
        value(&lines, "reader_stage"),
        Some("lookup"),
        "the compiled reader reached its lookup: {lines:?}"
    );
    let control = rich_edit();
    let command = reader_command(&script, control, "probe_visible");
    let (lines, _) = read_view("h3 row: rich edit control probe_visible", command, |_| {});
    assert_eq!(
        value(&lines, "reader_stage"),
        Some("lookup"),
        "the rich edit control probe reached its lookup: {lines:?}"
    );
    assert_ne!(unsafe { DestroyWindow(control) }, 0);
    drop(view);
}

/// A system rich edit window showing "ab" and "cd": the control the windows-2022 client
/// probe reads the same way as the view, through a provider that is not ours. Output: its
/// window, which the caller destroys.
fn rich_edit() -> HWND {
    let library: Vec<u16> = "Msftedit.dll\0".encode_utf16().collect();
    let loaded = unsafe { LoadLibraryW(library.as_ptr()) };
    assert!(!loaded.is_null(), "the rich edit library");
    let class: Vec<u16> = "RICHEDIT50W\0".encode_utf16().collect();
    let text: Vec<u16> = "ab\r\ncd\0".encode_utf16().collect();
    let style = WS_POPUP | WS_VISIBLE | ES_MULTILINE.unsigned_abs();
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            style,
            40,
            40,
            300,
            120,
            ptr::null_mut(),
            ptr::null_mut(),
            GetModuleHandleW(ptr::null()),
            ptr::null(),
        )
    };
    assert!(!window.is_null(), "a rich edit window");
    pump_for(Duration::from_millis(200));
    window
}

/// The variable naming the window a compiled reader child reads, set only by a row that
/// starts this test binary as a UI Automation client.
const READ_WINDOW: &str = "H3_UIA_READ_WINDOW";

/// The variable naming what a compiled reader child reads: `text`, `probe` or `full`.
const READ_MODE: &str = "H3_UIA_READ_MODE";

/// The compiled reader command: this test binary, or a copy of it, running only its reader
/// row on a window. Inputs: the binary, the window and the mode. Output: the command.
fn compiled_command(binary: &std::path::Path, hwnd: HWND, mode: &str) -> std::process::Command {
    let path = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    let row = format!("{path}::the_compiled_reader_reads_the_window_its_parent_names");
    let mut command = std::process::Command::new(binary);
    command
        .args([
            row.as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(READ_WINDOW, (hwnd as isize).to_string())
        .env(READ_MODE, mode);
    command
}

/// The H3 rows' child: without its variables it does nothing; with them, it reads that
/// window through IUIAutomation, the COM client screen readers use. Its first line ends
/// the test harness's own line, so every reader line stands alone.
#[test]
#[ignore = "a child of the H3 rows: the compiled UI Automation reader"]
fn the_compiled_reader_reads_the_window_its_parent_names() {
    let Some(handle) = std::env::var_os(READ_WINDOW) else {
        return;
    };
    let handle = handle.to_str().and_then(|text| text.parse::<isize>().ok());
    let handle = handle.expect("a window handle in the variable");
    assert_ne!(handle, 0);
    let mode = std::env::var(READ_MODE).expect("a mode in the variable");
    println!();
    let Some(read) = compiled_text(handle) else {
        return;
    };
    match mode.as_str() {
        "text" => {}
        "probe" => compiled_probe(&read),
        "full" => compiled_full(&read),
        other => panic!("no compiled reader mode {other}"),
    }
}

/// What the compiled reader holds once the window's element has a Text pattern: the
/// client, the element, its pattern and its document range.
struct Read {
    automation: IUIAutomation,
    element: IUIAutomationElement,
    pattern: IUIAutomationTextPattern,
    document: IUIAutomationTextRange,
}

/// Starts the COM client and reads the window's element as the PowerShell text mode does:
/// its window, process, type, name, Text pattern and text. Input: the window. Output: what
/// a later read needs, or none when the element has no Text pattern.
fn compiled_text(handle: isize) -> Option<Read> {
    let started = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    started.ok().expect("COM on this thread");
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .expect("the UI Automation client");
    println!("reader_stage=lookup");
    let window = windows::Win32::Foundation::HWND(handle as *mut core::ffi::c_void);
    let element = unsafe { automation.ElementFromHandle(window) }.expect("the window's element");
    let found = unsafe { element.CurrentNativeWindowHandle() }.expect("its window");
    println!("reader_hwnd={}", found.0 as isize as i32);
    println!(
        "reader_process={}",
        unsafe { element.CurrentProcessId() }.expect("its pid")
    );
    println!(
        "reader_control={}",
        unsafe { element.CurrentControlType() }.expect("type").0
    );
    println!(
        "reader_name={}",
        unsafe { element.CurrentName() }.expect("its name")
    );
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) };
    println!(
        "reader_text_pattern={}",
        if pattern.is_ok() { "True" } else { "False" }
    );
    let pattern = pattern.ok()?;
    let document = unsafe { pattern.DocumentRange() }.expect("the document");
    let text = unsafe { document.GetText(-1) }
        .expect("its text")
        .to_string();
    println!("reader_text={}", text.replace('\n', "|"));
    Some(Read {
        automation,
        element,
        pattern,
        document,
    })
}

/// Reads the view through IUIAutomation as the PowerShell probes do, the same ranges in the
/// same order, printing a step before each call so a crash names its call. Input: what the
/// text read found.
fn compiled_probe(read: &Read) {
    println!("reader_step=visible_array");
    let visible = unsafe { read.pattern.GetVisibleRanges() }.expect("the visible ranges");
    let first = unsafe { visible.GetElement(0) }.expect("the first visible range");
    println!(
        "reader_step=visible_read {}",
        unsafe { first.GetText(-1) }.expect("text").len()
    );
    let empty = unsafe { read.document.Clone() }.expect("a clone");
    let start = TextPatternRangeEndpoint_Start;
    unsafe { empty.MoveEndpointByRange(TextPatternRangeEndpoint_End, &empty, start) }
        .expect("collapsed to its start");
    println!("reader_step=empty_collapsed");
    println!(
        "reader_step=empty_read {}",
        unsafe { empty.GetText(-1) }.expect("text").len()
    );
    let selection = unsafe { read.pattern.GetSelection() }.expect("the selection");
    println!(
        "reader_step=selection_array {}",
        unsafe { selection.Length() }.expect("count")
    );
    let selected = unsafe { selection.GetElement(0) }.expect("the selected range");
    println!("reader_step=selection_indexed");
    let _copy = unsafe { selected.Clone() }.expect("a clone of it");
    println!("reader_step=selection_cloned");
    let text = unsafe { selected.GetText(-1) }.expect("its text");
    println!("reader_step=selection_read {}", text.len());
}

/// Reads as the full row asks: the second line by a line move, the selection's count and
/// length, the line's bounds and a Select the view refuses, then the text-changed events.
/// Input: what the text read found.
fn compiled_full(read: &Read) {
    let line = unsafe { read.document.Clone() }.expect("a clone");
    unsafe { line.ExpandToEnclosingUnit(TextUnit_Line) }.expect("a line");
    let moved = unsafe { line.Move(TextUnit_Line, 1) }.expect("a line move");
    println!("reader_line_moved={moved}");
    let text = unsafe { line.GetText(-1) }.expect("its text").to_string();
    println!("reader_line={}", text.replace('\n', "|"));
    let selection = unsafe { read.pattern.GetSelection() }.expect("the selection");
    let count = unsafe { selection.Length() }.expect("its count");
    let selected = unsafe { selection.GetElement(0) }.expect("the selected range");
    let length = unsafe { selected.GetText(-1) }.expect("its text").len();
    println!("reader_selection={count}:{length}");
    println!("reader_rects={}", rects_of(&line));
    match unsafe { read.document.Select() } {
        Ok(()) => println!("reader_select=accepted"),
        Err(error) => println!("reader_select=refused {:#010X}", error.code().0),
    }
    compiled_events(read);
}

/// A range's bounds as the reader prints them: each rectangle's left, top, width and height
/// in whole pixels, rectangles joined by semicolons. Input: the range. Output: the text.
fn rects_of(range: &IUIAutomationTextRange) -> String {
    let array = unsafe { range.GetBoundingRectangles() }.expect("the bounds");
    assert!(!array.is_null());
    let lower = unsafe { SafeArrayGetLBound(array, 1) }.expect("its lower bound");
    let upper = unsafe { SafeArrayGetUBound(array, 1) }.expect("its upper bound");
    let mut sides = Vec::new();
    for index in lower..=upper {
        let mut side = 0f64;
        let at = (&raw mut side).cast();
        unsafe { SafeArrayGetElement(array, &index, at) }.expect("a side");
        sides.push(side.round() as i32);
    }
    unsafe { SafeArrayDestroy(array) }.expect("the bounds freed");
    assert_eq!(sides.len() % 4, 0, "whole rectangles: {sides:?}");
    let rects: Vec<String> = sides
        .chunks(4)
        .map(|side| format!("{},{},{},{}", side[0], side[1], side[2], side[3]))
        .collect();
    rects.join(";")
}

/// Counts the view's text-changed events: listens on the element, says so, waits up to 15
/// seconds for one while the parent posts a new screen, then reads the text after. Input:
/// what the text read found.
fn compiled_events(read: &Read) {
    let seen = Arc::new(AtomicUsize::new(0));
    let handler: IUIAutomationEventHandler = Seen(Arc::clone(&seen)).into();
    let changed = UIA_Text_TextChangedEventId;
    let none = None::<&IUIAutomationCacheRequest>;
    let scope = TreeScope_Element;
    unsafe {
        read.automation
            .AddAutomationEventHandler(changed, &read.element, scope, none, &handler)
    }
    .expect("a text-changed handler");
    println!("reader_listening");
    let deadline = Instant::now() + Duration::from_secs(15);
    while seen.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    println!("reader_text_changed={}", seen.load(Ordering::SeqCst));
    let text = unsafe { read.document.GetText(-1) }
        .expect("its text")
        .to_string();
    println!("reader_text_after={}", text.replace('\n', "|"));
    unsafe { read.automation.RemoveAllEventHandlers() }.expect("the handler removed");
}

/// The compiled reader's event handler: counts each text-changed event UI Automation hands
/// it, on whichever thread it calls.
#[implement(IUIAutomationEventHandler)]
struct Seen(Arc<AtomicUsize>);

impl IUIAutomationEventHandler_Impl for Seen_Impl {
    fn HandleAutomationEvent(
        &self,
        _sender: Ref<IUIAutomationElement>,
        event: UIA_EVENT_ID,
    ) -> windows_core::Result<()> {
        if event == UIA_Text_TextChangedEventId {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// The view whose sink drops it on its fault, and the host platform layer's view.
const DROPPED_ON_FAULT: u64 = 8;
const HOST_VIEW: u64 = 9;

thread_local! {
    static HELD: RefCell<Option<View>> = const { RefCell::new(None) };
}

/// A sink that drops the held view when it hears the fault, as the host ends a view from
/// inside the fault's window message. It runs in a window procedure, where a panic aborts,
/// so it asserts nothing; the row reads what it did. Inputs: the view id and the event.
fn dropping_sink(id: u64, event: ViewEvent) {
    if id == DROPPED_ON_FAULT && event == ViewEvent::Faulted {
        let held = HELD.with(|held| held.borrow_mut().take());
        drop(held);
    }
}

/// The host ends a faulted view from inside the fault's own window message: the window
/// procedure reads nothing of the view after its sink, so the view and its window go
/// while that message runs and the thread goes on.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_view_its_sink_drops_on_the_fault_goes_from_inside_the_fault_message() {
    let (parent, webview) = stand_ins();
    let view =
        View::new(parent, webview, DROPPED_ON_FAULT, dropping_sink, &style()).expect("a view");
    let _ = placed_shown_and_drawn(parent, &view);
    view.focus();
    pump_until(
        "the view's focus",
        FARM_WAIT,
        || unsafe { GetFocus() } == view.hwnd(),
    );
    faulted_unheard(&view, DROPPED_ON_FAULT);
    let hwnd = view.hwnd();
    HELD.with(|held| *held.borrow_mut() = Some(view));
    pump_until("the fault drops the view", FARM_WAIT, || {
        HELD.with(|held| held.borrow().is_none())
    });
    assert_eq!(
        unsafe { IsWindow(hwnd) },
        0,
        "the view's window went with it"
    );
    pump_for(Duration::from_millis(300));
    println!("h5 row: the sink dropped the view inside its fault message, its window went");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// The host's Windows calls on a real child window: host calls answer for a missing view,
/// the parent lookup takes only a `WRY_WEBVIEW` child, and a view opens hidden, shows over
/// its box, draws in the host's cell size, and once faulted refuses to fit and to draw. The
/// only row that marks the process's main thread, as setup does, so one process may run
/// every row: two marking rows would trip the mark's one-thread assert.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn the_host_platform_opens_draws_faults_and_dismisses_a_view_over_its_webview() {
    platform::mark_main();
    platform::mark_main();
    assert!(
        platform::main().is_some(),
        "the row's thread is the app's main one"
    );
    let elsewhere = thread::spawn(|| platform::main().is_none()).join();
    assert!(elsewhere.expect("another thread"), "another thread");
    let focus = command("main", 1, Command::Focus);
    assert_eq!(
        focus,
        Err("native_terminal_view_unknown"),
        "no view is open"
    );
    assert_eq!(command("main", 1, Command::Close), Ok(()));
    let (parent, webview) = stand_ins();
    let missing = Some("native_terminal_webview_missing");
    assert_eq!(
        platform::parent(parent as isize).err(),
        missing,
        "the Tauri window"
    );
    assert_eq!(platform::parent(0).err(), missing, "no window");
    let host = platform::parent(webview as isize).expect("the webview's window");
    assert!(platform::parent_scale(&host) >= 1.0);
    let style = style();
    let events = Channel::new(|_| Ok(()));
    let opened = platform::open_view(
        platform::trial_main(),
        host,
        HOST_VIEW,
        100,
        "rows",
        &style,
        events,
        None,
    );
    let mut view = opened.expect("a view");
    assert!(!platform::shown(&view), "a view opens hidden");
    let rect = [PAGE.0, PAGE.1, PAGE.2, PAGE.3];
    let scale = platform::place_view(&mut view, rect, true).expect("a shown view's scale");
    assert!(platform::shown(&view), "a placed view shows");
    assert_eq!(platform::scale(&view), scale);
    let mut shared = Shared::new(scale as f32, &style).expect("the host's renderer");
    assert!(platform::fit(&mut view, &mut shared, (400, 300)));
    assert!(platform::ready(&mut view));
    assert_eq!(platform::marked(&view), None);
    let before = presents();
    let frame = fed_frame(&Theme::default(), b"host platform\r\n");
    platform::paint(&mut view, &mut shared, frame).expect("a posted frame");
    await_present("the host's first present", before);
    host_cell_and_access(&view, &shared);
    host_fault(&mut view, &mut shared);
    let _events = platform::dismiss_view(view);
    println!("h5 row: the host platform opened, drew, faulted and dismissed a view");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// The render thread's posted cell is the one the host sizes grids with, and a drawn
/// terminal reaches UI Automation without a fault. Inputs: the view and the host renderer.
fn host_cell_and_access(view: &platform::View, shared: &Shared) {
    pump_until("the render thread's cell", FARM_WAIT, || {
        platform::trial_cell(view).is_some()
    });
    assert_eq!(
        platform::trial_cell(view),
        Some(shared.cell_points()),
        "the host sizes grids with the cell the render thread draws"
    );
    let mut pane = Pane::new(true);
    assert!(pane.on_attached(40, 12));
    assert!(pane.on_output(b"read me\r\n"));
    let term = pane.term().expect("an attached pane");
    let geometry = a11y::Geometry {
        cell: shared.cell_points(),
        pad: 0.0,
        scale: 1.0,
    };
    platform::drawn(view, geometry, term);
}

/// A refused resize faults the view: it hides, refuses to fit and answers the GPU code to
/// a frame. Inputs: the view and the host renderer.
fn host_fault(view: &mut platform::View, shared: &mut Shared) {
    draw::trial_refuse_resize(HOST_VIEW);
    let wider = [50.0, 40.0, 420.0, 300.0];
    let _ = platform::place_view(view, wider, true);
    pump_until("the refused resize faults the view", FARM_WAIT, || {
        !platform::fit(view, shared, (420, 300))
    });
    let late = fed_frame(&Theme::default(), b"after the fault\r\n");
    assert_eq!(platform::paint(view, shared, late), Err(GPU_FAULT));
    pump_for(Duration::from_millis(300));
    assert!(!platform::shown(view), "a faulted view hides");
}

/// The view id of the mouse row.
const MOUSE_VIEW: u64 = 8;

/// The events the mouse row's view sent for some input, focus changes left out: exactly
/// `want` of them, once they came and the tail passed. Inputs: what is awaited, the input
/// and how many events. Output: the events.
fn heard(what: &str, inputs: &[INPUT], want: usize) -> Vec<ViewEvent> {
    let counted = |seen: &[(u64, ViewEvent)]| {
        seen.iter()
            .filter(|(_, event)| !matches!(event, ViewEvent::Focus(_)))
            .count()
    };
    take_seen();
    send(inputs);
    pump_until(what, KEY_WAIT, || {
        counted(&SEEN.lock().expect("the seen events")) >= want
    });
    pump_for(KEY_TAIL);
    let seen = take_seen();
    assert!(
        seen.iter().all(|(id, _)| *id == MOUSE_VIEW),
        "{what}: {seen:?}"
    );
    let events: Vec<ViewEvent> = seen
        .into_iter()
        .map(|(_, event)| event)
        .filter(|event| !matches!(event, ViewEvent::Focus(_)))
        .collect();
    assert_eq!(events.len(), want, "{what}: {events:?}");
    events
}

/// The view's point for a screen point, at the view's scale. Inputs: the view and the
/// point. Output: the point.
fn view_point(view: &View, at: POINT) -> (f64, f64) {
    let mut client = at;
    unsafe { ScreenToClient(view.hwnd(), &mut client) };
    let scale = view.scale();
    (f64::from(client.x) / scale, f64::from(client.y) / scale)
}

/// Whether two points are within a pixel, the absolute input's rounding.
fn close(seen: (f64, f64), want: (f64, f64)) -> bool {
    (seen.0 - want.0).abs() <= 1.0 && (seen.1 - want.1).abs() <= 1.0
}

/// Checks a mouse event's kind and point. Inputs: the event, the kind and the point.
/// Output: the Shift and the click count it carried.
fn mouse_event(event: &ViewEvent, kind: MouseKind, want: (f64, f64)) -> (bool, u8) {
    let ViewEvent::Mouse {
        kind: seen,
        point,
        option,
        clicks,
    } = event
    else {
        panic!("a mouse event, got {event:?}");
    };
    assert_eq!(*seen, kind, "{event:?}");
    assert!(close(*point, want), "{event:?} at about {want:?}");
    (*option, *clicks)
}

/// Waits past the system's double-click time, so the next press counts one.
fn past_a_double_click() {
    let time_ms = u64::from(unsafe { GetDoubleClickTime() });
    pump_for(Duration::from_millis(time_ms + 100));
}

fn presses_count_clicks(view: &View, centre: POINT) {
    past_a_double_click();
    let tap = [
        pointer(centre, MOUSEEVENTF_LEFTDOWN, 0),
        pointer(centre, MOUSEEVENTF_LEFTUP, 0),
    ];
    let inputs: Vec<INPUT> = tap.iter().cycle().take(6).copied().collect();
    let events = heard("a triple click", &inputs, 6);
    let at = view_point(view, centre);
    let presses = events.iter().step_by(2);
    let counts: Vec<u8> = presses
        .map(|event| mouse_event(event, MouseKind::Press, at).1)
        .collect();
    assert_eq!(counts, [1, 2, 3], "one, a double, then a triple click");
    for release in events.iter().skip(1).step_by(2) {
        mouse_event(release, MouseKind::Release, at);
    }
}

fn shift_drag_selects(view: &View, centre: POINT) {
    past_a_double_click();
    let beside = POINT {
        x: centre.x + 40,
        y: centre.y,
    };
    let inputs = [
        keyboard(VK_SHIFT, 0, 0),
        pointer(centre, MOUSEEVENTF_LEFTDOWN, 0),
        pointer(beside, 0, 0),
        pointer(beside, MOUSEEVENTF_LEFTUP, 0),
        keyboard(VK_SHIFT, 0, KEYEVENTF_KEYUP),
    ];
    let events = heard("a Shift drag", &inputs, 3);
    let (from, to) = (view_point(view, centre), view_point(view, beside));
    let press = mouse_event(&events[0], MouseKind::Press, from);
    assert_eq!(press, (true, 1), "Shift forces a local selection");
    assert!(
        mouse_event(&events[1], MouseKind::Drag, to).0,
        "the drag holds Shift"
    );
    assert!(
        mouse_event(&events[2], MouseKind::Release, to).0,
        "so does the release"
    );
}

fn a_lost_capture_releases(view: &View, webview: HWND, centre: POINT) {
    past_a_double_click();
    let at = view_point(view, centre);
    let pressed = heard("a press", &[pointer(centre, MOUSEEVENTF_LEFTDOWN, 0)], 1);
    mouse_event(&pressed[0], MouseKind::Press, at);
    unsafe { SetCapture(webview) };
    let lost = take_seen();
    assert_eq!(lost.len(), 1, "one release for the lost capture: {lost:?}");
    let (option, _) = mouse_event(&lost[0].1, MouseKind::Release, at);
    assert!(!option, "the press held no Shift");
    send(&[pointer(centre, MOUSEEVENTF_LEFTUP, 0)]);
    pump_for(KEY_TAIL);
    assert_ne!(unsafe { ReleaseCapture() }, 0, "the stand-in lets go");
    let after = take_seen();
    assert!(
        after.is_empty(),
        "the button went up on the capture's window: {after:?}"
    );
}

fn wheel_and_right_click(view: &View, centre: POINT) {
    let mut setting = 0u32;
    let to = ptr::from_mut(&mut setting).cast();
    let read = unsafe { SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, to, 0) };
    assert_ne!(read, 0, "the wheel setting reads");
    assert!(
        (1..=100).contains(&setting),
        "lines a notch on the farm, got {setting}"
    );
    let at = view_point(view, centre);
    let lines = f64::from(setting);
    for (what, delta, want) in [("a notch up", 120, lines), ("a notch down", -120, -lines)] {
        let notch = heard(what, &[pointer(centre, MOUSEEVENTF_WHEEL, delta)], 1);
        let ViewEvent::Wheel {
            point,
            delta,
            precise,
        } = &notch[0]
        else {
            panic!("{what}: a wheel, got {notch:?}");
        };
        assert!(close(*point, at) && !*precise, "{what}: {notch:?}");
        assert_eq!(*delta, want, "{what} scrolls the setting's lines");
    }
    let right = [
        pointer(centre, MOUSEEVENTF_RIGHTDOWN, 0),
        pointer(centre, MOUSEEVENTF_RIGHTUP, 0),
    ];
    let menu = heard("a right click", &right, 1);
    let opened = matches!(menu[0], ViewEvent::Menu { point } if close(point, at));
    assert!(opened, "the menu opens at the pointer: {menu:?}");
}

fn keyboard_menus(view: &View) {
    let frame = fed_frame(&Theme::default(), b"native rows H1\r\n");
    let cursor = frame
        .cursor
        .as_ref()
        .map(|cursor| (cursor.row, cursor.column));
    assert!(
        cursor.is_some() && view.cell().is_some(),
        "a cursor cell to open at"
    );
    let point = mouse::cursor_point(cursor, view.cell());
    let up = KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP;
    let menu_key = vec![
        keyboard(VK_APPS, 0, KEYEVENTF_EXTENDEDKEY),
        keyboard(VK_APPS, 0, up),
    ];
    for (what, inputs) in [
        ("Shift+F10", chord(&[VK_SHIFT], VK_F10)),
        ("the menu key", menu_key),
    ] {
        let opened = heard(what, &inputs, 1);
        let want = [ViewEvent::Menu { point }];
        assert_eq!(opened, want, "{what} opens the menu under the cursor");
        let typed = heard("a key after the menu", &chord(&[], VK_A), 1);
        assert_eq!(typed, [text(b"a")], "keys still type after {what}");
    }
}

/// A drag that a hide cuts, as the host hides a covered view, ends with its release first,
/// and a hidden view that kept focus sends no wheel and no menu, as macOS sends none to a
/// hidden view. Inputs: the view and its centre on the screen.
fn hidden_views_send_no_mouse(view: &View, centre: POINT) {
    let hwnd = view.hwnd();
    let at = view_point(view, centre);
    let mut client = centre;
    unsafe { ScreenToClient(hwnd, &mut client) };
    let packed = |p: POINT| ((p.y as u16 as isize) << 16) | (p.x as u16 as isize);
    take_seen();
    unsafe { SendMessageW(hwnd, WM_LBUTTONDOWN, 0, packed(client)) };
    view.place(None).expect("hidden");
    let seen = sent();
    assert_eq!(
        seen.len(),
        2,
        "a press, then the hide ends the drag: {seen:?}"
    );
    let _ = mouse_event(&seen[0], MouseKind::Press, at);
    let _ = mouse_event(&seen[1], MouseKind::Release, at);
    unsafe { SendMessageW(hwnd, WM_MOUSEWHEEL, 120 << 16, packed(centre)) };
    unsafe { SendMessageW(hwnd, WM_CONTEXTMENU, hwnd as usize, -1) };
    let seen = sent();
    assert!(seen.is_empty(), "no wheel or menu after the hide: {seen:?}");
    view.place(Some(PAGE)).expect("shown again");
    pump_for(KEY_TAIL);
    take_seen();
}

#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn the_mouse_selects_scrolls_and_opens_the_menu_as_the_panel_does() {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, MOUSE_VIEW, sink, &style()).expect("a view");
    let centre = placed_shown_and_drawn(parent, &view);
    clicked_into(&view, centre);
    presses_count_clicks(&view, centre);
    shift_drag_selects(&view, centre);
    a_lost_capture_releases(&view, webview, centre);
    wheel_and_right_click(&view, centre);
    keyboard_menus(&view);
    hidden_views_send_no_mouse(&view, centre);
    drop(view);
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// The views the dismissing rows open, one id per row, and the larger font the style row
/// hands the render thread.
const DISMISSED_CLICK: u64 = 41;
const DISMISSED_COMPOSITION: u64 = 42;
const DISMISSED_END: u64 = 43;
const DISMISSED_HIDE: u64 = 44;
const DISMISSED_FOCUS: u64 = 45;
const STYLE_FIRST: u64 = 46;
const STYLE_LATER: u64 = 47;
const LARGER_PX: f64 = 22.0;

thread_local! {
    static KEPT: RefCell<Option<View>> = const { RefCell::new(None) };
    static ARMED: std::cell::Cell<Option<(u64, &'static str)>> = const { std::cell::Cell::new(None) };
    static SEEN_AT_DISMISS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Where a dismissing row's red leaves its mark: `H5C_R3_MARK`, else the temp folder.
fn freed_mark() -> std::path::PathBuf {
    std::env::var_os("H5C_R3_MARK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("h5c-r3-freed.txt"))
}

/// A sink that drops the kept view when it hears the armed view's `Marked`, as the host's
/// settle ends a view from inside a window message, then checks the view's state outlived
/// the drop. It runs in a window procedure, where a panic aborts, so a freed state writes
/// the mark and aborts. Inputs: the view id and the event.
fn dismissing(id: u64, event: ViewEvent) {
    let marked = event == ViewEvent::Marked;
    sink(id, event);
    let Some((armed, case)) = ARMED.get().filter(|(armed, _)| marked && *armed == id) else {
        return;
    };
    ARMED.set(None);
    let view = KEPT.with(|kept| kept.borrow_mut().take());
    drop(view);
    if !view::trial_inner_live(armed) {
        let _ = std::fs::write(freed_mark(), format!("freed inside the dispatch: {case}\n"));
        std::process::abort();
    }
    SEEN_AT_DISMISS.set(Some(SEEN.lock().expect("the seen events").len()));
}

/// Marks text on a view through a trial composition. Input: the view's window.
fn mark_at(hwnd: HWND) {
    let flags = isize::try_from(GCS_COMPSTR).expect("composition flags fit an LPARAM");
    view::trial_composition(Some(vec![told(GCS_COMPSTR, "か")]));
    let handled = unsafe { SendMessageW(hwnd, WM_IME_COMPOSITION, 0, flags) };
    view::trial_composition(None);
    assert_eq!(handled, 0, "the view handles a composition itself");
}

/// One dismissing row: a shown, focused view, with marked text unless the message itself
/// marks it, is kept for the sink, which drops it on its `Marked` inside the message `arm`
/// sends. The message reads nothing of the view after that: its state outlives the drop,
/// goes once the message returns, and nothing reaches the sink and no capture is held after
/// it. Inputs: the case, the view id, whether to mark first and the message.
fn dismissed_inside(case: &'static str, id: u64, premark: bool, arm: impl FnOnce(HWND)) {
    let (parent, webview) = stand_ins();
    let view = View::new(parent, webview, id, dismissing, &style()).expect("a view");
    let centre = placed_shown_and_drawn(parent, &view);
    clicked_into(&view, centre);
    let hwnd = view.hwnd();
    if premark {
        mark_at(hwnd);
        assert!(view.marked_text().is_some(), "{case}: marked text");
    }
    KEPT.with(|kept| *kept.borrow_mut() = Some(view));
    take_seen();
    SEEN_AT_DISMISS.set(None);
    ARMED.set(Some((id, case)));
    arm(hwnd);
    assert_eq!(ARMED.get(), None, "{case}: the sink dismissed the view");
    assert!(
        KEPT.with(|kept| kept.borrow().is_none()),
        "{case}: the view went"
    );
    assert!(
        !view::trial_inner_live(id),
        "{case}: the view's state goes once the message returns"
    );
    let at = SEEN_AT_DISMISS.get().expect("the dismissal was seen");
    let seen = take_seen();
    assert_eq!(
        seen.len(),
        at,
        "{case}: nothing after the dismissal: {seen:?}"
    );
    assert_ne!(unsafe { GetCapture() }, hwnd, "{case}: no capture held");
    assert_eq!(unsafe { IsWindow(hwnd) }, 0, "{case}: the window went");
    pump_for(KEY_TAIL);
    assert_eq!(take_seen(), [], "{case}: nothing later either");
    println!("h5c r3 row: {case}: the view went inside the message, nothing read after");
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}

/// A client point as a mouse message's lParam. Input: the point. Output: the lParam.
fn client_lparam(x: i32, y: i32) -> LPARAM {
    (((y & 0xffff) << 16) | (x & 0xffff)) as LPARAM
}

/// CR-0436 (view.rs:401): a click whose cancel of the marked text ends the view.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_click_whose_cancel_dismisses_the_view_reads_nothing_after() {
    dismissed_inside("click", DISMISSED_CLICK, true, |hwnd| unsafe {
        SendMessageW(hwnd, WM_LBUTTONDOWN, 0, client_lparam(200, 150));
    });
}

/// CR-0436: a composition whose redraw ends the view, before the IME is placed.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_composition_whose_redraw_dismisses_the_view_reads_nothing_after() {
    dismissed_inside("composition", DISMISSED_COMPOSITION, false, mark_at);
}

/// CR-0436: an end of composition whose redraw ends the view.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn an_end_of_composition_that_dismisses_the_view_reads_nothing_after() {
    dismissed_inside("end", DISMISSED_END, true, |hwnd| unsafe {
        SendMessageW(hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
    });
}

/// CR-0436: a hide of a faulted view, on a DPI change, whose cancel ends the view.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_hide_whose_cancel_dismisses_the_view_reads_nothing_after() {
    dismissed_inside("hide", DISMISSED_HIDE, true, |hwnd| {
        draw::BOARD.fault(DISMISSED_HIDE);
        unsafe { SendMessageW(hwnd, WM_DPICHANGED_AFTERPARENT, 0, 0) };
    });
}

/// CR-0436: a focus loss whose cancel ends the view, before its focus word.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_focus_loss_whose_cancel_dismisses_the_view_reads_nothing_after() {
    dismissed_inside("focus", DISMISSED_FOCUS, true, |hwnd| unsafe {
        SendMessageW(hwnd, WM_KILLFOCUS, 0, 0);
    });
}

/// The cell a style gives at a view's scale, as the host sizes grids with it. Inputs: the
/// view and the style. Output: the cell.
fn cell_in(view: &View, style: &Style) -> (f64, f64) {
    let shared = Shared::new(view.scale() as f32, style).expect("a renderer in the style");
    shared.cell_points()
}

/// CR-0436 (host/windows.rs:180): a style the render thread is handed after its first
/// reaches it in order: a later open in a larger font, as the host's restyle gives every
/// open, re-posts every live view's cell in that font, and the first style handed back
/// restores it, so later rows in the process draw as before.
#[test]
#[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
fn a_later_style_reaches_the_render_thread_and_every_live_views_cell() {
    let (parent, webview) = stand_ins();
    let first = style();
    let larger = Style {
        font_px: LARGER_PX,
        ..style()
    };
    let view = View::new(parent, webview, STYLE_FIRST, sink, &first).expect("a view");
    let _ = placed_shown_and_drawn(parent, &view);
    pump_until("the first cell", FARM_WAIT, || view.cell().is_some());
    let (small, big) = (cell_in(&view, &first), cell_in(&view, &larger));
    assert_ne!(small, big, "the larger font has a larger cell");
    assert_eq!(
        view.cell(),
        Some(small),
        "the first view's cell is its style's"
    );
    let later = View::new(parent, webview, STYLE_LATER, sink, &larger).expect("a later view");
    let before = presents();
    assert!(later.feed(fed_frame(&larger.theme, b"larger\r\n")));
    later.place(Some(PAGE)).expect("a placed box");
    await_present("the later view's present", before);
    pump_until("the later view's cell", FARM_WAIT, || {
        later.cell().is_some()
    });
    let cells = || (view.cell(), later.cell());
    let wait = Instant::now() + FARM_WAIT;
    while cells() != (Some(big), Some(big)) && Instant::now() < wait {
        pump_for(Duration::from_millis(50));
    }
    println!("h5c r3 row: style cells {:?}, want {big:?}", cells());
    assert_eq!(
        cells(),
        (Some(big), Some(big)),
        "the later style reached the render thread and every live view's cell"
    );
    drop(later);
    draw::start(&first).expect("the render thread");
    pump_until("the first style's cell again", FARM_WAIT, || {
        view.cell() == Some(small)
    });
    drop(view);
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}
