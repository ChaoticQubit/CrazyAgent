use super::sandbox::row;
use crate::perf_kit::{SAMPLES, time};
use crate::sandbox::grants::{checked, socket_fit};
use crate::sandbox::policy::AccessGrant;
use std::fs;
use std::hint::black_box;

/// Times the socket grant checks on a real socket the user owns, with one link, in a temp
/// folder only the user can change, beside a data folder: the socket fit alone and the
/// socket arm of the grant check. Both accept before timing, so the whole path is timed.
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn socket_grants() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let real = root.join("perf-grant.sock");
    let _listener = crate::spawn_gate::bind_unix_std(&real).unwrap();
    let grant = AccessGrant::Socket { path: real.clone() };
    assert_eq!(socket_fit(&real), Ok(()));
    assert_eq!(checked(&grant, &data), Ok(grant.clone()));
    time(&row("grants.socket_fit", 10), SAMPLES, || {
        let _ = black_box(socket_fit(black_box(&real)));
    });
    time(&row("grants.checked.socket", 10), SAMPLES, || {
        let _ = black_box(checked(black_box(&grant), &data));
    });
}
