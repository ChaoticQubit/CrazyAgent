use super::a11y::{self, Geometry, Marks, Text};
use super::ime::{self, Took};
use super::keymap::{Press, key_input};
use super::view::{TermView, ViewEvent};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, DefinedClass};
use objc2_app_kit::{
    NSAccessibilityNotificationName, NSAccessibilityPostNotification,
    NSAccessibilitySelectedTextChangedNotification, NSAccessibilityValueChangedNotification,
    NSEvent, NSEventModifierFlags,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString,
};

/// One key event as the key map and the input method see it: its characters with and
/// without modifiers, the modifiers the key map reads, whether the input method took it
/// (inserted or marked text) and whether it passed a command back.
pub struct KeyDown {
    pub chars: String,
    pub bare: String,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
    pub handled: bool,
    pub command: bool,
}

impl KeyDown {
    /// Reads one key event, not yet taken. Input: the event. Output: the key.
    pub fn of(event: &NSEvent) -> KeyDown {
        let flags = event.modifierFlags();
        let text = |s: Option<Retained<NSString>>| s.map(|s| s.to_string()).unwrap_or_default();
        let key = KeyDown {
            chars: text(event.characters()),
            bare: text(event.charactersIgnoringModifiers()),
            shift: flags.contains(NSEventModifierFlags::Shift),
            option: flags.contains(NSEventModifierFlags::Option),
            control: flags.contains(NSEventModifierFlags::Control),
            handled: false,
            command: false,
        };
        assert!(!key.handled && !key.command);
        key
    }

    /// The key as the key map reads it. Output: the press.
    pub fn press(&self) -> Press<'_> {
        Press {
            chars: &self.chars,
            bare: &self.bare,
            shift: self.shift,
            option: self.option,
            control: self.control,
        }
    }
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

/// An attributed string with no attributes, as the input method and VoiceOver read text.
/// Input: the text. Output: the string, as many UTF-16 units long as the text.
pub fn attributed(text: &str) -> Retained<NSAttributedString> {
    let string = NSString::from_str(text);
    let attributed = NSAttributedString::initWithString(NSAttributedString::alloc(), &string);
    assert_eq!(string.length(), text.encode_utf16().count());
    assert_eq!(attributed.length(), string.length());
    attributed
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

impl TermView {
    /// Hands one key to the input method, then sends the key's own bytes once when
    /// ime::own_bytes says so (Return, Tab, Backspace, Escape and arrows arrive as commands,
    /// which send nothing themselves; a Korean syllable commits first, then the key goes on).
    /// Inputs: the event, its stashed characters and whether marked text was shown before it.
    pub(super) fn interpret(&self, event: &NSEvent, key: KeyDown, composing: bool) {
        let outer = self.ivars().key.replace(Some(key));
        let events = NSArray::from_slice(&[event]);
        self.interpretKeyEvents(&events);
        let Some(key) = self.ivars().key.replace(outer) else {
            unreachable!("the key stays stashed until interpretKeyEvents returns");
        };
        let took = Took {
            handled: key.handled,
            command: key.command,
            before: composing,
            after: self.ivars().ime.borrow().composing(),
        };
        if ime::own_bytes(took) {
            self.key_bytes(&key);
        }
    }

    /// Sends a key's own bytes from the key map to the host, if it has any. Input: the key.
    pub(super) fn key_bytes(&self, key: &KeyDown) {
        if let Some(input) = key_input(&key.press()) {
            self.send(ViewEvent::Key(input));
        }
    }

    /// doCommandBySelector: records that the input method passed the key back as a command;
    /// the command itself sends nothing.
    pub(super) fn command_passed(&self) {
        if let Some(key) = self.ivars().key.borrow_mut().as_mut() {
            key.command = true;
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
            self.send(ViewEvent::Marked);
        }
        for input in inputs {
            self.send(ViewEvent::Key(input));
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
        self.send(ViewEvent::Marked);
    }

    /// unmarkText: the input method ends the composition and keeps its text, so the marked
    /// text is committed to the pane.
    pub(super) fn marked_committed(&self) {
        let inputs = self.ivars().ime.borrow_mut().unmark();
        assert!(!self.ivars().ime.borrow().composing());
        if inputs.is_empty() {
            return;
        }
        self.send(ViewEvent::Marked);
        for input in inputs {
            self.send(ViewEvent::Key(input));
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
        self.send(ViewEvent::Marked);
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
        Some(attributed(&text))
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
        if stale && let Some(grid) = (self.ivars().screen)(self.id()) {
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
