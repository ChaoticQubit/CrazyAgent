use super::a11y::{Geometry, Grid};
use super::ime::{self, Ime, Mods, Route};
use super::pane::Input;
use super::view_text::{Access, KeyDown, attributed, range_or_none};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibilityTextAreaRole, NSEvent, NSEventModifierFlags, NSResponder, NSTextInputClient,
    NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSInteger, NSNotFound, NSPoint, NSRange,
    NSRect, NSSize, NSString, NSUInteger,
};
use std::cell::{Cell, RefCell};

/// The most clicks a press reports: 1 places the cursor, 2 a word, 3 a line.
const CLICKS_MAX: u8 = 3;

/// Which mouse event a view got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Press,
    Drag,
    Release,
}

/// What a native view tells its host, on the main thread, in the order AppKit delivered
/// it. Points are in the view's own flipped coordinates, in points. `Marked` asks for a
/// redraw: the input method's marked text changed (read it with `marked_text`).
#[derive(Debug, PartialEq)]
pub enum ViewEvent {
    Key(Input),
    Wheel {
        point: (f64, f64),
        delta: f64,
        precise: bool,
    },
    Mouse {
        kind: MouseKind,
        point: (f64, f64),
        option: bool,
        clicks: u8,
    },
    Menu {
        point: (f64, f64),
    },
    Copy,
    Paste,
    SelectAll,
    Focus(bool),
    Marked,
}

/// The host function a view hands its events to, with the view's id.
pub type Sink = fn(u64, ViewEvent);

/// The longest label VoiceOver says for a view, in bytes.
pub const LABEL_BYTES_MAX: usize = 256;

/// The host function VoiceOver's text is read from, with the view's id: the visible grid,
/// or none while the screen is busy, when the last text read serves.
pub type Screen = fn(u64) -> Option<Grid>;

/// The native view's own state: the id of the terminal view it draws, its host functions,
/// its marked text, the key `interpretKeyEvents` works on, the cursor cell and geometry of
/// the last frame, its label and what VoiceOver reads; nothing a getter needs waits on the
/// host.
pub struct Ivars {
    id: Cell<u64>,
    sink: Sink,
    label: RefCell<String>,
    pub(super) screen: Screen,
    pub(super) ime: RefCell<Ime>,
    pub(super) key: RefCell<Option<KeyDown>>,
    pub(super) cursor: Cell<Option<(usize, usize)>>,
    pub(super) geometry: Cell<Option<Geometry>>,
    pub(super) access: RefCell<Access>,
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

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(
            &self,
            range: NSRange,
            actual: *mut NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            self.marked_substring(range, actual)
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
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

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            self.focus(unsafe { msg_send![super(self), becomeFirstResponder] }, true)
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.cancel_marked();
            self.focus(unsafe { msg_send![super(self), resignFirstResponder] }, false)
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.key(event);
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSString>> {
            Some(unsafe { NSAccessibilityTextAreaRole }.retain())
        }

        #[unsafe(method_id(accessibilityLabel))]
        fn accessibility_label(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().label.borrow()))
        }

        #[unsafe(method_id(accessibilityValue))]
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

        #[unsafe(method_id(accessibilitySelectedText))]
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

        #[unsafe(method_id(accessibilityStringForRange:))]
        fn accessibility_string_for_range(&self, range: NSRange) -> Option<Retained<NSString>> {
            let range = (range.location, range.length);
            let found = self.with_text(|text| text.string_for_range(range));
            found.map(|(string, _)| NSString::from_str(&string))
        }

        #[unsafe(method_id(accessibilityAttributedStringForRange:))]
        fn accessibility_attributed_string_for_range(
            &self,
            range: NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            let range = (range.location, range.length);
            let found = self.with_text(|text| text.string_for_range(range));
            found.map(|(string, _)| attributed(&string))
        }

        #[unsafe(method(accessibilityFrameForRange:))]
        fn accessibility_frame_for_range(&self, range: NSRange) -> NSRect {
            self.range_frame(range)
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            self.key_equivalent(event)
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.send(ViewEvent::Copy);
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            self.send(ViewEvent::Paste);
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            self.send(ViewEvent::SelectAll);
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

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let point = self.point(event);
            self.send(ViewEvent::Menu { point });
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let point = self.point(event);
            let delta = event.scrollingDeltaY();
            let precise = event.hasPreciseScrollingDeltas();
            self.send(ViewEvent::Wheel { point, delta, precise });
        }
    }
);

impl TermView {
    /// Makes a native view for one terminal view, 1 x 1 until placed. Inputs: the main
    /// thread marker, the terminal view id, the host function its events go to and the one
    /// VoiceOver's text is read from. Output: the view.
    pub fn new(mtm: MainThreadMarker, id: u64, sink: Sink, screen: Screen) -> Retained<TermView> {
        assert!(id > 0);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            id: Cell::new(id),
            sink,
            label: RefCell::new(String::from("Terminal")),
            screen,
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

    /// The terminal view id this native view draws.
    pub fn id(&self) -> u64 {
        self.ivars().id.get()
    }

    /// Sets what VoiceOver says the view is, the page's label for its agent; an empty one
    /// keeps "Terminal". Input: the checked label.
    pub fn set_label(&self, label: &str) {
        assert!(label.len() <= LABEL_BYTES_MAX);
        if label.trim().is_empty() {
            return;
        }
        *self.ivars().label.borrow_mut() = label.to_owned();
        assert_eq!(*self.ivars().label.borrow(), label);
    }

    pub(super) fn send(&self, event: ViewEvent) {
        let id = self.ivars().id.get();
        assert!(id > 0);
        (self.ivars().sink)(id, event);
    }

    fn focus(&self, granted: bool, focused: bool) -> bool {
        if granted {
            self.send(ViewEvent::Focus(focused));
        }
        granted
    }

    fn point(&self, event: &NSEvent) -> (f64, f64) {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        assert!(point.x.is_finite() && point.y.is_finite());
        (point.x, point.y)
    }

    /// Routes one key event (ime::route): Command keys go to the key equivalents, Control
    /// and Option keys straight to the key map, the rest through the input method, whose
    /// passed-back commands send the key map's bytes. Input: the event.
    fn key(&self, event: &NSEvent) {
        let flags = event.modifierFlags();
        let mods = Mods {
            command: flags.contains(NSEventModifierFlags::Command),
            control: flags.contains(NSEventModifierFlags::Control),
            option: flags.contains(NSEventModifierFlags::Option),
        };
        let composing = self.ivars().ime.borrow().composing();
        match ime::route(mods, composing) {
            Route::Ignore => {}
            Route::Direct => self.key_bytes(&KeyDown::of(event)),
            Route::Interpret => self.interpret(event, KeyDown::of(event), composing),
        }
    }

    /// Takes Cmd-C, Cmd-V and Cmd-A while this view has keyboard focus. It lives outside
    /// `define_class!`, whose method bodies return the raw `Bool`, so they cannot return a
    /// `bool` early. Input: the key event. Output: whether the view took the key.
    fn key_equivalent(&self, event: &NSEvent) -> bool {
        let mine = self
            .window()
            .and_then(|w| w.firstResponder())
            .is_some_and(|r| {
                let me: &NSResponder = self;
                std::ptr::eq(&*r, me)
            });
        let flags = event.modifierFlags();
        let other = NSEventModifierFlags::Control | NSEventModifierFlags::Option;
        if !mine || !flags.contains(NSEventModifierFlags::Command) || flags.intersects(other) {
            return false;
        }
        let bare = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let taken = match bare.as_str() {
            "c" => ViewEvent::Copy,
            "v" => ViewEvent::Paste,
            "a" => ViewEvent::SelectAll,
            _ => return false,
        };
        assert!(matches!(bare.as_str(), "c" | "v" | "a"));
        self.send(taken);
        true
    }

    fn mouse(&self, event: &NSEvent, kind: MouseKind) {
        let point = self.point(event);
        let option = event.modifierFlags().contains(NSEventModifierFlags::Option);
        let clicks = u8::try_from(event.clickCount().clamp(1, CLICKS_MAX.into())).unwrap_or(1);
        assert!((1..=CLICKS_MAX).contains(&clicks));
        self.send(ViewEvent::Mouse {
            kind,
            point,
            option,
            clicks,
        });
    }
}
