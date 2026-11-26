use crazyagent::accounts::probe::{get, request};
#[cfg(unix)]
use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
#[cfg(unix)]
use crazyagent::engine::bootstrap::EngineService;
#[cfg(unix)]
use crazyagent::engine::owner::{AccountSecret, OwnerClient, OwnerClientKind};
use kernel_security::secret::wipe;
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const KEY_MARK: &[u8] = b"sk-residue-probe";
const HEADER_END: &[u8] = b"\r\n\r\n";

static SERIAL: Mutex<()> = Mutex::new(());
static WATCHING: AtomicBool = AtomicBool::new(false);
static FREED_WITH_KEY: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, handing out zeroed blocks, which also counts the byte blocks
/// (align 1) freed while the row watches that still hold the key's mark, the old block of
/// every growth included. Blocks of other layouts can hold padding, so they are never read.
struct FreeScan;

unsafe impl GlobalAlloc for FreeScan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        if WATCHING.load(Ordering::SeqCst) && layout.align() == 1 && layout.size() >= KEY_MARK.len()
        {
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
    let _serial = serial();
    FREED_WITH_KEY.store(0, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
    for key in &keys {
        let bytes = request("/v1/models", "127.0.0.1:9", Some(key));
        let shape = (
            bytes.len(),
            bytes.capacity(),
            bytes.ends_with(HEADER_END),
            bytes
                .windows(key.len())
                .any(|window| window == key.as_slice()),
        );
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

/// Takes the lock every row holds from its first step to its last, so no row counts a block
/// another row frees, before, during or after its watch. Output: the held lock.
fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Counts the blocks freed holding the key's mark while `steps` runs; the caller holds
/// [`serial`]. Input: the steps. Output: the count.
fn freed_while(steps: impl FnOnce()) -> usize {
    FREED_WITH_KEY.store(0, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
    steps();
    WATCHING.store(false, Ordering::SeqCst);
    FREED_WITH_KEY.load(Ordering::SeqCst)
}

/// A key of `length` bytes that starts with the mark. Input: the length. Output: the key.
fn marked_key(length: usize) -> Vec<u8> {
    let mut key = KEY_MARK.to_vec();
    key.extend((b'a'..=b'z').cycle().take(length - KEY_MARK.len()));
    key
}

/// A probe whose timeout ends it while the request is still being written, to a server that
/// accepts and never reads, frees no block that still holds the bearer key.
#[test]
fn a_probe_cut_off_by_its_timeout_mid_write_frees_no_copy_of_its_key() {
    const KEY_BYTES: usize = 32 << 20;
    let _serial = serial();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(kernel_processes::spawn_gate::bind_tcp("127.0.0.1:0"))
        .unwrap();
    let url = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    runtime.spawn(async move {
        let accepted = kernel_processes::spawn_gate::accept_tcp(&listener).await;
        std::future::pending::<()>().await;
        drop(accepted);
    });
    let mut key = marked_key(KEY_BYTES);
    let mut code = String::new();
    let freed = freed_while(|| {
        let sent = runtime.block_on(get(
            &url,
            "/v1/models",
            Some(&key),
            Duration::from_millis(100),
        ));
        code = sent.err().map(|error| error.0).unwrap_or_default();
    });
    drop(runtime);
    wipe(&mut key);
    assert_eq!(code, "account_url_unreachable");
    assert_eq!(freed, 0, "freed blocks still holding the key");
}

/// Checking an account whose URL is https, which the probe only dials and never sends the
/// key to, frees no block that still holds the account's saved key.
#[cfg(unix)]
#[test]
fn checking_an_https_account_frees_no_copy_of_its_saved_key() {
    let _serial = serial();
    let root = tempfile::Builder::new()
        .prefix("ca-wipe-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = kernel_processes::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (shutdown, stopped) = tokio::sync::watch::channel(false);
    let (mut service, client, id) = runtime.block_on(async {
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
                .await
                .unwrap()
                .0;
        let draft = AccountDraft {
            provider: Provider::OpenaiCompatible,
            method: SignInMethod::Url,
            name: "Lab server".into(),
            url: Some(format!("https://127.0.0.1:{port}/v1")),
            model: Some("lab-model".into()),
        };
        let key = AccountSecret::new(marked_key(64)).unwrap();
        let account = client.add_account(draft, Some(key)).await.unwrap();
        (service, client, account.id)
    });
    let freed = freed_while(|| {
        runtime.block_on(client.check_account(id.clone())).unwrap();
    });
    runtime.block_on(async {
        client.remove_account(id).await.unwrap();
        drop(client);
        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
    });
    drop(shutdown);
    drop(listener);
    assert_eq!(freed, 0, "freed blocks still holding the saved key");
}
