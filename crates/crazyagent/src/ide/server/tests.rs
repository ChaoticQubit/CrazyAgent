use super::*;
use crate::sandbox::{ProjectSandbox, SandboxNetwork};
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BUILD: &str = "vscodium-1.143.17183";
const GONE_WAIT: Duration = Duration::from_secs(15);
const PREPARATION_HELD: Duration = Duration::from_secs(15);
const CUT_START_LIMIT: Duration = Duration::from_secs(3);

/// The pinned VSCodium reh-web build that IDE-1 extracted into the project-local cache, in
/// the `.local-state/ide-cache` of the nearest checkout above this crate. A machine without
/// it fails these tests: the IDE server is unqualified there.
pub(crate) fn cached_build() -> PathBuf {
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

/// A register on `data` as the owner server makes one: its own grants and the owner socket
/// path the sandbox lets a server reach. Inputs: the data folder and the idle limit.
fn servers_in(data: &Path, idle: Duration) -> Servers {
    Servers::new(
        data,
        idle,
        START_LIMIT,
        Arc::default(),
        data.join("engine").join("owner.sock"),
    )
}

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

/// The process group the running launch recorded in a project's IDE folder. Input: the
/// folder. Output: the group's id; panics when none is recorded.
pub(crate) fn recorded_launch(folder: &Path) -> i32 {
    recorded_group(&folder.join(GROUP))
        .expect("a running launch records its process group")
        .0
        .as_raw_nonzero()
        .get()
}

fn present(group: i32) -> bool {
    group_present(Pid::from_raw(group).unwrap())
}

pub(crate) async fn gone(group: i32) {
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

fn pinned(info: &ServerInfo) -> String {
    format!(
        "/?workspace={}",
        proxy::encoded(info.workspace.to_str().unwrap())
    )
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
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("loopback");
    let info = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
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
    assert_eq!(
        servers
            .open(&project, checkout.path(), &cached_build(), false)
            .await
            .unwrap(),
        info
    );
    assert_eq!(servers.group(&project), Some(group));
    servers.stop_all().await;
    gone(group).await;
    assert!(listeners(info.server_port).unwrap().is_empty());
    assert!(listeners(info.port).unwrap().is_empty());
}

#[tokio::test]
async fn request_without_the_token_is_refused() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("refused");
    let info = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
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
    let page = pinned(&info);
    assert_eq!(
        info.workspace,
        servers.project_folder(&project).unwrap().join(WORKSPACE)
    );
    let owner = get(
        info.port,
        &host,
        &page,
        Some(&format!("vscode-tkn={token}")),
    )
    .await;
    assert!(status(&owner).contains(" 200 "), "{}", status(&owner));
    let elsewhere = get(
        info.port,
        &host,
        "/?folder=/private/tmp",
        Some(&format!("vscode-tkn={token}")),
    )
    .await;
    assert!(
        status(&elsewhere).contains(" 302 "),
        "{}",
        status(&elsewhere)
    );
    assert!(
        elsewhere.contains(&format!("\r\nLocation: {page}\r\n")),
        "{elsewhere}"
    );
    let cookie = get(info.port, &host, &format!("{page}&tkn={token}"), None).await;
    assert!(status(&cookie).contains(" 302 "), "{}", status(&cookie));
    assert!(cookie.contains(&format!("vscode-tkn={token}")));
    let other = format!("evil.example:{}", info.port);
    let rebound = get(info.port, &other, "/", Some(&format!("vscode-tkn={token}"))).await;
    assert!(status(&rebound).contains(" 403 "), "{}", status(&rebound));
    servers.stop_all().await;
}

#[tokio::test]
async fn token_file_is_0600_new_per_launch_and_outside_every_agent_read_allowlist() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("token");
    let build = cached_build();
    let first = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
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
    let second = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
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
        ("CRAZYAGENT_IDE_SOCKET", "/private/tmp/forged.sock"),
        ("CRAZYAGENT_IDE_LOCKED_FILE", "/private/tmp/forged-locks"),
        ("CODEX_HOME", "/private/tmp/codex"),
        ("GIT_ASKPASS", "/private/tmp/askpass"),
        ("NODE_OPTIONS", "--require /private/tmp/evil.js"),
        ("DYLD_INSERT_LIBRARIES", "/private/tmp/evil.dylib"),
        ("LC_", "bad"),
        ("LC_lower", "bad"),
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "core.fsmonitor"),
        ("GIT_CONFIG_VALUE_0", "/private/tmp/evil-monitor"),
        ("GIT_CEILING_DIRECTORIES", "/private/tmp/forged-ceiling"),
        ("GIT_CONFIG_GLOBAL", "/private/tmp/forged-config"),
    ];
    let source = given
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
    let capability = Path::new("/private/tmp/ide/project/capability");
    let socket = Path::new("/private/tmp/data/engine/owner.sock");
    let locked = Path::new("/private/tmp/ide/project/locked-settings.json");
    let ceiling = std::ffi::OsStr::new("/private/tmp/project/.crazyagent/worktrees");
    let git_config = Path::new("/private/tmp/ide/project/gitconfig");
    let built = env::server_env(source, capability, socket, locked, ceiling, git_config);
    assert!(built.len() <= env::ENV_MAX);
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
    let mut expected: Vec<(&str, &str)> = given[..10].to_vec();
    expected.push((
        env::CAPABILITY_VARIABLE,
        "/private/tmp/ide/project/capability",
    ));
    expected.push((env::SOCKET_VARIABLE, "/private/tmp/data/engine/owner.sock"));
    expected.push((
        crate::ide::settings::LOCKED_VARIABLE,
        "/private/tmp/ide/project/locked-settings.json",
    ));
    expected.push((
        crate::ide::git::CEILING_VARIABLE,
        "/private/tmp/project/.crazyagent/worktrees",
    ));
    expected.extend(crate::ide::git::CONFIG_ENV);
    expected.push(("GIT_CONFIG_GLOBAL", "/private/tmp/ide/project/gitconfig"));
    expected.push(("GIT_CONFIG_NOSYSTEM", "1"));
    expected.sort();
    assert_eq!(seen, expected);
    assert!(seen.contains(&("GIT_CONFIG_KEY_0", "diff.ignoreSubmodules")));
    assert!(seen.iter().all(|(name, _)| {
        env::allowed(std::ffi::OsStr::new(name))
            || *name == env::CAPABILITY_VARIABLE
            || *name == env::SOCKET_VARIABLE
            || *name == crate::ide::settings::LOCKED_VARIABLE
            || *name == crate::ide::git::CEILING_VARIABLE
            || *name == "GIT_CONFIG_GLOBAL"
            || *name == "GIT_CONFIG_NOSYSTEM"
            || crate::ide::git::CONFIG_ENV
                .iter()
                .any(|(fixed, _)| name == fixed)
    }));
    assert!(
        !printed.contains("fixture-") && !printed.contains("forged") && !printed.contains("evil")
    );
    assert!(!env::allowed(std::ffi::OsStr::new("SSH_AUTH_SOCK")));
    assert!(!printed.contains("SSH_AUTH_SOCK") && !printed.contains("agent.sock"));
}

#[tokio::test]
async fn port_is_stable_for_a_project_across_restarts() {
    let checkout = scratch();
    let data = scratch();
    let build = cached_build();
    let project = project("ports");
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let first = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
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
    let again = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
    assert_eq!(
        (again.port, again.server_port),
        (first.port, first.server_port)
    );
    assert!(servers.stop(&project).await);
    drop(servers);

    let held = crate::spawn_gate::bind_tcp_std(("127.0.0.1", first.server_port)).unwrap();
    let restarted = servers_in(data.path(), IDLE_LIMIT);
    let moved = restarted
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
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
    let kept = restarted
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
    assert_eq!(kept.server_port, moved.server_port);
    assert_eq!(kept.port, first.port);
    restarted.stop_all().await;
}

/// INT1-4: the IDE hooks switch reaches the git shim through the product's launch path. A
/// launch with it on writes the shim that lets git run hooks in the canonical project
/// folder alone; the next launch with it off writes the plain shim again.
#[tokio::test]
async fn the_hooks_switch_reaches_the_git_shim_through_open() {
    let checkout = scratch();
    let data = scratch();
    let build = cached_build();
    let project = project("hooks");
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    servers.open(&project, &root, &build, true).await.unwrap();
    let folder = servers.project_folder(&project).unwrap();
    let shim = settings::machine_folder(&folder.join(SERVER_DATA)).join(crate::ide::git::SHIM);
    let on = fs::read_to_string(&shim).unwrap();
    assert_eq!(
        on,
        crate::ide::git::shim_text(Some(&root), crate::research::runner::sandboxed_git()).unwrap()
    );
    assert_ne!(
        on,
        crate::ide::git::shim_text(None, crate::research::runner::sandboxed_git()).unwrap()
    );
    assert!(
        on.contains(&format!("if [ \"$(pwd -P)\" = '{}' ]", root.display())),
        "{on}"
    );
    assert!(servers.stop(&project).await);
    servers.open(&project, &root, &build, false).await.unwrap();
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(None, crate::research::runner::sandboxed_git()).unwrap()
    );
    servers.stop_all().await;
}

/// `open` leaves the git lookup, which runs xcrun on a Mac without the Command Line Tools,
/// to a blocking thread: the test's single async thread never makes it, and the shim it
/// feeds still names the same git (FZ7-R1-L1).
#[tokio::test]
async fn open_looks_up_the_ides_git_off_the_async_thread() {
    let checkout = scratch();
    let data = scratch();
    let build = cached_build();
    let project = project("lookup");
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    crate::research::runner::SANDBOXED_GIT_LOOKUPS.set(0);
    servers.open(&project, &root, &build, false).await.unwrap();
    assert_eq!(
        crate::research::runner::SANDBOXED_GIT_LOOKUPS.get(),
        0,
        "the async thread looked up git itself"
    );
    let folder = servers.project_folder(&project).unwrap();
    let shim = settings::machine_folder(&folder.join(SERVER_DATA)).join(crate::ide::git::SHIM);
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(None, crate::research::runner::sandboxed_git()).unwrap()
    );
    servers.stop_all().await;
}

#[tokio::test]
async fn idle_server_stops_after_the_idle_limit() {
    let checkout = scratch();
    let data = scratch();
    let idle = Duration::from_secs(3);
    let servers = servers_in(data.path(), idle);
    let project = project("idle");
    let info = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
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

/// Starts opening a project's IDE and drops the open once its launch is stored but before
/// it listens, as the IDE tab's request timeout cuts a slow start. While the open is still
/// pending its launch cannot have passed `ready`. Inputs: the register, the project, the
/// checkout and the build. Output: the cut launch's process group.
async fn cut_open(servers: &Servers, project: &ProjectId, checkout: &Path, build: &Path) -> i32 {
    let open = servers.open(project, checkout, build, false);
    tokio::pin!(open);
    let deadline = Instant::now() + START_LIMIT;
    loop {
        tokio::select! {
            opened = &mut open => panic!("the open finished before it could be cut: {opened:?}"),
            () = tokio::time::sleep(Duration::from_millis(2)) => {}
        }
        if let Some(group) = servers.group(project) {
            return group;
        }
        assert!(
            Instant::now() < deadline,
            "the open never stored its launch"
        );
    }
}

#[tokio::test]
async fn an_open_cut_while_its_server_starts_still_stops_when_idle() {
    let checkout = scratch();
    let data = scratch();
    let idle = Duration::from_secs(3);
    let servers = servers_in(data.path(), idle);
    let project = project("cut-idle");
    let group = cut_open(&servers, &project, checkout.path(), &cached_build()).await;
    let issued = servers.project_folder(&project).unwrap().join(token::FILE);
    assert!(issued.exists());
    assert!(
        present(group),
        "the cut launch runs on with nobody waiting for it"
    );
    gone(group).await;
    assert_eq!(servers.group(&project), None);
    assert!(!servers.touch(&project));
    let deadline = Instant::now() + Duration::from_secs(2);
    while issued.exists() {
        assert!(Instant::now() < deadline, "a cut launch's token stayed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn an_open_cut_while_its_server_starts_is_never_handed_back() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("cut-reuse");
    let build = cached_build();
    let cut = cut_open(&servers, &project, checkout.path(), &build).await;
    assert!(present(cut));

    let info = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
    let group = servers.group(&project).unwrap();
    assert_ne!(group, cut, "the next open started its own launch");
    gone(cut).await;
    assert!(
        group_listeners(group).contains(&format!("127.0.0.1:{}", info.server_port)),
        "the new launch, which passed its port check, holds the server port"
    );
    let again = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
    assert_eq!(again, info, "a ready launch is handed back");
    assert_eq!(servers.group(&project), Some(group));
    servers.stop_all().await;
    gone(group).await;
}

/// A cut launch that nobody opens again stops once its start limit passes, long before
/// the idle limit, since it never passed `ready`: with the production idle limit only the
/// start limit can end it.
#[tokio::test]
async fn an_open_cut_while_its_server_starts_stops_at_its_start_limit_well_before_idle() {
    let checkout = scratch();
    let data = scratch();
    let servers = Servers::new(
        data.path(),
        IDLE_LIMIT,
        CUT_START_LIMIT,
        Arc::default(),
        data.path().join("engine").join("owner.sock"),
    );
    let project = project("cut-start");
    let group = cut_open(&servers, &project, checkout.path(), &cached_build()).await;
    assert!(
        present(group),
        "the cut launch runs on with nobody waiting for it"
    );
    gone(group).await;
    assert_eq!(servers.group(&project), None);
    assert!(!servers.touch(&project));
}

/// An open that finds an extension preparation still running on its folder, as one an open
/// cut by the IDE tab's timeout leaves behind, waits for it and starts nothing beside it,
/// so two installs never race on one extensions folder; it goes on once that one ends.
#[tokio::test]
async fn an_open_waits_for_an_extension_preparation_still_running_on_its_folder() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("prepare-wait");
    let extensions = servers.project_folder(&project).unwrap().join(EXTENSIONS);
    let (holding, held) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let running = preparation(&extensions);
        let _running = locked(&running);
        holding.send(()).unwrap();
        let _ = released.recv();
    });
    held.recv().unwrap();
    let build = cached_build();
    let open = servers.open(&project, checkout.path(), &build, false);
    tokio::pin!(open);
    let deadline = Instant::now() + PREPARATION_HELD;
    while Instant::now() < deadline {
        tokio::select! {
            opened = &mut open => panic!("the open ran beside a running preparation: {opened:?}"),
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        assert_eq!(
            servers.group(&project),
            None,
            "the open started its server beside a running preparation"
        );
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    open.await.unwrap();
    let group = servers.group(&project).unwrap();
    servers.stop_all().await;
    gone(group).await;
}

#[tokio::test]
async fn no_ide_server_process_survives_engine_stop() {
    let checkout = scratch();
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
    let servers = service.owner_runtime().unwrap().ide_servers();
    let info = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
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
    let checkout = scratch();
    let other_checkout = scratch();
    let data = scratch();
    let build = cached_build();
    let forgotten = project("forgotten");
    let project = project("crashed");
    let (left, path, name, other, opened, other_opened) = (
        data.path().to_path_buf(),
        build.clone(),
        project.clone(),
        forgotten.clone(),
        checkout.path().to_path_buf(),
        other_checkout.path().to_path_buf(),
    );
    let (old, old_group, old_token, other_group) = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let crashed = servers_in(&left, IDLE_LIMIT);
        let info = runtime
            .block_on(crashed.open(&name, &opened, &path, false))
            .unwrap();
        runtime
            .block_on(crashed.open(&other, &other_opened, &path, false))
            .unwrap();
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
    let servers = servers_in(data.path(), IDLE_LIMIT);
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
    let info = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
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
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("log");
    let info = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
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
    let target = pinned(&info);
    let page = get(info.port, &host, &format!("{target}&tkn={token}"), None).await;
    assert!(status(&page).contains(" 302 "), "{}", status(&page));
    let page = get(info.port, &host, &target, Some(&cookie)).await;
    assert!(status(&page).contains(" 200 "), "{}", status(&page));
    servers.stop_all().await;
    let text = fs::read_to_string(&log).unwrap();
    assert!(!text.is_empty());
    assert!(text.lines().all(|line| !line.contains(&token)), "{text}");
    assert!(!text.contains(&token[..token::TOKEN_BYTES]));
}

#[tokio::test]
async fn the_workbench_page_gets_the_webview_endpoint_the_defaults_and_a_closed_policy() {
    let checkout = scratch();
    let data = scratch();
    let build = cached_build();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("page");
    let info = servers
        .open(&project, checkout.path(), &build, false)
        .await
        .unwrap();
    let token = fs::read_to_string(&info.token_path).unwrap();
    let cookie = format!("{}={token}", proxy::COOKIE);
    let port = info.port;
    let target = pinned(&info);
    for host in [format!("127.0.0.1:{port}"), format!("localhost:{port}")] {
        let page = get(port, &host, &target, Some(&cookie)).await;
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
        &target,
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
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("socket");
    let info = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
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

/// One project's real server launch, for running its git without starting the server.
/// Inputs: the register, the project and its folder. Output: the command the engine built
/// and the project's IDE folder.
fn built_server(servers: &Servers, project: &ProjectId, root: &Path) -> (Command, PathBuf) {
    let folder = servers.prepare(project).unwrap();
    let (token_path, _) = token::issue(&folder).unwrap();
    let info = ServerInfo {
        url: "http://127.0.0.1:47963/".to_owned(),
        port: 47_963,
        server_port: 47_961,
        token_path,
        capability_path: folder.join(CAPABILITY),
        workspace: folder.join(WORKSPACE),
    };
    let (_reader, writer) = crate::spawn_gate::guarded(std::io::pipe).unwrap();
    let built = command(
        Launch {
            project,
            build: &cached_build(),
            folder: &folder,
            root,
            socket: &servers.socket,
        },
        &info,
        writer,
        false,
    )
    .unwrap();
    (built, folder)
}

/// Runs git as a built IDE server starts it: the engine's shim under the server's installed
/// profile and environment, with the owner's home as `HOME`, as the app has it. Inputs: the
/// built command, the IDE folder, the owner's home, the working folder and git's arguments.
/// Output: git's output.
fn server_git(
    built: &Command,
    folder: &Path,
    owner: &Path,
    cwd: &Path,
    args: &[&str],
) -> std::process::Output {
    let shim = settings::machine_folder(&folder.join(SERVER_DATA)).join(crate::ide::git::SHIM);
    let mut git = sandbox::command(&folder.join(sandbox::PROFILE), &shim);
    git.args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(
            built
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
        .env("HOME", owner)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::spawn_gate::output(&mut git).unwrap()
}

/// An owner home holding git config files the IDE profile denies: `.gitconfig` with an
/// identity whose name needs quoting, and git's default ignore file. Output: the folder.
fn owner_home_with_git_config() -> tempfile::TempDir {
    let home = scratch();
    fs::write(
        home.path().join(".gitconfig"),
        "[user]\n\tname = \"Dana \\\"Q\\\" O\\\\wner\"\n\temail = dana@example.invalid\n",
    )
    .unwrap();
    let xdg = home.path().join(".config").join("git");
    fs::create_dir_all(&xdg).unwrap();
    fs::write(xdg.join("ignore"), "*.denied\n").unwrap();
    home
}

/// Makes an owner repository holding one uncommitted file, with none of the owner's config.
/// Input: its folder.
fn owner_repository(root: &Path) {
    let mut init = Command::new(crate::research::runner::setup_git());
    init.args(["init", "-q"])
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert!(
        crate::spawn_gate::output(&mut init)
            .unwrap()
            .status
            .success()
    );
    fs::write(root.join("a.txt"), "owner\n").unwrap();
}

/// Git dies on a user config it may not read instead of treating it as missing, which the
/// owner saw as "Git not found" in every project: with the owner's real home as `HOME`, every
/// git the IDE server starts reads a config its profile allows.
#[test]
fn the_servers_git_runs_beside_a_config_the_profile_denies_in_the_owners_home() {
    let (checkout, data, owner) = (scratch(), scratch(), owner_home_with_git_config());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    owner_repository(&root);
    let (built, folder) = built_server(&servers, &project("denied-config"), &root);
    for args in [&["--version"][..], &["status", "--porcelain"]] {
        let output = server_git(&built, &folder, owner.path(), &root, args);
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A commit made from the IDE carries the name and email of the owner's own git config,
/// which the engine hands the server because the server may not read it, and git's default
/// ignore file in the owner's home, denied too, draws no warning.
#[test]
fn a_commit_from_the_ide_carries_the_name_and_email_of_the_owners_config() {
    let (checkout, data, owner) = (scratch(), scratch(), owner_home_with_git_config());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    owner_repository(&root);
    let (built, folder) = built_server(&servers, &project("owner-identity"), &root);
    crate::ide::git::write_global(&folder, &root, Some(owner.path())).unwrap();
    let run = |args: &[&str]| server_git(&built, &folder, owner.path(), &root, args);
    let status = run(&["status", "--porcelain"]);
    let said = String::from_utf8_lossy(&status.stderr).into_owned();
    assert!(status.status.success(), "{said}");
    assert!(!said.contains("unable to access"), "{said}");
    for args in [&["add", "a.txt"][..], &["commit", "-q", "-m", "first"]] {
        let output = run(args);
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let log = run(&["log", "-1", "--format=%an <%ae>"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "Dana \"Q\" O\\wner <dana@example.invalid>"
    );
}

/// The git ceiling one built server command hands the IDE server. Inputs: the register,
/// the project and its folder. Output: the value.
fn command_ceiling(servers: &Servers, project: &ProjectId, root: &Path) -> OsString {
    let folder = servers.prepare(project).unwrap();
    let (token_path, _) = token::issue(&folder).unwrap();
    let info = ServerInfo {
        url: "http://127.0.0.1:47943/".to_owned(),
        port: 47_943,
        server_port: 47_941,
        token_path,
        capability_path: folder.join(CAPABILITY),
        workspace: folder.join(WORKSPACE),
    };
    let (_reader, writer) = crate::spawn_gate::guarded(std::io::pipe).unwrap();
    let command = command(
        Launch {
            project,
            build: &cached_build(),
            folder: &folder,
            root,
            socket: &servers.socket,
        },
        &info,
        writer,
        false,
    )
    .unwrap();
    let ceiling = command
        .get_envs()
        .find(|(name, _)| *name == crate::ide::git::CEILING_VARIABLE)
        .and_then(|(_, value)| value)
        .expect("the server's environment holds the git ceiling");
    ceiling.to_owned()
}

/// INT1-R3-1: the server's git ceiling follows the engine's records. A project that gained
/// a `.crazyagent` folder gets the new layout's agents root alone while the engine records
/// no agent of it in `<project>/worktrees`, another project's record there counting for
/// nothing; once the engine records one of its agents there, the older root is added.
#[test]
fn the_server_ceiling_keeps_the_older_agents_root_while_the_engine_records_an_agent_there() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("gained");
    let root = fs::canonicalize(checkout.path()).unwrap();
    fs::create_dir_all(root.join(".crazyagent").join("worktrees")).unwrap();
    let db = crate::landing::open(data.path()).unwrap();
    crate::team::initialize(&db).unwrap();
    let record = |owner: &str, worktree: &Path| {
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (?1,'sub','codex',0,'stopped',1,1)",
            [owner],
        )
        .unwrap();
        let agent = db.last_insert_rowid();
        db.execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                agent,
                owner,
                root.to_str().unwrap(),
                worktree.to_str().unwrap(),
                root.join(".git")
                    .join("worktrees")
                    .join("agent")
                    .to_str()
                    .unwrap()
            ],
        )
        .unwrap();
    };
    record("elsewhere", &root.join("worktrees").join("agent-1"));
    record(
        project.as_str(),
        &root.join(".crazyagent").join("worktrees").join("agent-2"),
    );
    assert_eq!(
        command_ceiling(&servers, &project, &root),
        crate::ide::git::ceiling(&root, false).unwrap()
    );
    assert_eq!(
        command_ceiling(&servers, &project, &root),
        root.join(".crazyagent").join("worktrees").into_os_string()
    );

    record(project.as_str(), &root.join("worktrees").join("agent-3"));
    let both = command_ceiling(&servers, &project, &root);
    assert_eq!(both, crate::ide::git::ceiling(&root, true).unwrap());
    assert_eq!(
        both.to_str().unwrap(),
        format!(
            "{}:{}",
            root.join(".crazyagent").join("worktrees").display(),
            root.join("worktrees").display()
        )
    );
}

/// INT1R3-L1: a research job's worktree is not a team record, yet it stays in the older
/// agents root after its job. The ceiling keeps that root while a folder there carries the
/// name the research runner gives a launch of one of the project's own jobs; another
/// project's job, the owner's own folder of a like name, a link and the new layout's
/// research folders count for nothing.
#[test]
fn the_server_ceiling_keeps_the_older_root_while_a_research_worktree_stays_there() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("gained");
    let root = fs::canonicalize(checkout.path()).unwrap();
    let older = root.join("worktrees");
    let layout = root.join(".crazyagent").join("worktrees");
    fs::create_dir_all(&older).unwrap();
    fs::create_dir_all(&layout).unwrap();
    let jobs = crate::research::jobs::ResearchJobs::open(&data.path().join("engine.db")).unwrap();
    let elsewhere = jobs.enqueue("elsewhere", "market", "codex", false).unwrap();
    let mine = jobs
        .enqueue(project.as_str(), "market", "codex", false)
        .unwrap();
    let layout_only = crate::ide::git::ceiling(&root, false).unwrap();
    assert_eq!(layout_only, layout.clone().into_os_string());

    fs::create_dir(older.join(format!("research-market-{elsewhere}-1"))).unwrap();
    fs::create_dir(layout.join(format!("research-market-{mine}-1"))).unwrap();
    fs::create_dir(older.join("research-notes")).unwrap();
    fs::create_dir(older.join(format!("research-other-{mine}-1"))).unwrap();
    std::os::unix::fs::symlink(
        layout.join(format!("research-market-{mine}-1")),
        older.join(format!("research-market-{mine}-2")),
    )
    .unwrap();
    assert_eq!(
        command_ceiling(&servers, &project, &root),
        layout_only,
        "no folder in the older root is a research worktree of this project's jobs"
    );

    fs::create_dir(older.join(format!("research-market-{mine}-1-2"))).unwrap();
    let both = command_ceiling(&servers, &project, &root);
    assert_eq!(both, crate::ide::git::ceiling(&root, true).unwrap());
    assert_eq!(
        both.to_str().unwrap(),
        format!("{}:{}", layout.display(), older.display()),
        "a later launch's worktree of the project's job keeps the older root"
    );
}

/// A project's older agents root with more entries than the engine reads for research
/// worktrees keeps the git stop rather than guessing it away.
#[test]
fn an_older_agents_root_too_large_to_read_keeps_the_git_stop() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("crowded");
    let root = fs::canonicalize(checkout.path()).unwrap();
    let older = root.join("worktrees");
    fs::create_dir_all(root.join(".crazyagent").join("worktrees")).unwrap();
    fs::create_dir_all(&older).unwrap();
    crate::research::jobs::ResearchJobs::open(&data.path().join("engine.db")).unwrap();
    for index in 0..crate::ide::workspace::OLDER_ENTRIES_MAX {
        fs::create_dir(older.join(format!("owner-{index}"))).unwrap();
    }
    assert_eq!(
        command_ceiling(&servers, &project, &root),
        crate::ide::git::ceiling(&root, false).unwrap(),
        "a root at the limit is read in full"
    );
    fs::create_dir(older.join("owner-last")).unwrap();
    assert_eq!(
        command_ceiling(&servers, &project, &root),
        crate::ide::git::ceiling(&root, true).unwrap(),
        "one entry past the limit keeps the older root"
    );
}

#[test]
fn the_server_command_line_is_verdict_3_under_the_ide_sandbox() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let project = project("command");
    let folder = servers.prepare(&project).unwrap();
    let build = cached_build();
    let (token_path, token) = token::issue(&folder).unwrap();
    let info = ServerInfo {
        url: "http://127.0.0.1:47933/".to_owned(),
        port: 47_933,
        server_port: 47_931,
        token_path: token_path.clone(),
        capability_path: folder.join(CAPABILITY),
        workspace: folder.join(WORKSPACE),
    };
    let (_reader, writer) = crate::spawn_gate::guarded(std::io::pipe).unwrap();
    let socket = data.path().join("engine").join("owner.sock");
    let command = command(
        Launch {
            project: &project,
            build: &build,
            folder: &folder,
            root: checkout.path(),
            socket: &socket,
        },
        &info,
        writer,
        false,
    )
    .unwrap();
    assert_eq!(command.get_program(), crate::sandbox::SANDBOX_EXEC);
    let profile = folder.join(sandbox::PROFILE);
    let expected: Vec<OsString> = [
        OsString::from("-f"),
        profile.clone().into(),
        build.join("bin").join("codium-server").into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        "47931".into(),
        "--connection-token-file".into(),
        token_path.clone().into(),
        "--server-data-dir".into(),
        folder.join("server-data").into(),
        "--user-data-dir".into(),
        folder.join("server-data").join("data").into(),
        "--extensions-dir".into(),
        folder.join("extensions").into(),
        "--accept-server-license-terms".into(),
        "--telemetry-level".into(),
        "off".into(),
        "--disable-workspace-trust".into(),
        "--reconnection-grace-time".into(),
        "30".into(),
    ]
    .into();
    let args: Vec<OsString> = command.get_args().map(ToOwned::to_owned).collect();
    assert_eq!(args, expected);
    assert!(
        args.iter()
            .all(|arg| !arg.to_string_lossy().contains(&token))
    );
    assert_eq!(
        command.get_current_dir(),
        Some(folder.join(SERVER_DATA).as_path()),
        "the server starts in a folder its profile lets it read, so getcwd works under Seatbelt"
    );
    let envs: Vec<(&std::ffi::OsStr, Option<&std::ffi::OsStr>)> = command.get_envs().collect();
    for (name, value) in crate::ide::git::CONFIG_ENV {
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new(name),
                Some(std::ffi::OsStr::new(value))
            )),
            "{name}"
        );
    }
    assert!(envs.contains(&(
        std::ffi::OsStr::new(env::SOCKET_VARIABLE),
        Some(socket.as_os_str())
    )));
    assert!(envs.contains(&(
        std::ffi::OsStr::new(env::CAPABILITY_VARIABLE),
        Some(info.capability_path.as_os_str())
    )));
    let ceiling = crate::ide::git::ceiling(checkout.path(), false).unwrap();
    assert!(envs.contains(&(
        std::ffi::OsStr::new(crate::ide::git::CEILING_VARIABLE),
        Some(ceiling.as_os_str())
    )));
    assert!(
        envs.iter()
            .all(|(_, value)| value.is_none_or(|value| !value.to_string_lossy().contains(&token)))
    );
    assert_eq!(mode(&token_path), 0o600);
    let home = folder.join("server-data");
    for path in [
        home.clone(),
        home.join("data"),
        home.join("data").join("Machine"),
        folder.join("extensions"),
    ] {
        assert_eq!(mode(&path), 0o700, "{}", path.display());
    }
    assert_eq!(
        settings::machine_folder(&home),
        home.join("data").join("Machine")
    );
    let machine: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(home.join("data").join("Machine").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(machine["chat.disableAIFeatures"], serde_json::json!(true));
    assert_eq!(
        machine["extensions.allowed"],
        crate::ide::extensions::allowed_setting(data.path())["extensions.allowed"],
        "each start writes the owner's extension pin list into the server's Remote settings"
    );
    assert_eq!(
        machine["extensions.allowed"]
            .as_object()
            .map(|pins| pins.keys().cloned().collect::<Vec<_>>()),
        Some(vec![crate::ide::extensions::BUILT_IN.to_owned()]),
        "with nothing approved only the built-in is pinned"
    );
    let text = fs::read_to_string(&profile).unwrap();
    assert!(text.contains("(local ip \"localhost:47931\")"), "{text}");
    assert!(!text.contains("47933"));
    assert!(text.contains(&format!("(subpath \"{}\")", home.display())));
    assert!(!text.contains(&folder.join("user-data").display().to_string()));
    for file in [
        &token_path,
        &info.capability_path,
        &info.workspace,
        &folder.join(settings::LOCKED_FILE),
    ] {
        assert!(
            text.contains(&format!("(literal \"{}\")", file.display())),
            "{}",
            file.display()
        );
    }
}

/// The handshake the built-in extension sends with the token in its capability file.
fn capability_packet(path: &Path) -> [u8; 37] {
    let text = fs::read_to_string(path).unwrap();
    assert_eq!(text.len(), 64);
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(bridge::MAGIC);
    for (index, byte) in packet[5..].iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
    }
    packet
}

#[tokio::test]
async fn a_launch_hands_the_built_in_extension_its_capability_until_it_stops() {
    let checkout = scratch();
    let data = scratch();
    let grants: Arc<bridge::Grants> = Arc::default();
    let socket = data.path().join("engine").join("owner.sock");
    let servers = Servers::new(
        data.path(),
        IDLE_LIMIT,
        START_LIMIT,
        Arc::clone(&grants),
        socket,
    );
    let project = project("built-in");
    let info = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
    let extensions = servers.project_folder(&project).unwrap().join(EXTENSIONS);
    let installed = extensions.join(bridge::EXTENSION);
    for (name, bytes) in bridge::EXTENSION_FILES {
        assert_eq!(fs::read(installed.join(name)).unwrap(), bytes, "{name}");
    }
    let listed: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(extensions.join("extensions.json")).unwrap()).unwrap();
    assert_eq!(
        listed
            .iter()
            .filter(|entry| entry["identifier"]["id"] == extensions::BUILT_IN
                && entry["relativeLocation"] == bridge::EXTENSION)
            .count(),
        1,
        "the server's profile lists the built-in, or the server deletes it at start: {listed:?}"
    );
    assert_eq!(
        info.capability_path,
        servers.project_folder(&project).unwrap().join(CAPABILITY)
    );
    assert_eq!(mode(&info.capability_path), 0o600);
    let packet = capability_packet(&info.capability_path);
    assert_eq!(
        grants.admit(&packet).unwrap_err().0,
        "ide_session_required",
        "the launch's capability file holds a live grant for the extension to take"
    );
    let group = servers.group(&project).unwrap();
    assert!(servers.stop(&project).await);
    gone(group).await;
    assert!(!info.capability_path.exists());
    assert_eq!(
        grants.admit(&packet).unwrap_err().0,
        "ide_capability_refused",
        "a stopped server's capability is revoked"
    );
    let again = servers
        .open(&project, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
    let renewed = capability_packet(&again.capability_path);
    assert_ne!(renewed, packet, "each launch gets a new capability");
    assert_eq!(
        grants.admit(&renewed).unwrap_err().0,
        "ide_session_required"
    );
    assert_eq!(
        grants.admit(&packet).unwrap_err().0,
        "ide_capability_refused"
    );
    servers.stop_all().await;
    assert!(!again.capability_path.exists());
    assert_eq!(
        grants.admit(&renewed).unwrap_err().0,
        "ide_capability_refused"
    );
}

#[tokio::test]
async fn a_start_stops_waiting_once_stop_all_began() {
    let slot = Slot::default();
    let group = Pid::from_raw(std::process::id() as i32).unwrap();
    assert_eq!(
        ready(&slot, 0, 1, group, START_LIMIT, &AtomicBool::new(true))
            .await
            .unwrap_err()
            .0,
        "ide_servers_closed"
    );
    assert_eq!(
        ready(&slot, 0, 1, group, START_LIMIT, &AtomicBool::new(false))
            .await
            .unwrap_err()
            .0,
        "ide_server_stopped"
    );
}

/// A project's IDE stays refused until the last Delete everything holding it ends, and the
/// open then goes on to its next check.
#[tokio::test]
async fn a_projects_ide_stays_refused_until_its_last_delete_hold_ends() {
    let folder = scratch();
    let servers = servers_in(folder.path(), Duration::from_secs(60));
    let held = project("held");
    let open = || servers.open(&held, folder.path(), folder.path(), false);
    let first = servers.hold_for_delete(&held).await;
    let second = servers.hold_for_delete(&held).await;
    drop(first);
    assert_eq!(open().await.unwrap_err().0, "ide_project_deleting");
    drop(second);
    assert_eq!(open().await.unwrap_err().0, "ide_build_invalid");
}

/// A hold that lands while an open is still starting waits for that open and stops what it
/// started, and the next open is refused.
#[tokio::test]
async fn a_delete_hold_landing_on_a_starting_open_stops_what_it_started() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let starting = project("starting");
    let recorded = data.path().join("ide").join("starting").join(GROUP);
    let build = cached_build();
    let opening = servers.open(&starting, checkout.path(), &build, false);
    let holding = async {
        let deadline = Instant::now() + START_LIMIT;
        let group = loop {
            if let Some((group, _)) = recorded_group(&recorded) {
                break group.as_raw_nonzero().get();
            }
            assert!(
                Instant::now() < deadline,
                "the launch never recorded its group"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        (group, servers.hold_for_delete(&starting).await)
    };
    let (opened, (group, hold)) = tokio::join!(opening, holding);
    assert!(opened.is_ok(), "{opened:?}");
    gone(group).await;
    assert_eq!(
        servers
            .open(&starting, checkout.path(), &build, false)
            .await
            .unwrap_err()
            .0,
        "ide_project_deleting"
    );
    drop(hold);
    servers.stop_all().await;
}

/// A timed-out open is dropped while its blocking work runs on, and that work writes the
/// project's settings. The work keeps the start lock until it ends, so a retry waits for it
/// instead of writing beside it, and the work never runs without the lock (ide-8 r1 sec F1).
#[tokio::test]
async fn a_dropped_open_keeps_the_start_lock_until_its_blocking_work_ends() {
    let start = Arc::new(tokio::sync::Mutex::new(()));
    let held = Arc::clone(&start).lock_owned().await;
    let (entered, running) = tokio::sync::oneshot::channel();
    let (finish, done) = std::sync::mpsc::channel::<()>();
    let open = tokio::spawn(async move {
        let mut held = Some(held);
        holding_start(&mut held, move || {
            entered.send(()).unwrap();
            done.recv().unwrap();
        })
        .await
    });
    running.await.unwrap();
    open.abort();
    assert!(open.await.unwrap_err().is_cancelled());
    assert!(
        start.try_lock().is_err(),
        "a retry must wait for the work a dropped open left running"
    );
    finish.send(()).unwrap();
    assert!(
        tokio::time::timeout(GONE_WAIT, start.lock()).await.is_ok(),
        "the lock is free once the work has ended"
    );
    let unlocked = tokio::spawn(async { holding_start(&mut None, || ()).await });
    assert!(unlocked.await.unwrap_err().is_panic());
}

/// A blocking call that never returns holds its project's start lock. The engine's stop waits
/// [`STOP_LEFTOVER_WAIT`] in all for such locks, however many there are, so the shutdown
/// steps after it still run (ide-8 r2, rv-code L1 and sec F1).
#[tokio::test]
async fn stop_all_goes_on_past_leftovers_that_hold_their_start_locks() {
    let data = scratch();
    let servers = servers_in(data.path(), Duration::from_secs(60));
    let mut held = Vec::new();
    for name in ["hung-one", "hung-two"] {
        let slot = servers.slot(&project(name)).unwrap();
        held.push(Arc::clone(&slot.start).lock_owned().await);
    }
    let limit = STOP_LEFTOVER_WAIT + STOP_LEFTOVER_WAIT / 2;
    assert!(
        tokio::time::timeout(limit, servers.stop_all())
            .await
            .is_ok(),
        "the engine's stop must go on past a hung leftover"
    );
    assert_eq!(held.len(), 2);
}

/// Only the guard keeps the lock of an open that was dropped, so a full table must still keep
/// that slot: otherwise the project's retry gets a new lock and writes beside the leftover
/// (ide-8 r2, sec F2).
#[tokio::test]
async fn a_full_table_keeps_the_slot_a_leftover_still_holds() {
    let data = scratch();
    let servers = servers_in(data.path(), Duration::from_secs(60));
    let leftover = project("leftover");
    let slot = servers.slot(&leftover).unwrap();
    let held = Arc::clone(&slot.start).lock_owned().await;
    drop(slot);
    for index in 1..PROJECTS_MAX {
        servers.slot(&project(&format!("filler-{index}"))).unwrap();
    }
    servers.slot(&project("one-more")).unwrap();
    let retry = servers.slot(&leftover).unwrap();
    assert!(
        Arc::ptr_eq(&retry.start, tokio::sync::OwnedMutexGuard::mutex(&held)),
        "a retry must wait on the leftover's lock"
    );
}

/// A recorded server behind a start lock that stays held must still end with the engine. Its
/// group goes, and its record, token and group file are removed once the wait runs out, so a
/// leftover cannot make an IDE server outlive the engine (ide-10 r1, rv-code L1 and sec F1).
#[tokio::test]
async fn stop_all_ends_a_recorded_server_behind_a_held_start_lock() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let id = project("behind-lock");
    servers
        .open(&id, checkout.path(), &cached_build(), false)
        .await
        .unwrap();
    let group = servers.group(&id).unwrap();
    let folder = servers.project_folder(&id).unwrap();
    assert!(folder.join(token::FILE).is_file());
    assert!(folder.join(GROUP).is_file());
    let held = servers.slot(&id).unwrap().start.clone().lock_owned().await;
    servers.stop_all().await;
    gone(group).await;
    assert_eq!(servers.group(&id), None, "the server stays on record");
    assert!(!folder.join(token::FILE).exists(), "token left behind");
    assert!(!folder.join(GROUP).exists(), "group file left behind");
    drop(held);
}
