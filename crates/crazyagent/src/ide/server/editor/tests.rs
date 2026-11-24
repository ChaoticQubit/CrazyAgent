use super::*;

#[test]
fn the_ide_server_spawns_through_editor_spawn() {
    let server = include_str!("../../server.rs");
    let editor = include_str!("../editor.rs");
    assert!(!server.contains("spawn_gate::spawn("));
    assert!(!editor.contains("spawn_gate::spawn("));
    assert_eq!(
        editor.matches("kernel_processes::editor::spawn(").count(),
        1
    );
    assert_eq!(
        server
            .matches("let required = editor::required(policy.access);")
            .count(),
        1
    );
}

#[test]
fn full_access_on_macos_passes_unconfined_not_editor_not_confined() {
    assert!(super::super::sandbox::qualified());
    assert!(!required(AgentAccess::Full));
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 0"]);
    let confinement = kernel_sandbox::confinement();
    let mut child =
        kernel_processes::editor::spawn(&mut command, &confinement, required(AgentAccess::Full))
            .expect("the Full access command started unconfined");
    assert!(child.wait().expect("the command ended").success());
}

#[test]
fn every_level_but_full_refuses_an_unconfined_command_on_macos() {
    assert!(super::super::sandbox::qualified());
    let confinement = kernel_sandbox::confinement();
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        assert!(required(access), "{access:?}");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let refused = kernel_processes::editor::spawn(&mut command, &confinement, required(access));
        assert_eq!(refused.err(), Some(kernel_processes::editor::NOT_CONFINED));
    }
}
