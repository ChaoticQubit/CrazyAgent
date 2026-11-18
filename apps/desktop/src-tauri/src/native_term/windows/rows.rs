use std::ptr;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CLR_INVALID, ClientToScreen, CreateSolidBrush, GetDC, GetPixel, ReleaseDC, ScreenToClient,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::Ime::{
    CFS_POINT, COMPOSITIONFORM, ImmGetCompositionWindow, ImmGetContext, ImmReleaseContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetFocus, GetKeyboardLayout, INPUT, INPUT_0, INPUT_KEYBOARD,
    INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    KLF_ACTIVATE, LoadKeyboardLayoutW, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEINPUT, MapVirtualKeyExW, SendInput, SetFocus, VK_A,
    VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_DOWN, VK_E, VK_END, VK_ESCAPE, VK_F5, VK_HOME,
    VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_OEM_7, VK_PRIOR, VK_Q, VK_RETURN, VK_RIGHT, VK_RMENU,
    VK_SHIFT, VK_TAB, VK_UP, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GW_CHILD, GetSystemMetrics,
    GetWindow, GetWindowRect, IsWindow, IsWindowVisible, MSG, PM_REMOVE, PeekMessageW,
    PostMessageW, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SendMessageW, TranslateMessage,
    WM_CHAR, WM_DPICHANGED_AFTERPARENT, WM_IME_CHAR, WM_IME_ENDCOMPOSITION,
    WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use super::super::event::ViewEvent;
use super::super::pane::{CONTROL, Input, Named, Pane, SHIFT};
use super::super::screen::{self, Frame};
use super::super::style::{Style, Theme, font_for};
use super::draw;
use super::place;
use super::view::{self, View};

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
    let webview_class = stand_in_class("NativeRowsWebview", GREEN);
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
    let mut pane = Pane::new(1);
    assert!(pane.on_attached(40, 12));
    assert!(pane.on_output(text));
    screen::frame(pane.term().expect("an attached pane"), theme, true)
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

fn click(at: POINT) {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(2);
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(2);
    let (dx, dy) = (at.x * 65_535 / (width - 1), at.y * 65_535 / (height - 1));
    let mouse = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        mouse(0),
        mouse(MOUSEEVENTF_LEFTDOWN),
        mouse(MOUSEEVENTF_LEFTUP),
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
    view.cover(true);
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
    view.cover(false);
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
    click(centre);
    pump_until(
        "the click's focus",
        FARM_WAIT,
        || unsafe { GetFocus() } == view.hwnd(),
    );
    let seen = take_seen();
    let want = [
        ViewEvent::Focus(true),
        ViewEvent::Focus(false),
        ViewEvent::Focus(true),
    ];
    let events: Vec<&ViewEvent> = seen.iter().map(|(_, event)| event).collect();
    assert_eq!(events, want.iter().collect::<Vec<_>>(), "{seen:?}");
    assert!(seen.iter().all(|(id, _)| *id == 1));
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
        (
            "Shift+Tab",
            chord(&[VK_SHIFT], VK_TAB),
            named(Named::BackTab, SHIFT),
        ),
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
    view.cover(true);
    take_seen();
    send(&chord(&[], VK_A));
    pump_for(KEY_TAIL);
    assert_eq!(typing(), 0, "a covered view types nothing");
    view.cover(false);
    click(centre);
    pump_until(
        "focus again",
        FARM_WAIT,
        || unsafe { GetFocus() } == view.hwnd(),
    );
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
    click(centre);
    pump_until(
        "the click's focus",
        FARM_WAIT,
        || unsafe { GetFocus() } == view.hwnd(),
    );
    keys_send_once();
    layouts_type_their_characters();
    unicode_and_ime_text_types_once(&view);
    ime_window_at_the_cursor_cell(&view);
    inactive_views_type_nothing(&view, webview, centre);
    drop(view);
    assert_ne!(unsafe { DestroyWindow(parent) }, 0, "the stand-in goes");
}
