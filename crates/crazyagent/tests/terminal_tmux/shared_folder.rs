use super::*;

#[test]
fn a_shared_terminal_folder_is_refused() {
    let root = root();
    let tmux = tmux(root.path());
    let folder = root.path().join(FOLDER);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let host = Tmux::open(
        Some(tmux),
        &folder,
        kernel_sandbox::confinement(),
        crazyagent::terminal::tmux::tmux_hooks(),
    );
    let agent = AgentId::team(1).unwrap();
    let command = shell(root.path(), Vec::new(), &["/bin/sh"]);
    assert_eq!(
        host.spawn(
            &agent,
            &kernel_processes::terminal::PaneCommand::from(&command)
        )
        .unwrap_err()
        .0,
        "terminal_folder_unsafe"
    );
}
