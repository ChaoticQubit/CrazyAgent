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

/// A refusing port turns a connect away on every platform. On unix it also stays taken while
/// its two ends live: no port-0 bind of this process gets it and a bind that names it without
/// `SO_REUSEADDR` fails, so a row naming it as a stopped provider never meets a listener there.
#[test]
fn a_refusing_port_turns_connects_away_and_stays_taken_while_held() {
    use std::net::{Ipv4Addr, SocketAddr};
    let (held, port) = super::refusing_tcp_port().unwrap();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let refused = super::connect_tcp_std(address).unwrap_err();
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::ConnectionRefused,
        "{refused}"
    );
    #[cfg(unix)]
    {
        let bound: Vec<_> = (0..64)
            .map(|_| super::bind_tcp_std("127.0.0.1:0").unwrap())
            .collect();
        assert!(
            bound
                .iter()
                .all(|listener| listener.local_addr().unwrap().port() != port),
            "a port-0 bind got the held port {port}"
        );
        let named = super::bind_tcp_std_exclusive(address).unwrap_err();
        assert_eq!(named.kind(), std::io::ErrorKind::AddrInUse, "{named}");
    }
    drop(held);
}

/// A low free port lies below every range the system hands out by itself, below the ports
/// Linux node services use, and apart from the owner's and the model proxy's ports. A
/// listener of its own binds it, and the next call in this process names another port.
#[test]
fn a_low_free_port_is_outside_the_system_and_proxy_ranges_and_each_call_moves_on() {
    use crate::sandbox::OWNER_PORTS;
    use crate::sandbox::model_proxy::{PICKED_PORTS_START, PROXY_PORTS};
    use std::net::{Ipv4Addr, SocketAddr};
    let first = super::free_low_tcp_port().unwrap();
    let second = super::free_low_tcp_port().unwrap();
    assert_ne!(first, second);
    for port in [first, second] {
        assert!(
            port > 1024 && port < 30_000 && port < PICKED_PORTS_START,
            "{port}"
        );
        assert!(
            !PROXY_PORTS.contains(&port) && !OWNER_PORTS.contains(&port),
            "{port}"
        );
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = super::bind_tcp_std_exclusive(address).unwrap();
        assert_eq!(listener.local_addr().unwrap(), address);
    }
}
