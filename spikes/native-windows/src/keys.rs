use crate::probe::{
    child, click_screen, click_term, log, on_main, on_main_within, take_notes, wide,
};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, IME_CMODE_FULLSHAPE, IME_CMODE_HANGUL, IME_CMODE_NATIVE,
    IME_CONVERSION_MODE, ImmGetContext, ImmGetConversionStatus, ImmGetOpenStatus,
    ImmReleaseContext, ImmSetCandidateWindow, ImmSetConversionStatus, ImmSetOpenStatus,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetKeyboardLayout, GetKeyboardLayoutList, GetKeyboardLayoutNameW, HKL,
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KLF_ACTIVATE, LoadKeyboardLayoutW, MAPVK_VK_TO_VSC, MapVirtualKeyExW, SendInput, SetFocus,
    VK_A, VK_E, VK_ESCAPE, VK_F, VK_G, VK_K, VK_LCONTROL, VK_LWIN, VK_M, VK_OEM_7, VK_OEM_PERIOD,
    VK_Q, VK_R, VK_RETURN, VK_RMENU, VK_S, VK_SPACE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, IsWindowVisible,
};

/// How long a row waits for a hung window thread to answer again before going on.
const RECOVER_WAIT: Duration = Duration::from_secs(200);

/// Where the IME rows ask the candidate list to open, in the view's client pixels.
const CANDIDATE_AT: POINT = POINT { x: 8, y: 400 };

/// Keyboard layouts the layout rows load: US-International for dead keys, German for AltGr.
const US_INTERNATIONAL: &str = "00020409";
const GERMAN: &str = "00000407";

/// How long a row waits after its last key for the messages to arrive.
const SETTLE: Duration = Duration::from_millis(600);

/// The most top-level windows the emoji row lists, and keys in one send.
const WINDOWS_MAX: usize = 256;
const STROKES_MAX: usize = 32;

/// Sends virtual keys through `SendInput` with their scan codes in a layout, so the
/// window's layout translates them (unlike `KEYEVENTF_UNICODE`, which skips it). Right Alt
/// and the Windows key go as extended keys. Inputs: (key, released) pairs and the layout.
/// Output: how many events Windows took.
pub fn send_keys(strokes: &[(u16, bool)], layout: isize) -> u32 {
    assert!(!strokes.is_empty() && strokes.len() <= STROKES_MAX);
    let inputs: Vec<INPUT> = strokes
        .iter()
        .map(|&(key, up)| {
            let scan = unsafe { MapVirtualKeyExW(u32::from(key), MAPVK_VK_TO_VSC, layout as HKL) };
            let release = if up { KEYEVENTF_KEYUP } else { 0 };
            let extended = if matches!(key, VK_RMENU | VK_LWIN) {
                KEYEVENTF_EXTENDEDKEY
            } else {
                0
            };
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: key,
                        wScan: scan as u16,
                        dwFlags: release | extended,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        })
        .collect();
    assert_eq!(inputs.len(), strokes.len());
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    }
}

/// A press and release of each key in turn. Input: the keys. Output: the strokes.
fn taps(keys: &[u16]) -> Vec<(u16, bool)> {
    let strokes: Vec<(u16, bool)> = keys
        .iter()
        .flat_map(|&key| [(key, false), (key, true)])
        .collect();
    assert_eq!(strokes.len(), keys.len() * 2);
    strokes
}

/// The window thread's layout name, like "00020409". Output: the name or "unknown".
fn layout_name() -> String {
    let mut name = [0u16; 9];
    if unsafe { GetKeyboardLayoutNameW(name.as_mut_ptr()) } == 0 {
        return "unknown".into();
    }
    String::from_utf16_lossy(&name[..8])
}

/// Loads and activates a layout on the window's thread, where it applies, and focuses the
/// view. Input: the layout id. Output: the layout before, the loaded one, and the line.
fn load_layout(id: &'static str) -> Option<(isize, isize, String)> {
    on_main(move || {
        let before = unsafe { GetKeyboardLayout(0) } as isize;
        let loaded = unsafe { LoadKeyboardLayoutW(wide(id).as_ptr(), KLF_ACTIVATE) } as isize;
        unsafe { SetFocus(child()) };
        let now = unsafe { GetKeyboardLayout(0) } as isize;
        let line = format!(
            "before={before:X} loaded={loaded:X} now={now:X} name={}",
            layout_name()
        );
        (before, loaded, line)
    })
}

/// Puts a layout back on the window's thread. Input: the layout.
fn restore_layout(layout: isize) {
    on_main(move || {
        unsafe { ActivateKeyboardLayout(layout as HKL, 0) };
    });
}

/// Sends strokes in a layout and logs every key, character and IME message the view got.
/// Inputs: the row name, the strokes and the layout.
fn sent_row(row: &str, strokes: &[(u16, bool)], layout: isize) {
    take_notes();
    let taken = send_keys(strokes, layout);
    std::thread::sleep(SETTLE);
    log(
        row,
        &format!("events={taken} got={}", take_notes().join(" ")),
    );
}

/// The dead-key row: US-International, apostrophe then e gives é through WM_DEADCHAR,
/// and apostrophe then space gives the apostrophe.
pub fn dead_key_rows() {
    click_term();
    let Some((before, loaded, line)) = load_layout(US_INTERNATIONAL) else {
        log("dead_key_layout", "timeout");
        return;
    };
    log("dead_key_layout", &line);
    sent_row("dead_key_quote_e", &taps(&[VK_OEM_7, VK_E]), loaded);
    sent_row("dead_key_quote_space", &taps(&[VK_OEM_7, VK_SPACE]), loaded);
    restore_layout(before);
}

/// The AltGr row: German, right Alt with Q is "@". Sent once as right Alt alone and once
/// as left Ctrl with right Alt, the pair Windows makes of AltGr on a real keyboard.
pub fn altgr_rows() {
    click_term();
    let Some((before, loaded, line)) = load_layout(GERMAN) else {
        log("altgr_layout", "timeout");
        return;
    };
    log("altgr_layout", &line);
    let alone = [
        (VK_RMENU, false),
        (VK_Q, false),
        (VK_Q, true),
        (VK_RMENU, true),
    ];
    sent_row("altgr_rmenu_q", &alone, loaded);
    let paired = [
        (VK_LCONTROL, false),
        (VK_RMENU, false),
        (VK_Q, false),
        (VK_Q, true),
        (VK_RMENU, true),
        (VK_LCONTROL, true),
    ];
    sent_row("altgr_lctrl_rmenu_q", &paired, loaded);
    restore_layout(before);
}

/// The installed layouts. Output: each as a number.
fn layouts() -> Vec<isize> {
    let mut found: [HKL; 32] = [std::ptr::null_mut(); 32];
    let count =
        unsafe { GetKeyboardLayoutList(found.len() as i32, found.as_mut_ptr()) }.max(0) as usize;
    assert!(count <= found.len());
    found[..count]
        .iter()
        .map(|&layout| layout as isize)
        .collect()
}

/// Asks the IME to open its candidate list at `CANDIDATE_AT`. Input: the view's context.
/// Output: whether it took the place and that place on the screen.
fn place_candidates(context: windows_sys::Win32::UI::Input::Ime::HIMC) -> String {
    let form = CANDIDATEFORM {
        dwIndex: 0,
        dwStyle: CFS_CANDIDATEPOS,
        ptCurrentPos: CANDIDATE_AT,
        rcArea: RECT::default(),
    };
    let placed = unsafe { ImmSetCandidateWindow(context, &form) };
    let mut screen = CANDIDATE_AT;
    unsafe { ClientToScreen(child(), &mut screen) };
    format!(
        "candidate_set={placed} candidate_at={},{}",
        screen.x, screen.y
    )
}

/// Activates an IME layout on the window's thread, opens it, sets its conversion mode and
/// keeps its sentence mode (zero would commit every kana unconverted), and places its
/// candidate list. Inputs: the layout and the mode. Output: the layout before and the
/// line, or None on a timeout.
fn open_ime(layout: isize, mode: IME_CONVERSION_MODE) -> Option<(isize, String)> {
    on_main(move || {
        let before = unsafe { GetKeyboardLayout(0) } as isize;
        log("ime_open_step", "activate");
        unsafe { ActivateKeyboardLayout(layout as HKL, 0) };
        log("ime_open_step", "focus");
        unsafe { SetFocus(child()) };
        let context = unsafe { ImmGetContext(child()) };
        if context.is_null() {
            return (before, "context_null".into());
        }
        log("ime_open_step", "open");
        let opened = unsafe { ImmSetOpenStatus(context, 1) };
        let (mut conversion, mut sentence) = (0u32, 0u32);
        unsafe { ImmGetConversionStatus(context, &mut conversion, &mut sentence) };
        log("ime_open_step", "mode");
        let moded = unsafe { ImmSetConversionStatus(context, mode, sentence) };
        log("ime_open_step", "done");
        unsafe { ImmGetConversionStatus(context, &mut conversion, &mut sentence) };
        let open = unsafe { ImmGetOpenStatus(context) };
        let candidates = place_candidates(context);
        unsafe { ImmReleaseContext(child(), context) };
        let now = unsafe { GetKeyboardLayout(0) } as isize;
        let line = format!(
            "now={now:X} open_set={opened} open={open} mode_set={moded} mode={conversion:X} \
             sentence={sentence:X} {candidates}"
        );
        (before, line)
    })
}

/// Types keys one by one, 250 ms apart, in a layout. Inputs: the keys and the layout.
fn type_keys(keys: &[u16], layout: isize) {
    for &key in keys {
        send_keys(&taps(&[key]), layout);
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The real IME rows, run after the language step installs ja-JP and ko-KR: MS-IME types
/// "ka" and converts twice (the second opens the candidate list, whose window and place
/// are logged) and Enter commits; the Korean IME types "gksrmf" (한글) and a space
/// commits. Each logs what reached the view, or INCONCLUSIVE when the layout is missing.
pub fn real_ime_rows() {
    let installed = layouts();
    let names: Vec<String> = installed
        .iter()
        .map(|layout| format!("{:08X}", *layout as usize & 0xFFFF_FFFF))
        .collect();
    log("ime_real_layouts", &names.join(","));
    let japanese = (
        0x0411,
        "ja",
        IME_CMODE_NATIVE | IME_CMODE_FULLSHAPE,
        vec![VK_K, VK_A, VK_SPACE, VK_SPACE],
        VK_RETURN,
    );
    let korean = (
        0x0412,
        "ko",
        IME_CMODE_HANGUL,
        vec![VK_G, VK_K, VK_S, VK_R, VK_M, VK_F],
        VK_SPACE,
    );
    for (language, label, mode, compose, commit) in [japanese, korean] {
        let row = format!("ime_real_{label}");
        let Some(&layout) = installed
            .iter()
            .find(|&&layout| layout & 0xFFFF == language)
        else {
            log(&row, "INCONCLUSIVE no layout for the language");
            continue;
        };
        click_term();
        let Some((before, line)) = open_ime(layout, mode) else {
            log(&row, "INCONCLUSIVE timeout opening the IME");
            let waited = std::time::Instant::now();
            let back = on_main_within(RECOVER_WAIT, || ()).is_some();
            let took = waited.elapsed().as_secs_f64();
            log(
                &format!("{row}_recovered"),
                &format!("{back} after={took:.1}s"),
            );
            continue;
        };
        log(&format!("{row}_open"), &line);
        take_notes();
        let windows = top_windows();
        type_keys(&compose, layout);
        std::thread::sleep(SETTLE);
        let appeared = new_windows(&windows);
        log(&format!("{row}_composed"), &take_notes().join(" "));
        log(&format!("{row}_windows"), &appeared.join(" ; "));
        type_keys(&[commit], layout);
        std::thread::sleep(SETTLE);
        log(&format!("{row}_committed"), &take_notes().join(" "));
        restore_layout(before);
    }
}

/// A visible top-level window: its handle, "class|title" and its screen rectangle.
struct Top {
    hwnd: isize,
    name: String,
    rect: RECT,
}

/// Visible top-level windows. Output: up to `WINDOWS_MAX` of them.
fn top_windows() -> Vec<Top> {
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> windows_sys::core::BOOL {
        let found = unsafe { &mut *(found as *mut Vec<Top>) };
        if unsafe { IsWindowVisible(hwnd) } != 0 {
            let (mut class, mut title) = ([0u16; 128], [0u16; 128]);
            let class_len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 128) }.max(0) as usize;
            let title_len =
                unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), 128) }.max(0) as usize;
            let mut rect = RECT::default();
            unsafe { GetWindowRect(hwnd, &mut rect) };
            let name = format!(
                "{}|{}",
                String::from_utf16_lossy(&class[..class_len.min(128)]),
                String::from_utf16_lossy(&title[..title_len.min(128)])
            );
            found.push(Top {
                hwnd: hwnd as isize,
                name,
                rect,
            });
        }
        i32::from(found.len() < WINDOWS_MAX)
    }
    let mut found: Vec<Top> = Vec::new();
    unsafe { EnumWindows(Some(each), &mut found as *mut Vec<Top> as LPARAM) };
    assert!(found.len() <= WINDOWS_MAX);
    found
}

/// The visible top-level windows that were not there before, with their rectangles.
/// Input: the windows before. Output: one "class|title [l,t r,b]" per new window.
fn new_windows(before: &[Top]) -> Vec<String> {
    top_windows()
        .into_iter()
        .filter(|window| before.iter().all(|old| old.hwnd != window.hwnd))
        .map(|window| {
            let r = window.rect;
            format!(
                "{} [{},{} {},{}]",
                window.name, r.left, r.top, r.right, r.bottom
            )
        })
        .collect()
}

/// The emoji panel row: Win+. on the focused view and the windows that appear, then Enter
/// and then a click at the panel's centre, each with the UTF-16 units that reach the
/// view; INCONCLUSIVE when no window appears. Escape closes the panel after.
pub fn emoji_rows() {
    click_term();
    take_notes();
    let before = top_windows();
    let chord = [
        (VK_LWIN, false),
        (VK_OEM_PERIOD, false),
        (VK_OEM_PERIOD, true),
        (VK_LWIN, true),
    ];
    let taken = send_keys(&chord, 0);
    std::thread::sleep(Duration::from_millis(2000));
    let panel: Vec<Top> = top_windows()
        .into_iter()
        .filter(|window| before.iter().all(|old| old.hwnd != window.hwnd))
        .collect();
    let named: Vec<String> = panel.iter().map(|window| window.name.clone()).collect();
    let verdict = if panel.is_empty() {
        "INCONCLUSIVE no panel window"
    } else {
        "panel"
    };
    log(
        "emoji_panel",
        &format!("{verdict} events={taken} new={}", named.join(" ; ")),
    );
    send_keys(&taps(&[VK_RETURN]), 0);
    std::thread::sleep(Duration::from_millis(1000));
    log("emoji_picked_enter", &take_notes().join(" "));
    if let Some(window) = panel.first() {
        let r = window.rect;
        let centre = POINT {
            x: (r.left + r.right) / 2,
            y: (r.top + r.bottom) / 2,
        };
        let clicked = click_screen(centre);
        std::thread::sleep(Duration::from_millis(1000));
        let got = take_notes().join(" ");
        log(
            "emoji_picked_click",
            &format!("at={},{} events={clicked} got={got}", centre.x, centre.y),
        );
    }
    send_keys(&taps(&[VK_ESCAPE]), 0);
    std::thread::sleep(Duration::from_millis(300));
}
