use super::*;
use crate::terminal::tmux::test_seams::{test_confinement, test_hooks};

/// Ends a row's tmux server when the row ends, passed or failed.
struct Ended {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

/// A host on a fresh private folder under `home`, with the guard that ends its server.
/// Input: the row's home folder. Output: the host, the guard and the folder.
fn host_in(home: &Path) -> (Tmux, Ended, PathBuf) {
    let folder = home.join("terminal");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let tmux = installed(home).expect("tmux is required for this test");
    let host = Tmux::open(
        Some(tmux.clone()),
        &folder,
        test_confinement(),
        test_hooks(),
    );
    let ended = Ended {
        tmux,
        socket: host.socket(),
    };
    (host, ended, folder)
}

/// A second host on the folder of a live server, as after an engine restart, reattaches
/// with the variables a lead's environment set still on the server.
#[test]
fn a_reattach_to_a_live_server_keeps_the_lead_environment() {
    let home = tempfile::tempdir().unwrap();
    let (first, _ended, folder) = host_in(home.path());
    let control = first.control().unwrap();
    control.run("set-environment -g LEAD_ONLY kept").unwrap();
    let again = Tmux::open(
        first.executable.clone(),
        &folder,
        test_confinement(),
        test_hooks(),
    );
    let shown = again.control().unwrap().run("show-environment -g").unwrap();
    assert_eq!(variable_names(&shown), ["LEAD_ONLY"]);
}

/// A server the engine did not start, holding a planted variable, is emptied on the first
/// connect, and a server started again after it ended is emptied again.
#[test]
fn a_server_no_connect_emptied_is_emptied_on_connect_and_after_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let (host, ended, folder) = host_in(home.path());
    let planted = crate::spawn_gate::status(
        Command::new(&ended.tmux)
            .args(["-u", "-f", "/dev/null", "-S"])
            .arg(&ended.socket)
            .args(["new-session", "-d", "-s", SESSION, "-n", HOLDER, "cat"])
            .env_clear()
            .env("PATH", SERVER_PATH)
            .env("PLANTED_TOKEN", "planted")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(planted.success());
    let control = host.control().unwrap();
    assert!(variable_names(&control.run("show-environment -g").unwrap()).is_empty());
    control.run("kill-server").ok();
    drop(control);
    let restarted = Tmux::open(
        host.executable.clone(),
        &folder,
        test_confinement(),
        test_hooks(),
    );
    let shown = restarted
        .control()
        .unwrap()
        .run("show-environment -g")
        .unwrap();
    assert!(variable_names(&shown).is_empty());
}
