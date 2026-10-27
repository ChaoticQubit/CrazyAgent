/// The folder under the engine's data folder that holds each project's IDE server state: its
/// connection token, ports, capability, log and server data. Every agent sandbox denies it.
pub const FOLDER: &str = "ide";

pub mod bridge;
pub mod env;
pub mod fetch;
pub mod git;
pub mod manifest;
#[cfg(unix)]
pub mod ports;
pub mod readonly;
#[cfg(unix)]
pub mod sandbox;
#[cfg(unix)]
pub mod server;
pub mod settings;
#[cfg(unix)]
pub mod token;
#[cfg(unix)]
pub mod webview;
pub mod workspace;
