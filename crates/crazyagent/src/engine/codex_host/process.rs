use super::process_custody;
use crate::adapters::codex::FRAME_BYTES_MAX;
use crate::engine::EngineError;
use crate::engine::bootstrap::NativeProfileUse;
use crate::platform::fd_wait::{self, Wake};
use crate::protocol::ProjectId;
use crate::routing::ResourceBudget;
use crate::sandbox::LineageCheck;
use std::collections::VecDeque;
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc, oneshot};

pub(super) const POLL_TIME: Duration = Duration::from_millis(10);
/// The longest one wait of the custody worker or of the host's reader for a live harness: a
/// missed wake or notify still lets the loop re-read its state.
pub(super) const WAIT_MAX: Duration = Duration::from_millis(250);
pub(super) const CLOSE_TIME: Duration = Duration::from_secs(5);
pub(super) const OUTPUT_QUEUE_MAX: usize = 128;
const COMMAND_QUEUE_MAX: usize = 16;

pub(super) enum CodexHostOutput {
    Stdout(Vec<u8>),
    Exited,
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
    pub(super) doubted: bool,
}

impl ProcessState {
    /// Notes a cleanup fault after which processes the harness started may still run; the
    /// doubt stays though the completion replaces the code (SA-VID14-R2 F2). Input: the
    /// code. Output: none.
    pub(super) fn doubt(&mut self, code: &'static str) {
        self.cleanup_error = Some(code);
        self.doubted = true;
    }
}

pub(super) struct ProcessShared {
    pub(super) state: Mutex<ProcessState>,
    pub(super) stop: AtomicBool,
    pub(super) close_stdin: AtomicBool,
    pub(super) changed: Notify,
    pub(super) ended: AtomicBool,
    pub(super) wake: Wake,
    pub(super) woken: Wake,
    #[cfg(test)]
    pub(super) custody_passes: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    pub(super) host_passes: std::sync::atomic::AtomicU64,
}

impl ProcessShared {
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, ProcessState>, EngineError> {
        self.state
            .lock()
            .map_err(|_| EngineError("codex_host_custody_poisoned".into()))
    }

    /// Records the run's first fault and stops it, and tells the host's reader. It does not
    /// rouse custody: custody calls it itself, on every pass once the clock has run out.
    /// Input: the fault code. Output: none.
    pub(super) fn fail(&self, code: &'static str) {
        if let Ok(mut state) = self.lock() {
            state.error.get_or_insert(code);
        }
        self.stop.store(true, Ordering::Release);
        self.changed.notify_one();
    }

    pub(super) fn accounting_failed(&self) {
        if let Ok(mut state) = self.lock() {
            state.doubt("codex_host_pipe_accounting");
        }
        self.fail("codex_host_pipe_accounting");
    }
}

/// Marks the custody worker's end in the shared state and tells the host's reader, however
/// the worker ends, a panic included. It runs before the thread counts as finished, so the
/// reader reads the mark, not only `JoinHandle::is_finished`.
struct WorkerEnd(Arc<ProcessShared>);

impl Drop for WorkerEnd {
    fn drop(&mut self) {
        self.0.ended.store(true, Ordering::Release);
        self.0.changed.notify_one();
    }
}

pub(super) struct CodexHostProcess {
    shared: Arc<ProcessShared>,
    input: mpsc::Sender<WriteRequest>,
    worker: JoinHandle<()>,
    deadline: Option<Instant>,
    close_time: Duration,
    pid: u32,
    pending_input: Option<PendingInput>,
    frame_max: usize,
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
        let run_time = Duration::from_millis(budget.wall_time_ms);
        Self::spawn_guarded(
            command,
            budget,
            Some(run_time),
            None,
            LineageCheck::Unneeded,
        )
        .await
    }

    /// Starts the harness under custody with its account lease. A harness whose boundary
    /// leaves processes outside its group running, Full access or the plain Linux Landlock
    /// helper (`check`), has what it left checked when it ends, which a close waits
    /// `STOP_TIME` longer for. Inputs: the wrapped command, the budget, the wall clock the
    /// harness runs under, none for no clock, the lease with the project whose leftover
    /// setting and owner view what it leaves running follow, and the boundary's check.
    /// Output: the running process or a plain code.
    pub(super) async fn spawn_guarded(
        command: Command,
        budget: &ResourceBudget,
        run_time: Option<Duration>,
        lease: Option<(NativeProfileUse, ProjectId)>,
        check: LineageCheck,
    ) -> Result<Self, EngineError> {
        budget.validate().map_err(EngineError)?;
        let deadline = match run_time {
            Some(time) => Some(
                Instant::now()
                    .checked_add(time)
                    .ok_or_else(|| EngineError("codex_host_deadline_range".into()))?,
            ),
            None => None,
        };
        assert_eq!(deadline.is_some(), run_time.is_some());
        let (wake, woken) = fd_wait::wake_pair()
            .map_err(|_| EngineError("codex_host_custodian_unavailable".into()))?;
        let shared = Arc::new(ProcessShared {
            state: Mutex::new(ProcessState {
                started: None,
                output: VecDeque::new(),
                observed: (0, 0),
                status: None,
                complete: false,
                error: None,
                cleanup_error: None,
                doubted: false,
            }),
            stop: AtomicBool::new(false),
            close_stdin: AtomicBool::new(false),
            changed: Notify::new(),
            ended: AtomicBool::new(false),
            wake,
            woken,
            #[cfg(test)]
            custody_passes: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            host_passes: std::sync::atomic::AtomicU64::new(0),
        });
        let (input, receiver) = mpsc::channel(COMMAND_QUEUE_MAX);
        let retained = Arc::clone(&shared);
        let limit = budget.output_bytes;
        let worker = std::thread::Builder::new()
            .name("codex-host-custody".into())
            .spawn(move || {
                let _end = WorkerEnd(Arc::clone(&retained));
                process_custody::run(command, lease, check, receiver, retained, deadline, limit);
            })
            .map_err(|_| EngineError("codex_host_custodian_unavailable".into()))?;
        let close_time = CLOSE_TIME
            + if check != LineageCheck::Unneeded {
                crate::harnesses::leftovers::STOP_TIME
            } else {
                Duration::ZERO
            };
        let mut process = Self {
            shared,
            input,
            worker,
            deadline,
            close_time,
            pid: 0,
            pending_input: None,
            frame_max: FRAME_BYTES_MAX,
        };
        process.await_start().await?;
        assert!(process.pid() > 1);
        Ok(process)
    }

    async fn await_start(&mut self) -> Result<(), EngineError> {
        loop {
            #[cfg(test)]
            self.shared.host_passes.fetch_add(1, Ordering::SeqCst);
            let started = self.shared.lock()?.started;
            if let Some(result) = started {
                self.pid = result.map_err(|code| EngineError(code.into()))?;
                return Ok(());
            }
            if self.worker_ended() {
                return Err(EngineError("codex_host_custodian_stopped".into()));
            }
            self.check_deadline()?;
            self.changed(self.deadline).await;
        }
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    /// The state the process shares with its custody worker, so a row can count the passes
    /// of both loops while it drives the process. Output: the shared state.
    #[cfg(all(test, unix))]
    pub(super) fn shared(&self) -> Arc<ProcessShared> {
        Arc::clone(&self.shared)
    }

    #[cfg(all(test, unix))]
    pub(super) fn observed_bytes(&self) -> (u64, u64) {
        match self.shared.state.lock() {
            Ok(state) => state.observed,
            Err(poisoned) => poisoned.into_inner().observed,
        }
    }

    /// Poisons the custody's shared state with a real panic under its lock, as a panic in
    /// any holder would, so the next output the custody counts fails its accounting.
    /// Input: the host. Output: none.
    #[cfg(all(test, unix))]
    pub(super) fn poison_shared_state(&self) {
        let shared = Arc::clone(&self.shared);
        let poisoned = std::thread::spawn(move || {
            let _held = shared.state.lock();
            panic!("a holder of the custody state panicked");
        })
        .join();
        assert!(poisoned.is_err());
        assert!(self.shared.state.is_poisoned());
    }

    #[cfg(all(test, unix))]
    pub(super) fn exit_status(&self) -> Option<ExitStatus> {
        match self.shared.state.lock() {
            Ok(state) => state.status,
            Err(poisoned) => poisoned.into_inner().status,
        }
    }

    /// Bounds each frame this process takes for the harness by the owner's master frame size
    /// limit (F39) in place of the engine wire frame. Input: the bound in bytes, from
    /// `FRAME_BYTES_MAX` to `master_frame_limit::BYTES_MAX`. Output: none.
    pub(super) fn limit_input(&mut self, bytes: usize) {
        assert!((FRAME_BYTES_MAX..=crate::master_frame_limit::BYTES_MAX).contains(&bytes));
        assert!(self.pending_input.is_none());
        self.frame_max = bytes;
    }

    pub(super) async fn send(&mut self, bytes: &[u8]) -> Result<(), EngineError> {
        if bytes.is_empty() || bytes.len() > self.frame_max + 1 {
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
            self.shared.wake.rouse();
            self.pending_input = Some(PendingInput {
                bytes: bytes.to_vec(),
                reply,
            });
        }
        assert!(self.pending_input.is_some());
        let deadline = self.deadline;
        let pending = self.pending_input.as_mut().expect("retained native input");
        let observed = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                tokio::time::timeout(remaining, &mut pending.reply)
                    .await
                    .ok()
            }
            None => Some((&mut pending.reply).await),
        };
        self.pending_input = None;
        match observed {
            Some(result) => result.map_err(|_| EngineError("codex_host_input_closed".into()))?,
            None => {
                self.shared.fail("codex_host_deadline");
                Err(EngineError("codex_host_deadline".into()))
            }
        }
    }

    pub(super) fn close_stdin(&self) {
        self.shared.close_stdin.store(true, Ordering::Release);
        self.shared.wake.rouse();
    }

    pub(super) fn request_stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.close_stdin();
    }

    /// Says whether the harness completed with no cleanup ever in doubt, so nothing it
    /// started may still run (SA-VID14-R2 F2): false before completion, after a doubt the
    /// completion's own code replaced, and when the state cannot be read. Input: the
    /// process. Output: the answer.
    pub(super) fn settled(&self) -> bool {
        self.shared
            .lock()
            .is_ok_and(|state| state.complete && !state.doubted)
    }

    pub(super) fn operation_result(&self) -> Result<(), EngineError> {
        match self.shared.lock()?.error {
            Some(code) => Err(EngineError(code.into())),
            None => Ok(()),
        }
    }

    pub(super) async fn next_output(&mut self) -> Result<CodexHostOutput, EngineError> {
        loop {
            #[cfg(test)]
            self.shared.host_passes.fetch_add(1, Ordering::SeqCst);
            {
                let mut state = self.shared.lock()?;
                let held = state.complete
                    && state
                        .status
                        .and_then(|status| status.code())
                        .and_then(|code| crate::sandbox::held_stop(std::env::consts::OS, code))
                        .is_some();
                if held {
                    return Err(EngineError(crate::sandbox::HELD_STOP_CAUSE.into()));
                }
                if let Some(code) = state.error.or(state.cleanup_error) {
                    return Err(EngineError(code.into()));
                }
                if let Some(output) = state.output.pop_front() {
                    if state.output.len() + 1 == OUTPUT_QUEUE_MAX {
                        self.shared.wake.rouse();
                    }
                    return Ok(output);
                }
                if state.complete {
                    return state
                        .status
                        .map(|_| CodexHostOutput::Exited)
                        .ok_or_else(|| EngineError("codex_host_reap_missing".into()));
                }
            }
            if self.worker_ended() {
                return Err(EngineError("codex_host_cleanup_uncertain".into()));
            }
            self.check_deadline()?;
            self.changed(self.deadline).await;
        }
    }

    pub(super) async fn close(&mut self) -> Result<ExitStatus, EngineError> {
        self.request_stop();
        let deadline = Instant::now() + self.close_time;
        loop {
            #[cfg(test)]
            self.shared.host_passes.fetch_add(1, Ordering::SeqCst);
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
            if Instant::now() >= deadline || self.worker_ended() {
                return Err(EngineError("codex_host_cleanup_uncertain".into()));
            }
            self.changed(Some(deadline)).await;
        }
    }

    /// Waits until custody reports a change, or [`WAIT_MAX`] passes, at most until `until`, so
    /// the caller re-reads the shared state. A change reported while no one waited is kept as
    /// a permit and ends the next wait at once. Input: the instant to stop at, or none.
    /// Output: none.
    async fn changed(&self, until: Option<Instant>) {
        let mut wait = WAIT_MAX;
        if let Some(until) = until {
            wait = wait.min(until.saturating_duration_since(Instant::now()));
        }
        assert!(wait <= WAIT_MAX);
        let _ = tokio::time::timeout(wait, self.shared.changed.notified()).await;
    }

    /// Whether the custody worker has ended: its end mark, set before the thread counts as
    /// finished, or the finished thread. Input: the process. Output: the answer.
    fn worker_ended(&self) -> bool {
        self.shared.ended.load(Ordering::Acquire) || self.worker.is_finished()
    }

    /// Fails the process once its wall clock has run out; a process with no clock never
    /// fails here. Input: the process. Output: Ok, or `codex_host_deadline`.
    fn check_deadline(&self) -> Result<(), EngineError> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
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
