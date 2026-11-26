use crazyagent::tui::keys::{Key, PASTE_CHARS_MAX};
use crazyagent::tui::widgets::TextField;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const FIELD_CHARS: usize = 97;
const FIELD_BYTES: usize = FIELD_CHARS * 4;
const BACKSPACE_MARK: &[u8; 16] = b"bksp-mark-0001ZZ";
const DELETE_MARK: &[u8; 16] = b"dele-mark-0002YY";
const WORD_MARK: &[u8; 16] = b"word-mark-0003XX";
const PASTE_MARK: &[u8; 16] = b"past-mark-0004WW";
const TYPED: char = '\u{20ac}';
const TYPED_COUNT: usize = 16;

static SERIAL: Mutex<()> = Mutex::new(());
static WATCHING: AtomicBool = AtomicBool::new(false);
static MARK: AtomicUsize = AtomicUsize::new(0);
static MARK_FREED: AtomicUsize = AtomicUsize::new(0);
static WIDE_MARK_FREED: AtomicUsize = AtomicUsize::new(0);
static FIELD_DIRTY: AtomicUsize = AtomicUsize::new(0);
static TYPED_FREED: AtomicUsize = AtomicUsize::new(0);
static TYPED_RUN_FREED: AtomicUsize = AtomicUsize::new(0);

/// The row's mark, chosen by index so the allocator reads it without locking or
/// allocating. Output: the mark, or none while no row has set one.
fn mark() -> Option<&'static [u8; 16]> {
    match MARK.load(Ordering::SeqCst) {
        1 => Some(BACKSPACE_MARK),
        2 => Some(DELETE_MARK),
        3 => Some(WORD_MARK),
        4 => Some(PASTE_MARK),
        _ => None,
    }
}

/// Says whether the bytes hold the mark as `char`s, four bytes each, as a `Vec<char>`
/// copy of the text lays it out. Inputs: the bytes and the mark. Output: true on a hit.
fn holds_wide(bytes: &[u8], mark: &[u8; 16]) -> bool {
    let wide = mark.len() * 4;
    bytes.len() >= wide
        && (0..=bytes.len() - wide).any(|start| {
            mark.iter().enumerate().all(|(index, byte)| {
                let at = start + index * 4;
                bytes[at] == *byte && bytes[at + 1..at + 4] == [0, 0, 0]
            })
        })
}

/// The system allocator, handing out zeroed blocks, which also reads the byte blocks
/// (align 1) freed while a row watches for the row's mark, a field-sized block not all
/// zero and typed characters, and the `char` blocks (align 4) for the mark as `char`s.
/// Blocks of other layouts can hold padding, so they are never read.
struct FreeScan;

unsafe impl GlobalAlloc for FreeScan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        let wide = layout.align() == 4 && layout.size().is_multiple_of(4);
        if WATCHING.load(Ordering::SeqCst) && wide {
            let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
            if let Some(mark) = mark()
                && holds_wide(bytes, mark)
            {
                WIDE_MARK_FREED.fetch_add(1, Ordering::SeqCst);
            }
        }
        if WATCHING.load(Ordering::SeqCst) && layout.align() == 1 {
            let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
            if let Some(mark) = mark()
                && bytes.windows(mark.len()).any(|window| window == mark)
            {
                MARK_FREED.fetch_add(1, Ordering::SeqCst);
            }
            if bytes.len() == FIELD_BYTES && bytes.iter().any(|byte| *byte != 0) {
                FIELD_DIRTY.fetch_add(1, Ordering::SeqCst);
            }
            let mut typed = [0u8; 4];
            let typed = TYPED.encode_utf8(&mut typed).as_bytes();
            if bytes == typed {
                TYPED_FREED.fetch_add(1, Ordering::SeqCst);
            }
            if bytes.len() >= typed.len() * 2
                && bytes.windows(typed.len() * 2).any(|window| {
                    window[..typed.len()] == *typed && window[typed.len()..] == *typed
                })
            {
                TYPED_RUN_FREED.fetch_add(1, Ordering::SeqCst);
            }
        }
        unsafe { System.dealloc(block, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeScan = FreeScan;

/// What the allocator saw freed during one watched run: the mark as bytes, the mark as
/// `char`s, field-sized blocks not all zero, lone typed characters and typed runs.
#[derive(Debug, PartialEq, Eq)]
struct Freed {
    mark: usize,
    wide_mark: usize,
    field_dirty: usize,
    typed: usize,
    typed_run: usize,
}

/// Runs one masked field's life with the allocator watching, the row's lock held. Inputs:
/// the row's mark index, 0 for none, and the steps on a fresh field, which drop it.
/// Output: what was freed.
fn watched(mark: usize, steps: impl FnOnce(TextField)) -> Freed {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let field = TextField::secret("Key", FIELD_CHARS);
    for counter in [
        &MARK_FREED,
        &WIDE_MARK_FREED,
        &FIELD_DIRTY,
        &TYPED_FREED,
        &TYPED_RUN_FREED,
    ] {
        counter.store(0, Ordering::SeqCst);
    }
    MARK.store(mark, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
    steps(field);
    WATCHING.store(false, Ordering::SeqCst);
    MARK.store(0, Ordering::SeqCst);
    Freed {
        mark: MARK_FREED.load(Ordering::SeqCst),
        wide_mark: WIDE_MARK_FREED.load(Ordering::SeqCst),
        field_dirty: FIELD_DIRTY.load(Ordering::SeqCst),
        typed: TYPED_FREED.load(Ordering::SeqCst),
        typed_run: TYPED_RUN_FREED.load(Ordering::SeqCst),
    }
}

/// Presses one key the field must take. Inputs: the field and the key.
fn press(field: &mut TextField, key: Key) {
    assert!(field.key(&key), "the field refused {key:?}");
}

/// Types text one character key at a time. Inputs: the field and the text.
fn typed(field: &mut TextField, text: &str) {
    for character in text.chars() {
        press(field, Key::Char(character));
    }
}

const CLEAN: Freed = Freed {
    mark: 0,
    wide_mark: 0,
    field_dirty: 0,
    typed: 0,
    typed_run: 0,
};

/// Text typed and then backspaced off the end frees no block that still holds it once
/// the field is dropped, and the field's own block is all zeros when freed.
#[test]
fn a_masked_field_backspaced_then_dropped_frees_no_byte_of_the_removed_text() {
    let freed = watched(1, |mut field| {
        typed(&mut field, "p");
        typed(&mut field, std::str::from_utf8(BACKSPACE_MARK).unwrap());
        for _ in 0..BACKSPACE_MARK.len() {
            press(&mut field, Key::Backspace);
        }
        assert_eq!(field.len(), 1);
        drop(field);
    });
    assert_eq!(freed, CLEAN);
}

/// Text deleted from the front, which moves the rest left over it, leaves the field's
/// block all zeros when the dropped field frees it.
#[test]
fn a_masked_field_deleted_from_then_dropped_frees_no_byte_of_the_removed_text() {
    let freed = watched(2, |mut field| {
        typed(&mut field, std::str::from_utf8(DELETE_MARK).unwrap());
        press(&mut field, Key::Home);
        for _ in 0..DELETE_MARK.len() {
            press(&mut field, Key::Delete);
        }
        assert!(field.is_empty());
        drop(field);
    });
    assert_eq!(freed, CLEAN);
}

/// A word erased with Ctrl-W is freed in no block, neither as text nor as the characters
/// the erase reads, and the field's block is all zeros when the dropped field frees it.
#[test]
fn a_masked_field_word_erased_then_dropped_frees_no_copy_of_the_word() {
    let freed = watched(3, |mut field| {
        typed(&mut field, "p ");
        typed(&mut field, std::str::from_utf8(WORD_MARK).unwrap());
        press(&mut field, Key::Ctrl('w'));
        assert_eq!(field.len(), 2);
        drop(field);
    });
    assert_eq!(freed, CLEAN);
}

/// A paste into a masked field leaves no copy of the pasted text in a freed block; the
/// row's own paste key is dropped once the watch ends.
#[test]
fn a_masked_field_pasted_into_frees_no_copy_of_the_paste() {
    let paste = Key::Paste(std::str::from_utf8(PASTE_MARK).unwrap().to_owned());
    let freed = watched(4, |mut field| {
        assert!(field.key(&paste), "the field refused the paste");
        assert_eq!(field.len(), PASTE_MARK.len());
        drop(field);
    });
    let Key::Paste(text) = paste else {
        unreachable!("the row built a paste")
    };
    let mut bytes = text.into_bytes();
    kernel_security::secret::wipe(&mut bytes);
    assert_eq!(freed, CLEAN);
}

/// Typing into a masked field one key at a time frees no block holding a typed
/// character, and the field never grows, so no freed block holds two typed in a row.
#[test]
fn typing_into_a_masked_field_frees_no_copy_of_a_typed_character() {
    let freed = watched(0, |mut field| {
        for _ in 0..TYPED_COUNT {
            press(&mut field, Key::Char(TYPED));
        }
        assert_eq!(field.len(), TYPED_COUNT);
        drop(field);
    });
    assert_eq!(freed, CLEAN);
}

/// The text a terminal paste event carries, built as the terminal library builds it: one
/// `String` of exactly the pasted length. Input: the pasted bytes. Output: the text.
fn terminal_paste(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// A terminal paste made into a paste key and typed into a masked field leaves no copy of
/// the pasted text in a freed block once the key and the field are dropped.
#[test]
fn a_terminal_paste_made_into_a_key_frees_no_copy_of_the_paste() {
    let text = terminal_paste(PASTE_MARK);
    let freed = watched(4, |mut field| {
        let paste = Key::paste(text);
        assert!(field.key(&paste), "the field refused the paste");
        assert_eq!(field.len(), PASTE_MARK.len());
        drop(paste);
        drop(field);
    });
    assert_eq!(freed, CLEAN);
}

/// A paste longer than `PASTE_CHARS_MAX` characters keeps exactly that many, and the cut
/// tail is freed in no block once the paste key is dropped.
#[test]
fn a_paste_cut_at_the_limit_frees_no_copy_of_the_cut_tail() {
    let mut bytes = vec![b'x'; PASTE_CHARS_MAX];
    bytes.extend_from_slice(PASTE_MARK);
    let text = terminal_paste(&bytes);
    drop(bytes);
    let freed = watched(4, |field| {
        let paste = Key::paste(text);
        let Key::Paste(kept) = &paste else {
            unreachable!("a paste key")
        };
        assert_eq!(kept.chars().count(), PASTE_CHARS_MAX);
        drop(paste);
        drop(field);
    });
    assert_eq!(freed, CLEAN);
}
