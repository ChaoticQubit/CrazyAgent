use crazyagent::engine::owner::AccountSecret;
use crazyagent::harnesses::launch::Secret;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

const UNSEEN: u8 = 0;
const WIPED: u8 = 1;
const KEPT: u8 = 2;

static WATCHED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicU8 = AtomicU8::new(UNSEEN);

/// The system allocator, which also records whether the one watched block held only zeros
/// when it was freed.
struct FreeWatch;

unsafe impl GlobalAlloc for FreeWatch {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        let address = block as usize;
        if address != 0
            && WATCHED
                .compare_exchange(address, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
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

/// Watches the block holding `bytes`, which must fill it, hands the bytes to `make`, and
/// reports what the block held when it was freed. Inputs: the bytes and what consumes them.
/// Output: `WIPED` or `KEPT`, `UNSEEN` when the block was not freed.
fn freed_after(bytes: Vec<u8>, make: impl FnOnce(Vec<u8>)) -> u8 {
    assert_eq!(bytes.capacity(), bytes.len());
    FREED.store(UNSEEN, Ordering::SeqCst);
    WATCHED.store(bytes.as_ptr() as usize, Ordering::SeqCst);
    make(bytes);
    WATCHED.store(0, Ordering::SeqCst);
    FREED.load(Ordering::SeqCst)
}

/// One row of [`secret_types_free_only_zeros`]: the secret's bytes and the call that
/// makes it and lets it go.
type Case<'a> = (&'a [u8], &'a dyn Fn(Vec<u8>));

/// SECRET-DROP: an account secret and a launch's API key each leave only zeros in the block
/// they free, whether they were dropped after use or refused when made.
#[test]
fn secret_types_free_only_zeros() {
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).unwrap();
    let cases: [Case<'_>; 4] = [
        (b"account-secret-fixture-4417", &|bytes| {
            drop(AccountSecret::new(bytes).unwrap())
        }),
        (b"account-secret\nfixture", &|bytes| {
            assert!(AccountSecret::new(bytes).is_err())
        }),
        (b"sk-launch-secret-fixture-9902", &|bytes| {
            drop(Secret::new(text(bytes)).unwrap())
        }),
        (b"sk-launch secret-fixture", &|bytes| {
            assert!(Secret::new(text(bytes)).is_err())
        }),
    ];
    for (bytes, make) in cases {
        let freed = freed_after(bytes.to_vec(), make);
        assert_eq!(freed, WIPED, "{}", String::from_utf8_lossy(bytes));
    }
}
