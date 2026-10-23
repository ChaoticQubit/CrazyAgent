use super::process_custody;
use crate::adapters::codex::FRAME_BYTES_MAX;
use crate::engine::EngineError;
use crate::engine::bootstrap::NativeProfileUse;
use crate::routing::ResourceBudget;
use std::collections::VecDeque;
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

pub(super) const POLL_TIME: Duration = Duration::from_millis(10);
pub(super) const CLOSE_TIME: Duration = Duration::from_secs(5);
pub(super) const OUTPUT_QUEUE_MAX: usize = 128;
const COMMAND_QUEUE_MAX: usize = 16;

pub(super) enum CodexHostOutput {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exited(ExitStatus),
}

pub(super) struct WriteRequest {
    pub(super) bytes: Vec<u8>,
    pub(super) done: oneshot::Sender<Result<(), EngineError>>,
}

pub(super) struct ProcessState {
    pub(super) started: Option<Result<u32, &'static str>>,
    pub(super) output: VecDeque<CodexHostOutput>,
    pub(super) observed: (u64, u64),
    pub(super) status: Option<ExitStatus>,
    pub(super) complete: bool,
    pub(super) error: Option<&'static str>,
    pub(super) cleanup_error: Option<&'static str>,
}

pub(super) struct ProcessShared {
    pub(super) state: Mutex<ProcessState>,
    pub(super) stop: AtomicBool,
    pub(super) close_stdin: AtomicBool,
}

impl ProcessShared {
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, ProcessState>, EngineError> {
        self.state
            .lock()
            .map_err(|_| EngineError("codex_host_custody_poisoned".into()))
    }

    pub(super) fn fail(&self, code: &'static str) {
        if let Ok(mut state) = self.lock() {
            state.error.get_or_insert(code);
        }
        self.stop.store(true, Ordering::Release);
    }

    pub(super) fn accounting_failed(&self) {
        if let Ok(mut state) = self.lock() {
            state.cleanup_error = Some("codex_host_pipe_accounting");
        }
        self.fail("codex_host_pipe_accounting");
    }
}

pub(super) struct CodexHostProcess {
    shared: Arc<ProcessShared>,
    input: mpsc::Sender<WriteRequest>,
    worker: JoinHandle<()>,
    deadline: Instant,
    pid: u32,
    pending_input: Option<PendingInput>,
}

struct PendingInput {
    bytes: Vec<u8>,
    reply: oneshot::Receiver<Result<(), EngineError>>,
}

impl CodexHostProcess {
    #[cfg(test)]
    pub(super) async fn spawn(
        command: Command,
        budget: &ResourceBudget,
    ) -> Result<Self, EngineError> {
        Self::spawn_guarded(command, budget, None).await
    }

    pub(super) async fn spawn_guarded(
        command: Command,
        budget: &ResourceBudget,
        profile: Option<NativeProfileUse>,
    ) -> Result<Self, EngineError> {
        budget.validate().map_err(EngineError)?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(budget.wall_time_ms))
            .ok_or_else(|| EngineError("codex_host_deadline_range".into()))?;
        let shared = Arc::new(ProcessShared {
            state: Mutex::new(ProcessState {
                started: None,
                output: VecDeque::new(),
                observed: (0, 0),
                status: None,
                complete: false,
                error: None,
                cleanup_error: None,
            }),
            stop: AtomicBool::new(false),
            close_stdin: AtomicBool::new(false),
        });
        let (input, receiver) = mpsc::channel(COMMAND_QUEUE_MAX);
        let retained = Arc::clone(&shared);
        let limit = budget.output_bytes;
        let worker = std::thread::Builder::new()
            .name("codex-host-custody".into())
            .spawn(move || {
                process_custody::run(command, profile, receiver, retained, deadline, limit);
            })
            .map_err(|_| EngineError("codex_host_custodian_unavailable".into()))?;
        let mut process = Self {
            shared,
            input,
            worker,
            deadline,
            pid: 0,
            pending_input: None,
        };
        process.await_start().await?;
        assert!(process.pid() > 1);
        Ok(process)
    }

    async fn await_start(&mut self) -> Result<(), EngineError> {
        loop {
            let started = self.shared.lock()?.started;
            if let Some(result) = started {
                self.pid = result.map_err(|code| EngineError(code.into()))?;
                return Ok(());
            }
            if self.worker.is_finished() {
                return Err(EngineError("codex_host_custodian_stopped".into()));
            }
            self.check_deadline()?;
            tokio::time::sleep(POLL_TIME).await;
        }
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    #[cfg(test)]
    pub(super) fn observed_bytes(&self) -> (u64, u64) {
        match self.shared.state.lock() {
            Ok(state) => state.observed,
            Err(poisoned) => poisoned.into_inner().observed,
        }
    }

    pub(super) async fn send(&mut self, bytes: &[u8]) -> Result<(), EngineError> {
        if bytes.is_empty() || bytes.len() > FRAME_BYTES_MAX + 1 {
            return Err(EngineError("codex_host_input_limit".into()));
        }
        if self.shared.close_stdin.load(Ordering::Acquire) {
            return Err(EngineError("codex_host_input_closed".into()));
        }
        self.check_deadline()?;
        if let Some(pending) = &self.pending_input {
            if pending.bytes != bytes {
                return Err(EngineError("codex_host_input_busy".into()));
            }
        } else {
            let (done, reply) = oneshot::channel();
            let request = WriteRequest {
                bytes: bytes.to_vec(),
                done,
            };
            self.input
                .try_send(request)
                .map_err(|_| EngineError("codex_host_input_unavailable".into()))?;
            self.pending_input = Some(PendingInput {
                bytes: bytes.to_vec(),
                reply,
            });
        }
        assert!(self.pending_input.is_some());
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let pending = self.pending_input.as_mut().expect("retained native input");
        let observed = tokio::time::timeout(remaining, &mut pending.reply).await;
        self.pending_input = None;
        match observed {
            Ok(result) => result.map_err(|_| EngineError("codex_host_input_closed".into()))?,
            Err(_) => {
                self.shared.fail("codex_host_deadline");
                Err(EngineError("codex_host_deadline".into()))
            }
        }
    }

    pub(super) fn close_stdin(&self) {
        self.shared.close_stdin.store(true, Ordering::Release);
    }

    pub(super) fn request_stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.close_stdin();
    }

    pub(super) fn operation_result(&self) -> Result<(), EngineError> {
        match self.shared.lock()?.error {
            Some(code) => Err(EngineError(code.into())),
            None => Ok(()),
        }
    }

    pub(super) async fn next_output(&mut self) -> Result<CodexHostOutput, EngineError> {
        loop {
            {
                let mut state = self.shared.lock()?;
                if let Some(code) = state.error.or(state.cleanup_error) {
                    return Err(EngineError(code.into()));
                }
                if let Some(output) = state.output.pop_front() {
                    return Ok(output);
                }
                if state.complete {
                    return state
                        .status
                        .map(CodexHostOutput::Exited)
                        .ok_or_else(|| EngineError("codex_host_reap_missing".into()));
                }
            }
            if self.worker.is_finished() {
                return Err(EngineError("codex_host_cleanup_uncertain".into()));
            }
            self.check_deadline()?;
            tokio::time::sleep(POLL_TIME).await;
        }
    }

    pub(super) async fn close(&mut self) -> Result<ExitStatus, EngineError> {
        self.request_stop();
        let deadline = Instant::now() + CLOSE_TIME;
        loop {
            {
                let mut state = self.shared.lock()?;
                state.output.clear();
                if let Some(code) = state.cleanup_error {
                    return Err(EngineError(code.into()));
                }
                if state.complete {
                    return state
                        .status
                        .ok_or_else(|| EngineError("codex_host_reap_missing".into()));
                }
            }
            if Instant::now() >= deadline || self.worker.is_finished() {
                return Err(EngineError("codex_host_cleanup_uncertain".into()));
            }
            tokio::time::sleep(POLL_TIME).await;
        }
    }

    fn check_deadline(&self) -> Result<(), EngineError> {
        if Instant::now() >= self.deadline {
            self.shared.fail("codex_host_deadline");
            return Err(EngineError("codex_host_deadline".into()));
        }
        Ok(())
    }
}

impl Drop for CodexHostProcess {
    fn drop(&mut self) {
        self.request_stop();
    }
}
