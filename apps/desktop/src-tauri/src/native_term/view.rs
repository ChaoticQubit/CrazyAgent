use super::a11y::Geometry;
use super::ime::Ime;
use super::pane::{Input, Named};
use super::view_text::{Access, KeyDown, range_or_none};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2::{define_class, msg_send};
use objc2_app_kit::{
    NSAccessibilityTextAreaRole, NSEvent, NSEventModifierFlags, NSResponder, NSTextInputClient,
    NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSInteger, NSNotFound, NSPoint, NSRange,
    NSRect, NSSize, NSString, NSUInteger,
};
use std::cell::{Cell, RefCell};

/// The native view's own state: the id of the terminal view it draws, its marked text, the
/// key `interpretKeyEvents` works on, the cursor cell and geometry of the last frame, and
/// what VoiceOver reads; nothing a getter needs waits on the native state.
pub struct Ivars {
    id: Cell<u64>,
    pub(super) ime: RefCell<Ime>,
    pub(super) key: RefCell<Option<KeyDown>>,
    pub(super) cursor: Cell<Option<(usize, usize)>>,
    pub(super) geometry: Cell<Option<Geometry>>,
    pub(super) access: RefCell<Access>,
}

impl Ivars {
    /// The id of the terminal view this native view draws. Output: the id, never 0.
    pub(super) fn id(&self) -> u64 {
        let id = self.id.get();
        assert!(id > 0);
        id
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CrazyAgentNativeTerminalView"]
    #[ivars = Ivars]
    pub struct TermView;

    unsafe impl NSObjectProtocol for TermView {}

    unsafe impl NSTextInputClient for TermView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, _replacement: NSRange) {
            self.text_inserted(string);
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command(&self, _selector: Sel) {
            self.command_passed();
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, string: &AnyObject, selected: NSRange, _replacement: NSRange) {
            self.marked_set(string, selected);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.marked_committed();
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            range_or_none(self.ivars().ime.borrow().selected_range())
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            range_or_none(self.ivars().ime.borrow().marked_range())
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            self.ivars().ime.borrow().composing()
        }

        #[unsafe(method(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(
            &self,
            range: NSRange,
            actual: *mut NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            self.marked_substring(range, actual)
        }

        #[unsafe(method(validAttributesForMarkedText))]
        fn valid_attributes(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect(&self, range: NSRange, actual: *mut NSRange) -> NSRect {
            self.marked_rect(range, actual)
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }
    }

    impl TermView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.cancel_marked();
            unsafe { msg_send![super(self), resignFirstResponder] }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.key_event(event);
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSString>> {
            Some(unsafe { NSAccessibilityTextAreaRole }.retain())
        }

        #[unsafe(method(accessibilityLabel))]
        fn accessibility_label(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str("Terminal"))
        }

        #[unsafe(method(accessibilityValue))]
        fn accessibility_value(&self) -> Option<Retained<NSString>> {
            Some(self.with_text(|text| NSString::from_str(&text.string)))
        }

        #[unsafe(method(accessibilityNumberOfCharacters))]
        fn accessibility_characters(&self) -> NSInteger {
            self.with_text(|text| text.units() as NSInteger)
        }

        #[unsafe(method(accessibilityVisibleCharacterRange))]
        fn accessibility_visible_range(&self) -> NSRange {
            self.with_text(|text| NSRange::new(0, text.units()))
        }

        #[unsafe(method(accessibilityInsertionPointLineNumber))]
        fn accessibility_insertion_line(&self) -> NSInteger {
            self.with_text(|text| text.line_for_index(text.cursor) as NSInteger)
        }

        #[unsafe(method(accessibilitySelectedTextRange))]
        fn accessibility_selected_range(&self) -> NSRange {
            self.with_text(|text| {
                let (at, length) = text.selection.unwrap_or((text.cursor, 0));
                NSRange::new(at, length)
            })
        }

        #[unsafe(method(accessibilitySelectedText))]
        fn accessibility_selected_text(&self) -> Option<Retained<NSString>> {
            let selected = self.with_text(|text| {
                let range = text.selection?;
                text.string_for_range(range).map(|found| found.0)
            });
            Some(NSString::from_str(&selected.unwrap_or_default()))
        }

        #[unsafe(method(accessibilityLineForIndex:))]
        fn accessibility_line_for_index(&self, index: NSInteger) -> NSInteger {
            let index = usize::try_from(index).unwrap_or(0);
            self.with_text(|text| text.line_for_index(index) as NSInteger)
        }

        #[unsafe(method(accessibilityRangeForLine:))]
        fn accessibility_range_for_line(&self, line: NSInteger) -> NSRange {
            let line = usize::try_from(line).ok();
            range_or_none(self.with_text(|text| line.and_then(|line| text.range_for_line(line))))
        }

        #[unsafe(method(accessibilityRangeForIndex:))]
        fn accessibility_range_for_index(&self, index: NSInteger) -> NSRange {
            let index = usize::try_from(index).ok();
            let found = self.with_text(|text| {
                index.and_then(|index| text.string_for_range((index, 1)).map(|found| found.1))
            });
            range_or_none(found)
        }

        #[unsafe(method(accessibilityStringForRange:))]
        fn accessibility_string_for_range(&self, range: NSRange) -> Option<Retained<NSString>> {
            let range = (range.location, range.length);
            let found = self.with_text(|text| text.string_for_range(range));
            found.map(|(string, _)| NSString::from_str(&string))
        }

        #[unsafe(method(accessibilityAttributedStringForRange:))]
        fn accessibility_attributed_string_for_range(
            &self,
            range: NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            let range = (range.location, range.length);
            let found = self.with_text(|text| text.string_for_range(range));
            let string = NSString::from_str(&found?.0);
            Some(NSAttributedString::initWithString(NSAttributedString::alloc(), &string))
        }

        #[unsafe(method(accessibilityFrameForRange:))]
        fn accessibility_frame_for_range(&self, range: NSRange) -> NSRect {
            self.range_frame(range)
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let mine = self.window().and_then(|w| w.firstResponder()).is_some_and(|r| {
                let me: &NSResponder = self;
                std::ptr::eq(&*r, me)
            });
            let flags = event.modifierFlags();
            if !mine || !flags.contains(NSEventModifierFlags::Command) {
                return false;
            }
            let bare = event
                .charactersIgnoringModifiers()
                .map(|s| s.to_string())
                .unwrap_or_default();
            match bare.as_str() {
                "c" => super::on_copy(self.ivars().id.get(), event.timestamp()),
                "v" => super::on_paste(self.ivars().id.get(), event.timestamp()),
                _ => return false,
            }
            true
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            super::on_copy(self.ivars().id.get(), 0.0);
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            super::on_paste(self.ivars().id.get(), 0.0);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.cancel_marked();
            if let Some(window) = self.window() {
                let responder: &NSResponder = self;
                window.makeFirstResponder(Some(responder));
            }
            self.mouse(event, MouseKind::Press);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Drag);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.mouse(event, MouseKind::Release);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let delta = event.scrollingDeltaY();
            let precise = event.hasPreciseScrollingDeltas();
            let id = self.ivars().id.get();
            super::on_wheel(id, (point.x, point.y), delta, precise, event.timestamp());
        }
    }
);

/// Which mouse event a view got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Press,
    Drag,
    Release,
}

impl TermView {
    /// Makes a native view for one terminal view, 1 x 1 until placed. Inputs: the main
    /// thread marker and the terminal view id. Output: the view.
    pub fn new(mtm: MainThreadMarker, id: u64) -> Retained<TermView> {
        assert!(id > 0);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            id: Cell::new(id),
            ime: RefCell::new(Ime::default()),
            key: RefCell::new(None),
            cursor: Cell::new(None),
            geometry: Cell::new(None),
            access: RefCell::new(Access::default()),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
        let view: Retained<TermView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        assert_eq!(view.ivars().id.get(), id);
        view
    }

    fn mouse(&self, event: &NSEvent, kind: MouseKind) {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let option = event.modifierFlags().contains(NSEventModifierFlags::Option);
        super::on_mouse(
            self.ivars().id.get(),
            kind,
            (point.x, point.y),
            option,
            event.timestamp(),
        );
    }
}

/// The input one key event makes, from its characters with and without modifiers: the
/// function keys AppKit reports in the private-use range become named keys, Shift-Tab the
/// back tab, any other text its UTF-8 bytes. Inputs: the characters and the characters
/// ignoring modifiers. Output: the input, or none for keys that send nothing.
pub fn key_input(chars: &str, bare: &str) -> Option<Input> {
    let named = match bare.chars().next() {
        Some('\u{F700}') => Some(Named::Up),
        Some('\u{F701}') => Some(Named::Down),
        Some('\u{F702}') => Some(Named::Left),
        Some('\u{F703}') => Some(Named::Right),
        Some('\u{F728}') => Some(Named::Delete),
        Some('\u{F729}') => Some(Named::Home),
        Some('\u{F72B}') => Some(Named::End),
        Some('\u{F72C}') => Some(Named::PageUp),
        Some('\u{F72D}') => Some(Named::PageDown),
        _ => None,
    };
    if let Some(key) = named {
        return Some(Input::Key(key));
    }
    if chars == "\u{19}" {
        return Some(Input::Key(Named::BackTab));
    }
    if bare == super::ime::ENTER_KEYPAD {
        return Some(Input::Text(b"\r".to_vec()));
    }
    let private = chars
        .chars()
        .any(|c| ('\u{F700}'..='\u{F8FF}').contains(&c));
    if chars.is_empty() || private {
        return None;
    }
    assert!(!chars.is_empty() && !private);
    Some(Input::Text(chars.as_bytes().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_inputs() {
        assert!(matches!(
            key_input("\u{F700}", "\u{F700}"),
            Some(Input::Key(Named::Up))
        ));
        assert!(matches!(
            key_input("\u{19}", "\t"),
            Some(Input::Key(Named::BackTab))
        ));
        assert!(matches!(key_input("q", "q"), Some(Input::Text(b)) if b == b"q"));
        assert!(matches!(key_input("\u{3}", "c"), Some(Input::Text(b)) if b == b"\x03"));
        assert!(matches!(key_input("\r", "\r"), Some(Input::Text(b)) if b == b"\r"));
        assert!(key_input("\u{F704}", "\u{F704}").is_none());
        assert!(key_input("", "").is_none());
    }

    #[test]
    fn keypad_enter_is_return_and_control_c_stays_control_c() {
        assert!(matches!(key_input("\u{3}", "\u{3}"), Some(Input::Text(b)) if b == b"\r"));
        assert!(matches!(key_input("\u{3}", "c"), Some(Input::Text(b)) if b == b"\x03"));
        assert!(matches!(key_input("\r", "\r"), Some(Input::Text(b)) if b == b"\r"));
    }
}
