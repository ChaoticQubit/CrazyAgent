use super::*;

#[test]
fn quiet_through_the_engine_host_answers_for_an_agent_on_the_engine_server()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root();
    let tmux = tmux(root.path());
    let path = root.path().canonicalize()?;
    let _server = Server {
        tmux: tmux.clone(),
        socket: path.join(FOLDER).join(SOCKET),
    };
    let host = crazyagent::terminal::tmux_host(&path, Some(tmux));
    let agent = AgentId::team(4)?;
    host.spawn(
        &agent,
        &kernel_processes::terminal::PaneCommand::from(&shell(&path, Vec::new(), &["/bin/sh"])),
    )?;
    let quiet = host.quiet(&agent)?;
    assert!(
        quiet.is_some_and(|quiet| quiet < Duration::from_secs(60)),
        "{quiet:?}"
    );
    host.close(&[])?;
    Ok(())
}
