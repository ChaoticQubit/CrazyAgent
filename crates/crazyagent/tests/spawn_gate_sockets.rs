#![cfg(unix)]

use crazyagent::spawn_gate;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const TRIALS: usize = 400;
const CHURNERS: usize = 4;

/// Counts the descriptors a fresh `/bin/ls /dev/fd` child finds open, which
/// is its own three stdio streams, the listing's folder and anything inherited.
/// Input: none. Output: the number of listed descriptors.
fn descriptors_in_a_child() -> usize {
    let listing = spawn_gate::output(
        Command::new("/bin/ls")
            .env_clear()
            .arg("/dev/fd")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(listing.status.success(), "{listing:?}");
    String::from_utf8(listing.stdout).unwrap().lines().count()
}

/// Opens and closes every kind of socket the engine makes: TCP listen,
/// connect and accept, Unix listen and connect, Unix datagram and pair.
/// Input: a private folder for the socket files and the round number.
/// Output: none; panics when a real socket call fails.
fn churn_sockets(folder: &Path, round: usize) {
    let tcp = spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let mut client = spawn_gate::connect_tcp_std(tcp.local_addr().unwrap()).unwrap();
    let (mut served, _) = spawn_gate::accept_tcp_std(&tcp).unwrap();
    client.write_all(b"x").unwrap();
    let mut byte = [0_u8; 1];
    served.read_exact(&mut byte).unwrap();
    let stream_path = folder.join(format!("s{round}"));
    let listener = spawn_gate::bind_unix_std(&stream_path).unwrap();
    let local = spawn_gate::connect_unix_std(&stream_path).unwrap();
    let datagram_path = folder.join(format!("d{round}"));
    let datagram = spawn_gate::bind_unix_datagram_std(&datagram_path).unwrap();
    let pair = spawn_gate::unix_pair_std().unwrap();
    drop((tcp, client, served, listener, local, datagram, pair));
    std::fs::remove_file(stream_path).unwrap();
    std::fs::remove_file(datagram_path).unwrap();
}

#[test]
fn sockets_made_while_children_start_never_reach_a_child() {
    let baseline = descriptors_in_a_child();
    let stop = Arc::new(AtomicBool::new(false));
    let rounds = Arc::new(AtomicUsize::new(0));
    let churners: Vec<_> = (0..CHURNERS)
        .map(|_| {
            let stop = stop.clone();
            let rounds = rounds.clone();
            std::thread::spawn(move || {
                let folder = tempfile::Builder::new()
                    .prefix("ca-gate-")
                    .tempdir_in("/tmp")
                    .unwrap();
                let mut round = 0;
                while !stop.load(Ordering::SeqCst) {
                    churn_sockets(folder.path(), round);
                    round += 1;
                    rounds.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    while rounds.load(Ordering::SeqCst) < CHURNERS {
        std::thread::yield_now();
    }
    let before = rounds.load(Ordering::SeqCst);
    let leaked = (0..TRIALS)
        .filter(|_| descriptors_in_a_child() != baseline)
        .count();
    let during = rounds.load(Ordering::SeqCst) - before;
    stop.store(true, Ordering::SeqCst);
    for churner in churners {
        churner.join().unwrap();
    }
    assert!(during > 0, "no socket was made while the children started");
    assert_eq!(
        leaked, 0,
        "{leaked} of {TRIALS} children inherited a socket made by another thread"
    );
}
