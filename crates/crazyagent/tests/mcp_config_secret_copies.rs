use crazyagent::launch_context::claude_config;
use crazyagent::plugins::{LaunchSecret, LaunchServer, SecretValue, ServerSpec};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const SECRET: &[u8] = b"mcp-config-secret-fixture-7340";

static WATCHING: AtomicBool = AtomicBool::new(false);
static FREED_WITH_SECRET: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, which also counts the blocks freed while the row watches that
/// still hold the secret, growth of a buffer included.
struct FreeScan;

unsafe impl GlobalAlloc for FreeScan {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        if WATCHING.load(Ordering::SeqCst) && layout.size() >= SECRET.len() {
            let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
            if bytes.windows(SECRET.len()).any(|window| window == SECRET) {
                FREED_WITH_SECRET.fetch_add(1, Ordering::SeqCst);
            }
        }
        unsafe { System.dealloc(block, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: FreeScan = FreeScan;

/// One approved server holding the secret under `name`. Inputs: the server name, its spec
/// and the secret's name. Output: the server.
fn server(name: &str, spec: ServerSpec, secret: &str) -> LaunchServer {
    LaunchServer {
        name: name.into(),
        engine: false,
        spec,
        secrets: vec![LaunchSecret {
            name: secret.into(),
            value: SecretValue::new(SECRET.to_vec()),
        }],
    }
}

/// SECRET-CLAUDE-CONFIG: writing Claude Code's MCP config frees no block that still holds
/// a server secret, so the only copy left is the text the launch writes and then wipes.
#[test]
fn a_claude_mcp_config_frees_no_copy_of_a_secret() {
    let servers = [
        server(
            "tools",
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: vec!["--stdio".into()],
            },
            "API_TOKEN",
        ),
        server(
            "docs",
            ServerSpec::Http {
                url: "https://mcp.example.invalid/mcp".into(),
            },
            "Authorization",
        ),
    ];
    WATCHING.store(true, Ordering::SeqCst);
    let config = claude_config(&servers);
    WATCHING.store(false, Ordering::SeqCst);
    let mut text = config.unwrap().into_bytes();
    let held = text
        .windows(SECRET.len())
        .filter(|window| *window == SECRET)
        .count();
    crazyagent::credentials::wipe(&mut text);
    assert_eq!(held, 2);
    assert_eq!(FREED_WITH_SECRET.load(Ordering::SeqCst), 0);
}
