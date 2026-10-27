use super::*;
use crate::project_setup::{NewProjectPlan, Staged};
use crate::projects_home::{AddMode, AddPlan, HomeEffect, MovePlan, folder_size};
use std::sync::atomic::{AtomicU64, Ordering};

const SIZE_MARGIN: Duration = Duration::from_millis(250);
const UNKNOWN: &str = "owner_outcome_unknown_reconcile_same_identity";
const UNSETTLED: [&str; 2] = [UNKNOWN, "owner_actor_stopped_outcome_unknown"];
static EXCHANGES: AtomicU64 = AtomicU64::new(1);

enum Step {
    Reply(Box<OwnerResponse>),
    Record(HomeEffect),
    Staged(Result<Staged, String>),
    Size(u64),
}

enum Work {
    Add(AddPlan),
    Move(MovePlan),
    New(NewProjectPlan),
}

impl Work {
    fn interrupted(&self) -> &'static str {
        match self {
            Self::Add(_) => "home_add_interrupted",
            Self::Move(_) => "home_move_interrupted",
            Self::New(_) => "home_new_project_interrupted",
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
        }
    }
}

enum Planned {
    Step(Step),
    Work(Work),
}

/// True for the owner requests that touch folders outside the engine data:
/// AddExistingFolder, MoveProjectsHome that moves the projects, SetupProject
/// (New Project), FolderSize and ProjectFiles. These run their slow disk work
/// here, off the storage actor, and only then reach the engine database.
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
        } | OwnerOperation::Read {
            query: OwnerRead::FolderSize { .. } | OwnerRead::ProjectFiles { .. }
        }
    )
}

/// Serves AddExistingFolder, MoveProjectsHome, SetupProject and FolderSize
/// for one owner. Input: the stream, engine, authority, owner packet and
/// admission, the validated request and the exchange deadline. Output: the
/// reply written under the owner egress barrier. The exchange deadline bounds
/// the replay check and the plan; the disk work runs under its own bounds and
/// a fresh owner deadline then bounds the record and the reply, so a finished
/// effect is recorded as Applied or as Rejected with the refusal code. A home
/// move then sends a settle job for this exchange's pending move: one whose
/// record did not finish it (a stale grant, a slow barrier, a failed record)
/// is finished from disk, and one whose disk work never ran is forgotten. The
/// exchange number is unique within the engine process; engine start finishes
/// any move left by an earlier process before serving.
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
    let (step, deadline, ran) = match planned {
        Ok(Planned::Work(work)) => {
            let interrupted = work.interrupted();
            let step = tokio::task::spawn_blocking(move || work.run())
                .await
                .map_err(|_| failure(interrupted));
            (step, Instant::now() + engine.timeout, true)
        }
        Ok(Planned::Step(step)) => (Ok(step), deadline, false),
        Err(e) => (Err(e), deadline, false),
    };
    let staging = match &step {
        Ok(Step::Staged(Ok(staged))) => Some(staged.staging().to_path_buf()),
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
        write_owner_reply(stream, result, id, mutation).await
    };
    let answered = tokio::time::timeout_at(deadline, answer).await;
    if moving
        && let Err(e) = engine
            .owner_job(|reply| Job::HomeMoveSettle(exchange, ran, reply))
            .await
    {
        eprintln!("projects_home_move_not_settled cause={}", e.0);
    }
    answered.map_err(|_| failure(UNKNOWN))?
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
        OwnerMutation::AddExistingFolder { source, mode } => {
            let (source, mode): (String, AddMode) = (source.clone(), *mode);
            engine
                .owner_job(|reply| Job::HomeAddPlan(a.clone(), expected, source, mode, reply))
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
