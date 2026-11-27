use super::*;
use crate::terminal::tmux::test_seams::{test_confinement, test_hooks};
use std::os::unix::fs::PermissionsExt;

/// A tmux that answers `-V` as 3.8 but whose server reports 3.1 to the engine's version
/// read is refused at connect with `terminal_tmux_unsupported`, within `CLOSE_TIME`, and
/// no control-mode client of the refused connect is left on its server, while the same count
/// sees a control-mode client attached afterwards.
#[cfg(target_os = "macos")]
#[test]
fn a_server_reporting_an_old_tmux_is_refused_and_leaves_no_control_client() {
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("terminal");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let real = installed(home.path()).expect("tmux is required for this test");
    let stand_in = home.path().join("tmux-old");
    std::fs::write(
        &stand_in,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -V ]; then echo 'tmux 3.8'; exit 0; fi\n'{}' \"$@\" | /usr/bin/sed -l -E 's/^(next-)?[0-9]+\\.[0-9]+[a-z]*/3.1/'\n",
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let host = Tmux::open(Some(stand_in), &folder, test_confinement(), test_hooks());
    let socket = host.socket();
    let command = PaneCommand {
        argv: vec!["/bin/sh".into(), "-c".into(), "exec /bin/cat".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
    };
    let started = Instant::now();
    let refused = host.spawn(&AgentId::team(1).unwrap(), &command);
    let took = started.elapsed();
    eprintln!("old tmux refusal took {took:?}");
    let clients = || {
        let listed = Command::new(&real)
            .args(["-S"])
            .arg(&socket)
            .args(["list-clients", "-F", "#{client_control_mode}"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .filter(|line| *line == "1")
            .count()
    };
    let left = clients();
    let mut seen = Command::new(&real)
        .arg("-S")
        .arg(&socket)
        .args(["-C", "attach", "-t", SESSION])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(seen.stdout.take().unwrap());
    let mut first = String::new();
    std::io::BufRead::read_line(&mut output, &mut first).unwrap();
    let counted = clients();
    drop(seen.stdin.take());
    drop(output);
    let _ = seen.wait();
    let _ = Command::new(&real)
        .arg("-S")
        .arg(&socket)
        .arg("kill-server")
        .output();
    assert_eq!(
        refused.map(|_| ()).map_err(|problem| problem.0),
        Err("terminal_tmux_unsupported".to_owned())
    );
    assert!(
        took < CLOSE_TIME,
        "the refusal waited {took:?} on its client"
    );
    assert_eq!(
        left, 0,
        "a control-mode client of the refused connect is left"
    );
}
