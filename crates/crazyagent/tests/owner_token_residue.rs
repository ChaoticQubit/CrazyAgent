#![cfg(unix)]

mod freed_bytes_support;

use crazyagent::config::resolve_config;
use crazyagent::engine::Engine;
use crazyagent::engine::owner::{OwnerClient, OwnerClientKind, OwnerRuntimeHandle, OwnerServer};
use crazyagent::engine::secret_frame::{OwnerToken, owner_token};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// The most tokens one row watches for at once.
const WATCHED_MAX: usize = 2;

/// The bytes of one owner token.
const TOKEN_BYTES: usize = 32;

static SERIAL: Mutex<()> = Mutex::new(());
static WATCHING: AtomicBool = AtomicBool::new(false);
static FREED_WITH_TOKEN: AtomicUsize = AtomicUsize::new(0);
static WATCHED_COUNT: AtomicUsize = AtomicUsize::new(0);
static WATCHED: [[AtomicU8; TOKEN_BYTES]; WATCHED_MAX] =
    [const { [const { AtomicU8::new(0) }; TOKEN_BYTES] }; WATCHED_MAX];

/// The system allocator, counting every freed block of any alignment that still holds a
/// watched token's bytes. It reads the tokens from atomics, so the scan never allocates.
struct FreeScan;

unsafe impl GlobalAlloc for FreeScan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if WATCHING.load(Ordering::SeqCst) && layout.size() >= TOKEN_BYTES {
            let block = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            if holds_watched(block) {
                FREED_WITH_TOKEN.fetch_add(1, Ordering::SeqCst);
            }
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeScan = FreeScan;

/// Whether a block holds any watched token's 32 bytes in a row. Input: the block's bytes.
/// Output: true on a match.
fn holds_watched(block: &[u8]) -> bool {
    let count = WATCHED_COUNT.load(Ordering::SeqCst);
    assert!(count <= WATCHED_MAX);
    (0..count).any(|i| {
        block.windows(TOKEN_BYTES).any(|window| {
            window
                .iter()
                .zip(WATCHED[i].iter())
                .all(|(byte, watched)| *byte == watched.load(Ordering::SeqCst))
        })
    })
}

/// Takes the lock every row holds from its first step to its last, so no row counts a block
/// another row frees. Output: the held lock.
fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Starts counting the freed blocks that hold any of the tokens; the caller holds
/// [`serial`]. Input: the tokens, at most two.
fn watch(tokens: &[&[u8; TOKEN_BYTES]]) {
    assert!(!tokens.is_empty() && tokens.len() <= WATCHED_MAX);
    for (slot, token) in WATCHED.iter().zip(tokens) {
        for (cell, byte) in slot.iter().zip(token.iter()) {
            cell.store(*byte, Ordering::SeqCst);
        }
    }
    WATCHED_COUNT.store(tokens.len(), Ordering::SeqCst);
    FREED_WITH_TOKEN.store(0, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
}

/// Stops counting and clears the watched tokens. Output: the freed blocks counted.
fn stop() -> usize {
    WATCHING.store(false, Ordering::SeqCst);
    let freed = FREED_WITH_TOKEN.load(Ordering::SeqCst);
    for slot in &WATCHED {
        for cell in slot {
            cell.store(0, Ordering::SeqCst);
        }
    }
    WATCHED_COUNT.store(0, Ordering::SeqCst);
    freed
}

/// Reads one owner key file into a buffer wiped when dropped, so the row's own copy is no
/// residue. Input: the key file. Output: the token.
fn key(path: &Path) -> OwnerToken {
    let mut token = owner_token();
    std::fs::File::open(path)
        .unwrap()
        .read_exact(&mut **token)
        .unwrap();
    token
}

/// A current-thread runtime, so each row drives its async steps without holding the row
/// lock across an await. Output: the runtime.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A private engine root with an open engine and its owner server bound. Output: the root,
/// the engine, the server and its host.
async fn bound() -> (
    tempfile::TempDir,
    Arc<Engine>,
    OwnerServer,
    OwnerRuntimeHandle,
) {
    let root = tempfile::Builder::new()
        .prefix("ca-token-residue-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), config.clone())
            .await
            .unwrap(),
    );
    let (server, host) = OwnerServer::bind(root.path(), engine.clone(), &config)
        .await
        .unwrap();
    (root, engine, server, host)
}

#[test]
fn a_dropped_owner_client_frees_no_copy_of_its_token() {
    let _serial = serial();
    let runtime = runtime();
    let (root, engine, server, host) = runtime.block_on(bound());
    let token = key(&root.path().join("owner-tui.cap"));
    let client = Box::new(
        OwnerClient::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(2)).unwrap(),
    );
    watch(&[&token]);
    drop(client);
    let freed = stop();
    drop((server, host));
    runtime.block_on(engine.close()).unwrap();
    assert_eq!(freed, 0, "the dropped client freed its owner token unwiped");
}

#[test]
fn a_closed_engine_frees_no_copy_of_its_owner_tokens() {
    let _serial = serial();
    let runtime = runtime();
    let (root, engine, server, host) = runtime.block_on(bound());
    let tui = key(&root.path().join("owner-tui.cap"));
    let desktop = key(&root.path().join("owner-desktop.cap"));
    watch(&[&tui, &desktop]);
    drop((server, host));
    runtime.block_on(engine.close()).unwrap();
    drop(engine);
    drop(runtime);
    let freed = stop();
    drop(root);
    assert_eq!(freed, 0, "the closed engine freed an owner token unwiped");
}
