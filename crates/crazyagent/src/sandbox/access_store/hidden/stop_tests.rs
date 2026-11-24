use super::*;

/// A code graph build's stop turns yes once a folder its sandbox did not hide is saved
/// hidden, stays no while every saved folder is one it hid or lies below one, and still
/// follows the stop it wraps.
#[test]
fn a_code_graph_build_stops_once_a_folder_saved_hidden_is_not_one_it_hid() {
    let database = Connection::open_in_memory().unwrap();
    let project = "hide-stops";
    let (hid, below, later) = (
        PathBuf::from("/tmp/hide-stops/vault"),
        PathBuf::from("/tmp/hide-stops/vault/keys"),
        PathBuf::from("/tmp/hide-stops/dump"),
    );
    let listed = kernel_sandbox::hidden_paths::starting(
        kernel_sandbox::hidden_paths::RunKind::CodeGraph,
        project,
    )
    .unwrap();
    listed.hid(std::slice::from_ref(&hid));
    save(&database, project, &[hid.clone(), below]).unwrap();
    assert!(!hide_stops(&database, project, &listed, &|| false)());
    assert!(hide_stops(&database, project, &listed, &|| true)());
    save(&database, project, &[hid, later]).unwrap();
    assert!(hide_stops(&database, project, &listed, &|| false)());
}
