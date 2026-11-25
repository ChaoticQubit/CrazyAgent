use crate::env_names::agent_env_refused;
use crate::names::{
    is_dotted, is_snake, is_store_account, is_store_name, single_component, validate_identity,
};
use crate::net::metadata_address;
use crate::perf_kit::{Row, SAMPLES, time};
use crate::relay_names::{is_placeholder, is_relay_token};
use crate::{hash, windows_device};
use std::ffi::OsStr;
use std::hint::black_box;
use std::net::{IpAddr, Ipv4Addr};

const BATCH: usize = 1000;
const HASH_BATCH: usize = 100;
const VARIABLE: &str = "HOME";
const KIND: &str = "embedding";
const PERMISSION: &str = "secrets.find";
const CHILD: &str = "notes.md";
const SERVICE: &str = "dev.crazyagent.plugins";
const ACCOUNT: &str = "plugin-key-openai";
const IDENTITY: &str = "agent-7f3c.lead";
const METADATA: IpAddr = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254));
const TOKEN: &str =
    "crazyagent-relay-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const HASHED: &[u8; 64] = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const DEVICE: &str = "COM1.txt";

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "common",
        name,
        batch,
    }
}

/// Times every public call of kernel_common on one fixed input each, the inputs the 2.4.4
/// twins take too, after checking each answer once so both builds do the same work.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn common_calls() {
    assert!(!agent_env_refused(OsStr::new(VARIABLE)));
    assert!(is_snake(KIND) && is_dotted(PERMISSION) && single_component(CHILD));
    assert!(is_store_name(SERVICE) && is_store_account(ACCOUNT));
    assert_eq!(validate_identity(IDENTITY), Ok(()));
    assert!(metadata_address(METADATA));
    assert!(is_placeholder(TOKEN) && is_relay_token(TOKEN));
    assert_eq!(hash(HASHED).len(), 64);
    assert!(windows_device(DEVICE));
    time(&row("env_names.agent_env_refused", BATCH), SAMPLES, || {
        black_box(agent_env_refused(black_box(OsStr::new(VARIABLE))));
    });
    time(&row("names.is_snake", BATCH), SAMPLES, || {
        black_box(is_snake(black_box(KIND)));
    });
    time(&row("names.is_dotted", BATCH), SAMPLES, || {
        black_box(is_dotted(black_box(PERMISSION)));
    });
    time(&row("names.single_component", BATCH), SAMPLES, || {
        black_box(single_component(black_box(CHILD)));
    });
    time(&row("names.is_store_name", BATCH), SAMPLES, || {
        black_box(is_store_name(black_box(SERVICE)));
    });
    time(&row("names.is_store_account", BATCH), SAMPLES, || {
        black_box(is_store_account(black_box(ACCOUNT)));
    });
    time(&row("names.validate_identity", BATCH), SAMPLES, || {
        let _ = black_box(validate_identity(black_box(IDENTITY)));
    });
    time(&row("net.metadata_address", BATCH), SAMPLES, || {
        black_box(metadata_address(black_box(METADATA)));
    });
    time(&row("relay_names.is_placeholder", BATCH), SAMPLES, || {
        black_box(is_placeholder(black_box(TOKEN)));
    });
    time(&row("relay_names.is_relay_token", BATCH), SAMPLES, || {
        black_box(is_relay_token(black_box(TOKEN)));
    });
    time(&row("hash", HASH_BATCH), SAMPLES, || {
        black_box(hash(black_box(HASHED)));
    });
    time(&row("windows_device", BATCH), SAMPLES, || {
        black_box(windows_device(black_box(DEVICE)));
    });
}
