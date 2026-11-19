use super::host::{self, Look};
use super::stamps;
use super::view::TermView;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSResponder, NSView,
};
use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSString};
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

/// The longest text one trial step types or composes, in bytes.
pub const TEXT_BYTES_MAX: usize = 4096;

/// The most views whose last cover(false) entry stamp is kept, the oldest going first.
const ASKED_MAX: usize = 32;

static ASKED: Mutex<VecDeque<(u64, f64)>> = Mutex::new(VecDeque::new());

/// Stamps a cover(false) as its bridge command starts, before the main-thread hop, so a
/// trial splits a switch into the page's share and the host's. Input: the view id the page
/// named. Output: none.
pub fn asked(page: u64) {
    let now = stamps::now_ms();
    let mut asked = ASKED.lock().unwrap_or_else(PoisonError::into_inner);
    asked.retain(|(held, _)| *held != page);
    if asked.len() == ASKED_MAX {
        asked.pop_front();
    }
    asked.push_back((page, now));
    assert!(asked.len() <= ASKED_MAX);
    assert!(asked.iter().filter(|(held, _)| *held == page).count() == 1);
}

fn asked_at(page: u64) -> Option<f64> {
    let asked = ASKED.lock().unwrap_or_else(PoisonError::into_inner);
    let at = asked
        .iter()
        .find(|(held, _)| *held == page)
        .map(|(_, at)| *at);
    assert!(at.is_none_or(|at| at.is_finite() && at > 0.0));
    at
}

/// One step a native-webdriver trial takes on an open native view, through the view's own
/// AppKit selectors so its input reaches the pane as the owner's would: a key down, an input
/// method's marked text, an input method's commit, a backing scale every view reads from
/// then on (1 to 4, or 0 for the window's own), or nothing before a read.
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Step {
    Key { characters: String, code: u16 },
    Compose { text: String },
    Commit { text: String },
    Scale { factor: f64 },
    Read,
}

/// What a trial reads after its step: the visible rows, whether the direct path draws the
/// view, whether the view is its window's first responder, whether the app is active and its
/// window key, the window's backing scale (0 with no window), the marked text, the private
/// pasteboard's text, the view's frame as x, y, width and height in its webview's own
/// coordinates and that webview's height, both in points, whether the view is hidden, its
/// show stamps (`ShowStamps`) and the host's clock at the read, both in ms since 1970, when
/// its last cover(false) command started, the draws it skipped since its uncover with the
/// first reason, and the app-wide counts of page commands answered busy and of covers told
/// refused.
#[derive(serde::Serialize, Debug, PartialEq)]
pub struct Seen {
    pub rows: Vec<String>,
    pub direct: bool,
    pub focused: bool,
    pub active: bool,
    pub key: bool,
    pub scale: f64,
    pub marked: Option<String>,
    pub pasteboard: Option<String>,
    pub frame: [f64; 4],
    pub webview_height: f64,
    pub hidden: bool,
    pub uncovered_ms: Option<f64>,
    pub presented_ms: Option<f64>,
    pub now_ms: f64,
    pub asked_ms: Option<f64>,
    pub skipped: u32,
    pub skip_first: Option<&'static str>,
    pub busy_answers: u64,
    pub cover_refused: u64,
}

/// Takes one trial step on a view the calling window opened, then reads it. Inputs: the
/// marker, the window label, the view id and the step. Output: what it reads, or a plain
/// code: `native_terminal_view_unknown`, `native_terminal_probe_failed`.
pub fn take(
    mtm: MainThreadMarker,
    window: &str,
    page: u64,
    step: Step,
) -> Result<Seen, &'static str> {
    assert!(!window.is_empty());
    assert!(page > 0);
    let view = host::look(window, page)?.view;
    match step {
        Step::Key { characters, code } => {
            let characters = checked(&characters)?;
            key(&view, &characters, code)?;
        }
        Step::Compose { text } => {
            let text = checked(&text)?;
            compose(&view, &text);
        }
        Step::Commit { text } => {
            let text = checked(&text)?;
            commit(&view, &text);
        }
        Step::Scale { factor: 0.0 } => host::trial_scale(None),
        Step::Scale { factor } if (1.0..=4.0).contains(&factor) => {
            host::trial_scale(Some(factor));
        }
        Step::Scale { .. } => return Err("native_terminal_probe_failed"),
        Step::Read => {}
    }
    let look = host::look(window, page)?;
    Ok(seen(mtm, look, asked_at(page)))
}

fn checked(text: &str) -> Result<Retained<NSString>, &'static str> {
    if text.is_empty() || text.len() > TEXT_BYTES_MAX {
        return Err("native_terminal_probe_failed");
    }
    let string = NSString::from_str(text);
    assert_eq!(string.to_string(), text);
    Ok(string)
}

fn no_replacement() -> NSRange {
    let range = NSRange::new(NSNotFound as usize, 0);
    assert_eq!(range.length, 0);
    range
}

fn key(view: &TermView, characters: &NSString, code: u16) -> Result<(), &'static str> {
    let window = view.window().ok_or("native_terminal_probe_failed")?;
    let number = window.windowNumber();
    assert!(number > 0);
    let none: *const AnyObject = std::ptr::null();
    let event: Option<Retained<NSEvent>> = unsafe {
        msg_send![
            NSEvent::class(),
            keyEventWithType: NSEventType::KeyDown,
            location: NSPoint::new(0.0, 0.0),
            modifierFlags: NSEventModifierFlags::empty(),
            timestamp: 0.0f64,
            windowNumber: number,
            context: none,
            characters: characters,
            charactersIgnoringModifiers: characters,
            isARepeat: false,
            keyCode: code
        ]
    };
    let event = event.ok_or("native_terminal_probe_failed")?;
    assert_eq!(event.r#type(), NSEventType::KeyDown);
    let () = unsafe { msg_send![view, keyDown: &*event] };
    Ok(())
}

fn compose(view: &TermView, text: &NSString) {
    let selected = NSRange::new(text.length(), 0);
    assert!(selected.location > 0);
    let () = unsafe {
        msg_send![
            view,
            setMarkedText: text,
            selectedRange: selected,
            replacementRange: no_replacement()
        ]
    };
}

fn commit(view: &TermView, text: &NSString) {
    assert!(text.length() > 0);
    let () = unsafe { msg_send![view, insertText: text, replacementRange: no_replacement()] };
}

fn seen(mtm: MainThreadMarker, look: Look, asked_ms: Option<f64>) -> Seen {
    let window = look.view.window();
    let responder: &NSResponder = &look.view;
    let focused = window
        .as_ref()
        .and_then(|window| window.firstResponder())
        .is_some_and(|first| std::ptr::eq(&*first, responder));
    let seen = Seen {
        rows: look.rows,
        direct: look.direct,
        focused,
        active: NSApplication::sharedApplication(mtm).isActive(),
        key: window.as_ref().is_some_and(|window| window.isKeyWindow()),
        scale: window
            .as_ref()
            .map_or(0.0, |window| window.backingScaleFactor()),
        marked: look.view.marked_text(),
        pasteboard: look.pasteboard,
        frame: in_webview(&look.view, &look.webview),
        webview_height: look.webview.bounds().size.height,
        hidden: look.hidden,
        uncovered_ms: look.stamps.uncovered_ms,
        presented_ms: look.stamps.presented_ms,
        now_ms: look.now_ms,
        asked_ms,
        skipped: look.stamps.skipped,
        skip_first: look.stamps.skip_first,
        busy_answers: look.busy_answers,
        cover_refused: look.cover_refused,
    };
    assert!(!seen.focused || window.is_some());
    assert!(seen.scale >= 0.0 && seen.scale.is_finite());
    assert!(seen.webview_height >= 0.0 && seen.webview_height.is_finite());
    assert!(seen.hidden == seen.uncovered_ms.is_none());
    assert!(seen.presented_ms.is_none() || seen.uncovered_ms.is_some());
    assert!(seen.skipped == 0 || seen.uncovered_ms.is_some());
    assert_eq!(seen.skip_first.is_some(), seen.skipped > 0);
    seen
}

/// Reads where AppKit placed a view, counted in its webview's own coordinates, so a trial
/// can set it against the page rect it was given. Inputs: the view and its webview.
/// Output: x, y, width and height in points.
fn in_webview(view: &TermView, webview: &NSView) -> [f64; 4] {
    let superview = unsafe { view.superview() };
    let placed = webview.convertRect_fromView(view.frame(), superview.as_deref());
    let frame = [
        placed.origin.x,
        placed.origin.y,
        placed.size.width,
        placed.size.height,
    ];
    assert!(frame.iter().all(|side| side.is_finite()));
    assert!(frame[2] >= 0.0 && frame[3] >= 0.0);
    frame
}
