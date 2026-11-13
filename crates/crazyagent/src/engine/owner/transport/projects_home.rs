use super::*;
use crate::project_setup::{NewProjectPlan, Staged};
use crate::projects_home::{
    AddMode, AddOverride, AddPlan, DeletePlan, HomeEffect, MovePlan, Moved, OldSource, Placed,
    ProjectMove, folder_size,
};
use std::sync::atomic::{AtomicU64, Ordering};

const SIZE_MARGIN: Duration = Duration::from_millis(250);
const UNKNOWN: &str = "owner_outcome_unknown_reconcile_same_identity";
const UNSETTLED: [&str; 2] = [UNKNOWN, "owner_actor_stopped_outcome_unknown"];
const OLD_FOLDER_LEFT: &str = "move_project_old_folder_left";
const SETTLE_TRIES: usize = 3;
const SETTLE_GAP: Duration = Duration::from_millis(50);
static EXCHANGES: AtomicU64 = AtomicU64::new(1);

/// The owner changes this engine is running, each by its owner and request id, so a resend
/// or a check of one still running answers `InProgress` instead of planning it again.
pub(super) type RunningSet = Mutex<std::collections::HashSet<(String, RequestId)>>;

/// One owner change's claim on its request id while this exchange runs it. Dropping it,
/// after the record and the reply, lets a later resend reach the recorded result.
struct Running<'a> {
    set: &'a RunningSet,
    key: (String, RequestId),
}

impl<'a> Running<'a> {
    /// Claims a request id for this run. Inputs: the set, the owner and the request id.
    /// Output: the claim, or None while another exchange runs the same id.
    fn claim(set: &'a RunningSet, owner_id: &str, id: &RequestId) -> Option<Self> {
        assert!(!owner_id.is_empty());
        let key = (owner_id.to_owned(), id.clone());
        let mut held = set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            held.len() < DEFERRED_MAX,
            "each run holds one deferred place"
        );
        if !held.insert(key.clone()) {
            return None;
        }
        assert!(held.contains(&key));
        Some(Self { set, key })
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut held = self
            .set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = held.remove(&self.key);
        assert!(removed, "a claim frees only its own request id");
    }
}

/// Whether this engine is running an owner change. Inputs: the authority, the admission
/// and the change's request id. Output: true from the change's first plan until its reply.
pub(super) fn running(authority: &Authority, a: &Admission, id: &RequestId) -> bool {
    let held = authority
        .running
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(held.len() <= DEFERRED_MAX);
    held.contains(&(a.owner_id.clone(), id.clone()))
}

/// The answer for a resend or a check of a change still running, after the grant check
/// every owner answer passes under the owner barrier. Inputs: the engine, the authority, the
/// client packet, the admission and the change's request id. Output: `InProgress` for the
/// change, or the grant's refusal.
pub(super) async fn still_running(
    engine: &Engine,
    authority: &Authority,
    packet: &[u8; 37],
    a: &Admission,
    id: &RequestId,
) -> Result<OwnerResponse, EngineError> {
    let _barrier = engine.owner_egress.write().await;
    match authority.authenticate(packet) {
        Ok(current) if &current == a => Ok(OwnerResponse::InProgress {
            operation_id: id.clone(),
        }),
        Ok(_) => Err(failure("owner_grant_stale_or_revoked")),
        Err(e) => Err(e),
    }
}

enum Step {
    Reply(Box<OwnerResponse>),
    Record(HomeEffect),
    Staged(Result<Staged, String>),
    Size(crate::projects_home::Measured),
    Deleted(Result<(PathBuf, Vec<crate::projects_home::RecordedFile>), String>),
}

enum Work {
    Add(AddPlan),
    Move(MovePlan),
    MoveProject(ProjectMove),
    New(NewProjectPlan),
    Demo(NewProjectPlan),
    Delete(DeletePlan, Vec<i64>),
}

impl Work {
    fn interrupted(&self) -> &'static str {
        match self {
            Self::Add(_) => "home_add_interrupted",
            Self::Move(_) => "home_move_interrupted",
            Self::MoveProject(_) => "move_project_interrupted",
            Self::New(_) | Self::Demo(_) => "home_new_project_interrupted",
            Self::Delete(..) => "project_delete_interrupted",
        }
    }

    /// Runs the disk work. Output: the step to record and, for a project move, what it
    /// leaves for after the record.
    fn run(self) -> (Step, After) {
        let step = match self {
            Self::Add(plan) => {
                let source = plan.source().to_path_buf();
                Step::Record(plan.run().map(|()| Some(source)).map_err(|e| e.0))
            }
            Self::Move(plan) => Step::Record(plan.run().map(|()| None).map_err(|e| e.0)),
            Self::MoveProject(plan) => {
                let source = plan.source().to_path_buf();
                return match plan.run().map(Moved::into_parts) {
                    Ok((placed, landing)) => (
                        Step::Record(Ok(Some(source))),
                        After {
                            old_folder: match placed {
                                Placed::Whole => None,
                                Placed::Copied(old) => Some(old),
                            },
                            landing: Some(landing),
                        },
                    ),
                    Err(e) => (Step::Record(Err(e.0)), After::default()),
                };
            }
            Self::New(plan) => Step::Staged(plan.run()),
            Self::Demo(plan) => Step::Staged(plan.run().and_then(seeded)),
            Self::Delete(plan, research) => Step::Deleted(plan.run().map(|()| {
                let files = plan.engine_files(&research);
                (plan.database, files)
            })),
        };
        (step, After::default())
    }
}

/// Writes the demo web page into a staged New Project folder and commits it.
/// Input: the staged folder. Output: the same staged folder, or the seed's code
/// with the staging folder removed, as a failed New Project leaves it.
fn seeded(staged: Staged) -> Result<Staged, String> {
    match crate::demo::seed(staged.staging()) {
        Ok(()) => Ok(staged),
        Err(code) => {
            let _ = std::fs::remove_dir_all(staged.staging());
            Err(code.into())
        }
    }
}

/// What a project move's disk work leaves for after its record: the old folder of a move
/// that went by copy, cleared only once the record is applied, and the landing holds,
/// dropped once the record was tried.
#[derive(Default)]
struct After {
    old_folder: Option<OldSource>,
    landing: Option<crate::landing::RepositoryHolds>,
}

enum Planned {
    Step(Step),
    Work(Work),
}

/// True for the owner requests that touch folders outside the engine data:
/// AddExistingFolder, MoveProjectsHome that moves the projects, MoveProjectHome,
/// SetupProject (New Project), CreateDemo (the demo's New Project with its web page),
/// DeleteProject (Delete everything), FolderSize and ProjectFiles.
/// These run their slow disk work here, off the storage actor, and only then
/// reach the engine database.
pub(super) fn handles(request: &OwnerRequest) -> bool {
    matches!(
        &request.operation,
        OwnerOperation::Apply {
            mutation: OwnerMutation::AddExistingFolder { .. }
                | OwnerMutation::MoveProjectsHome {
                    move_projects: true,
                    ..
                }
                | OwnerMutation::MoveProjectHome { .. }
                | OwnerMutation::SetupProject { .. }
                | OwnerMutation::CreateDemo { .. }
                | OwnerMutation::DeleteProject { .. }
        } | OwnerOperation::Read {
            query: OwnerRead::FolderSize { .. } | OwnerRead::ProjectFiles { .. }
        }
    )
}

/// Serves AddExistingFolder, MoveProjectsHome, MoveProjectHome, SetupProject, CreateDemo
/// and FolderSize for one owner. A change claims its request id first: a resend of one this
/// engine still runs answers `InProgress` and plans nothing (DEC-159). Input: the stream,
/// engine, authority, owner packet and admission, the validated request and the exchange
/// deadline. Output: the
/// reply written under the owner egress barrier. The exchange deadline bounds
/// the replay check, the plan, a grant check under the owner barrier and, for a
/// home move, a recheck of agent work under the move's hold just before the disk
/// work, which runs under its own bounds and without the barrier; a move the
/// recheck refuses is recorded as Rejected with its code and never runs; a fresh owner deadline then bounds the record and the reply, so a finished
/// effect is recorded as Applied or as Rejected with the refusal code. A home
/// move then sends a settle job for this exchange's pending move, trying again
/// when that fails: one whose
/// record did not finish it (a stale grant, a slow barrier, a failed record)
/// is finished from disk, and one whose disk work never ran is forgotten. A project
/// move (MoveProjectHome) stops the project's IDE server and refuses its opens from
/// before its wait for the team until this exchange ends, and is settled the same way
/// after the reply, a Copy never from disk alone. A project move that went by copy keeps
/// its whole old folder until the record is applied; only then, outside the barrier, are
/// the entries its check matched deleted, and when any stays the reply is
/// `move_project_old_folder_left` for a move that is recorded and complete in the home.
/// A Delete
/// everything stops the project's IDE server before its wait and moves, and forgets
/// its plan before its reply, trying again when that fails, so the reply means projects
/// home moves may go again; a plan the reply could not reach is forgotten after it.
/// The exchange number is unique within the engine process; engine start finishes
/// any move and forgets any delete plan left by an earlier process before serving.
pub(super) async fn serve(
    stream: &mut UnixStream,
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    request: OwnerRequest,
    deadline: Instant,
) -> Result<(), EngineError> {
    assert!(handles(&request));
    let id = request.request_id.clone();
    let mutation = matches!(request.operation, OwnerOperation::Apply { .. });
    let exchange = EXCHANGES.fetch_add(1, Ordering::Relaxed);
    assert!(exchange > 0);
    let _running = match mutation {
        true => match Running::claim(&authority.running, &a.owner_id, &id) {
            Some(running) => Some(running),
            None => {
                let reply =
                    still_running(engine, authority, packet, a, &id)
                        .await
                        .map(|response| OwnerReply {
                            request_id: id.clone(),
                            response,
                        });
                return write_owner_reply(stream, reply, id, mutation).await;
            }
        },
        false => None,
    };
    let planned = tokio::time::timeout_at(deadline, plan(engine, a, &request, deadline, exchange))
        .await
        .unwrap_or_else(|_| Err(failure(UNKNOWN)));
    let moving = matches!(
        &request.operation,
        OwnerOperation::Apply {
            mutation: OwnerMutation::MoveProjectsHome { .. }
        }
    );
    let deleting = matches!(
        &request.operation,
        OwnerOperation::Apply {
            mutation: OwnerMutation::DeleteProject { .. }
        }
    );
    let moving_project = matches!(
        &request.operation,
        OwnerOperation::Apply {
            mutation: OwnerMutation::MoveProjectHome { .. }
        }
    );
    #[cfg(unix)]
    let mut _moved_ide = None;
    let mut after = After::default();
    let (step, deadline, ran) = match planned {
        Ok(Planned::Work(work)) => {
            let admitted = tokio::time::timeout_at(deadline, async {
                let _barrier = engine.owner_egress.write().await;
                match authority.authenticate(packet) {
                    Ok(current) if &current == a => Ok(()),
                    Ok(_) => Err(failure("owner_grant_stale_or_revoked")),
                    Err(e) => Err(e),
                }
            })
            .await
            .unwrap_or_else(|_| Err(failure(UNKNOWN)));
            let admitted = match (admitted, &work) {
                (Ok(()), Work::Move(_)) => tokio::time::timeout_at(
                    deadline,
                    engine.owner_job(|reply| Job::HomeMoveRecheck(exchange, reply)),
                )
                .await
                .unwrap_or_else(|_| Err(failure(UNKNOWN))),
                (admitted, _) => admitted.map(Ok),
            };
            match admitted {
                Ok(Ok(())) => {
                    #[cfg(unix)]
                    let _ide_refused = match &work {
                        Work::Delete(plan, _) => {
                            Some(authority.ide_servers.hold_for_delete(&plan.project).await)
                        }
                        _ => None,
                    };
                    #[cfg(unix)]
                    if let Work::MoveProject(plan) = &work {
                        _moved_ide =
                            Some(authority.ide_servers.hold_for_delete(plan.project()).await);
                    }
                    let interrupted = work.interrupted();
                    let step = tokio::task::spawn_blocking(move || work.run())
                        .await
                        .map_err(|_| failure(interrupted))
                        .map(|(step, left)| {
                            after = left;
                            step
                        });
                    (step, Instant::now() + engine.timeout, true)
                }
                Ok(Err(code)) => (Ok(Step::Record(Err(code))), deadline, false),
                Err(e) => (Err(e), deadline, false),
            }
        }
        Ok(Planned::Step(step)) => (Ok(step), deadline, false),
        Err(e) => (Err(e), deadline, false),
    };
    let staging = match &step {
        Ok(Step::Staged(Ok(staged))) => Some(staged.staging().to_path_buf()),
        _ => None,
    };
    let deleted = match &step {
        Ok(Step::Deleted(Ok(files))) => Some(files.clone()),
        _ => None,
    };
    let answer = async {
        let mut barrier = Some(engine.owner_egress.write().await);
        let mut result = match authority.authenticate(packet) {
            Ok(current) if &current == a => finish(engine, a, request, step).await,
            Ok(_) => Err(failure("owner_grant_stale_or_revoked")),
            Err(e) => Err(e),
        };
        drop(after.landing.take());
        if let Some(old) = after.old_folder.take()
            && matches!(
                &result,
                Ok(OwnerReply { response: OwnerResponse::Applied { operation }, .. })
                    if operation.result == OwnerResult::Applied
            )
        {
            drop(barrier.take());
            let cleared = tokio::task::spawn_blocking(move || old.clear()).await;
            barrier = Some(engine.owner_egress.write().await);
            result = match authority.authenticate(packet) {
                Ok(current) if &current == a => match cleared {
                    Ok(Ok(())) => result,
                    _ => Err(failure(OLD_FOLDER_LEFT)),
                },
                Ok(_) => Err(failure("owner_grant_stale_or_revoked")),
                Err(e) => Err(e),
            };
        }
        let settled = !matches!(&result, Err(e) if UNSETTLED.contains(&e.0.as_str()));
        if let Some(staging) = staging
            && settled
        {
            remove_staging(staging).await;
        }
        let removed = matches!(
            &result,
            Ok(OwnerReply { response: OwnerResponse::Applied { operation }, .. })
                if operation.result == OwnerResult::Applied && !operation.historical_replay
        );
        let forgotten = deleting && settle_delete(engine, exchange).await;
        let written = write_owner_reply(stream, result, id, mutation).await;
        drop(barrier);
        (written, removed, forgotten)
    };
    let answered = tokio::time::timeout_at(deadline, answer).await;
    if let (Ok((_, true, _)), Some((database, files))) = (&answered, deleted) {
        let done = tokio::task::spawn_blocking(move || {
            crate::projects_home::remove_engine_files(&database, &files)
        })
        .await
        .unwrap_or_default();
        if let Err(e) = engine
            .owner_job(|reply| Job::DeleteFilesClear(done, reply))
            .await
        {
            eprintln!("project_delete_files_not_cleared cause={}", e.0);
        }
    }
    let forgotten = matches!(&answered, Ok((_, _, true)));
    let answered = answered.map(|(written, _, _)| written);
    if moving {
        settled(engine, "projects_home_move_not_settled", |reply| {
            Job::HomeMoveSettle(exchange, ran, reply)
        })
        .await;
    }
    if moving_project {
        settled(engine, "project_move_not_settled", |reply| {
            Job::HomeProjectMoveSettle(exchange, ran, reply)
        })
        .await;
    }
    if deleting && !forgotten {
        settle_delete(engine, exchange).await;
    }
    answered.map_err(|_| failure(UNKNOWN))?
}

/// Forgets a Delete everything's plan. Inputs: the engine and the exchange. Output: whether
/// the plan is gone.
async fn settle_delete(engine: &Arc<Engine>, exchange: u64) -> bool {
    settled(engine, "project_delete_not_settled", |reply| {
        Job::DeleteSettle(exchange, reply)
    })
    .await
}

/// Runs one settling job, trying again after a full queue or a SQLite failure, since a row
/// left behind refuses later projects home changes until the engine restarts. Inputs: the
/// engine, the log line's name and the job for one try. Output: whether it settled.
async fn settled(
    engine: &Arc<Engine>,
    name: &str,
    job: impl Fn(oneshot::Sender<Result<(), EngineError>>) -> Job,
) -> bool {
    for attempt in 0..SETTLE_TRIES {
        if attempt > 0 {
            tokio::time::sleep(SETTLE_GAP).await;
        }
        match engine.owner_job(&job).await {
            Ok(()) => return true,
            Err(e) => eprintln!("{name} cause={}", e.0),
        }
    }
    false
}

async fn plan(
    engine: &Arc<Engine>,
    a: &Admission,
    request: &OwnerRequest,
    deadline: Instant,
    exchange: u64,
) -> Result<Planned, EngineError> {
    let mutation = match &request.operation {
        OwnerOperation::Read {
            query: OwnerRead::FolderSize { path },
        } => return measure(path, deadline).await.map(Planned::Step),
        OwnerOperation::Read {
            query:
                OwnerRead::ProjectFiles {
                    project_id,
                    query,
                    limit,
                },
        } => {
            return files(engine, a, project_id, query, *limit, deadline)
                .await
                .map(Planned::Step);
        }
        OwnerOperation::Apply { mutation } => mutation,
        _ => return Err(failure("owner_request_invalid")),
    };
    if let Some(step) = replayed(engine, a, request).await? {
        return Ok(Planned::Step(step));
    }
    let expected = request.expected_revision.clone();
    let work = match mutation {
        OwnerMutation::AddExistingFolder {
            source,
            mode,
            overrides,
        } => {
            let (source, mode, overrides): (String, AddMode, Vec<AddOverride>) =
                (source.clone(), *mode, overrides.clone());
            engine
                .owner_job(|reply| {
                    Job::HomeAddPlan(a.clone(), expected, source, mode, overrides, reply)
                })
                .await
                .map(Work::Add)
        }
        OwnerMutation::MoveProjectsHome { path, .. } => {
            let picked = path.clone();
            let id = request.request_id.clone();
            engine
                .owner_job(|reply| {
                    Job::HomeMovePlan(a.clone(), expected, picked, id, exchange, reply)
                })
                .await
                .map(Work::Move)
        }
        OwnerMutation::MoveProjectHome {
            project_id,
            mode,
            overrides,
        } => {
            let (project, mode, overrides): (ProjectId, AddMode, Vec<AddOverride>) =
                (project_id.clone(), *mode, overrides.clone());
            let id = request.request_id.clone();
            engine
                .owner_job(|reply| {
                    Job::HomeProjectMovePlan(
                        a.clone(),
                        expected,
                        project,
                        mode,
                        overrides,
                        id,
                        exchange,
                        reply,
                    )
                })
                .await
                .map(Work::MoveProject)
        }
        OwnerMutation::DeleteProject { .. } => {
            let request = Box::new(request.clone());
            return Ok(
                match engine
                    .owner_job(|reply| Job::DeletePlan(a.clone(), request, exchange, reply))
                    .await?
                {
                    Ok((plan, research)) => Planned::Work(Work::Delete(plan, research)),
                    Err(code) => Planned::Step(Step::Deleted(Err(code))),
                },
            );
        }
        OwnerMutation::SetupProject { .. } | OwnerMutation::CreateDemo { .. } => {
            let demo = matches!(mutation, OwnerMutation::CreateDemo { .. });
            let request = Box::new(request.clone());
            return Ok(
                match engine
                    .owner_job(|reply| Job::SetupPlan(a.clone(), request, reply))
                    .await?
                {
                    Ok(plan) if demo => Planned::Work(Work::Demo(plan)),
                    Ok(plan) => Planned::Work(Work::New(plan)),
                    Err(code) => Planned::Step(Step::Staged(Err(code))),
                },
            );
        }
        _ => return Err(failure("owner_request_invalid")),
    };
    Ok(match work {
        Ok(work) => Planned::Work(work),
        Err(e) => Planned::Step(Step::Record(Err(e.0))),
    })
}

/// Removes a New Project's staging folder once the record job has answered
/// and the folder was not moved into place: a refusal, a replay of an earlier
/// receipt or a failed record. Input: the staging folder.
async fn remove_staging(staging: std::path::PathBuf) {
    let _ = tokio::task::spawn_blocking(move || {
        if std::fs::symlink_metadata(&staging).is_ok() {
            let _ = std::fs::remove_dir_all(&staging);
        }
    })
    .await;
}

/// Finds an earlier recording of this exact mutation, so a retry after a lost
/// reply returns the first receipt instead of repeating the disk work.
async fn replayed(
    engine: &Arc<Engine>,
    a: &Admission,
    request: &OwnerRequest,
) -> Result<Option<Step>, EngineError> {
    let hash = request.mutation_hash()?;
    let id = request.request_id.clone();
    Ok(
        match engine
            .owner_job(|reply| Job::OwnerReconcile(a.clone(), id, Some(hash), reply))
            .await?
        {
            OwnerResponse::Committed { mut operation } => {
                operation.historical_replay = true;
                Some(Step::Reply(Box::new(OwnerResponse::Applied { operation })))
            }
            OwnerResponse::NotRecordedAtBarrier { .. } => None,
            other => Some(Step::Reply(Box::new(other))),
        },
    )
}

async fn measure(path: &str, deadline: Instant) -> Result<Step, EngineError> {
    let path = path.to_owned();
    let stop = deadline
        .into_std()
        .checked_sub(SIZE_MARGIN)
        .unwrap_or_else(std::time::Instant::now);
    match tokio::task::spawn_blocking(move || folder_size(&path, stop))
        .await
        .map_err(|_| failure("folder_size_interrupted"))?
    {
        Ok(measured) => Ok(Step::Size(measured)),
        Err(e) => Ok(Step::Reply(Box::new(OwnerResponse::Rejected { code: e.0 }))),
    }
}

/// Lists a set-up project's files off the storage actor: reads the stored setup for the
/// folder, then runs the bounded git listing on a blocking thread for at most
/// `project_files::LISTING_TIME`. Inputs: the engine, the admission, the project, the
/// query, the limit and the exchange deadline. Output: the read reply with the paths, or
/// a refusal: `project_setup_missing` for a project with no setup, or the listing's code.
async fn files(
    engine: &Arc<Engine>,
    a: &Admission,
    project_id: &ProjectId,
    query: &str,
    limit: u16,
    deadline: Instant,
) -> Result<Step, EngineError> {
    let read = OwnerRead::ProjectSetup {
        project_id: project_id.clone(),
    };
    let mut state = engine
        .owner_job(|reply| Job::OwnerRead(a.clone(), read, reply))
        .await?;
    let folder = match &state.data {
        OwnerData::ProjectSetup {
            setup: Some(setup), ..
        } => setup.project_folder(),
        _ => return Ok(rejected("project_setup_missing")),
    };
    let query = query.to_owned();
    let now = std::time::Instant::now();
    let stop = deadline
        .into_std()
        .checked_sub(SIZE_MARGIN)
        .unwrap_or(now)
        .min(now + crate::project_files::LISTING_TIME);
    let listed = tokio::task::spawn_blocking(move || {
        crate::project_files::list(&folder, &query, limit, stop)
    })
    .await
    .map_err(|_| failure("project_files_interrupted"))?;
    Ok(match listed {
        Ok(paths) => {
            state.data = OwnerData::ProjectFiles {
                project_id: project_id.clone(),
                paths,
            };
            Step::Reply(Box::new(OwnerResponse::Read { state }))
        }
        Err(e) => rejected(&e.0),
    })
}

fn rejected(code: &str) -> Step {
    Step::Reply(Box::new(OwnerResponse::Rejected { code: code.into() }))
}

async fn finish(
    engine: &Arc<Engine>,
    a: &Admission,
    request: OwnerRequest,
    step: Result<Step, EngineError>,
) -> Result<OwnerReply, EngineError> {
    let id = request.request_id.clone();
    let response = match step? {
        Step::Reply(response) => *response,
        Step::Record(effect) => OwnerResponse::Applied {
            operation: engine
                .owner_job(|reply| Job::HomeRecord(a.clone(), Box::new(request), effect, reply))
                .await?,
        },
        Step::Staged(staged) => OwnerResponse::Applied {
            operation: engine
                .owner_job(|reply| Job::SetupRecord(a.clone(), Box::new(request), staged, reply))
                .await?,
        },
        Step::Deleted(outcome) => OwnerResponse::Applied {
            operation: engine
                .owner_job(|reply| {
                    let files = outcome.map(|(_, files)| files);
                    Job::DeleteRecord(a.clone(), Box::new(request), files, reply)
                })
                .await?,
        },
        Step::Size(measured) => {
            let mut state = engine
                .owner_job(|reply| Job::OwnerRead(a.clone(), OwnerRead::ProjectsHome, reply))
                .await?;
            state.data = OwnerData::FolderSize {
                bytes: measured.bytes,
                placeholders: measured.placeholders,
            };
            OwnerResponse::Read { state }
        }
    };
    Ok(OwnerReply {
        request_id: id,
        response,
    })
}
