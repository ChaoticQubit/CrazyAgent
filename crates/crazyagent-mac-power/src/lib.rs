//! The Keep awake lid helper (DEC-167 addendum 1, threat model docs/security/keep-awake-helper.md):
//! a root LaunchDaemon that keeps a Mac awake with the lid closed while our signed engine renews
//! a lease, and puts every setting back by itself when it stops. macOS only; every `unsafe`
//! block lives in `ffi`.

#![cfg(target_os = "macos")]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod caller;
#[allow(unsafe_code)]
mod ffi;
pub mod floor;
pub mod helper;
pub mod journal;
pub mod lease;
pub mod lid;
pub mod pin;
pub mod pmset;
pub mod protocol;
pub mod server;
