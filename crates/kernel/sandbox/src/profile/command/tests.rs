use super::*;

/// A confined pane command the keeper wraps reads back as the same command, and the
/// boundary program is the sandbox runner.
#[test]
fn a_kept_pane_reads_back_as_the_command_it_wrapped() {
    let plain: Vec<String> = [SANDBOX_EXEC, "-f", "/tmp/agent.sb", "/bin/zsh", "-l"]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    let kept = kept_pane(plain.clone(), None).unwrap();
    assert_eq!(kept.len(), KEEPER.len() + PANE_SHIM.len() + plain.len());
    assert_eq!(unkept_pane(&kept), Some(plain));
    assert!(starts_at_boundary(std::ffi::OsStr::new(SANDBOX_EXEC)));
}
