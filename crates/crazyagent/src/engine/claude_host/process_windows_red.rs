use super::diagnostic::StreamDiagnostic;
use super::{ClaudeHostLimits, Fault, Phase, Status, Turn};
use crate::adapters::claude::ClaudeProtocol;
use crate::adapters::events::ObservedEvent;
use crate::engine::bootstrap::NativeProfileUse;
use crate::engine::codex_host::pump::Signal;
use crate::protocol::ProjectId;
use crate::sandbox::LineageCheck;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

/// The channels a Windows Claude Code custody takes, as on Unix, its wake a raised signal.
pub(super) struct Channels {
    pub(super) input: Receiver<Turn>,
    pub(super) output: mpsc::Sender<ObservedEvent>,
    pub(super) report: watch::Sender<Status>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) doubt: Arc<AtomicBool>,
    pub(super) group: Arc<AtomicU32>,
    pub(super) diagnostic: Arc<Mutex<Option<StreamDiagnostic>>>,
    pub(super) woken: Arc<Signal>,
    #[cfg(test)]
    pub(super) passes: Arc<std::sync::atomic::AtomicU64>,
}

/// Refuses Claude Code custody on Windows, where it has no process tree or pumps yet: the
/// run retires at once with `claude_host_unavailable`. Takes the parts of one custody, all
/// unused. Output: none; the status channel reports the refusal.
pub(super) fn start(
    _command: Command,
    _lineage: LineageCheck,
    _lease: (Option<NativeProfileUse>, ProjectId),
    _protocol: ClaudeProtocol,
    _limits: ClaudeHostLimits,
    _tools: bool,
    channels: Channels,
) {
    channels.report.send_replace(Status {
        phase: Phase::Retired,
        fault: Some(Fault::Host("claude_host_unavailable")),
    });
}

#[cfg(all(test, windows))]
#[path = "process_windows_tests.rs"]
mod windows_tests;
