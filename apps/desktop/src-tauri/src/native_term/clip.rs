use super::codec::COPY_BYTES_MAX;
use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

/// The name prefix of the private pasteboard an e2e build copies to and pastes from, so a
/// driven Cmd-C or Cmd-V never touches the owner's general pasteboard.
pub const PRIVATE_BOARD: &str = "com.crazyagent.native-terminal.e2e";

/// The pasteboard the views' Cmd-C, Cmd-V and copy calls use: the general one, or in a
/// `native-webdriver` build a private one named for this process.
pub struct Clip {
    board: Retained<NSPasteboard>,
}

impl Clip {
    /// Opens the views' pasteboard. Output: the pasteboard.
    pub fn new() -> Clip {
        assert!(PRIVATE_BOARD.starts_with("com.crazyagent."));
        #[cfg(feature = "native-webdriver")]
        let board = {
            let name = format!("{PRIVATE_BOARD}.{}", std::process::id());
            NSPasteboard::pasteboardWithName(&NSString::from_str(&name))
        };
        #[cfg(not(feature = "native-webdriver"))]
        let board = NSPasteboard::generalPasteboard();
        let clip = Clip { board };
        assert_eq!(clip.private(), cfg!(feature = "native-webdriver"));
        clip
    }

    /// Whether this is the private e2e pasteboard. Output: the answer.
    pub fn private(&self) -> bool {
        let name = self.board.name().to_string();
        name.starts_with(PRIVATE_BOARD)
    }

    /// Puts plain text on the pasteboard in place of what it held. Input: 1 to
    /// `COPY_BYTES_MAX` bytes of text. Output: whether the pasteboard took it.
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
