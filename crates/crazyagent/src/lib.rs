#![forbid(unsafe_code)]

pub mod accounts;
pub mod adapters;
pub mod agents;
pub mod assets;
pub mod client_controller;
pub mod config;
pub mod conversation;
pub mod coordination;
pub mod credentials;
pub mod engine;
pub mod github;
pub mod harnesses;
#[cfg(unix)]
pub mod isolation;
pub mod landing;
pub(crate) mod landing_turns;
pub mod launch_context;
pub mod master_session;
pub mod memory;
pub mod model_gateway;
pub mod onboarding;
pub mod owner_profile;
pub mod paths;
pub mod platform;
pub mod plugins;
pub mod pools;
pub mod project_setup;
pub mod projects_home;
pub mod protocol;
pub mod qualification;
pub mod rcli_host;
pub mod research;
pub mod routing;
pub mod sandbox;
pub mod service;
pub mod setup;
pub mod skills;
/// The short address a Unix socket is bound and reached at when its own path is too long
/// for a socket address.
#[cfg(unix)]
pub mod socket_address;
/// The one place this crate starts child processes and creates sockets, so no child
/// inherits a pipe or socket another thread has not marked close-on-exec yet.
pub mod spawn_gate;
pub mod storage;
pub mod team;
pub mod terminal;
pub mod tui;
/// `crazyagent update`: release channel, signed download, verification and executable swap.
pub mod update;
pub mod verification;
pub mod version;
pub mod worker_dispatch;
pub mod worktrees;
