use super::*;
use crate::sandbox::{ProjectSandbox, SandboxNetwork};
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BUILD: &str = "vscodium-1.143.17183";
const GONE_WAIT: Duration = Duration::from_secs(15);

/// The pinned VSCodium reh-web build that IDE-1 extracted into the project-local cache, in
/// the `.local-state/ide-cache` of the nearest checkout above this crate. A machine without
/// it fails these tests: the IDE server is unqualified there.
fn cached_build() -> PathBuf {
    let build = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|folder| folder.join(".local-state").join("ide-cache").join(BUILD))
        .find(|build| build.join(SERVER_MAIN).is_file())
        .expect("no pinned VSCodium reh-web build in a .local-state/ide-cache above the crate");
    fs::canonicalize(build).unwrap()
}

fn scratch() -> tempfile::TempDir {
    let folder = tempfile::Builder::new()
        .prefix("ca-ide-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

fn present(group: i32) -> bool {
    group_present(Pid::from_raw(group).unwrap())
}

async fn gone(group: i32) {
    let deadline = Instant::now() + GONE_WAIT;
    while present(group) {
        assert!(
            Instant::now() < deadline,
            "IDE server group {group} still has processes"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn request(port: u16, head: &str) -> String {
    let mut stream = crate::spawn_gate::connect_tcp(("127.0.0.1", port))
        .await
        .unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&reply).into_owned()
}

async fn get(port: u16, host: &str, path: &str, cookie: Option<&str>) -> String {
    let cookie = cookie
        .map(|cookie| format!("Cookie: {cookie}\r\n"))
        .unwrap_or_default();
    request(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{cookie}Connection: close\r\n\r\n"),
    )
    .await
}

fn status(reply: &str) -> &str {
    reply.split("\r\n").next().unwrap_or_default()
}

/// Every TCP address a process group listens on, read with real `lsof` across the machine.
fn group_listeners(group: i32) -> Vec<String> {
    let output = crate::spawn_gate::output(
        Command::new("/usr/sbin/lsof")
            .env_clear()
            .env("LC_ALL", "C")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpgn"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut current = None;
    let mut addresses = Vec::new();
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        if line.starts_with('p') {
            current = None;
        } else if let Some(pgid) = line.strip_prefix('g') {
            current = pgid.parse::<i32>().ok();
        } else if let Some(address) = line.strip_prefix('n')
            && current == Some(group)
        {
            addresses.push(address.to_owned());
        }
    }
    addresses
}

#[tokio::test]
async fn ide_server_listens_only_on_loopback() {
    let data = scratch();
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    let project = project("loopback");
    let info = servers.open(&project, &cached_build()).await.unwrap();
    let group = servers.group(&project).unwrap();
    assert_eq!(info.url, format!("http://127.0.0.1:{}/", info.port));
    assert_ne!(info.port, info.server_port);
    let addresses = group_listeners(group);
    assert!(
        addresses.contains(&format!("127.0.0.1:{}", info.server_port)),
        "{addresses:?}"
    );
    assert!(!addresses.contains(&format!("127.0.0.1:{}", info.port)));
    assert!(
        addresses
            .iter()
            .all(|address| address.starts_with("127.0.0.1:")),
        "{addresses:?}"
    );
    assert_eq!(
        listeners(info.port).unwrap(),
        vec![(
            std::process::id() as i32,
            format!("127.0.0.1:{}", info.port)
        )]
    );
    assert_eq!(servers.open(&project, &cached_build()).await.unwrap(), info);
    assert_eq!(servers.group(&project), Some(group));
    servers.stop_all().await;
    gone(group).await;
    assert!(listeners(info.server_port).unwrap().is_empty());
    assert!(listeners(info.port).unwrap().is_empty());
}

#[tokio::test]
async fn request_without_the_token_is_refused() {
    let data = scratch();
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    let project = project("refused");
    let info = servers.open(&project, &cached_build()).await.unwrap();
    let host = format!("127.0.0.1:{}", info.port);
    let token = fs::read_to_string(&info.token_path).unwrap();
    let wrong = "0".repeat(token.len());
    for port in [info.port, info.server_port] {
        let host = format!("127.0.0.1:{port}");
        let none = get(port, &host, "/", None).await;
        assert!(status(&none).contains(" 403 "), "{port}: {}", status(&none));
        let forged = get(port, &host, "/", Some(&format!("vscode-tkn={wrong}"))).await;
        assert!(
            status(&forged).contains(" 403 "),
            "{port}: {}",
            status(&forged)
        );
        let queried = get(port, &host, &format!("/?tkn={wrong}"), None).await;
        assert!(
            status(&queried).contains(" 403 "),
            "{port}: {}",
            status(&queried)
        );
    }
    let short = get(
        info.port,
        &host,
        "/",
        Some(&format!("vscode-tkn={}", &token[1..])),
    )
    .await;
    assert!(status(&short).contains(" 403 "), "{}", status(&short));
    let owner = get(info.port, &host, "/", Some(&format!("vscode-tkn={token}"))).await;
    assert!(status(&owner).contains(" 200 "), "{}", status(&owner));
    let cookie = get(info.port, &host, &format!("/?tkn={token}"), None).await;
    assert!(status(&cookie).contains(" 302 "), "{}", status(&cookie));
    assert!(cookie.contains(&format!("vscode-tkn={token}")));
    let other = format!("evil.example:{}", info.port);
    let rebound = get(info.port, &other, "/", Some(&format!("vscode-tkn={token}"))).await;
    assert!(status(&rebound).contains(" 403 "), "{}", status(&rebound));
    servers.stop_all().await;
}

#[tokio::test]
async fn token_file_is_0600_new_per_launch_and_outside_every_agent_read_allowlist() {
    let data = scratch();
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    let project = project("token");
    let build = cached_build();
    let first = servers.open(&project, &build).await.unwrap();
    let folder = servers.project_folder(&project).unwrap();
    assert_eq!(first.token_path, folder.join(token::FILE));
    assert_eq!(first.capability_path, folder.join(CAPABILITY));
    assert!(
        fs::symlink_metadata(&first.token_path)
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(mode(&first.token_path), 0o600);
    assert_eq!(mode(&folder), 0o700);
    assert_eq!(mode(&data.path().join(crate::ide::FOLDER)), 0o700);
    let old = fs::read_to_string(&first.token_path).unwrap();
    assert_eq!(old.len(), 2 * token::TOKEN_BYTES);
    assert!(old.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(servers.stop(&project).await);
    assert!(!first.token_path.exists());
    let second = servers.open(&project, &build).await.unwrap();
    assert_eq!(second.token_path, first.token_path);
    assert_eq!(mode(&second.token_path), 0o600);
    let new = fs::read_to_string(&second.token_path).unwrap();
    assert_eq!(new.len(), old.len());
    assert_ne!(new, old);

    let agent = scratch();
    let root = agent.path();
    for name in ["project", "home", "tmp", "profiles"] {
        fs::create_dir(root.join(name)).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &root.join("project"),
        &root.join("home"),
        &root.join("tmp"),
        SandboxNetwork::Open,
    )
    .unwrap()
    .guard_engine(data.path())
    .unwrap();
    let ide = data.path().join(crate::ide::FOLDER);
    assert!(sandbox.secret_paths().contains(&ide));
    assert!(sandbox.clone().allow_read(&ide).is_err());
    assert!(sandbox.clone().allow_read(&folder).is_err());
    assert!(sandbox.clone().allow_worktree(&folder).is_err());
    assert!(crate::sandbox::extra_paths::checked(folder.to_str().unwrap(), data.path()).is_err());
    let profile = root.join("profiles").join("agent.sb");
    sandbox.install(&profile).unwrap();
    let mut cat = sandbox.command(&profile, Path::new("/bin/cat"));
    cat.arg(&second.token_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let read = crate::spawn_gate::output(&mut cat).unwrap();
    assert!(!read.status.success());
    assert!(!String::from_utf8_lossy(&read.stdout).contains(&new));
    servers.stop_all().await;
}

#[test]
fn server_env_holds_only_allowlisted_names() {
    let given = [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/Users/owner"),
        ("USER", "owner"),
        ("LOGNAME", "owner"),
        ("SHELL", "/bin/zsh"),
        ("LANG", "en_US.UTF-8"),
        ("LC_ALL", "C"),
        ("LC_CTYPE", "UTF-8"),
        ("TERM", "xterm-256color"),
        ("TMPDIR", "/private/tmp/"),
        ("SSH_AUTH_SOCK", "/private/tmp/agent.sock"),
        ("ANTHROPIC_API_KEY", "fixture-provider-key"),
        ("OPENAI_API_KEY", "fixture-provider-key"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "fixture-provider-token"),
        ("GITHUB_TOKEN", "fixture-github-token"),
        ("GH_TOKEN", "fixture-github-token"),
        ("AWS_SECRET_ACCESS_KEY", "fixture-cloud-key"),
        ("CRAZYAGENT_POOL_ACCOUNT", "fixture-pool-account"),
        ("CRAZYAGENT_IDE_CAPABILITY_FILE", "/private/tmp/forged"),
        ("CODEX_HOME", "/private/tmp/codex"),
        ("GIT_ASKPASS", "/private/tmp/askpass"),
        ("NODE_OPTIONS", "--require /private/tmp/evil.js"),
        ("DYLD_INSERT_LIBRARIES", "/private/tmp/evil.dylib"),
        ("LC_", "bad"),
        ("LC_lower", "bad"),
    ];
    let source = given
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
    let capability = Path::new("/private/tmp/ide/project/capability");
    let built = env::server_env(source, capability);
    let mut command = Command::new("/usr/bin/env");
    command
        .env_clear()
        .envs(built.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = crate::spawn_gate::output(&mut command).unwrap();
    assert!(output.status.success());
    let printed = String::from_utf8(output.stdout).unwrap();
    let mut seen: Vec<(&str, &str)> = printed
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    seen.sort();
    let mut expected: Vec<(&str, &str)> = given[..11].to_vec();
    expected.push((
        env::CAPABILITY_VARIABLE,
        "/private/tmp/ide/project/capability",
    ));
    expected.sort();
    assert_eq!(seen, expected);
    assert!(
        seen.iter()
            .all(|(name, _)| env::allowed(std::ffi::OsStr::new(name))
                || *name == env::CAPABILITY_VARIABLE)
    );
    assert!(
        !printed.contains("fixture-") && !printed.contains("forged") && !printed.contains("evil")
    );
}

#[tokio::test]
async fn port_is_stable_for_a_project_across_restarts() {
    let data = scratch();
    let build = cached_build();
    let project = project("ports");
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    let first = servers.open(&project, &build).await.unwrap();
    let folder = servers.project_folder(&project).unwrap();
    assert_ne!(first.port, first.server_port);
    assert_eq!(
        ports::recorded(&folder),
        Some(Ports {
            server: first.server_port,
            proxy: first.port
        })
    );
    assert!(servers.stop(&project).await);
    let again = servers.open(&project, &build).await.unwrap();
    assert_eq!(
        (again.port, again.server_port),
        (first.port, first.server_port)
    );
    assert!(servers.stop(&project).await);
    drop(servers);

    let held = crate::spawn_gate::bind_tcp_std(("127.0.0.1", first.server_port)).unwrap();
    let restarted = Servers::new(data.path(), IDLE_LIMIT);
    let moved = restarted.open(&project, &build).await.unwrap();
    assert_ne!(moved.server_port, first.server_port);
    assert_eq!(moved.port, first.port);
    assert_eq!(
        ports::recorded(&folder),
        Some(Ports {
            server: moved.server_port,
            proxy: moved.port
        })
    );
    drop(held);
    assert!(restarted.stop(&project).await);
    let kept = restarted.open(&project, &build).await.unwrap();
    assert_eq!(kept.server_port, moved.server_port);
    assert_eq!(kept.port, first.port);
    restarted.stop_all().await;
}

#[tokio::test]
async fn idle_server_stops_after_the_idle_limit() {
    let data = scratch();
    let idle = Duration::from_secs(3);
    let servers = Servers::new(data.path(), idle);
    let project = project("idle");
    let info = servers.open(&project, &cached_build()).await.unwrap();
    let group = servers.group(&project).unwrap();
    let mut untouched = Instant::now();
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        untouched = Instant::now();
        assert!(servers.touch(&project));
        assert!(present(group));
    }
    gone(group).await;
    assert!(untouched.elapsed() >= idle);
    assert_eq!(servers.group(&project), None);
    assert!(!servers.touch(&project));
    let deadline = Instant::now() + Duration::from_secs(2);
    while info.token_path.exists() {
        assert!(Instant::now() < deadline, "an idle server's token stayed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!servers.touch(&ProjectId::new("never-opened").unwrap()));
}

#[tokio::test]
async fn no_ide_server_process_survives_engine_stop() {
    let root = scratch();
    let global = root.path().join("global.toml");
    fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = crate::engine::bootstrap::EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let project = project("engine-stop");
    let servers = service.owner_runtime().unwrap().ide();
    let info = servers.open(&project, &cached_build()).await.unwrap();
    assert!(
        info.token_path
            .starts_with(root.path().join(crate::ide::FOLDER))
    );
    let group = servers.group(&project).unwrap();
    assert!(present(group));
    tokio::time::timeout(Duration::from_secs(30), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while present(group) {
        assert!(
            Instant::now() < deadline,
            "an IDE server process outlived the engine"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(listeners(info.server_port).unwrap().is_empty());
    assert!(listeners(info.port).unwrap().is_empty());
    assert!(!info.token_path.exists());
    drop(stop);
}

#[tokio::test]
async fn a_server_left_by_a_crashed_engine_is_ended_before_its_project_opens_again() {
    let data = scratch();
    let build = cached_build();
    let forgotten = project("forgotten");
    let project = project("crashed");
    let (left, path, name, other) = (
        data.path().to_path_buf(),
        build.clone(),
        project.clone(),
        forgotten.clone(),
    );
    let (old, old_group, old_token, other_group) = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let crashed = Servers::new(&left, IDLE_LIMIT);
        let info = runtime.block_on(crashed.open(&name, &path)).unwrap();
        runtime.block_on(crashed.open(&other, &path)).unwrap();
        let group = crashed.group(&name).unwrap();
        let other_group = crashed.group(&other).unwrap();
        let token = fs::read_to_string(&info.token_path).unwrap();
        std::mem::forget(crashed);
        (info, group, token, other_group)
    })
    .join()
    .unwrap();
    assert!(present(old_group));
    assert!(present(other_group));
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    servers.sweep_left().await;
    let other_leader = Pid::from_raw(other_group).unwrap();
    assert!(
        rustix::process::waitpid(Some(other_leader), rustix::process::WaitOptions::NOHANG)
            .unwrap()
            .is_some()
    );
    gone(other_group).await;
    assert_eq!(servers.group(&forgotten), None);
    assert!(
        !servers
            .project_folder(&forgotten)
            .unwrap()
            .join(GROUP)
            .exists()
    );
    let info = servers.open(&project, &build).await.unwrap();
    let leader = Pid::from_raw(old_group).unwrap();
    assert!(
        rustix::process::waitpid(Some(leader), rustix::process::WaitOptions::NOHANG)
            .unwrap()
            .is_some()
    );
    gone(old_group).await;
    assert_eq!((info.port, info.server_port), (old.port, old.server_port));
    assert_ne!(fs::read_to_string(&info.token_path).unwrap(), old_token);
    assert_ne!(servers.group(&project), Some(old_group));
    servers.stop_all().await;
}

#[tokio::test]
async fn the_server_log_never_holds_the_token() {
    let data = scratch();
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    let project = project("log");
    let info = servers.open(&project, &cached_build()).await.unwrap();
    let token = fs::read_to_string(&info.token_path).unwrap();
    let log = servers.project_folder(&project).unwrap().join(LOG);
    assert_eq!(mode(&log), 0o600);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut text = fs::read_to_string(&log).unwrap();
    while !text.contains("Web UI available") {
        assert!(
            Instant::now() < deadline,
            "the server never printed its URL: {text}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        text = fs::read_to_string(&log).unwrap();
    }
    let printed = text
        .lines()
        .find(|line| line.contains("Web UI available"))
        .unwrap();
    assert!(
        printed.contains(&format!("tkn={}", output::REDACTED)),
        "{printed}"
    );
    let host = format!("127.0.0.1:{}", info.port);
    let cookie = format!("{}={token}", proxy::COOKIE);
    let page = get(info.port, &host, &format!("/?tkn={token}"), None).await;
    assert!(status(&page).contains(" 302 "), "{}", status(&page));
    let page = get(info.port, &host, "/", Some(&cookie)).await;
    assert!(status(&page).contains(" 200 "), "{}", status(&page));
    servers.stop_all().await;
    let text = fs::read_to_string(&log).unwrap();
    assert!(!text.is_empty());
    assert!(text.lines().all(|line| !line.contains(&token)), "{text}");
    assert!(!text.contains(&token[..token::TOKEN_BYTES]));
}

#[tokio::test]
async fn the_workbench_page_gets_the_webview_endpoint_the_defaults_and_a_closed_policy() {
    let data = scratch();
    let build = cached_build();
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    let project = project("page");
    let info = servers.open(&project, &build).await.unwrap();
    let token = fs::read_to_string(&info.token_path).unwrap();
    let cookie = format!("{}={token}", proxy::COOKIE);
    let port = info.port;
    for host in [format!("127.0.0.1:{port}"), format!("localhost:{port}")] {
        let page = get(port, &host, "/", Some(&cookie)).await;
        assert!(status(&page).contains(" 200 "), "{}", status(&page));
        let (head, body) = page.split_once("\r\n\r\n").unwrap();
        let header = |name: &str| {
            head.split("\r\n").find_map(|line| {
                line.split_once(':')
                    .filter(|(known, _)| known.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.trim().to_owned())
            })
        };
        assert_eq!(header("content-length"), Some(body.len().to_string()));
        assert_eq!(header("connection").as_deref(), Some("close"));
        let policy = header("content-security-policy").unwrap();
        let directives: Vec<&str> = policy
            .split(';')
            .map(str::trim)
            .filter(|directive| !directive.is_empty())
            .collect();
        for (name, expected) in [
            (
                "frame-src",
                format!("frame-src 'self' http://*.localhost:{port} data:"),
            ),
            ("img-src", "img-src 'self' data: blob:".to_owned()),
            ("connect-src", format!("connect-src 'self' ws://{host}")),
        ] {
            let named: Vec<&&str> = directives
                .iter()
                .filter(|directive| directive.split(' ').next() == Some(name))
                .collect();
            assert_eq!(named, vec![&expected.as_str()], "{policy}");
        }
        for kept in [
            "default-src 'self'",
            "child-src 'self'",
            "worker-src 'self' data: blob:",
        ] {
            assert!(directives.contains(&kept), "{kept}: {policy}");
        }
        assert!(
            directives
                .iter()
                .any(|directive| directive.starts_with("script-src 'self' 'unsafe-eval'")),
            "{policy}"
        );
        assert_eq!(
            body.matches("vscode-workbench-web-configuration").count(),
            1
        );
        assert!(
            body.contains(&format!(
                "&quot;webviewEndpoint&quot;:&quot;{}&quot;",
                proxy::template(port)
            )),
            "{body}"
        );
        assert!(body.contains("&quot;configurationDefaults&quot;:{"));
        for member in [
            "&quot;workbench.startupEditor&quot;:&quot;none&quot;",
            "&quot;workbench.welcomePage.extraAnnouncements&quot;:false",
            "&quot;task.allowAutomaticTasks&quot;:&quot;off&quot;",
        ] {
            assert!(body.contains(member), "{member}");
        }
    }
    let direct = get(
        info.server_port,
        &format!("127.0.0.1:{}", info.server_port),
        "/",
        Some(&cookie),
    )
    .await;
    assert!(status(&direct).contains(" 200 "), "{}", status(&direct));
    assert!(!direct.contains("webviewEndpoint"));
    assert!(direct.contains("connect-src 'self' ws: wss: https:;"));

    let webview = format!("0123456789abcdefghijklmnopqrstuv0123456789abcdefghij.localhost:{port}");
    let pre = build.join(proxy::PRE);
    for (name, kind) in proxy::PRE_FILES {
        let reply = get(port, &webview, &format!("/{name}?id=1&swVersion=4"), None).await;
        assert!(
            status(&reply).contains(" 200 "),
            "{name}: {}",
            status(&reply)
        );
        assert!(reply.contains(&format!("Content-Type: {kind}\r\n")));
        let file = fs::read_to_string(pre.join(name)).unwrap();
        assert!(reply.ends_with(&format!("\r\n\r\n{file}")), "{name}");
    }
    for path in ["/", "/host.js", "/out/server-main.js", "/version"] {
        let reply = get(port, &webview, path, Some(&cookie)).await;
        assert!(
            status(&reply).contains(" 404 "),
            "{path}: {}",
            status(&reply)
        );
    }
    servers.stop_all().await;
}

#[tokio::test]
async fn websocket_upgrades_pass_the_proxy_only_with_the_token() {
    let data = scratch();
    let servers = Servers::new(data.path(), IDLE_LIMIT);
    let project = project("socket");
    let info = servers.open(&project, &cached_build()).await.unwrap();
    let token = fs::read_to_string(&info.token_path).unwrap();
    let host = format!("127.0.0.1:{}", info.port);
    let upgrade = |cookie: &str| {
        format!(
            "GET /?reconnectionToken=8f1d3c3e-2b7a-4e55-9d63-5a0b3c1e7f21&reconnection=false&skipWebSocketFrames=false HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{cookie}\r\n"
        )
    };
    let refused = request(info.port, &upgrade("")).await;
    assert!(status(&refused).contains(" 403 "), "{refused}");
    let mut stream = crate::spawn_gate::connect_tcp(("127.0.0.1", info.port))
        .await
        .unwrap();
    stream
        .write_all(upgrade(&format!("Cookie: {}={token}\r\n", proxy::COOKIE)).as_bytes())
        .await
        .unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(20), async {
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).await.unwrap(), 1, "{head:?}");
            head.push(byte[0]);
        }
    })
    .await
    .unwrap();
    let head = String::from_utf8(head).unwrap();
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "{head}"
    );
    assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
    drop(stream);
    servers.stop_all().await;
}
