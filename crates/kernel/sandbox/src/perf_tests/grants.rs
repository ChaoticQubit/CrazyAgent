use super::row;
use crate::grants::{GrantHooks, checked, socket_fit};
use crate::perf_kit::{SAMPLES, time};
use crate::policy::AccessGrant;
use std::fs;
use std::hint::black_box;
use std::os::unix::net::UnixListener;

/// The harness folders 2.4.4 named in its fixed secret list, which 2.5 takes from the engine.
const HARNESSES: [&str; 3] = [".codex", ".claude", ".claude.json"];

/// The stand-in grant hooks with the harness folders above and no IDE ports, so the socket
/// check walks the same secret list on both builds. Output: the hooks.
fn hooks() -> GrantHooks {
    GrantHooks {
        harness_folders: || HARNESSES.to_vec(),
        ide_ports: |_| Vec::new(),
    }
}

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
    let _listener = UnixListener::bind(&real).unwrap();
    let grant = AccessGrant::Socket { path: real.clone() };
    let hooks = hooks();
    assert_eq!(socket_fit(&real), Ok(()));
    assert_eq!(checked(&hooks, &grant, &data), Ok(grant.clone()));
    time(&row("grants.socket_fit", 10), SAMPLES, || {
        let _ = black_box(socket_fit(black_box(&real)));
    });
    time(&row("grants.checked.socket", 10), SAMPLES, || {
        let _ = black_box(checked(&hooks, black_box(&grant), &data));
    });
}
