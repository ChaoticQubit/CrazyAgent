use super::{Busy, Grant, Order, Report, TeamRuntime, error, full_stop};
use crate::agents::Agents;
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::harnesses::interactive::{self, Support, Transcript};
use crate::harnesses::terminal_session::{RecordSource, Resume};
use crate::harnesses::transcript::{TranscriptReader, pending};
use crate::project_setup::StoredSetup;
use crate::protocol::CredentialRef;
use crate::recovery::{self, HeldItem, Interrupted, RunState};
use crate::team::master::{self, InputRefused};
use crate::team::{AgentRole, AgentStatus, launch_harness, names, now_ms};
use crate::terminal::PANES_PER_PROJECT_MAX;
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;
use std::time::{Duration, UNIX_EPOCH};

#[cfg(all(test, unix))]
#[path = "resume/tests.rs"]
mod tests;

const RECORD_MISSING: &str = "its session record is missing";
const RECORD_UNREADABLE: &str = "its session record can no longer be read";
const NOT_RESUMABLE: &str = "its harness session has no id it can be resumed by";
const NO_TERMINAL: &str = "its harness cannot resume in a terminal pane here";
const STOPPED: &str = "it stopped before its session resumed";
const NOTHING_WAITING: &str = "its task or message is no longer waiting";
const TEAMMATE: &str = "a teammate starts only from its harness";
const LIVE_OFF: &str = "the live master is turned off";
const LET_GO: &str = "Its task or message was let go, so it is never typed twice";
const RETRY_ONCE: &str =
    "Its task or message waits for the owner's retry; if the retry fails too, it is let go";
const RETRY_LATER: &str = "Its task or message waits until a retry resumes it";
const UNCONFIRMED: &str =
    "its old terminal was not confirmed stopped (launch_adopt_cleanup_uncertain)";

/// Where the work an interrupted run was doing stands: still waiting for the resume to
/// hand it back, already handed to the resumed agent, or no longer the run's to finish.
enum Work {
    Waiting(Busy),
    Handed,
    Gone,
}

/// Names the harness conversation an interrupted run continues, as the harness resumes
/// it: the id read from the exact session record the run kept, in the run's working folder.
/// Takes the run. Returns the conversation, or the plain reason it cannot be resumed.
pub(super) fn conversation(run: &Interrupted) -> Result<Resume, &'static str> {
    let (Some(pane), Some(cwd), Some(file)) = (&run.record, &run.cwd, &run.record_file) else {
        return Err(RECORD_MISSING);
    };
    if !file.is_file() {
        return Err(RECORD_MISSING);
    }
    let source = match &pane.source {
        RecordSource::CodexRollouts { sessions } => Transcript::CodexRollouts {
            sessions: sessions.clone(),
        },
        RecordSource::ClaudeSession { file } => Transcript::ClaudeSession { file: file.clone() },
    };
    let reader =
        TranscriptReader::resumed(source, cwd, file, None).map_err(|_| RECORD_UNREADABLE)?;
    let (id, record) = reader.conversation().ok_or(RECORD_UNREADABLE)?;
    if !interactive::resumable(&id) {
        return Err(NOT_RESUMABLE);
    }
    Ok(Resume { id, record })
}

/// Writes the note an interrupted agent resumes with: when it was cut short, each command
/// its session record started without a result, and that it verifies before it goes on,
/// then finishes the task or answers the message it had, quoted whole, since its session
/// may not show it when the stop came as it was handed over; a wake task reads as
/// `WAKE_DELIVERED`, since the note carries no messages. The commands may name secrets
/// the agent itself used, so the note goes only to that agent and is never logged or
/// stored. Takes the run and the task's prompt or the message's text, when it still waits.
/// Returns the note, plain text.
pub(super) fn note(run: &Interrupted, item: Option<&str>) -> String {
    let at = u64::try_from(run.at)
        .ok()
        .and_then(|ms| UNIX_EPOCH.checked_add(Duration::from_millis(ms)))
        .unwrap_or(UNIX_EPOCH);
    let unknown = match (&run.record, &run.cwd, &run.record_file) {
        (Some(pane), Some(cwd), Some(record)) => pending::unknown(&pane.source, cwd, record),
        _ => Vec::new(),
    };
    let outcomes = if unknown.is_empty() {
        "No command it started is left with an unknown outcome".to_owned()
    } else {
        let listed: Vec<String> = unknown
            .iter()
            .map(|command| format!("{}: {}", command.tool, command.summary))
            .collect();
        format!(
            "These commands have an unknown outcome: {}",
            listed.join("; ")
        )
    };
    let then = match (run.last_turn.task, run.last_turn.message, item) {
        (Some(_), _, Some(prompt)) => format!(
            " Then finish the task you were given and reply with a short summary of what you did. The task, in case your session does not show it:\n{}",
            if prompt == master::WAKE_TASK {
                master::WAKE_DELIVERED
            } else {
                prompt
            }
        ),
        (Some(_), _, None) => {
            " Then finish the task you were given and reply with a short summary of what you did."
                .to_owned()
        }
        (None, Some(_), Some(text)) => format!(
            " Then answer the message you were answering. The message, in case your session does not show it:\n{text}"
        ),
        (None, Some(_), None) => " Then answer the message you were answering.".to_owned(),
        (None, None, _) => String::new(),
    };
    format!(
        "You were interrupted at {}. {outcomes}. Verify before continuing.{then}",
        crate::research::runner::utc_date(at)
    )
}

impl TeamRuntime {
    /// Resumes each interrupted run of a project in its own native harness session: a live
    /// master gets its note once the live master is back in its conversation, any other
    /// agent starts again in its worktree and home resuming the session it recorded, with
    /// its held task or message and the note. A run that cannot resume is recorded failed
    /// with its reason and waits for the owner's retry; its work never starts fresh.
    /// Takes the setup, the live access and the paused accounts. Returns any SQLite failure.
    pub(super) fn resume_interrupted(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        for run in recovery::open_runs(&self.connection, &stored.project_id)? {
            match run.state {
                RunState::Interrupted
                    if run.kind == AgentRole::Master && run.last_turn.message.is_none() =>
                {
                    self.resume_live(stored, &run)?;
                }
                RunState::Interrupted => self.restart(stored, access, paused, &run)?,
                RunState::Resuming => self.hand(stored, access, &run)?,
                RunState::Resumed | RunState::Failed => {}
            }
        }
        Ok(())
    }

    /// Types the note into the project's live master once it runs the run's agent again in
    /// its kept conversation, briefed and idle; the live master's own start resumes that
    /// conversation. Takes the setup and the run. Returns any SQLite failure.
    fn resume_live(&mut self, stored: &StoredSetup, run: &Interrupted) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if !master::live(&self.connection, project)? {
            return self.fail(run, RunState::Interrupted, LIVE_OFF);
        }
        let Some(id) = self.master(project) else {
            return Ok(());
        };
        if id != run.agent {
            return self.fail(
                run,
                RunState::Interrupted,
                "another live master runs the project now",
            );
        }
        let Some(host) = self.terminal.clone() else {
            return self.fail(run, RunState::Interrupted, NO_TERMINAL);
        };
        let worker = &self.workers[&id];
        if !worker.live || !worker.briefed || worker.stopping {
            return Ok(());
        }
        let typed = master::master_input(
            &self.connection,
            host.as_ref(),
            self.agents.as_deref().map(Agents::typing),
            project,
            &note(run, None),
        );
        match typed {
            Ok(()) => {
                recovery::mark(
                    &self.connection,
                    run.id,
                    RunState::Interrupted,
                    RunState::Resumed,
                    None,
                )?;
                Ok(())
            }
            Err(InputRefused::Invalid) => self.fail(
                run,
                RunState::Interrupted,
                "its note could not be typed into the master",
            ),
            Err(InputRefused::Failed(code)) => self.fail(
                run,
                RunState::Interrupted,
                &format!("its note could not be typed into the master: {code}"),
            ),
            Err(
                InputRefused::NoMaster
                | InputRefused::Busy
                | InputRefused::Unready
                | InputRefused::OwnerTyping,
            ) => Ok(()),
        }
    }

    /// Starts a stopped agent again under its own id, worktree and home, resuming the
    /// harness session its run recorded, then hands it the run's work. A sub-agent that
    /// still runs in the pane this engine took over gets its work there instead. Waits
    /// while any other agent still runs, the team has no room or an account still waits for
    /// its sign-in re-check; fails the run when it cannot resume.
    /// Takes the setup, live access, paused accounts and the run. Returns any SQLite failure.
    fn restart(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
        run: &Interrupted,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if self.workers.contains_key(&run.agent) {
            return self.resume_in_pane(paused, run);
        }
        let resume = match conversation(run) {
            Ok(resume) => resume,
            Err(NOT_RESUMABLE) => return self.give_up(run, RunState::Interrupted, NOT_RESUMABLE),
            Err(reason) => return self.fail(run, RunState::Interrupted, reason),
        };
        let terminal = launch_harness(&run.harness)
            .is_some_and(|launch| interactive::support(launch) == Support::Terminal);
        if !terminal {
            return self.give_up(run, RunState::Interrupted, NO_TERMINAL);
        }
        if self.terminal.is_none() {
            return self.fail(run, RunState::Interrupted, NO_TERMINAL);
        }
        let Work::Waiting(busy) = self.work(run)? else {
            return self.give_up(run, RunState::Interrupted, NOTHING_WAITING);
        };
        let tools = match run.kind {
            AgentRole::Teammate => return self.give_up(run, RunState::Interrupted, TEAMMATE),
            AgentRole::Sub => {
                let Some(slot) = super::super::slot(&stored.team, &run.harness) else {
                    return self.fail(
                        run,
                        RunState::Interrupted,
                        "the team plan no longer has its harness",
                    );
                };
                let subs_max = usize::from(stored.team.max_agents.saturating_sub(1));
                if self.live(project, AgentRole::Sub, Some(&run.harness))
                    >= usize::from(slot.agents)
                    || self.live(project, AgentRole::Sub, None) >= subs_max
                {
                    return Ok(());
                }
                slot.tools
            }
            AgentRole::Master => {
                if stored.team.master != run.harness {
                    return self.fail(
                        run,
                        RunState::Interrupted,
                        "the team plan has another master harness now",
                    );
                }
                if master::live(&self.connection, project)? {
                    return self.fail(run, RunState::Interrupted, "the master runs live now");
                }
                if self.master(project).is_some() {
                    return Ok(());
                }
                stored.team.master_tools
            }
        };
        if self.panes(project) >= PANES_PER_PROJECT_MAX {
            return Ok(());
        }
        let account = match self.account_for(access, project, &run.harness, paused) {
            Ok(Some(account)) => account,
            Ok(None) => return Ok(()),
            Err(reason) => {
                return self.fail_for_now(
                    run,
                    RunState::Interrupted,
                    &format!("no account can run it: {reason}"),
                );
            }
        };
        self.resuming = Some((run.agent, resume));
        let revived = self.revive(
            stored,
            run.agent,
            run.kind,
            &run.harness,
            tools,
            &account,
            None,
        );
        self.resuming = None;
        if let Err(reason) = revived? {
            return self.fail(
                run,
                RunState::Interrupted,
                &format!("it could not start again: {reason}"),
            );
        }
        self.workers
            .get_mut(&run.agent)
            .expect("a revived agent has a worker")
            .busy = Some(busy);
        recovery::mark(
            &self.connection,
            run.id,
            RunState::Interrupted,
            RunState::Resuming,
            None,
        )?;
        self.hand(stored, access, run)
    }

    /// Hands a restarted agent its run's work with the note, once: the turn goes out with a
    /// signed-in account's profile when its account is managed, and the work is marked
    /// handed without a new attempt. A profile still in use waits for the next tick.
    /// Takes the setup, live access and the run. Returns any SQLite failure.
    fn hand(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        run: &Interrupted,
    ) -> Result<(), EngineError> {
        let Some(worker) = self.workers.get(&run.agent) else {
            return self.fail(run, RunState::Resuming, STOPPED);
        };
        if worker.stopping {
            return Ok(());
        }
        let managed = worker.managed;
        let busy = match self.work(run)? {
            Work::Waiting(busy) => busy,
            Work::Handed => return Ok(()),
            Work::Gone => {
                self.stop(run.agent);
                return self.give_up(run, RunState::Resuming, NOTHING_WAITING);
            }
        };
        let grant = if managed {
            match self.grant(access, run.agent, &stored.project_id) {
                Ok(Some(grant)) => Some(grant),
                Ok(None) => return Ok(()),
                Err(reason) => {
                    self.stop(run.agent);
                    return self.fail_for_now(
                        run,
                        RunState::Resuming,
                        &format!("its account could not be handed to it: {reason}"),
                    );
                }
            }
        } else {
            None
        };
        self.send_work(run, busy, grant).map(drop)
    }

    /// Hands an interrupted sub-agent that runs again in the pane this engine took over the
    /// task its run holds, with the note, typed as one turn into the session it never left,
    /// once the takeover settled (its worker is ready), its worker is free and runs no turn
    /// of its own, a kept native lead has its signed-in account again (it is briefed), and
    /// its account is not paused; the session is already running, so the run is resumed
    /// once the turn goes out.
    /// A takeover that fails ends the worker with nothing handed, so the run restarts
    /// in a new pane once the old pane's processes were confirmed gone; otherwise it waits
    /// for the owner's retry (see [`Self::hold_unconfirmed`]). A task no longer waiting gives
    /// the run up. Any other agent that runs again keeps the run waiting until it stops.
    /// Takes the paused accounts and the run. Returns any SQLite or ledger failure.
    fn resume_in_pane(
        &mut self,
        paused: &BTreeSet<CredentialRef>,
        run: &Interrupted,
    ) -> Result<(), EngineError> {
        let worker = &self.workers[&run.agent];
        let handed = worker.kept_session && worker.briefed;
        if run.kind != AgentRole::Sub
            || !worker.ready
            || (worker.managed && !handed)
            || worker.own_turn.is_some()
            || worker.stopping
            || worker.busy.is_some()
            || paused.contains(&worker.connection)
        {
            return Ok(());
        }
        let Work::Waiting(busy @ Busy::Task(_)) = self.work(run)? else {
            return self.give_up(run, RunState::Interrupted, NOTHING_WAITING);
        };
        self.workers
            .get_mut(&run.agent)
            .expect("a running agent has a worker")
            .busy = Some(busy);
        if !self.send_work(run, busy, None)? {
            self.workers
                .get_mut(&run.agent)
                .expect("a running agent has a worker")
                .busy = None;
            return Ok(());
        }
        recovery::mark(
            &self.connection,
            run.id,
            RunState::Interrupted,
            RunState::Resumed,
            None,
        )?;
        Ok(())
    }

    /// Types a run's note with the task or message its agent's worker holds into that
    /// agent as one turn, bounded by the project's task and session limits as every turn is,
    /// then marks the item handed to it and the agent working. Takes the run, the item and
    /// the turn's account grant, if any. Returns whether the turn went out, or any SQLite
    /// failure or stored time limit the engine refuses.
    fn send_work(
        &mut self,
        run: &Interrupted,
        busy: Busy,
        grant: Option<Box<Grant>>,
    ) -> Result<bool, EngineError> {
        let item = match busy {
            Busy::Task(task) => self.connection.query_row(
                "SELECT prompt FROM team_tasks WHERE id=?1",
                [task],
                |row| row.get::<_, String>(0),
            ),
            Busy::Message(message) => self.connection.query_row(
                "SELECT text FROM team_messages WHERE id=?1",
                [message],
                |row| row.get::<_, String>(0),
            ),
        }
        .map_err(error)?;
        let bound = super::turn_bound(
            crate::team::time_limits(&self.connection, &run.project)?,
            self.workers[&run.agent].ran(),
        );
        let background = match busy {
            Busy::Task(_) => crate::team::background_at_end(&self.connection, &run.project)?,
            Busy::Message(_) => crate::team::BackgroundAtEnd::Wait,
        };
        let worker = self
            .workers
            .get_mut(&run.agent)
            .expect("the agent handed its work has a worker");
        assert_eq!(worker.busy, Some(busy));
        if worker
            .orders
            .send(Order::Turn(
                note(run, Some(&item)),
                grant,
                super::TurnRules {
                    limit: bound.map(|(limit, _)| limit),
                    background,
                },
            ))
            .is_err()
        {
            return Ok(false);
        }
        worker.bound = bound.map(|(_, by)| by);
        match busy {
            Busy::Task(task) => {
                let now = now_ms();
                let started = self
                    .connection
                    .execute(
                        "UPDATE team_tasks SET status='running', agent=?2, attempts=attempts+1, started_ms=?3, error=NULL
                         WHERE id=?1 AND status='queued'",
                        params![task, run.agent, now],
                    )
                    .map_err(error)?;
                assert!(started <= 1);
                if started == 1 {
                    let project = &run.project;
                    let named = names::name(&self.connection, run.agent)?
                        .and_then(|name| name.handle)
                        .is_some();
                    if !named {
                        names::assign_task_role(&self.connection, project, run.agent, task)?;
                    }
                    crate::activity::note_task(&self.connection, project, run.agent, task, now);
                }
                self.mark_agent(run.agent, AgentStatus::Working, Some(task))?;
            }
            Busy::Message(message) => {
                self.connection
                    .execute(
                        "UPDATE team_messages SET state='sent' WHERE id=?1 AND state='pending'",
                        [message],
                    )
                    .map_err(error)?;
                self.mark_agent(run.agent, AgentStatus::Working, None)?;
            }
        }
        Ok(true)
    }

    /// Follows one worker report for recovery before the runtime applies it: an agent that
    /// starts its harness for each turn keeps that turn's session record while it runs, and
    /// a run being resumed is resumed once its session is ready, or put back when its
    /// session ended first: failed with the reason, or waiting again when the team paused.
    /// Takes the agent and the report. Returns any SQLite failure.
    pub(super) fn follow_resume(&mut self, id: i64, report: &Report) -> Result<(), EngineError> {
        if !matches!(
            report,
            Report::Ready { .. } | Report::Finished { .. } | Report::Ended { .. }
        ) {
            return Ok(());
        }
        let Some(worker) = self.workers.get(&id) else {
            return Ok(());
        };
        let per_turn = worker.managed && !worker.live;
        match report {
            Report::Ready {
                pane: Some(record), ..
            } if per_turn => recovery::remember(&self.connection, id, record)?,
            Report::Finished { .. } if per_turn => recovery::forget(&self.connection, id)?,
            Report::Ended { .. } => recovery::forget(&self.connection, id)?,
            _ => {}
        }
        let Some(run) = recovery::resuming(&self.connection, id)? else {
            return Ok(());
        };
        let cause = match report {
            Report::Ready { .. } => {
                recovery::mark(
                    &self.connection,
                    run.id,
                    RunState::Resuming,
                    RunState::Resumed,
                    None,
                )?;
                return Ok(());
            }
            Report::Finished { cause, .. } => cause.as_deref(),
            Report::Ended { problem } => problem.as_deref(),
            _ => return Ok(()),
        };
        let worker = self.workers.get_mut(&id).expect("worker exists");
        let held = worker.held;
        let pin = worker.narrowed.is_none().then_some(id);
        if let Some(busy) = worker.busy.take() {
            self.release(pin, busy)?;
        }
        if held {
            recovery::mark(
                &self.connection,
                run.id,
                RunState::Resuming,
                RunState::Interrupted,
                None,
            )?;
            return Ok(());
        }
        let reason = format!(
            "its session did not resume: {}",
            cause.unwrap_or("its harness stopped")
        );
        self.fail(&run, RunState::Resuming, &reason)
    }

    /// Holds the interrupted run of an agent whose kept pane could not be taken over and
    /// whose processes were not confirmed gone: failed for now with that reason, its task or
    /// message kept, so no new pane starts beside them before the owner's retry.
    /// Takes the agent. Returns any SQLite failure.
    pub(super) fn hold_unconfirmed(&mut self, id: i64) -> Result<(), EngineError> {
        let project = self.workers[&id].project.clone();
        for run in recovery::open_runs(&self.connection, &project)? {
            if run.agent == id && run.state == RunState::Interrupted {
                self.fail_for_now(&run, RunState::Interrupted, UNCONFIRMED)?;
            }
        }
        Ok(())
    }

    /// Reads where the run's task or message stands. Takes the run. Returns it.
    fn work(&self, run: &Interrupted) -> Result<Work, EngineError> {
        match (run.last_turn.task, run.last_turn.message) {
            (Some(task), _) => {
                let row: Option<(String, Option<i64>)> = self
                    .connection
                    .query_row(
                        "SELECT status,agent FROM team_tasks WHERE id=?1 AND project=?2",
                        params![task, run.project.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(error)?;
                Ok(match row {
                    Some((status, _)) if status == "queued" => Work::Waiting(Busy::Task(task)),
                    Some((status, Some(agent))) if status == "running" && agent == run.agent => {
                        Work::Handed
                    }
                    _ => Work::Gone,
                })
            }
            (None, Some(message)) => {
                let state: Option<String> = self
                    .connection
                    .query_row(
                        "SELECT state FROM team_messages WHERE id=?1 AND project=?2",
                        params![message, run.project.as_str()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(error)?;
                Ok(match state.as_deref() {
                    Some("pending") => Work::Waiting(Busy::Message(message)),
                    Some("sent") => Work::Handed,
                    _ => Work::Gone,
                })
            }
            (None, None) => Ok(Work::Gone),
        }
    }

    /// Stops a restarted agent that will not get its run's work, freeing what it held.
    fn stop(&mut self, id: i64) {
        let worker = self.workers.get_mut(&id).expect("worker exists");
        worker.busy = None;
        worker.stopping = true;
        let _ = worker.orders.send(Order::Stop);
    }

    /// Gives up a run that can never resume, or whose work is gone: records it failed with
    /// its reason, then lets go of its task or message and settles that item's delivery as
    /// an engine start settles one no run holds, so the item never waits behind the run and
    /// is never typed twice. Takes the run, the state it is in and the plain reason.
    /// Returns any SQLite or ledger failure.
    fn give_up(
        &mut self,
        run: &Interrupted,
        from: RunState,
        reason: &str,
    ) -> Result<(), EngineError> {
        self.failed(run, from, reason, None)
    }

    /// Records a run's resume failed with its reason and tells the project once, so the
    /// owner can retry it; a run the owner already retried gives up instead, as
    /// [`Self::give_up`] does, so its task or message waits behind no more retries.
    /// Takes the run, the state it is in and the plain reason. Returns any SQLite failure.
    fn fail(&mut self, run: &Interrupted, from: RunState, reason: &str) -> Result<(), EngineError> {
        let retried = recovery::retried(&self.connection, run.id)?;
        self.failed(run, from, reason, (!retried).then_some(RETRY_ONCE))
    }

    /// Records a run's resume failed for a reason that passes with time, such as no
    /// account free to run it during a pool-wide limit, and tells the project once; the run
    /// keeps its task or message through every retry, so a later retry can still resume it.
    /// Takes the run, the state it is in and the plain reason. Returns any SQLite failure.
    fn fail_for_now(
        &mut self,
        run: &Interrupted,
        from: RunState,
        reason: &str,
    ) -> Result<(), EngineError> {
        self.failed(run, from, reason, Some(RETRY_LATER))
    }

    /// Records a run's resume failed with its reason and tells the project once, with what
    /// becomes of its task or message, then, when it gives up and it was in the expected
    /// state, lets go of that item and settles its delivery. Takes the run, its state, the
    /// plain reason and, for a run that keeps its item, the sentence that says how long.
    /// Returns any SQLite or ledger failure.
    fn failed(
        &mut self,
        run: &Interrupted,
        from: RunState,
        reason: &str,
        kept: Option<&str>,
    ) -> Result<(), EngineError> {
        if !recovery::mark(
            &self.connection,
            run.id,
            from,
            RunState::Failed,
            Some(reason),
        )? {
            return Ok(());
        }
        let then = kept.unwrap_or(LET_GO);
        self.note(
            &run.project,
            &full_stop(format!(
                "An agent cut short when CrazyAgent stopped could not resume: {reason}. {then}"
            )),
        )?;
        if kept.is_some() {
            return Ok(());
        }
        let (held, busy) = match (run.last_turn.task, run.last_turn.message) {
            (Some(task), _) => (HeldItem::Task(task), Busy::Task(task)),
            (None, Some(message)) => (HeldItem::Message(message), Busy::Message(message)),
            (None, None) => return Ok(()),
        };
        recovery::release(&self.connection, held)?;
        super::delivery::settle_released(&self.connection, busy)
    }
}
