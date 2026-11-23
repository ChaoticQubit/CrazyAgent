use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::*;
use crate::net::capture::CaptureRules;
use crate::secrets::{Handles, Scopes};
use crate::store::{SecretRef, Store};

/// The timed requests per row.
const SAMPLES: usize = 200;
/// The percentile both rows gate on.
const PERCENTILE: usize = 95;

/// A store the rows must never open: every call panics.
struct UnopenedStore;

impl Store for UnopenedStore {
    fn put(&self, _: &str, _: &str, _: &SecretString) -> Result<SecretRef, StoreError> {
        unreachable!("a held bearer needs no store call")
    }

    fn get(&self, _: &SecretRef) -> Result<SecretString, StoreError> {
        unreachable!("a held bearer needs no store call")
    }

    fn delete(&self, _: &SecretRef) -> Result<(), StoreError> {
        unreachable!("a held bearer needs no store call")
    }

    fn git_credential_helper(&self, _: &SecretRef, _: &str) -> Result<String, StoreError> {
        unreachable!("a held bearer needs no store call")
    }
}

/// Egress that allows every origin, resolves to one public address and answers at once,
/// so a row times only the kernel.
struct ImmediateEgress;

impl Egress for ImmediateEgress {
    fn allows(&self, _: &'static str, _: &str, _: &str, _: u16) -> Result<(), StoreError> {
        Ok(())
    }

    fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, StoreError> {
        Ok(vec![IpAddr::from([140, 82, 112, 4])])
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        std::hint::black_box(sealed.config().len());
        Ok(RawReply {
            status: 200,
            headers: Vec::new(),
            body: b"{}".to_vec(),
        })
    }
}

/// Times `SAMPLES` kernel requests that each fill one bearer from a held secret and send
/// through egress that answers at once. Output: the sorted times.
fn kernel_request_times() -> Vec<Duration> {
    let scopes = Scopes::new(&[("budget", "budget")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(
        &UnopenedStore,
        "dev.crazyagent.e0123456789ab",
        &scopes,
        &handles,
        None,
    );
    let bearer = secrets
        .hold(
            "budget",
            SecretString::bearer(b"gho_budget-held-token".to_vec()).unwrap(),
        )
        .unwrap();
    let rules = CaptureRules::new(&[]).unwrap();
    let requests = Requests::new(&secrets, &ImmediateEgress, &rules);
    let mut times = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let request = Request {
            method: Method::Get,
            url: "https://api.github.com/user".to_owned(),
            headers: Vec::new(),
            bearer: Some(bearer),
            form: Vec::new(),
            secret_form: Vec::new(),
            json: None,
            reply_headers: Vec::new(),
            capture: Vec::new(),
        };
        let start = Instant::now();
        std::hint::black_box(requests.request("budget", request)).unwrap();
        times.push(start.elapsed());
    }
    times.sort_unstable();
    times
}

/// The sample at `PERCENTILE` of sorted times. Input: the times. Output: the sample.
fn p95(sorted: &[Duration]) -> Duration {
    assert_eq!(sorted.len(), SAMPLES);
    assert!(sorted.windows(2).all(|pair| pair[0] <= pair[1]));
    sorted[sorted.len() * PERCENTILE / 100]
}

/// Measures filling and sealing one request with a held bearer and no store call: p95 at
/// most 10 us quiet, 50 us loaded. Run:
/// `OVERHEAD_MAC=quiet cargo test --release -p crazyagent-kernel-security --lib net::request::budget_tests::fill_p95 -- --ignored --nocapture`
/// (`OVERHEAD_MAC=loaded` on the loaded Mac).
#[test]
#[ignore = "budget row: run with OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded"]
fn fill_p95() {
    let (budget, condition) = match std::env::var("OVERHEAD_MAC").as_deref() {
        Ok("quiet") => (Duration::from_micros(10), "quiet"),
        Ok("loaded") => (Duration::from_micros(50), "loaded"),
        other => panic!("set OVERHEAD_MAC=quiet or OVERHEAD_MAC=loaded, got {other:?}"),
    };
    let found = p95(&kernel_request_times());
    println!(
        "security_budget net.fill condition={condition} samples={SAMPLES} p95_ns={}",
        found.as_nanos()
    );
    assert!(
        found <= budget,
        "p95 {found:?} over {budget:?} ({condition})"
    );
}

/// Measures the whole kernel request path with egress that answers at once: 95% of calls
/// under 20 ms. Run on the quiet and on the loaded Mac:
/// `cargo test --release -p crazyagent-kernel-security --lib net::request::budget_tests::request_kernel_path_p95 -- --ignored --nocapture`
#[test]
#[ignore = "budget row: run on the quiet and on the loaded Mac"]
fn request_kernel_path_p95() {
    let found = p95(&kernel_request_times());
    println!(
        "security_budget net.request_kernel_path samples={SAMPLES} p95_ns={}",
        found.as_nanos()
    );
    assert!(found < Duration::from_millis(20), "p95 {found:?}");
}
