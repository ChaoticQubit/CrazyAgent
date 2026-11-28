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
        quiet_of(&host, &pane) >= Duration::from_secs(3)
    });
    let typed = host.input(&pane, b"hello\r");
    assert_eq!(typed, Ok(()), "typing into the pane over tmux");
    until("the echoed line", || std::fs::read_to_string(&out).is_ok());
    let after = quiet_of(&host, &pane);
    assert!(after < Duration::from_secs(3), "{after:?}");
}

/// How long the pane drew nothing, read over tmux; a failed read or a pane with no quiet
/// time fails the row with the outcome. Inputs: the host and the pane. Output: the time.
fn quiet_of(host: &Tmux, pane: &kernel_processes::terminal::AgentId) -> Duration {
    match host.quiet(pane) {
        Ok(Some(quiet)) => quiet,
        outcome => panic!("tmux quiet for {pane:?}: {outcome:?}"),
    }
}
