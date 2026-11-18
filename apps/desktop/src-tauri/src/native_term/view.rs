use super::pane::{Input, Named};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSResponder, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::Cell;

/// The native view's own state: the id of the terminal view it draws.
pub struct Ivars {
    id: Cell<u64>,
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

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            if flags.contains(NSEventModifierFlags::Command) {
                return;
            }
            let chars = event.characters().map(|s| s.to_string()).unwrap_or_default();
            let bare = event
                .charactersIgnoringModifiers()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if let Some(input) = key_input(&chars, &bare) {
                super::on_native(self.ivars().id.get(), input, event.timestamp());
            }
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
        let this = Self::alloc(mtm).set_ivars(Ivars { id: Cell::new(id) });
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
