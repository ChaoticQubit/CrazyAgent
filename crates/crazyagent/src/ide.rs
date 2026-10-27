/// The folder under the engine's data folder that holds each project's IDE server state: its
/// connection token, ports, capability, log and server data. Every agent sandbox denies it.
pub const FOLDER: &str = "ide";

pub mod env;
#[cfg(unix)]
pub mod ports;
#[cfg(unix)]
pub mod sandbox;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod token;
#[cfg(unix)]
pub mod webview;
