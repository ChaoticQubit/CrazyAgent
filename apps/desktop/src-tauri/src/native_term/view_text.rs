use super::HookInput;
use super::a11y::{self, Geometry, Marks, Text};
use super::ime::{self, Mods, Route};
use super::pane::Input;
use super::view::{TermView, key_input};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, DefinedClass, msg_send};
use objc2_app_kit::{
    NSAccessibilityNotificationName, NSAccessibilityPostNotification,
    NSAccessibilitySelectedTextChangedNotification, NSAccessibilityValueChangedNotification,
    NSEvent, NSEventModifierFlags,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSInteger, NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString,
};

/// The key event `interpretKeyEvents` is working on: its characters with and without
/// modifiers, and whether the input method took it (inserted or marked text).
pub struct KeyDown {
    pub chars: String,
    pub bare: String,
    pub handled: bool,
}

/// What VoiceOver reads, kept on the view so no getter waits on the native state: the last
/// text, whether a frame drawn since made it stale, and what the last frame showed (a hash
/// of its rows, its cursor and selection) to post only real changes.
pub struct Access {
    pub text: Text,
    pub stale: bool,
    pub rows_key: u64,
    pub marks: Marks,
}

impl Default for Access {
    fn default() -> Access {
        Access {
            text: Text::default(),
            stale: true,
            rows_key: 0,
            marks: Marks::default(),
        }
    }
}

/// An AppKit range from an optional UTF-16 range: none is {NSNotFound, 0}. Input: the range.
/// Output: the NSRange.
pub fn range_or_none(range: Option<(usize, usize)>) -> NSRange {
    let ns = range.map_or(NSRange::new(NSNotFound as usize, 0), |(at, length)| {
        NSRange::new(at, length)
    });
    assert!(range.is_some() || ns.location == NSNotFound as usize);
    ns
}

/// The plain text of what an input method hands over, an NSString or an NSAttributedString.
/// Input: the object. Output: its text, empty for any other class.
fn plain_text(string: &AnyObject) -> String {
    if let Some(attributed) = string.downcast_ref::<NSAttributedString>() {
        return attributed.string().to_string();
    }
    string
        .downcast_ref::<NSString>()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// What the hook's accessibility read saw through the view's own selectors: whether AppKit
/// gave the view an input context (it declares NSTextInputClient), its role, value,
/// insertion line, selected range and marked range.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AccessReport {
    pub input_context: bool,
    pub role: String,
    pub value: String,
    pub insertion_line: isize,
    pub selected: (usize, usize),
    pub marked: Option<(usize, usize)>,
}

/// Runs one hook text operation through the view's Objective-C selectors, as an input
/// method or VoiceOver would. Inputs: the view and the operation. Output: the report of an
/// accessibility read, none for the others.
pub fn hook_text(view: &TermView, input: &HookInput) -> Option<AccessReport> {
    assert!(input.text_op());
    let none = range_or_none(None);
    match input {
        HookInput::Marked { text, at } => {
            let string = NSString::from_str(text);
            let selected = NSRange::new(*at, 0);
            let _: () = unsafe {
                msg_send![
                    view,
                    setMarkedText: &*string,
                    selectedRange: selected,
                    replacementRange: none
                ]
            };
            None
        }
        HookInput::Insert { text } => {
            let string = NSString::from_str(text);
            let _: () = unsafe { msg_send![view, insertText: &*string, replacementRange: none] };
            None
        }
        HookInput::Unmark => {
            let _: () = unsafe { msg_send![view, unmarkText] };
            None
        }
        HookInput::Access => Some(access_report(view)),
        _ => unreachable!("only text operations reach hook_text"),
    }
}

/// Reads a view's accessibility and input method state through its selectors. Input: the
/// view. Output: the report.
fn access_report(view: &TermView) -> AccessReport {
    let value: Option<Retained<NSString>> = unsafe { msg_send![view, accessibilityValue] };
    let role: Option<Retained<NSString>> = unsafe { msg_send![view, accessibilityRole] };
    let line: NSInteger = unsafe { msg_send![view, accessibilityInsertionPointLineNumber] };
    let selected: NSRange = unsafe { msg_send![view, accessibilitySelectedTextRange] };
    let marked: NSRange = unsafe { msg_send![view, markedRange] };
    let shown = marked.location != NSNotFound as usize;
    assert!(shown || marked.length == 0);
    AccessReport {
        input_context: view.inputContext().is_some(),
        role: role.map(|s| s.to_string()).unwrap_or_default(),
        value: value.map(|s| s.to_string()).unwrap_or_default(),
        insertion_line: line,
        selected: (selected.location, selected.length),
        marked: shown.then_some((marked.location, marked.length)),
    }
}

impl TermView {
    /// Routes one key event (ime::route): Command keys go nowhere, Control keys straight to
    /// the pane, Option keys as Meta, Shift+Enter as Ctrl+J, the rest through the input
    /// method. Input: the event.
    pub(super) fn key_event(&self, event: &NSEvent) {
        let flags = event.modifierFlags();
        let mods = Mods {
            command: flags.contains(NSEventModifierFlags::Command),
            control: flags.contains(NSEventModifierFlags::Control),
            option: flags.contains(NSEventModifierFlags::Option),
            shift: flags.contains(NSEventModifierFlags::Shift),
        };
        let chars = event
            .characters()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let bare = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let composing = self.ivars().ime.borrow().composing();
        let at = event.timestamp();
        match ime::route(mods, &bare, composing) {
            Route::Ignore => {}
            Route::Bypass => self.send(key_input(&chars, &bare), at),
            Route::Meta => self.send(ime::meta_input(&bare), at),
            Route::NewLine => self.send(Some(Input::Text(b"\n".to_vec())), at),
            Route::Interpret => {
                let key = KeyDown {
                    chars,
                    bare,
                    handled: false,
                };
                self.interpret(event, key, composing);
            }
        }
    }

    /// Hands one key to the input method; when it neither inserted nor marked text and no
    /// composition was or is shown, the key's own bytes go to the pane once (Return, Tab,
    /// Backspace, Escape, arrows arrive as commands, which send nothing themselves). Inputs:
    /// the event, its stashed characters and whether marked text was shown before it.
    fn interpret(&self, event: &NSEvent, key: KeyDown, composing: bool) {
        let outer = self.ivars().key.replace(Some(key));
        let events = NSArray::from_slice(&[event]);
        self.interpretKeyEvents(&events);
        let Some(key) = self.ivars().key.replace(outer) else {
            unreachable!("the key stays stashed until interpretKeyEvents returns");
        };
        let after = self.ivars().ime.borrow().composing();
        if !key.handled && !composing && !after {
            self.send(key_input(&key.chars, &key.bare), event.timestamp());
        }
    }

    /// Sends one input to the view's pane through the native host. Inputs: the input, if
    /// any, and the event's timestamp.
    fn send(&self, input: Option<Input>, at: f64) {
        if let Some(input) = input {
            super::on_native(self.ivars().id(), input, at);
        }
    }

    /// Marks the key being interpreted as taken by the input method.
    fn take_key(&self) {
        if let Some(key) = self.ivars().key.borrow_mut().as_mut() {
            key.handled = true;
        }
    }

    /// insertText: commits text (a key, an input method's conversion, the emoji picker),
    /// ending any composition. A text of function characters only leaves the key to its own
    /// bytes. Input: the NSString or NSAttributedString.
    pub(super) fn text_inserted(&self, string: &AnyObject) {
        let text = plain_text(string);
        let was = self.ivars().ime.borrow().composing();
        let inputs = self.ivars().ime.borrow_mut().commit(&text);
        assert!(!self.ivars().ime.borrow().composing());
        if !inputs.is_empty() || was {
            self.take_key();
        }
        if was {
            super::on_redraw(self.ivars().id());
        }
        for input in inputs {
            super::on_native(self.ivars().id(), input, 0.0);
        }
    }

    /// setMarkedText: shows what the input method composes, never sent until committed.
    /// Inputs: the text and its selected UTF-16 range.
    pub(super) fn marked_set(&self, string: &AnyObject, selected: NSRange) {
        let text = plain_text(string);
        let range = (selected.location, selected.length);
        let composing = self.ivars().ime.borrow_mut().set_marked(&text, range);
        assert_eq!(composing, self.ivars().ime.borrow().composing());
        self.take_key();
        super::on_redraw(self.ivars().id());
    }

    /// unmarkText: the input method ends the composition and keeps its text, so the marked
    /// text is committed to the pane.
    pub(super) fn marked_committed(&self) {
        let inputs = self.ivars().ime.borrow_mut().unmark();
        assert!(!self.ivars().ime.borrow().composing());
        if inputs.is_empty() {
            return;
        }
        super::on_redraw(self.ivars().id());
        for input in inputs {
            super::on_native(self.ivars().id(), input, 0.0);
        }
    }

    /// Drops any marked text with nothing sent (focus lost, hidden, closed, clicked): the
    /// state clears first, so an unmarkText the input context sends back commits nothing.
    pub fn cancel_marked(&self) {
        let was = self.ivars().ime.borrow_mut().cancel();
        assert!(!self.ivars().ime.borrow().composing());
        if !was {
            return;
        }
        if let Some(context) = self.inputContext() {
            context.discardMarkedText();
        }
        super::on_redraw(self.ivars().id());
    }

    /// The marked text the next frame draws. Output: the text, or none when not composing.
    pub fn marked_text(&self) -> Option<String> {
        let ime = self.ivars().ime.borrow();
        ime.composing().then(|| ime.marked().to_owned())
    }

    /// attributedSubstringForProposedRange:actualRange: over the marked text. Inputs: the
    /// range and where to write the range served (may be null). Output: the text, or none.
    pub(super) fn marked_substring(
        &self,
        range: NSRange,
        actual: *mut NSRange,
    ) -> Option<Retained<NSAttributedString>> {
        let found = self
            .ivars()
            .ime
            .borrow()
            .substring((range.location, range.length));
        let (text, (at, length)) = found?;
        if !actual.is_null() {
            unsafe { actual.write(NSRange::new(at, length)) };
        }
        let string = NSString::from_str(&text);
        Some(NSAttributedString::initWithString(
            NSAttributedString::alloc(),
            &string,
        ))
    }

    /// firstRectForCharacterRange:actualRange: in screen coordinates: the cell after the
    /// marked text before the range's start, from the terminal cursor, so the candidate
    /// window follows the composition. Inputs: the range and where to write the range
    /// served (may be null). Output: the rectangle, zero before the first frame.
    pub(super) fn marked_rect(&self, range: NSRange, actual: *mut NSRange) -> NSRect {
        if !actual.is_null() {
            unsafe { actual.write(range) };
        }
        let (Some(cursor), Some(geometry)) =
            (self.ivars().cursor.get(), self.ivars().geometry.get())
        else {
            return NSRect::ZERO;
        };
        let cells = self.ivars().ime.borrow().cells_before(range.location);
        self.screen_rect(geometry, cursor.0, cursor.1 + cells, 1)
    }

    /// A run of cells in screen coordinates. Inputs: the geometry, the row, the first column
    /// and the width in cells. Output: the rectangle, zero when the view has no window.
    fn screen_rect(&self, geometry: Geometry, row: usize, column: usize, width: usize) -> NSRect {
        let (x, y, w, h) = a11y::cell_rect(geometry, row, column, width);
        let local = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        let in_window = self.convertRect_toView(local, None);
        self.window()
            .map_or(NSRect::ZERO, |window| window.convertRectToScreen(in_window))
    }

    /// Takes what one drawn frame showed: the geometry and cursor the input method's rect
    /// needs, the accessibility text marked stale, and VoiceOver told when the rows or the
    /// cursor and selection changed. Inputs: the geometry, the cursor and selection, and a
    /// hash of the frame's rows.
    pub fn drawn(&self, geometry: Geometry, marks: Marks, rows_key: u64) {
        self.ivars().geometry.set(Some(geometry));
        self.ivars().cursor.set(marks.cursor);
        let (value, selection) = {
            let mut access = self.ivars().access.borrow_mut();
            access.stale = true;
            let value = access.rows_key != rows_key;
            let selection = access.marks != marks;
            access.rows_key = rows_key;
            access.marks = marks;
            (value, selection)
        };
        if value {
            self.post(unsafe { NSAccessibilityValueChangedNotification });
        }
        if selection {
            self.post(unsafe { NSAccessibilitySelectedTextChangedNotification });
        }
        let composing = self.ivars().ime.borrow().composing();
        if composing && let Some(context) = self.inputContext() {
            context.invalidateCharacterCoordinates();
        }
    }

    /// Posts one accessibility notification for this view. Input: the notification.
    fn post(&self, notification: &NSAccessibilityNotificationName) {
        let element: &AnyObject = self.as_ref();
        unsafe { NSAccessibilityPostNotification(element, notification) };
    }

    /// Reads the accessibility text, rebuilt from the pane's screen when a frame made it
    /// stale; while the native state is busy the last text serves. Input: the reader.
    /// Output: what it read.
    pub(super) fn with_text<T>(&self, read: impl FnOnce(&Text) -> T) -> T {
        let stale = self.ivars().access.borrow().stale;
        if stale && let Some(grid) = super::a11y_grid(self.ivars().id()) {
            let text = a11y::build(&grid);
            let mut access = self.ivars().access.borrow_mut();
            access.text = text;
            access.stale = false;
        }
        read(&self.ivars().access.borrow().text)
    }

    /// accessibilityFrameForRange: the first line's cells of a range in screen coordinates.
    /// Input: the range. Output: the rectangle, zero when it misses the text or before the
    /// first frame.
    pub(super) fn range_frame(&self, range: NSRange) -> NSRect {
        let Some(geometry) = self.ivars().geometry.get() else {
            return NSRect::ZERO;
        };
        let cells = self.with_text(|text| text.cells_for_range((range.location, range.length)));
        let Some((row, first, past)) = cells else {
            return NSRect::ZERO;
        };
        assert!(first < past);
        self.screen_rect(geometry, row, first, past - first)
    }
}
