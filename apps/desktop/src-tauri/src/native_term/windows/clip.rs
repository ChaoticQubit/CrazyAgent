use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, HWND_MESSAGE};

use super::super::codec::COPY_BYTES_MAX;

/// How often an open is tried while another app holds the clipboard.
const OPEN_TRIES: u32 = 10;

/// The wait between two tries of an open.
const OPEN_GAP: Duration = Duration::from_millis(10);

/// The most UTF-16 units a read scans for the terminator: a paste of `COPY_BYTES_MAX`
/// bytes never needs more, since every unit makes at least one UTF-8 byte.
const READ_UNITS_MAX: usize = COPY_BYTES_MAX;

/// The text a clipboard block holds: its units up to the first NUL, an unpaired surrogate
/// as U+FFFD, kept only at 1 to `COPY_BYTES_MAX` bytes; none when the units seen stop at
/// the scan cap with no NUL. Inputs: the units seen and whether the block runs past them.
/// Output: the text, or none.
fn block_text(units: &[u16], cut: bool) -> Option<String> {
    let _ = (units, cut);
    unimplemented!("H4 red")
}

/// The board the views' copy and paste use: the desktop clipboard, or one in process
/// memory for rows and driven builds, so a driven copy or paste never touches the owner's
/// clipboard. Desktop calls run on the thread that made the Clip; another app holding the
/// clipboard is waited for a bounded time, then refused.
pub struct Clip {
    owner: HWND,
}

impl Clip {
    /// The desktop clipboard, through a message-only window owned here. Output: the Clip;
    /// its calls refuse when Windows made no window.
    pub fn desktop() -> Clip {
        unimplemented!("H4 red")
    }

    /// An empty board in process memory. Output: it.
    pub fn private() -> Clip {
        unimplemented!("H4 red")
    }

    /// Whether the board is the in-process one. Output: the answer.
    pub fn is_private(&self) -> bool {
        let _ = self.owner;
        unimplemented!("H4 red")
    }

    /// Puts plain text on the board in place of what it held. Input: the text, refused
    /// when empty or over `COPY_BYTES_MAX` bytes. Output: whether the board took it.
    pub fn write(&self, text: &str) -> bool {
        let _ = text;
        unimplemented!("H4 red")
    }

    /// Reads the board's plain text, kept only at 1 to `COPY_BYTES_MAX` bytes. Output: the
    /// text, or none when the board holds none, holds too much or stays held by another app.
    pub fn read(&self) -> Option<String> {
        unimplemented!("H4 red")
    }
}

/// A message-only window to own the clipboard while a write empties and fills it, of the
/// system STATIC class so nothing is registered. Output: the window, or null.
fn message_window() -> HWND {
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    assert_eq!(class.last(), Some(&0), "a nul-terminated class name");
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            GetModuleHandleW(ptr::null()),
            ptr::null(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
        SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// Runs a call with the clipboard open for a window of this thread, then closes it.
    fn with_clipboard<R>(call: impl FnOnce() -> R) -> R {
        let owner = message_window();
        assert!(!owner.is_null(), "a row window");
        assert_ne!(
            unsafe { OpenClipboard(owner) },
            0,
            "the row opens the clipboard"
        );
        let out = call();
        assert_ne!(
            unsafe { CloseClipboard() },
            0,
            "the row closes the clipboard"
        );
        unsafe { DestroyWindow(owner) };
        out
    }

    /// Puts a raw block of exactly these units on the clipboard in a format, with no NUL
    /// added, as another app could.
    fn raw(format: u32, block: &[u16]) {
        with_clipboard(|| {
            assert_ne!(
                unsafe { EmptyClipboard() },
                0,
                "the row empties the clipboard"
            );
            let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, size_of_val(block)) };
            assert!(!memory.is_null(), "a raw block");
            let target = unsafe { GlobalLock(memory) }.cast::<u16>();
            assert!(
                !target.is_null() && target.is_aligned(),
                "a locked raw block"
            );
            unsafe { ptr::copy_nonoverlapping(block.as_ptr(), target, block.len()) };
            unsafe { GlobalUnlock(memory) };
            if unsafe { SetClipboardData(format, memory) }.is_null() {
                unsafe { GlobalFree(memory) };
                panic!("the clipboard took no raw block");
            }
        });
    }

    /// The size in units of the clipboard's text block now.
    fn text_block_units() -> usize {
        with_clipboard(|| {
            let block = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT)) };
            assert!(!block.is_null(), "a text block");
            let size = unsafe { GlobalSize(block) };
            size / size_of::<u16>()
        })
    }

    #[test]
    fn the_private_board_takes_one_to_cap_bytes_and_gives_them_back() {
        let clip = Clip::private();
        assert!(clip.is_private());
        assert_eq!(clip.read(), None);
        assert!(clip.write("one"));
        assert_eq!(clip.read().as_deref(), Some("one"));
        assert!(!clip.write(""));
        assert!(!clip.write(&"x".repeat(COPY_BYTES_MAX + 1)));
        assert_eq!(clip.read().as_deref(), Some("one"));
        assert!(clip.write(&"x".repeat(COPY_BYTES_MAX)));
        assert_eq!(clip.read().map(|text| text.len()), Some(COPY_BYTES_MAX));
    }

    #[test]
    fn a_block_reads_to_its_first_nul_and_never_past_the_cap() {
        assert_eq!(block_text(&units("ab\0c"), false).as_deref(), Some("ab"));
        assert_eq!(block_text(&units("abc"), false).as_deref(), Some("abc"));
        assert_eq!(block_text(&units("abc"), true), None);
        assert_eq!(block_text(&units("ab\0"), true).as_deref(), Some("ab"));
        assert_eq!(block_text(&units("\0ab"), false), None);
        assert_eq!(block_text(&[], false), None);
        let lone_high = [0xD800, u16::from(b'a'), 0];
        assert_eq!(block_text(&lone_high, false).as_deref(), Some("\u{FFFD}a"));
        let lone_low = [u16::from(b'a'), 0xDC00];
        assert_eq!(block_text(&lone_low, false).as_deref(), Some("a\u{FFFD}"));
        let most = vec![u16::from(b'x'); COPY_BYTES_MAX];
        assert_eq!(
            block_text(&most, false).map(|text| text.len()),
            Some(COPY_BYTES_MAX)
        );
        let euros = vec![0x20AC; COPY_BYTES_MAX / 3 + 1];
        assert_eq!(block_text(&euros, false), None);
    }

    fn round_trips(clip: &Clip) {
        for text in ["native-clip-1", "端末 コピー", "a 😊 pair", "one\r\ntwo"] {
            assert!(clip.write(text), "{text:?} written");
            assert_eq!(clip.read().as_deref(), Some(text));
        }
        assert!(!clip.write(""));
        assert!(!clip.write(&"x".repeat(COPY_BYTES_MAX + 1)));
        assert_eq!(clip.read().as_deref(), Some("one\r\ntwo"));
        println!("h4 row: 4 texts round trip, empty and oversized writes refused");
    }

    fn refuses_while_held(clip: &Clip) {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = thread::spawn(move || {
            let owner = message_window();
            assert!(!owner.is_null(), "a holder window");
            assert_ne!(unsafe { OpenClipboard(owner) }, 0, "the holder opens");
            held_tx.send(()).expect("the row waits for the hold");
            release_rx.recv().expect("the row releases the hold");
            unsafe { CloseClipboard() };
            unsafe { DestroyWindow(owner) };
        });
        held_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the holder holds");
        let started = Instant::now();
        assert!(!clip.write("while-held"));
        let write_took = started.elapsed();
        let started = Instant::now();
        assert_eq!(clip.read(), None);
        let read_took = started.elapsed();
        release_tx.send(()).expect("the holder waits");
        holder.join().expect("the holder ends");
        let tried = OPEN_GAP * (OPEN_TRIES - 1);
        let bound = OPEN_GAP * OPEN_TRIES * 5;
        assert!(
            write_took >= tried && write_took < bound,
            "write {write_took:?}"
        );
        assert!(
            read_took >= tried && read_took < bound,
            "read {read_took:?}"
        );
        assert!(clip.write("after-held"));
        assert_eq!(clip.read().as_deref(), Some("after-held"));
        println!(
            "h4 row: a held clipboard refused the write in {write_took:?} and the read in \
             {read_took:?}, then both worked"
        );
    }

    fn bounds_raw_blocks(clip: &Clip) {
        let text = u32::from(CF_UNICODETEXT);
        raw(text, &units("abc"));
        let held = text_block_units();
        let read = clip.read().expect("a block with no NUL still reads");
        assert!(read.starts_with("abc"), "{read:?}");
        assert!(
            read.encode_utf16().count() <= held,
            "{read:?} within {held} units"
        );
        raw(text, &vec![u16::from(b'x'); READ_UNITS_MAX + 8]);
        assert_eq!(clip.read(), None);
        let mut euros = vec![0x20AC; COPY_BYTES_MAX / 3 + 1];
        euros.push(0);
        raw(text, &euros);
        assert_eq!(clip.read(), None);
        raw(text, &[0xD800, u16::from(b'a'), 0]);
        assert_eq!(clip.read().as_deref(), Some("\u{FFFD}a"));
        println!(
            "h4 row: a block with no NUL read as a prefix within its {held} units, \
             over-cap blocks refused, a lone surrogate read as U+FFFD"
        );
    }

    fn reads_none_without_text(clip: &Clip) {
        with_clipboard(|| assert_ne!(unsafe { EmptyClipboard() }, 0, "emptied"));
        assert_eq!(clip.read(), None);
        let name = units("CrazyAgent.H4.Row\0");
        let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        assert_ne!(format, 0, "a row format");
        raw(format, &[1, 2, 3]);
        assert_eq!(clip.read(), None);
        println!("h4 row: an empty clipboard and a custom format alone read none");
    }

    #[test]
    #[ignore = "needs a Windows desktop: the farm runs it on windows-2022 and windows-2025"]
    fn the_desktop_clipboard_round_trips_refuses_while_held_and_bounds_every_block() {
        let clip = Clip::desktop();
        assert!(!clip.is_private());
        round_trips(&clip);
        refuses_while_held(&clip);
        bounds_raw_blocks(&clip);
        reads_none_without_text(&clip);
    }
}
