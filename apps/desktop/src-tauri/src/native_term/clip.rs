use super::codec::COPY_BYTES_MAX;
use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

/// The name of the private pasteboard the test hook copies to and pastes from, so a timed
/// copy or paste never touches the owner's general pasteboard.
pub const PROBE_BOARD: &str = "com.crazyagent.native-terminal.probe";

/// One Mac pasteboard: the general one the owner's Cmd-C and Cmd-V use, or the private
/// probe one.
pub struct Clip {
    board: Retained<NSPasteboard>,
}

impl Clip {
    /// Opens the general pasteboard, or the private probe one. Input: whether it is the probe
    /// one. Output: the pasteboard.
    pub fn new(probe: bool) -> Clip {
        let board = if probe {
            NSPasteboard::pasteboardWithName(&NSString::from_str(PROBE_BOARD))
        } else {
            NSPasteboard::generalPasteboard()
        };
        assert!(!PROBE_BOARD.is_empty());
        assert!(PROBE_BOARD.starts_with("com.crazyagent."));
        Clip { board }
    }

    /// Puts plain text on the pasteboard in place of what it held. Input: the text, at most
    /// `COPY_BYTES_MAX` bytes. Output: whether the pasteboard took it.
    pub fn write(&self, text: &str) -> bool {
        assert!(text.len() <= COPY_BYTES_MAX, "a copy is bounded");
        assert!(!text.is_empty(), "an empty selection is never copied");
        self.board.clearContents();
        unsafe {
            self.board
                .setString_forType(&NSString::from_str(text), NSPasteboardTypeString)
        }
    }

    /// Reads the pasteboard's plain text. Output: the text, or none when it holds none.
    pub fn read(&self) -> Option<String> {
        let text = unsafe { self.board.stringForType(NSPasteboardTypeString) }?;
        Some(text.to_string())
    }
}
