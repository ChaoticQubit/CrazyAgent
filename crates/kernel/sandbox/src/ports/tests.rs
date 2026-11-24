use super::{
    KEPT_PORTS_MAX, OWNER_PORTS, PICKED_PORTS_START, PROXY_PORTS, RELAY_PORTS, REMOTES_MAX,
};
use crate::hidden_paths::{HIDDEN_MAX, HIDDEN_PATHS_MAX};
use crate::lines::HELD_NOT_PUT_BACK_EXIT;
use kernel_processes::spawn_gate::LOW_PORTS;

#[test]
fn the_owner_ports_and_the_sandbox_limits_keep_their_values() {
    assert_eq!(OWNER_PORTS, [1455, 11434]);
    assert_eq!(KEPT_PORTS_MAX, 64);
    assert_eq!(REMOTES_MAX, 8);
    assert_eq!(HIDDEN_MAX, HIDDEN_PATHS_MAX + 2);
    assert_eq!(HIDDEN_MAX, 10);
    assert_eq!(HELD_NOT_PUT_BACK_EXIT, 122);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn the_proxy_and_relay_blocks_off_linux_sit_below_the_picked_ports() {
    assert_eq!(PROXY_PORTS, 41_300..=41_427);
    assert_eq!(RELAY_PORTS, 41_428..=41_555);
    assert_eq!(PICKED_PORTS_START, 49_152);
}

#[cfg(target_os = "linux")]
#[test]
fn the_proxy_and_relay_blocks_on_linux_sit_below_the_picked_ports() {
    assert_eq!(PROXY_PORTS, 29_300..=29_427);
    assert_eq!(RELAY_PORTS, 29_428..=29_555);
    assert_eq!(PICKED_PORTS_START, 32_768);
}

#[test]
fn no_owner_port_falls_in_a_proxy_relay_or_low_port_block() {
    assert_eq!(*RELAY_PORTS.start(), *PROXY_PORTS.end() + 1);
    assert!(*RELAY_PORTS.end() < PICKED_PORTS_START);
    assert!(*LOW_PORTS.end() < *PROXY_PORTS.start());
    for port in OWNER_PORTS {
        assert!(!PROXY_PORTS.contains(&port), "{port}");
        assert!(!RELAY_PORTS.contains(&port), "{port}");
        assert!(!LOW_PORTS.contains(&port), "{port}");
        assert!(port < PICKED_PORTS_START, "{port}");
    }
}
