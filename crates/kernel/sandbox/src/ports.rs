/// The lowest port a loopback bridge carries on Linux: inside a network namespace of its
/// own an agent's helper has no capability to listen below it.
pub const BRIDGE_PORT_MIN: u16 = 1024;

/// The fixed ports the owner's tools listen on, which no tools-on agent may bind or listen
/// on at any address: the Codex sign-in callback and Ollama's local model server. Seatbelt
/// matches a local address by port only (`localhost` there matches every local address), so
/// it cannot keep an agent's listener on loopback; it can keep these ports free.
pub const OWNER_PORTS: [u16; 2] = [1455, 11434];
const _: () = {
    use kernel_processes::spawn_gate::LOW_PORTS;
    assert!(*LOW_PORTS.start() > 1024 && *LOW_PORTS.end() < 30_000);
    assert!(*LOW_PORTS.end() < PICKED_PORTS_START);
    assert!(*LOW_PORTS.end() < *PROXY_PORTS.start());
    let mut index = 0;
    while index < OWNER_PORTS.len() {
        let port = OWNER_PORTS[index];
        assert!(port < *LOW_PORTS.start() || port > *LOW_PORTS.end());
        index += 1;
    }
};

/// The most ports of the owner's configured local model endpoints one tools-on sandbox keeps
/// free beside [`OWNER_PORTS`], which bounds the profile's deny rules.
pub const KEPT_PORTS_MAX: usize = 64;

/// The most ports one sandbox's limited network lists, loopback and any-host together,
/// which bounds the Linux helper's routes, bridges and egress rules.
pub const REMOTES_MAX: usize = 8;

/// The loopback ports a model proxy takes when no saved port is given. The block is fixed,
/// so every Open-network sandbox, whenever it started, denies binding all of it: no
/// sandboxed agent can take a proxy's port, not even while the engine that held it is down.
/// It lies below the ports the system hands out by itself ([`PICKED_PORTS_START`]).
#[cfg(not(target_os = "linux"))]
pub const PROXY_PORTS: std::ops::RangeInclusive<u16> = 41_300..=41_427;
/// The same block on Linux, which hands out ports from 32768 up by itself: a bind to port 0
/// or a listen with no bind gets such a port with no Landlock check, so the block lies
/// below them, and below the 30000 to 32767 that Kubernetes node ports use.
#[cfg(target_os = "linux")]
pub const PROXY_PORTS: std::ops::RangeInclusive<u16> = 29_300..=29_427;
/// The loopback ports a model key relay listens on, the block right after [`PROXY_PORTS`].
/// Every Open-network sandbox denies binding all of it, as it does the proxy block, so no
/// sandboxed agent can take a relay's port, not even after the relay ended while a kept
/// pane still sends its requests there.
#[cfg(not(target_os = "linux"))]
pub const RELAY_PORTS: std::ops::RangeInclusive<u16> = 41_428..=41_555;
/// The same block on Linux, right after its proxy block, below the ports Linux hands out by
/// itself and the 30000 up that Kubernetes node ports use.
#[cfg(target_os = "linux")]
pub const RELAY_PORTS: std::ops::RangeInclusive<u16> = 29_428..=29_555;
/// Where the ports the system hands out by itself start unless an administrator moved
/// them: 49152 on macOS and Windows, 32768 on Linux.
#[cfg(not(target_os = "linux"))]
pub(crate) const PICKED_PORTS_START: u16 = 49_152;
/// Where the ports Linux hands out by itself start unless an administrator moved them.
#[cfg(target_os = "linux")]
pub(crate) const PICKED_PORTS_START: u16 = 32_768;
const _: () = assert!(*PROXY_PORTS.start() > 1024 && *PROXY_PORTS.end() < PICKED_PORTS_START);
const _: () = assert!(
    *RELAY_PORTS.start() == *PROXY_PORTS.end() + 1 && *RELAY_PORTS.end() < PICKED_PORTS_START
);
const _: () = {
    let mut index = 0;
    while index < OWNER_PORTS.len() {
        let port = OWNER_PORTS[index];
        assert!(port < *PROXY_PORTS.start() || port > *PROXY_PORTS.end());
        assert!(port < *RELAY_PORTS.start() || port > *RELAY_PORTS.end());
        index += 1;
    }
};

#[cfg(test)]
mod tests;
