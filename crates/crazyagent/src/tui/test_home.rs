use crate::client_controller::OwnerController;
use crate::engine::owner::{OwnerResponse, OwnerResult};
use std::os::unix::fs::PermissionsExt;

/// Sets a fresh projects home beside the engine root, never inside it.
/// Input: an attached owner controller and the mutation generation.
/// Output: the home folder, kept alive by the caller; panics unless the
/// engine applied it.
pub(in crate::tui) async fn projects_home(
    controller: &mut OwnerController,
    generation: u64,
) -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("ca-tui-home-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::fs::canonicalize(home.path()).unwrap();
    let response = controller
        .set_projects_home(path.to_str().unwrap().into(), generation)
        .await
        .unwrap();
    assert!(
        matches!(
            &response,
            OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
        ),
        "{response:?}"
    );
    home
}

/// Tells a test pump that its client is still attaching to the engine, so a pause in the
/// traffic is not yet the engine settling: under load the attach can outlast the pump's
/// quiet rounds, and a pump that stopped then left every first read unsent.
/// Input: the shell's link. Output: true while it connects or reconnects.
pub(in crate::tui) fn connecting(link: &crate::tui::ctx::Link) -> bool {
    matches!(
        link,
        crate::tui::ctx::Link::Connecting | crate::tui::ctx::Link::Reconnecting
    )
}

/// How long a TUI engine test lets the engine and its client wait for one answer.
/// Parallel test engines share one disk, so their flushed writes queue behind
/// each other and one write can take seconds; the 1 s default then answers
/// OutcomeUnknown although the engine is healthy.
pub(in crate::tui) const ENGINE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Engine settings for TUI engine tests: the defaults with the owner wait
/// raised to `ENGINE_WAIT`.
/// Input: none. Output: the effective configuration to start the engine with.
pub(in crate::tui) fn engine_config() -> crate::config::EffectiveConfig {
    let config = crate::config::resolve_config_bytes(
        &format!("timeout_ms = {}\n", ENGINE_WAIT.as_millis()),
        "",
    )
    .unwrap();
    assert_eq!(
        u128::from(config.settings.timeout_ms),
        ENGINE_WAIT.as_millis()
    );
    config
}

/// Stands for the seeded local account's address in test settings. A test engine swaps it
/// for a loopback port that refuses every connection, so no row reaches a model server the
/// host runs (a host Ollama on 127.0.0.1:11434 made the account read ready).
pub(in crate::tui) const LOCAL_ADDRESS: &str = "LOCAL_ADDRESS";

/// Binds a loopback port without listening on it, so every connection to it is refused
/// while the socket lives and no other process can take the port. Output: the socket, which
/// the test engine holds, and its address.
pub(in crate::tui) fn refusing_port() -> (tokio::net::TcpSocket, std::net::SocketAddr) {
    let socket =
        crate::spawn_gate::bind_tcp_unlistened(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .unwrap();
    let address = socket.local_addr().unwrap();
    assert!(
        address.ip().is_loopback() && address.port() != 0,
        "{address}"
    );
    (socket, address)
}

/// Swaps `LOCAL_ADDRESS` in test engine settings for a loopback port that refuses every
/// connection. Input: settings that name `LOCAL_ADDRESS`. Output: the bound socket, which
/// the caller holds for as long as its engine runs, and the settings to start it with.
pub(in crate::tui) fn refusing_settings(settings: &str) -> (tokio::net::TcpSocket, String) {
    assert!(settings.contains(LOCAL_ADDRESS), "{settings}");
    let (socket, address) = refusing_port();
    let settings = settings.replace(LOCAL_ADDRESS, &address.to_string());
    assert!(!settings.contains(LOCAL_ADDRESS), "{settings}");
    (socket, settings)
}

/// rv-sec2 L388 F1: the port a TUI test engine seeds turns a connect away at once on macOS
/// and Linux, so each account probe takes the refused path in well under its 400 ms budget,
/// never a dropped SYN that waits the probe out.
#[test]
fn the_seeded_port_refuses_a_connect_within_a_tenth_of_a_second() {
    let (_held, address) = refusing_port();
    let started = std::time::Instant::now();
    let refused =
        crate::spawn_gate::connect_tcp_std_timeout(&address, std::time::Duration::from_secs(1))
            .unwrap_err();
    let took = started.elapsed();
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::ConnectionRefused,
        "{refused} after {took:?}"
    );
    assert!(took < std::time::Duration::from_millis(100), "{took:?}");
}

/// Asserts that every http model endpoint in a test engine's settings names a loopback port
/// that is none of the owner's tool ports and refuses connections, so the engine never
/// reaches a model server the host runs, such as Ollama on 11434 (backlog L388).
/// Input: the settings file the engine started with. Output: none; panics on a breach.
pub(in crate::tui) async fn assert_no_host_model_server(global: &std::path::Path) {
    let text = std::fs::read_to_string(global).unwrap();
    let settings: toml::Value = toml::from_str(&text).unwrap();
    let connections = settings
        .get("model_access")
        .and_then(|access| access.get("connections"))
        .and_then(toml::Value::as_array)
        .expect("the settings list model connections");
    let mut checked = 0;
    for connection in connections {
        let Some(url) = connection
            .get("endpoint")
            .and_then(|endpoint| endpoint.get("url"))
            .and_then(toml::Value::as_str)
        else {
            continue;
        };
        let url = url::Url::parse(url).unwrap();
        let port = url.port_or_known_default().unwrap();
        assert!(
            !crate::sandbox::OWNER_PORTS.contains(&port),
            "{url} names a port the owner's tools listen on"
        );
        let host: std::net::IpAddr = url.host_str().unwrap().parse().unwrap();
        assert!(host.is_loopback(), "{url} is not a loopback address");
        assert!(
            !crate::accounts::probe::reachable(&url, std::time::Duration::from_millis(400)).await,
            "{url} accepted a connection"
        );
        checked += 1;
    }
    assert!(checked > 0, "no http endpoint was checked: {text}");
}

/// Runs real git in a folder with no system, global or hook config, for tests that build
/// a project checkout. Inputs: the folder and the git arguments. Output: git's standard
/// output; panics when git fails.
pub(in crate::tui) fn git(folder: &std::path::Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["-c", "core.hooksPath=/dev/null", "-C"])
            .arg(folder)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Makes a folder a git checkout on main with its own identity and a committed README,
/// then writes each instruction file as "<name> rule": the tracked ones committed, the
/// untracked ones left out of git. Inputs: the folder and the two name lists. Output: the
/// tip commit id.
pub(in crate::tui) fn instruction_repo(
    folder: &std::path::Path,
    tracked: &[&str],
    untracked: &[&str],
) -> String {
    git(folder, &["init", "-q", "-b", "main"]);
    git(folder, &["config", "user.name", "Owner"]);
    git(folder, &["config", "user.email", "owner@example.invalid"]);
    std::fs::write(folder.join("README.md"), "notes\n").unwrap();
    git(folder, &["add", "README.md"]);
    for name in tracked {
        std::fs::write(folder.join(name), format!("{name} rule\n")).unwrap();
        git(folder, &["add", name]);
    }
    git(folder, &["commit", "-q", "-m", "start"]);
    for name in untracked {
        std::fs::write(folder.join(name), format!("{name} rule\n")).unwrap();
    }
    git(folder, &["rev-parse", "HEAD"]).trim().to_owned()
}
