/// The folder under the engine's data folder that holds each project's IDE server state: its
/// connection token, ports, capability, log and server data. Every agent sandbox denies it.
pub const FOLDER: &str = "ide";

pub mod agent_config;
pub mod bridge;
pub mod env;
pub mod extensions;
pub mod fetch;
pub mod git;
pub mod hide;
pub mod manifest;
#[cfg(unix)]
pub mod output;
#[cfg(unix)]
pub mod ports;
#[cfg(unix)]
pub mod proxy;
pub mod readonly;
#[cfg(unix)]
pub mod sandbox;
#[cfg(unix)]
pub mod server;
pub mod settings;
pub mod terminal;
#[cfg(unix)]
pub mod token;
pub mod workspace;
