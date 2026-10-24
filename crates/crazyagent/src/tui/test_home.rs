use crate::client_controller::OwnerController;
use crate::engine::owner::{OwnerResponse, OwnerResult};
use std::os::unix::fs::PermissionsExt;

/// Sets a fresh projects home beside the engine root, never inside it.
/// Input: an attached owner controller and the mutation generation.
/// Output: the home folder, kept alive by the caller; panics unless the
/// engine applied it.
pub(in crate::tui) async fn projects_home(
    controller: &mut OwnerController,
    generation: u64,
) -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("ca-tui-home-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::fs::canonicalize(home.path()).unwrap();
    let response = controller
        .set_projects_home(path.to_str().unwrap().into(), generation)
        .await
        .unwrap();
    assert!(
        matches!(
            &response,
            OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
        ),
        "{response:?}"
    );
    home
}
