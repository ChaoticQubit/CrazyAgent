/// A refusing port stands for a stopped model provider in a row's settings, so it lies where
/// an owner's model port would: below every range the system hands out by itself, which on
/// Linux a kept model port must stay out of or no Landlock agent starts (G1 N8 P4), and apart
/// from the owner's and the model proxy's ports.
#[test]
fn a_refusing_port_lies_below_the_ports_the_system_hands_out() {
    use super::OWNER_PORTS;
    use super::model_proxy::{PICKED_PORTS_START, PROXY_PORTS};
    let held: Vec<_> = (0..4)
        .map(|_| kernel_processes::spawn_gate::refusing_tcp_port().unwrap())
        .collect();
    for (_, port) in &held {
        assert!(*port > 1024 && *port < PICKED_PORTS_START, "{port}");
        assert!(
            !PROXY_PORTS.contains(port) && !OWNER_PORTS.contains(port),
            "{port}"
        );
    }
    assert_eq!(held.len(), 4);
}

/// A low free port lies below every range the system hands out by itself, below the ports
/// Linux node services use, and apart from the owner's and the model proxy's ports. A
/// listener of its own binds it, and the next call in this process names another port.
#[test]
fn a_low_free_port_is_outside_the_system_and_proxy_ranges_and_each_call_moves_on() {
    use super::OWNER_PORTS;
    use super::model_proxy::{PICKED_PORTS_START, PROXY_PORTS};
    use std::net::{Ipv4Addr, SocketAddr};
    let first = kernel_processes::spawn_gate::free_low_tcp_port().unwrap();
    let second = kernel_processes::spawn_gate::free_low_tcp_port().unwrap();
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
        let listener = kernel_processes::spawn_gate::bind_tcp_std_exclusive(address).unwrap();
        assert_eq!(listener.local_addr().unwrap(), address);
    }
}
