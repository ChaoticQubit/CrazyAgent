use super::keymap::{Press, key_input};
use super::pane::Input;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSResponder, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::Cell;

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
/// it. Points are in the view's own flipped coordinates, in points.
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
}

/// The host function a view hands its events to, with the view's id.
pub type Sink = fn(u64, ViewEvent);

/// The native view's own state: the id of the terminal view it draws and its host.
pub struct Ivars {
    id: Cell<u64>,
    sink: Sink,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "CrazyAgentNativeTerminalView"]
    #[ivars = Ivars]
    pub struct TermView;

    unsafe impl NSObjectProtocol for TermView {}

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
            self.focus(unsafe { msg_send![super(self), resignFirstResponder] }, false)
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.key(event);
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
    /// thread marker, the terminal view id and the host function its events go to. Output:
    /// the view.
    pub fn new(mtm: MainThreadMarker, id: u64, sink: Sink) -> Retained<TermView> {
        assert!(id > 0);
        let this = Self::alloc(mtm).set_ivars(Ivars {
            id: Cell::new(id),
            sink,
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

    fn send(&self, event: ViewEvent) {
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

    fn key(&self, event: &NSEvent) {
        let flags = event.modifierFlags();
        if flags.contains(NSEventModifierFlags::Command) {
            return;
        }
        let chars = event
            .characters()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let bare = event
            .charactersIgnoringModifiers()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let press = Press {
            chars: &chars,
            bare: &bare,
            shift: flags.contains(NSEventModifierFlags::Shift),
            option: flags.contains(NSEventModifierFlags::Option),
            control: flags.contains(NSEventModifierFlags::Control),
        };
        if let Some(input) = key_input(&press) {
            self.send(ViewEvent::Key(input));
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
