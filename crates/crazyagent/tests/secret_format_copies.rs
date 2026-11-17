use crazyagent::credentials::Piece::{Escaped, Form, Hex, Plain};
use crazyagent::credentials::{SecretString, sized};
use crazyagent::model_gateway::relay::Relay;
use crazyagent::usage::extras::Adapter;
use crazyagent::usage::poll::poll_balance;
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(unix)]
use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(unix)]
use std::time::Duration;

const GITHUB_KEY: &[u8] = b"sk-or-format-copy-fixture-5521";
const RELAY_KEY: &[u8] = b"sk-relay-format-copy-fixture-6634";
const CLOSED: &str = "127.0.0.1:9";

static SERIAL: Mutex<()> = Mutex::new(());
static WATCHED: AtomicUsize = AtomicUsize::new(usize::MAX);
static WATCHING: AtomicBool = AtomicBool::new(false);
static FREED_WITH_KEY: AtomicUsize = AtomicUsize::new(0);
const KEYS: [&[u8]; 2] = [GITHUB_KEY, RELAY_KEY];

/// The system allocator, which also counts the blocks freed while a row watches that still
/// hold that row's key, growth of a buffer included.
struct FreeScan;

unsafe impl GlobalAlloc for FreeScan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        if WATCHING.load(Ordering::SeqCst) {
            let key = KEYS[WATCHED.load(Ordering::SeqCst)];
            if layout.size() >= key.len() {
                let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
                if bytes.windows(key.len()).any(|window| window == key) {
                    FREED_WITH_KEY.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        unsafe { System.dealloc(block, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeScan = FreeScan;

/// Runs `work` while the allocator watches for freed blocks holding the key at `index` in
/// `KEYS`. Inputs: the key's index and the work. Output: the number of such blocks.
fn freed_with_key(index: usize, work: impl FnOnce()) -> usize {
    FREED_WITH_KEY.store(0, Ordering::SeqCst);
    WATCHED.store(index, Ordering::SeqCst);
    WATCHING.store(true, Ordering::SeqCst);
    work();
    WATCHING.store(false, Ordering::SeqCst);
    FREED_WITH_KEY.load(Ordering::SeqCst)
}

/// SECRET-FORMAT-COPIES: a request through the GitHub and usage curl path (here a saved
/// key's balance poll) frees no block that still holds the key while it builds curl's
/// config, so the only copy is the config itself, which is wiped once written.
#[test]
fn a_curl_request_frees_no_copy_of_its_token() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = SecretString::new(GITHUB_KEY.to_vec()).unwrap();
    let url = url::Url::parse(&format!("http://{CLOSED}/api/v1/credits")).unwrap();
    let freed = freed_with_key(0, || {
        let _ = poll_balance(Adapter::OpenRouter, url, &key, true);
    });
    assert_eq!(freed, 0, "freed blocks still holding the token");
}

/// SECRET-FORMAT-COPIES: a request the model key relay forwards frees no block that still
/// holds the real key while the relay builds curl's config. The closed upstream makes the
/// relay answer 502 only after it has built that config and started curl.
#[cfg(unix)]
#[test]
fn a_relayed_request_frees_no_copy_of_the_key() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = SecretString::new(RELAY_KEY.to_vec()).unwrap();
    let relay = Relay::start(&format!("http://{CLOSED}/v1"), key).unwrap();
    let request = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: {}\r\nanthropic-version: 2023-06-01\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}",
        relay.token()
    );
    let mut reply = Vec::new();
    let freed = freed_with_key(1, || {
        let mut stream =
            crazyagent::spawn_gate::connect_tcp_std(("127.0.0.1", relay.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.read_to_end(&mut reply).unwrap();
    });
    assert!(
        reply.starts_with(b"HTTP/1.1 502"),
        "{}",
        String::from_utf8_lossy(&reply)
    );
    assert_eq!(freed, 0, "freed blocks still holding the key");
}

/// SECRET-FORMAT-COPIES on Windows: curl cannot inherit the pipe that carries a request
/// body there, so the relay refuses to start and never holds or relays a key.
#[cfg(windows)]
#[test]
fn a_relay_refuses_to_start_where_curl_cannot_take_the_body_pipe() {
    let key = SecretString::new(RELAY_KEY.to_vec()).unwrap();
    let refused = Relay::start(&format!("http://{CLOSED}/v1"), key).err();
    assert_eq!(
        refused.map(|error| error.0),
        Some("model_relay_unavailable".to_owned())
    );
}

/// SECRET-FORMAT-COPIES: `credentials::sized` writes each piece as the formatting it
/// replaced did (the `replace` escaping, the form serializer, `{:02x}`), into a buffer of
/// exactly that size, and refuses a control byte only in an escaped piece.
#[test]
fn sized_writes_each_piece_as_the_old_formatting_did() {
    let text = "a b&c=d%e\"f\\g\u{e9}*-._~";
    let quoted = text.replace('\\', "\\\\").replace('"', "\\\"");
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(text, text)
        .finish();
    let bytes: Vec<u8> = (0..=255).collect();
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let written = sized(&[
        Plain("<"),
        Escaped(text),
        Plain("|"),
        Form(text),
        Plain("="),
        Form(text),
        Plain("|"),
        Hex(&bytes),
        Plain(">"),
    ])
    .unwrap();
    assert_eq!(written, format!("<{quoted}|{form}|{hex}>").as_bytes());
    assert_eq!(written.capacity(), written.len());
    assert!(sized(&[Plain("kept"), Escaped("line\u{1}break")]).is_none());
    assert!(sized(&[Escaped("delete\u{7f}")]).is_none());
    assert_eq!(
        sized(&[Plain("tab\tkept"), Form("\u{1}")]).unwrap(),
        b"tab\tkept%01"
    );
}
