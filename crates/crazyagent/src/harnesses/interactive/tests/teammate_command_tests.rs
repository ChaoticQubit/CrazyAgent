use super::*;

/// UMASK-MODE-ASSERTS: an engine that inherited a umask clearing the owner's write bit
/// still writes a native lead's teammate command as a private 0700 program, where it used
/// to panic on the file's mode after writing.
#[test]
fn a_teammate_command_written_under_a_narrow_umask_is_a_private_program() {
    let folder = tempfile::tempdir().unwrap();
    let status = kernel_processes::spawn_gate::row_alone(
        "harnesses::interactive::tests::a_teammate_command_written_under_a_narrow_umask_in_its_own_process",
        ":",
        folder.path(),
    );
    assert!(
        status.success(),
        "the write under umask 0277 ended in {status}"
    );
    assert_eq!(
        std::fs::read_to_string(folder.path().join("outcome")).unwrap(),
        "ok 700"
    );
}

/// The pane command a terminal host runs keeps the interactive command's argument
/// vector, working folder and environment byte-equal; only the session record stays behind.
#[test]
fn pane_command_from_an_interactive_command_keeps_every_field() {
    let command = InteractiveCommand {
        argv: vec![
            "/usr/bin/sandbox-exec".into(),
            "-f".into(),
            "/tmp/profile with space.sb".into(),
            "codex".into(),
            "--flag=ü".into(),
        ],
        cwd: PathBuf::from("/work/project one"),
        env: vec![
            ("HOME".into(), "/home/agent".into()),
            ("CODEX_HOME".into(), String::new()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ],
        transcript: Transcript::Unavailable,
    };
    let pane = kernel_processes::terminal::PaneCommand::from(&command);
    assert_eq!(pane.argv, command.argv);
    assert_eq!(pane.cwd, command.cwd);
    assert_eq!(pane.env, command.env);
}
