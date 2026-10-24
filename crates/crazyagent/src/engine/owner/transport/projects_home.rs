use super::*;
use crate::projects_home::{AddMode, folder_size};

const SIZE_MARGIN: Duration = Duration::from_millis(250);

enum Step {
    Reply(OwnerResponse),
    Record,
    Size(u64),
}

/// True for the owner requests that touch folders outside the engine data:
/// AddExistingFolder and FolderSize. These run their slow disk work here,
/// off the storage actor, and only then reach the engine database.
pub(super) fn handles(request: &OwnerRequest) -> bool {
    matches!(
        &request.operation,
        OwnerOperation::Apply {
            mutation: OwnerMutation::AddExistingFolder { .. }
        } | OwnerOperation::Read {
            query: OwnerRead::FolderSize { .. }
        }
    )
}

/// Serves AddExistingFolder and FolderSize for one authenticated owner.
/// Input: the stream, engine, authority, owner packet and admission, the
/// validated request and the exchange deadline. Output: the reply written
/// under the owner egress barrier; an add is recorded only after the folder
/// is complete in the projects home, and a refusal is written as Rejected.
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
        } => prepare_add(engine, a, &request, source, *mode).await,
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

async fn prepare_add(
    engine: &Arc<Engine>,
    a: &Admission,
    request: &OwnerRequest,
    source: &str,
    mode: AddMode,
) -> Result<Step, EngineError> {
    let hash = request.mutation_hash()?;
    let id = request.request_id.clone();
    match engine
        .owner_job(|reply| Job::OwnerReconcile(a.clone(), id, Some(hash), reply))
        .await?
    {
        OwnerResponse::Committed { mut operation } => {
            operation.historical_replay = true;
            return Ok(Step::Reply(OwnerResponse::Applied { operation }));
        }
        OwnerResponse::NotRecordedAtBarrier { .. } => {}
        other => return Ok(Step::Reply(other)),
    }
    let source = source.to_owned();
    let plan = match engine
        .owner_job(|reply| Job::HomeAddPlan(a.clone(), source, mode, reply))
        .await
    {
        Ok(plan) => plan,
        Err(e) => return Ok(Step::Reply(OwnerResponse::Rejected { code: e.0 })),
    };
    match tokio::task::spawn_blocking(move || plan.run())
        .await
        .map_err(|_| failure("home_add_interrupted"))?
    {
        Ok(()) => Ok(Step::Record),
        Err(e) => Ok(Step::Reply(OwnerResponse::Rejected { code: e.0 })),
    }
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
        Err(e) => Ok(Step::Reply(OwnerResponse::Rejected { code: e.0 })),
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
        Step::Reply(response) => response,
        Step::Record => OwnerResponse::Applied {
            operation: engine
                .owner_job(|reply| Job::OwnerApply(a.clone(), request, reply))
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
