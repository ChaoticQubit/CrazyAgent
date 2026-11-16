use crazyagent::keep_awake::journal::JournalLock;
use std::process::{Command, Stdio};

fn crazyagent(args: &[&str]) -> std::process::Output {
    crazyagent::spawn_gate::output(
        Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

#[test]
fn version_prints_semver_channel_and_short_commit() {
    let output = crazyagent(&["--version"]);
    assert!(output.status.success());
    let line = String::from_utf8(output.stdout).unwrap();
    let line = line.strip_suffix('\n').unwrap();
    let prefix = format!("crazyagent {} (", env!("CARGO_PKG_VERSION"));
    assert!(line.starts_with(&prefix), "{line}");
    let (channel, commit) = line[prefix.len()..]
        .strip_suffix(')')
        .unwrap()
        .split_once(", ")
        .unwrap();
    assert!(matches!(channel, "stable" | "dev"), "{line}");
    assert!(
        commit == "unknown" || (commit.len() == 7 && commit.bytes().all(|b| b.is_ascii_hexdigit())),
        "{line}"
    );
}

#[test]
fn default_engine_form_refuses_relative_home_and_unknown_flags_before_starting() {
    let relative = crazyagent(&["engine", "--home", "relative/root"]);
    assert!(!relative.status.success());
    assert!(
        String::from_utf8_lossy(&relative.stderr)
            .contains("--home must be an absolute folder path")
    );
    for args in [
        &["engine", "--unknown", "/x"][..],
        &["engine", "--home"][..],
        &["engine", "--home", "/a", "--home", "/b"][..],
    ] {
        let output = crazyagent(args);
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .starts_with("usage: crazyagent engine [--home DIR]"),
            "{args:?}"
        );
    }
}

#[test]
fn service_and_tui_usage_errors_are_plain() {
    let output = crazyagent(&["service", "restart-everything"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("usage: crazyagent service install|uninstall|status|start|stop|link-cli")
    );
    let output = crazyagent(&["tui"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires terminal"));
    let help = crazyagent(&["--help"]);
    let text = String::from_utf8(help.stdout).unwrap();
    for line in [
        "crazyagent --version",
        "crazyagent engine [--home DIR]",
        "crazyagent tui\n",
        "crazyagent tui SOCKET",
        "crazyagent service install|uninstall|status|start|stop|link-cli",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
}

/// Runs the CLI with its data folder in `home`, so a row never reads or changes the
/// owner's engine root. Inputs: the folder and the arguments. Output: the output.
fn crazyagent_at(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    crazyagent::spawn_gate::output(
        Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(args)
            .env("CRAZYAGENT_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

/// Plan Task 14: `crazyagent keep-awake restore` is listed, says its usage plainly, puts
/// back what an engine left recorded and says in words what it could not.
#[test]
fn keep_awake_restore_runs_the_engine_start_undo_under_the_data_folder() {
    let home = tempfile::tempdir().unwrap();
    for args in [&["keep-awake"][..], &["keep-awake", "undo-everything"][..]] {
        let output = crazyagent_at(home.path(), args);
        assert!(!output.status.success(), "{args:?}");
        let said = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            said.contains("usage: crazyagent keep-awake restore"),
            "{said}"
        );
    }
    let help = String::from_utf8(crazyagent(&["--help"]).stdout).unwrap();
    assert!(help.contains("crazyagent keep-awake restore\n"), "{help}");

    let nothing = crazyagent_at(home.path(), &["keep-awake", "restore"]);
    let said = String::from_utf8_lossy(&nothing.stdout).into_owned();
    assert!(nothing.status.success(), "{said}");
    assert!(
        said.contains("Keep awake has nothing left to put back."),
        "{said}"
    );

    let journal = home.path().join("keep-awake").join("journal.json");
    let entry = r#"[{"kind":"stand_in","restore":"before"}]"#;
    std::fs::write(&journal, entry).unwrap();
    let left = crazyagent_at(home.path(), &["keep-awake", "restore"]);
    let said = String::from_utf8_lossy(&left.stderr).into_owned();
    assert!(!left.status.success(), "{said}");
    assert!(
        said.contains(crazyagent::keep_awake::RESTORE_LEFT),
        "{said}"
    );
    assert!(!said.contains("stand_in"), "{said}");
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), entry);
}

/// rv-sec3 KA T14 r1 F2 and rv-ds241 nit 1: restore refuses while the engine holds the
/// journal lock and leaves the journal as it is, and a data folder no engine ever made
/// has nothing to put back and is not made.
#[test]
fn keep_awake_restore_refuses_beside_the_engine_and_makes_no_data_folder() {
    let home = tempfile::tempdir().unwrap();
    let never = home.path().join("never-started");
    let nothing = crazyagent_at(&never, &["keep-awake", "restore"]);
    let said = String::from_utf8_lossy(&nothing.stdout).into_owned();
    assert!(nothing.status.success(), "{said}");
    assert!(
        said.contains("Keep awake has nothing left to put back."),
        "{said}"
    );
    assert!(!never.exists(), "restore makes no data folder");

    let engine = JournalLock::try_take(home.path()).unwrap();
    assert!(engine.is_some(), "the row stands in for the engine");
    let journal = home.path().join("keep-awake").join("journal.json");
    let entry = r#"[{"kind":"stand_in","restore":"before"}]"#;
    std::fs::write(&journal, entry).unwrap();
    let beside = crazyagent_at(home.path(), &["keep-awake", "restore"]);
    let said = String::from_utf8_lossy(&beside.stderr).into_owned();
    assert!(!beside.status.success(), "{said}");
    assert!(
        said.contains(crazyagent::keep_awake::RESTORE_ENGINE_RUNNING),
        "{said}"
    );
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), entry);
}
