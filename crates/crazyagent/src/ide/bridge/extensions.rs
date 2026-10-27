use crate::engine::EngineError;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::ide::extensions::{self as policy, Asked};
use crate::protocol::ProjectId;
use std::path::PathBuf;

/// The approved extensions, this project's asks waiting for the desktop confirm and the
/// available updates, as the IDE shows them. Inputs: the owner runtime and the project.
/// Output: `{approved, asks, updates}`, or a plain refusal.
pub(super) async fn listing(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
) -> Result<serde_json::Value, EngineError> {
    let project = project.clone();
    let listing = blocking(runtime, move |root| {
        policy::listing(&root, &project, crate::onboarding::now_ms())
    })
    .await?;
    serde_json::to_value(listing).map_err(|_| failure("ide_extensions_unavailable"))
}

/// Asks the owner to approve one extension version, or `latest`, in the desktop app;
/// nothing is approved from the IDE. Inputs: the owner runtime, the project, the id and
/// the version. Output: `{"state": "approved"}` when that version is approved already,
/// else `{"state": "confirm"}`, or a plain refusal.
pub(super) async fn request(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    id: String,
    version: String,
) -> Result<serde_json::Value, EngineError> {
    let project = project.clone();
    let id = id.to_ascii_lowercase();
    let asked = blocking(runtime, move |root| {
        policy::ask(&root, &project, &id, &version, crate::onboarding::now_ms())
    })
    .await?;
    let state = match asked {
        Asked::Approved => "approved",
        Asked::Confirm => "confirm",
    };
    Ok(serde_json::json!({ "state": state }))
}

/// The IDE's choice on an ask or an available update: approve asks the owner to confirm
/// it in the desktop app, as a request does; otherwise the ask and update are dismissed.
/// Inputs: the owner runtime, the project, the id, the version and the choice. Output:
/// the request's state, `{"state": "dismissed"}`, or a plain refusal.
pub(super) async fn decide(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    id: String,
    version: String,
    approve: bool,
) -> Result<serde_json::Value, EngineError> {
    if approve {
        return request(runtime, project, id, version).await;
    }
    let project = project.clone();
    let id = id.to_ascii_lowercase();
    blocking(runtime, move |root| {
        policy::dismiss(&root, &project, &id, &version)
    })
    .await?;
    Ok(serde_json::json!({ "state": "dismissed" }))
}

/// Runs one policy step on a blocking thread against the engine root. Inputs: the owner
/// runtime and the step. Output: the step's result, or `ide_extensions_unavailable`.
async fn blocking<T: Send + 'static>(
    runtime: &OwnerRuntimeHandle,
    step: impl FnOnce(PathBuf) -> Result<T, EngineError> + Send + 'static,
) -> Result<T, EngineError> {
    let root = runtime.ide()?.root;
    tokio::task::spawn_blocking(move || step(root))
        .await
        .map_err(|_| failure("ide_extensions_unavailable"))?
}
