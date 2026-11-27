mod freed_bytes_support;

use crazyagent::plugins::SecretValue;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

const SECRET: &[u8] = b"mcp-secret-value-fixture-5521";
const UNSEEN: u8 = 0;
const WIPED: u8 = 1;
const KEPT: u8 = 2;

static WATCHED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicU8 = AtomicU8::new(UNSEEN);

/// The system allocator, handing out zeroed blocks, which also records whether the one
/// watched block held only zeros when it was freed. Zeroed blocks leave no byte of it
/// unwritten, capacity past what was asked for included.
struct FreeWatch;

unsafe impl GlobalAlloc for FreeWatch {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc_zeroed(layout) }
    }

    /// Checks a freed block through a [`freed_bytes_support::Copied`] copy, so the scan reads
    /// no uninitialized byte, then frees it. Inputs: the block and its layout. Safety: the
    /// block is valid for reads of the layout's size, as `GlobalAlloc::dealloc` guarantees.
    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        let address = block as usize;
        if address != 0
            && WATCHED
                .compare_exchange(address, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            let copied = unsafe { freed_bytes_support::Copied::new(block, layout.size()) };
            let bytes = copied.bytes();
            let seen = if bytes.iter().all(|byte| *byte == 0) {
                WIPED
            } else {
                KEPT
            };
            FREED.store(seen, Ordering::SeqCst);
        }
        unsafe { System.dealloc(block, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeWatch = FreeWatch;

/// SECRET-ZERO-VOLATILE: a dropped `SecretValue` leaves only zeros in the block it frees,
/// so an MCP server secret does not outlive its launch in freed memory.
#[test]
fn a_dropped_secret_value_frees_only_zeros() {
    let bytes = SECRET.to_vec();
    assert_eq!(bytes.capacity(), bytes.len());
    WATCHED.store(bytes.as_ptr() as usize, Ordering::SeqCst);
    let secret = SecretValue::new(bytes);
    assert_eq!(secret.expose(), SECRET);
    drop(secret);
    assert_eq!(FREED.load(Ordering::SeqCst), WIPED);
}
