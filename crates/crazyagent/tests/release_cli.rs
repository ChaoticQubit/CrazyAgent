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
