use crazyagent::accounts::probe::request;
use kernel_security::secret::wipe;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const KEY_MARK: &[u8] = b"sk-residue-probe";
const HEADER_END: &[u8] = b"\r\n\r\n";

static WATCHING: AtomicBool = AtomicBool::new(false);
static FREED_WITH_KEY: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, which also counts the blocks freed while the row watches that
/// still hold the key's mark, the old block of every growth included.
struct FreeScan;

unsafe impl GlobalAlloc for FreeScan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        if WATCHING.load(Ordering::SeqCst) && layout.size() >= KEY_MARK.len() {
            let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
            if bytes
                .windows(KEY_MARK.len())
                .any(|window| window == KEY_MARK)
            {
                FREED_WITH_KEY.fetch_add(1, Ordering::SeqCst);
            }
        }
        unsafe { System.dealloc(block, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeScan = FreeScan;

/// A probe request with a bearer key of any length from 16 to 256 bytes frees no block
/// that still holds the key while it is written, and is written into one buffer of
/// exactly its size, which the wipe then clears whole.
#[test]
fn a_probe_request_frees_no_copy_of_its_key() {
    let keys: Vec<Vec<u8>> = (KEY_MARK.len()..=256)
        .map(|length| {
            let mut key = KEY_MARK.to_vec();
            key.extend((b'a'..=b'z').cycle().take(length - KEY_MARK.len()));
            key
        })
        .collect();
    let mut grown = Vec::new();
    FREED_WITH_KEY.store(0, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
    for key in &keys {
        let mut bytes = request("/v1/models", "127.0.0.1:9", Some(key));
        let shape = (
            bytes.len(),
            bytes.capacity(),
            bytes.ends_with(HEADER_END),
            bytes
                .windows(key.len())
                .any(|window| window == key.as_slice()),
        );
        wipe(&mut bytes);
        drop(bytes);
        if shape.0 != shape.1 || !shape.2 || !shape.3 {
            grown.push((key.len(), shape));
        }
    }
    WATCHING.store(false, Ordering::SeqCst);
    let freed = FREED_WITH_KEY.load(Ordering::SeqCst);
    for mut key in keys {
        wipe(&mut key);
    }
    assert_eq!(freed, 0, "freed blocks still holding the key");
    assert!(
        grown.is_empty(),
        "requests not of exactly their size: {grown:?}"
    );
}
