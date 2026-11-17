use super::*;

/// DEC-161 bug 2: a master harness saved while a failed start left the live master off
/// starts it on the new harness at the next team read, with no other open of the view, and
/// the old harness's line goes; a read naming the same harness by its alias starts nothing.
#[test]
fn a_saved_master_harness_switch_starts_a_stopped_live_master_on_the_new_harness_at_once() {
    let codex = "The live master was turned off: it could not start: No account in this project's pool can run codex";
    let mut shell = master_view(100, 24);
    requests(&mut shell);
    shell.arrive(harnessed(Some(false), "codex", None));
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|sent| live_master_change(sent, true)),
        "the open starts it: {sent:?}"
    );
    shell.arrive(turned(true, Ok(())));
    requests(&mut shell);
    let mut refused = harnessed_status(Some(false), "codex", None);
    refused.live_problem = Some(codex.into());
    shell.arrive(team_read(refused));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| !live_master_change(sent, true)),
        "a refused start is not sent again on its own"
    );
    let mut switched = harnessed_status(Some(false), "claude-code", None);
    switched.live_problem = Some(codex.into());
    shell.arrive(team_read(switched));
    let sent = requests(&mut shell);
    assert!(
        sent.iter().any(|sent| live_master_change(sent, true)),
        "the saved switch starts it on the new harness at once: {sent:?}"
    );
    let rows = render(&mut shell);
    assert!(
        rows.iter().all(|row| !row.contains("can run codex")),
        "the old harness's line is gone: {rows:?}"
    );
    shell.arrive(turned(true, Ok(())));
    requests(&mut shell);
    let mut alias = harnessed_status(Some(false), "claude", None);
    alias.live_problem =
        Some("The live master was turned off: it could not start: claude is not installed".into());
    shell.arrive(team_read(alias));
    assert!(
        requests(&mut shell)
            .iter()
            .all(|sent| !live_master_change(sent, true)),
        "the alias names the same harness, so nothing starts again"
    );
}
