use super::*;

/// K2b: tmux tells how long an agent's window drew nothing: it grows while the pane is
/// silent and starts again from zero when the pane prints.
#[test]
fn tmux_quiet_grows_while_a_pane_is_silent_and_resets_on_output() {
    let mut fixture = Fixture::new();
    let host = fixture.host();
    let agent = fixture.agent("sub", "codex", "idle", None);
    let out = fixture.base.join("out.txt");
    fixture.pane(&host, agent, READ_ONE_LINE, &out);
    let pane = kernel_processes::terminal::AgentId::team(agent).unwrap();
    until("three quiet seconds", || {
        host.quiet(&pane).unwrap().unwrap() >= Duration::from_secs(3)
    });
    host.input(&pane, b"hello\r").unwrap();
    until("the echoed line", || std::fs::read_to_string(&out).is_ok());
    let after = host.quiet(&pane).unwrap().unwrap();
    assert!(after < Duration::from_secs(3), "{after:?}");
}
