use super::*;
use crate::project_setup::{NewProjectPlan, Staged};
use crate::projects_home::{AddMode, AddPlan, MovePlan, folder_size};

const SIZE_MARGIN: Duration = Duration::from_millis(250);
const UNKNOWN: &str = "owner_outcome_unknown_reconcile_same_identity";
const UNSETTLED: [&str; 2] = [UNKNOWN, "owner_actor_stopped_outcome_unknown"];

enum Step {
    Reply(Box<OwnerResponse>),
    Record(Result<(), String>),
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
            Self::Add(plan) => Step::Record(plan.run().map_err(|e| e.0)),
            Self::Move(plan) => Step::Record(plan.run().map_err(|e| e.0)),
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
/// (New Project) and FolderSize. These run their slow disk work here, off the
/// storage actor, and only then reach the engine database.
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
            query: OwnerRead::FolderSize { .. }
        }
    )
}

/// Serves AddExistingFolder, MoveProjectsHome, SetupProject and FolderSize
/// for one owner. Input: the stream, engine, authority, owner packet and
/// admission, the validated request and the exchange deadline. Output: the
/// reply written under the owner egress barrier. The exchange deadline bounds
/// the replay check and the plan; the disk work runs under its own bounds and
/// a fresh owner deadline then bounds the record and the reply, so a finished
/// effect is recorded as Applied or as Rejected with the refusal code.
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
    let planned = tokio::time::timeout_at(deadline, plan(engine, a, &request, deadline))
        .await
        .unwrap_or_else(|_| Err(failure(UNKNOWN)));
    let (step, deadline) = match planned {
        Ok(Planned::Work(work)) => {
            let interrupted = work.interrupted();
            let step = tokio::task::spawn_blocking(move || work.run())
                .await
                .map_err(|_| failure(interrupted));
            (step, Instant::now() + engine.timeout)
        }
        Ok(Planned::Step(step)) => (Ok(step), deadline),
        Err(e) => (Err(e), deadline),
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
    tokio::time::timeout_at(deadline, answer)
        .await
        .map_err(|_| failure(UNKNOWN))?
}

async fn plan(
    engine: &Arc<Engine>,
    a: &Admission,
    request: &OwnerRequest,
    deadline: Instant,
) -> Result<Planned, EngineError> {
    let mutation = match &request.operation {
        OwnerOperation::Read {
            query: OwnerRead::FolderSize { path },
        } => return measure(path, deadline).await.map(Planned::Step),
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
            engine
                .owner_job(|reply| Job::HomeMovePlan(a.clone(), expected, picked, reply))
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
