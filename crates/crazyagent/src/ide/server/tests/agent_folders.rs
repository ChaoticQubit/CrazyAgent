use super::*;

/// DEC-124 r1 M1: in a project with no `.crazyagent` folder, an agent folder the engine makes
/// under `<folder>/worktrees` after the IDE opened keeps every file under No delete or rename,
/// while the owner's own files stay removable.
#[test]
fn a_project_without_a_crazyagent_folder_holds_agents_started_after_its_ide_opened() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    assert_eq!(crate::projects_home::state_folder(&root), root);
    let id = project("legacy-later-agent");
    set_agent_files(
        data.path(),
        &id,
        AgentAccess::Sandboxed,
        IdeAgentFiles::NoDeleteOrRename,
    );
    let (_, folder) = built_server(&servers, &id, &root);
    let agent = root.join("worktrees").join("agent-1");
    fs::create_dir_all(&agent).unwrap();
    let file = agent.join("a.txt");
    fs::write(&file, "agent\n").unwrap();
    let own = root.join("own.txt");
    fs::write(&own, "owner\n").unwrap();
    let removes = |path: &Path| {
        let mut rm = sandbox::command(&folder.join(sandbox::PROFILE), Path::new("/bin/rm"));
        rm.arg("-f")
            .arg(path)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        kernel_processes::spawn_gate::output(&mut rm)
            .unwrap()
            .status
            .success()
    };
    assert!(!removes(&file));
    assert_eq!(fs::read_to_string(&file).unwrap(), "agent\n");
    assert!(removes(&own));
    assert!(!own.exists());
}
