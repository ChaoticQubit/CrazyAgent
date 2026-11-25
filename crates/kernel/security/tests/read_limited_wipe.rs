use kernel_security::read_limited::read_limited;
use kernel_security::secret::wipe;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const KEYCHAIN_MARK: &[u8] = b"kc-residue-mark:";
const GH_MARK: &[u8] = b"gh-residue-mark:";
const LIMIT_MARK: &[u8] = b"lm-residue-mark:";
const MARKS: [&[u8]; 3] = [KEYCHAIN_MARK, GH_MARK, LIMIT_MARK];
const CHUNK: usize = 7;

static SERIAL: Mutex<()> = Mutex::new(());
static WATCHED: AtomicUsize = AtomicUsize::new(0);
static WATCHING: AtomicBool = AtomicBool::new(false);
static FREED_WITH_MARK: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, handing out zeroed blocks, which also counts the byte blocks
/// (align 1) freed while a row watches that still hold that row's mark, the old block of
/// every growth included. Blocks of other layouts can hold padding, so they are never read.
struct FreeScan;

unsafe impl GlobalAlloc for FreeScan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        if WATCHING.load(Ordering::SeqCst) && layout.align() == 1 {
            let mark = MARKS[WATCHED.load(Ordering::SeqCst)];
            if layout.size() >= mark.len() {
                let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
                if bytes.windows(mark.len()).any(|window| window == mark) {
                    FREED_WITH_MARK.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        unsafe { System.dealloc(block, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeScan = FreeScan;

/// A child's output as a pipe hands it out, a few bytes per read.
struct Chunks {
    bytes: Vec<u8>,
    at: usize,
}

impl Read for Chunks {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = CHUNK.min(buffer.len()).min(self.bytes.len() - self.at);
        buffer[..count].copy_from_slice(&self.bytes[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

/// A secret of `length` bytes that opens with the mark at `index` in `MARKS`. Inputs: the
/// mark's index and the length. Output: the bytes.
fn fixture(index: usize, length: usize) -> Vec<u8> {
    let mut bytes = MARKS[index].to_vec();
    bytes.extend((b'a'..=b'z').cycle().take(length - bytes.len()));
    assert_eq!(bytes.len(), length);
    bytes
}

/// Reads `secret` through [`read_limited`] with `limit` while the allocator watches for
/// freed blocks holding the mark at `index`, then wipes what was read. Inputs: the mark's
/// index, the secret and the limit. Output: the freed blocks with the mark, the read's
/// result, the bytes read and the buffer's capacity.
fn read_watched(index: usize, secret: Vec<u8>, limit: usize) -> (usize, usize, usize, usize) {
    let mut source = Chunks {
        bytes: secret,
        at: 0,
    };
    FREED_WITH_MARK.store(0, Ordering::SeqCst);
    WATCHED.store(index, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
    let (read, mut bytes) = read_limited(&mut source, limit);
    let shape = (
        read.expect("the fixture reader never fails"),
        bytes.len(),
        bytes.capacity(),
    );
    let same = source.bytes.starts_with(&bytes);
    wipe(&mut bytes);
    drop(bytes);
    WATCHING.store(false, Ordering::SeqCst);
    wipe(&mut source.bytes);
    assert!(same, "the read bytes are the secret's first bytes");
    (
        FREED_WITH_MARK.load(Ordering::SeqCst),
        shape.0,
        shape.1,
        shape.2,
    )
}

/// A keychain answer read as the macOS store reads `security`'s output frees no block that
/// still holds it, and lands in one buffer of the limit and one byte.
#[test]
fn a_keychain_answer_read_frees_no_copy_of_it() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (freed, read, length, capacity) = read_watched(0, fixture(0, 1500), 4096);
    assert_eq!((read, length), (1500, 1500));
    assert_eq!(freed, 0, "freed blocks still holding the answer");
    assert_eq!(capacity, 4097);
}

/// A token read as the GitHub sign-in reads `gh auth token` frees no block that still holds
/// it, and lands in one buffer of the limit and one byte.
#[test]
fn a_gh_token_read_frees_no_copy_of_it() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (freed, read, length, capacity) = read_watched(1, fixture(1, 300), 2048);
    assert_eq!((read, length), (300, 300));
    assert_eq!(freed, 0, "freed blocks still holding the token");
    assert_eq!(capacity, 2049);
}

/// Output longer than the limit stops one byte past it, so the caller can refuse it, and the
/// buffer never grows past the size reserved before the first byte came in.
#[test]
fn a_read_of_more_than_the_limit_never_grows_its_buffer() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (freed, read, length, capacity) = read_watched(2, fixture(2, 73), 64);
    assert_eq!((read, length), (65, 65));
    assert_eq!(freed, 0, "freed blocks still holding the output");
    assert_eq!(capacity, 65);
}
