/// The lowest port a loopback bridge carries on Linux: inside a network namespace of its
/// own an agent's helper has no capability to listen below it.
pub const BRIDGE_PORT_MIN: u16 = 1024;
