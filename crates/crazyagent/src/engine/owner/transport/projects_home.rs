use super::*;
use crate::project_setup::{NewProjectPlan, Staged};
use crate::projects_home::{
    AddMode, AddOverride, AddPlan, DeletePlan, HomeEffect, MovePlan, folder_size,
};
use std::sync::atomic::{AtomicU64, Ordering};

const SIZE_MARGIN: Duration = Duration::from_millis(250);
const UNKNOWN: &str = "owner_outcome_unknown_reconcile_same_identity";
const UNSETTLED: [&str; 2] = [UNKNOWN, "owner_actor_stopped_outcome_unknown"];
const SETTLE_TRIES: usize = 3;
const SETTLE_GAP: Duration = Duration::from_millis(50);
static EXCHANGES: AtomicU64 = AtomicU64::new(1);

enum Step {
    Reply(Box<OwnerResponse>),
    Record(HomeEffect),
    Staged(Result<Staged, String>),
    Size(u64),
    Deleted(Result<(PathBuf, Vec<String>), String>),
}

enum Work {
    Add(AddPlan),
    Move(MovePlan),
    New(NewProjectPlan),
    Delete(DeletePlan, Vec<i64>),
}

impl Work {
    fn interrupted(&self) -> &'static str {
        match self {
            Self::Add(_) => "home_add_interrupted",
            Self::Move(_) => "home_move_interrupted",
            Self::New(_) => "home_new_project_interrupted",
            Self::Delete(..) => "project_delete_interrupted",
        }
    }

    fn run(self) -> Step {
        match self {
            Self::Add(plan) => {
                let source = plan.source().to_path_buf();
                Step::Record(plan.run().map(|()| Some(source)).map_err(|e| e.0))
            }
            Self::Move(plan) => Step::Record(plan.run().map(|()| None).map_err(|e| e.0)),
            Self::New(plan) => Step::Staged(plan.run()),
            Self::Delete(plan, research) => Step::Deleted(plan.run().map(|()| {
                let files = plan.engine_files(&research);
                (plan.database, files)
            })),
        }
    }
}

enum Planned {
    Step(Step),
    Work(Work),
}

/// True for the owner requests that touch folders outside the engine data:
/// AddExistingFolder, MoveProjectsHome that moves the projects, SetupProject
/// (New Project), DeleteProject (Delete everything), FolderSize and ProjectFiles.
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
                | OwnerMutation::SetupProject { .. }
                | OwnerMutation::DeleteProject { .. }
        } | OwnerOperation::Read {
            query: OwnerRead::FolderSize { .. } | OwnerRead::ProjectFiles { .. }
        }
    )
}

/// Serves AddExistingFolder, MoveProjectsHome, SetupProject and FolderSize
/// for one owner. Input: the stream, engine, authority, owner packet and
/// admission, the validated request and the exchange deadline. Output: the
/// reply written under the owner egress barrier. The exchange deadline bounds
/// the replay check, the plan, a grant check under the owner barrier and, for a
/// home move, a recheck of agent work under the move's hold just before the disk
/// work, which runs under its own bounds and without the barrier; a move the
/// recheck refuses is recorded as Rejected with its code and never runs; a fresh owner deadline then bounds the record and the reply, so a finished
/// effect is recorded as Applied or as Rejected with the refusal code. A home
/// move then sends a settle job for this exchange's pending move: one whose
/// record did not finish it (a stale grant, a slow barrier, a failed record)
/// is finished from disk, and one whose disk work never ran is forgotten. A Delete
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
    #[cfg(unix)]
    let mut _ide_refused = None;
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
                    if let Work::Delete(plan, _) = &work {
                        _ide_refused =
                            Some(authority.ide_servers.hold_for_delete(&plan.project).await);
                    }
                    let interrupted = work.interrupted();
                    let step = tokio::task::spawn_blocking(move || work.run())
                        .await
                        .map_err(|_| failure(interrupted));
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
        let _barrier = engine.owner_egress.write().await;
        let result = match authority.authenticate(packet) {
            Ok(current) if &current == a => finish(engine, a, request, step).await,
            Ok(_) => Err(failure("owner_grant_stale_or_revoked")),
            Err(e) => Err(e),
        };
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
        (
            write_owner_reply(stream, result, id, mutation).await,
            removed,
            forgotten,
        )
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
    if moving
        && let Err(e) = engine
            .owner_job(|reply| Job::HomeMoveSettle(exchange, ran, reply))
            .await
    {
        eprintln!("projects_home_move_not_settled cause={}", e.0);
    }
    if deleting && !forgotten {
        settle_delete(engine, exchange).await;
    }
    answered.map_err(|_| failure(UNKNOWN))?
}

/// Forgets a Delete everything's plan, trying again after a full queue or a SQLite failure,
/// since a plan left behind refuses every projects home move until the engine restarts.
/// Inputs: the engine and the exchange. Output: whether the plan is gone.
async fn settle_delete(engine: &Arc<Engine>, exchange: u64) -> bool {
    for attempt in 0..SETTLE_TRIES {
        if attempt > 0 {
            tokio::time::sleep(SETTLE_GAP).await;
        }
        match engine
            .owner_job(|reply| Job::DeleteSettle(exchange, reply))
            .await
        {
            Ok(()) => return true,
            Err(e) => eprintln!("project_delete_not_settled cause={}", e.0),
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
        OwnerMutation::SetupProject { .. } => {
            let request = Box::new(request.clone());
            return Ok(
                match engine
                    .owner_job(|reply| Job::SetupPlan(a.clone(), request, reply))
                    .await?
                {
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
        Ok(bytes) => Ok(Step::Size(bytes)),
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
        Step::Size(bytes) => {
            let mut state = engine
                .owner_job(|reply| Job::OwnerRead(a.clone(), OwnerRead::ProjectsHome, reply))
                .await?;
            state.data = OwnerData::FolderSize { bytes };
            OwnerResponse::Read { state }
        }
    };
    Ok(OwnerReply {
        request_id: id,
        response,
    })
}
