use super::{guarded, output, spawn, status};
use std::process::{Command, Stdio};

#[test]
#[should_panic(expected = "spawn_gate entered again on a thread that already holds it")]
fn entering_the_gate_inside_the_gate_panics_instead_of_deadlocking() {
    let _ = guarded(|| spawn(&mut Command::new("/usr/bin/true")));
}

#[test]
fn the_gate_is_usable_again_after_a_reentry_panic() {
    let reentry = std::thread::spawn(|| guarded(|| status(&mut Command::new("/usr/bin/true"))));
    assert!(reentry.join().is_err());
    assert!(
        status(&mut Command::new("/usr/bin/true"))
            .unwrap()
            .success()
    );
}

#[test]
fn output_keeps_the_stdio_the_caller_set() {
    let captured = output(
        Command::new("/bin/sh")
            .args(["-c", "printf out; printf err >&2"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(captured.status.success());
    assert_eq!(captured.stdout, b"out");
    assert!(captured.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn accept_failures_a_later_accept_outlives_are_retried_and_lasting_ones_end_the_server() {
    use rustix::io::Errno;
    for passing in [
        Errno::CONNABORTED,
        Errno::CONNRESET,
        Errno::INTR,
        Errno::MFILE,
        Errno::NFILE,
        Errno::NOBUFS,
        Errno::NOMEM,
        Errno::PROTO,
    ] {
        assert!(
            super::passing(&std::io::Error::from(passing)),
            "{passing:?} is retried"
        );
    }
    for lasting in [Errno::BADF, Errno::INVAL, Errno::NOTSOCK, Errno::OPNOTSUPP] {
        assert!(
            !super::passing(&std::io::Error::from(lasting)),
            "{lasting:?} ends the server"
        );
    }
}

/// An exclusive listener binds on either loopback address, holds its port against a second
/// exclusive bind, which fails with `AddrInUse`, and accepts a connection there (S4B-2W1,
/// G1).
#[cfg(unix)]
#[test]
fn an_exclusive_listener_binds_each_loopback_keeps_its_port_and_accepts() {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    for loopback in [
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
    ] {
        let listener = super::bind_tcp_std_exclusive(loopback).unwrap();
        let bound = listener.local_addr().unwrap();
        assert_eq!(bound.ip(), loopback.ip());
        assert_ne!(bound.port(), 0);
        let again = super::bind_tcp_std_exclusive(bound).unwrap_err();
        assert_eq!(again.kind(), std::io::ErrorKind::AddrInUse, "{bound}");
        let _client = super::connect_tcp_std(bound).unwrap();
        let (_, peer) = super::accept_tcp_std(&listener).unwrap();
        assert_eq!(peer.ip(), loopback.ip());
    }
}
