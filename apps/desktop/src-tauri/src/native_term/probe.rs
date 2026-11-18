use super::host::{self, Look};
use super::view::TermView;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, MainThreadMarker, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSResponder};
use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSString};

/// The longest text one trial step types or composes, in bytes.
pub const TEXT_BYTES_MAX: usize = 4096;

/// One step a native-webdriver trial takes on an open native view, through the view's own
/// AppKit selectors so its input reaches the pane as the owner's would: a key down, an input
/// method's marked text, an input method's commit, or nothing before a read.
#[derive(serde::Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Step {
    Key { characters: String, code: u16 },
    Compose { text: String },
    Commit { text: String },
    Read,
}

/// What a trial reads after its step: the visible rows, whether the direct path draws the
/// view, whether the view is its window's first responder, whether the app is active and its
/// window key, the window's backing scale (0 with no window), the marked text and the
/// private pasteboard's text.
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
        Step::Key { characters, code } => key(&view, &checked(&characters)?, code)?,
        Step::Compose { text } => compose(&view, &checked(&text)?),
        Step::Commit { text } => commit(&view, &checked(&text)?),
        Step::Read => {}
    }
    let look = host::look(window, page)?;
    Ok(seen(mtm, look))
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

fn seen(mtm: MainThreadMarker, look: Look) -> Seen {
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
        scale: window.as_ref().map_or(0.0, |window| window.backingScaleFactor()),
        marked: look.view.marked_text(),
        pasteboard: look.pasteboard,
    };
    assert!(!seen.focused || window.is_some());
    assert!(seen.scale >= 0.0 && seen.scale.is_finite());
    seen
}
