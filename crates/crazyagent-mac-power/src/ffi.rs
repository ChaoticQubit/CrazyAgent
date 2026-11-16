//! Every call into macOS the helper makes that Rust cannot make safely: the peer's audit token,
//! signing information, launchd's socket, the thermal pressure key, the lid, the monotonic
//! clock, signals, `poll` and syslog. The one file of this crate allowed `unsafe`.
