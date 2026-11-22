#![forbid(unsafe_code)]

pub mod accounts;
pub mod activity;
pub mod adapters;
pub mod agent_git;
pub mod agent_home;
pub mod agents;
pub mod app_plugins;
pub mod assets;
pub mod cleanup_settings;
pub mod client_controller;
pub mod config;
pub mod context_mode;
pub mod conversation;
pub mod coord;
pub mod coordination;
pub mod credentials;
pub mod demo;
pub mod engine;
pub mod file_identity;
#[cfg(unix)]
pub mod file_lock;
pub mod github;
pub mod harnesses;
/// The engine side of the IDE tab: per-project loopback IDE servers, their tokens, ports
/// and environment.
pub mod ide;
pub mod instruction_check;
/// Edit with AI for the owner's instructions: the prompt, the proposal read out of a
/// model reply, the pool rule and the one confined model run.
pub mod instruction_cleanup;
pub mod instruction_files;
/// The owner-class endpoints, named by path: the engine and owner listeners, their clients
/// and the check of the peer at each end: a Unix socket guarded by its parent folder, or on
/// Windows a named pipe whose pipes file in that folder pins the serving engine.
pub(crate) mod ipc;
#[cfg(unix)]
pub mod isolation;
pub mod landing;
pub mod landing_log;
pub mod landing_turns;
pub mod launch_context;
pub mod learning;
pub mod ledger;
pub mod master_frame_limit;
pub mod master_session;
pub mod master_sessions;
pub mod master_step_limit;
pub mod master_time_limit;
pub mod memory;
pub mod model_gateway;
pub mod onboarding;
pub mod owner_profile;
pub mod owner_texts;
pub mod paths;
pub mod planning;
pub mod platform;
pub mod plugins;
pub mod pools;
pub mod project_files;
pub mod project_overview;
pub mod project_setup;
pub mod projects_home;
pub mod protocol;
pub mod provider_reach;
pub mod provider_retry;
pub mod qualification;
pub mod questions;
pub mod quit_settings;
pub mod rcli_host;
pub mod recovery;
pub mod research;
pub mod restore;
pub mod roadmap_status;
pub mod routing;
pub mod sandbox;
pub mod sandbox_settings;
pub mod scheduling;
/// The capability SDK slice: the owner's quit save runs through the generated SDK, the bus
/// gateway's grant check and the bundled agent-quit plugin.
mod sdk_slice;
pub mod service;
pub mod sessions_log;
pub mod settings_log;
pub mod setup;
pub mod skills;
/// The engine's ordered event log in engine.db (v2.5 L1): one `seq` per event, the
/// transaction's revision, a closed aggregate, and snapshot runs in chunks.
pub mod state_log;
pub mod storage;
pub mod team;
pub mod terminal;
pub mod tools;
pub mod tui;
/// `crazyagent update`: release channel, signed download, verification and executable swap.
pub mod update;
pub mod usage;
pub mod verification;
/// A client's side of the engine watch (v2.5 S3b, S3d): where it resumes, which topics go
/// out at once and how often a reload for changes of unknown subject may follow.
pub mod watch_feed;
pub mod worker_dispatch;
pub mod worktrees;

#[cfg(all(test, unix))]
#[path = "../tests/poll_support/mod.rs"]
mod poll_support;

#[cfg(all(test, unix))]
mod budget_processes_tests;
