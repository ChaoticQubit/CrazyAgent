use super::*;
use crate::projects_home::{AddMode, folder_size};

const SIZE_MARGIN: Duration = Duration::from_millis(250);

enum Step {
    Reply(Box<OwnerResponse>),
    Record(Result<(), String>),
    Size(u64),
}

/// True for the owner requests that touch folders outside the engine data:
/// AddExistingFolder, MoveProjectsHome that moves the projects, and
/// FolderSize. These run their slow disk work here, off the storage actor,
/// and only then reach the engine database.
pub(super) fn handles(request: &OwnerRequest) -> bool {
    matches!(
        &request.operation,
        OwnerOperation::Apply {
            mutation: OwnerMutation::AddExistingFolder { .. }
                | OwnerMutation::MoveProjectsHome {
                    move_projects: true,
                    ..
                }
        } | OwnerOperation::Read {
            query: OwnerRead::FolderSize { .. }
        }
    )
}

/// Serves AddExistingFolder, MoveProjectsHome and FolderSize for one owner.
/// Input: the stream, engine, authority, owner packet and admission, the
/// validated request and the exchange deadline. Output: the reply written
/// under the owner egress barrier. An add or home move is recorded after its
/// disk work, as Applied once it is complete or as Rejected with the refusal
/// code; a FolderSize refusal is written as Rejected.
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
    let step = match &request.operation {
        OwnerOperation::Apply {
            mutation: OwnerMutation::AddExistingFolder { source, mode },
        } => match replayed(engine, a, &request).await {
            Ok(Some(step)) => Ok(step),
            Ok(None) => prepare_add(engine, a, &request, source, *mode).await,
            Err(e) => Err(e),
        },
        OwnerOperation::Apply {
            mutation: OwnerMutation::MoveProjectsHome { path, .. },
        } => match replayed(engine, a, &request).await {
            Ok(Some(step)) => Ok(step),
            Ok(None) => prepare_move(engine, a, &request, path).await,
            Err(e) => Err(e),
        },
        OwnerOperation::Read {
            query: OwnerRead::FolderSize { path },
        } => measure(path, deadline).await,
        _ => Err(failure("owner_request_invalid")),
    };
    let _barrier = engine.owner_egress.write().await;
    let result = match authority.authenticate(packet) {
        Ok(current) if &current == a => finish(engine, a, request, step).await,
        Ok(_) => Err(failure("owner_grant_stale_or_revoked")),
        Err(e) => Err(e),
    };
    write_owner_reply(stream, result, id, mutation).await
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

async fn prepare_move(
    engine: &Arc<Engine>,
    a: &Admission,
    request: &OwnerRequest,
    path: &str,
) -> Result<Step, EngineError> {
    let picked = path.to_owned();
    let expected = request.expected_revision.clone();
    let plan = match engine
        .owner_job(|reply| Job::HomeMovePlan(a.clone(), expected, picked, reply))
        .await
    {
        Ok(plan) => plan,
        Err(e) => return Ok(Step::Record(Err(e.0))),
    };
    let effect = tokio::task::spawn_blocking(move || plan.run())
        .await
        .map_err(|_| failure("home_move_interrupted"))?;
    Ok(Step::Record(effect.map_err(|e| e.0)))
}

async fn prepare_add(
    engine: &Arc<Engine>,
    a: &Admission,
    request: &OwnerRequest,
    source: &str,
    mode: AddMode,
) -> Result<Step, EngineError> {
    let source = source.to_owned();
    let expected = request.expected_revision.clone();
    let plan = match engine
        .owner_job(|reply| Job::HomeAddPlan(a.clone(), expected, source, mode, reply))
        .await
    {
        Ok(plan) => plan,
        Err(e) => return Ok(Step::Record(Err(e.0))),
    };
    let effect = tokio::task::spawn_blocking(move || plan.run())
        .await
        .map_err(|_| failure("home_add_interrupted"))?;
    Ok(Step::Record(effect.map_err(|e| e.0)))
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
