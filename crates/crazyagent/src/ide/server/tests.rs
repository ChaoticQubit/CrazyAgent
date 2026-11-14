use super::*;
use crate::ide::workspace::{IdePolicy, ide_policy};
use crate::sandbox::policy::{
    AgentAccess, AgentFilesRule, AgentNetwork, AgentPolicy, IdeAgentFiles,
};
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
        None,
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
        .open(&project, checkout.path(), &cached_build())
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
            .open(&project, checkout.path(), &cached_build())
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
        .open(&project, checkout.path(), &cached_build())
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
        .open(&project, checkout.path(), &build)
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
        .open(&project, checkout.path(), &build)
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
        ("CRAZYAGENT_IDE_ACCESS", "full"),
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
    let git = env::ServerGit {
        ceiling,
        config: git_config,
    };
    let built = env::server_env(
        source,
        capability,
        socket,
        locked,
        AgentAccess::OwnBranch,
        Some(git),
    );
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
    expected.push((env::ACCESS_VARIABLE, "own_branch"));
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
            || *name == env::ACCESS_VARIABLE
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
        .open(&project, checkout.path(), &build)
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
        .open(&project, checkout.path(), &build)
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
        .open(&project, checkout.path(), &build)
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
        .open(&project, checkout.path(), &build)
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
    set_policy(data.path(), &project, AgentAccess::Sandboxed, true);
    servers.open(&project, &root, &build).await.unwrap();
    let folder = servers.project_folder(&project).unwrap();
    let shim = settings::machine_folder(&folder.join(SERVER_DATA)).join(crate::ide::git::SHIM);
    let on = fs::read_to_string(&shim).unwrap();
    assert_eq!(
        on,
        crate::ide::git::shim_text(
            Some(&root),
            crate::research::runner::sandboxed_git(),
            Some((
                &root,
                &folder
                    .join(SERVER_DATA)
                    .join(crate::ide::git::PRIVATE_INDEX)
            ))
        )
        .unwrap()
    );
    assert_ne!(
        on,
        crate::ide::git::shim_text(
            None,
            crate::research::runner::sandboxed_git(),
            Some((
                &root,
                &folder
                    .join(SERVER_DATA)
                    .join(crate::ide::git::PRIVATE_INDEX)
            ))
        )
        .unwrap()
    );
    assert!(
        on.contains(&format!("if [ \"$(pwd -P)\" = '{}' ]", root.display())),
        "{on}"
    );
    assert!(servers.stop(&project).await);
    set_policy(data.path(), &project, AgentAccess::Sandboxed, false);
    servers.open(&project, &root, &build).await.unwrap();
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(
            None,
            crate::research::runner::sandboxed_git(),
            Some((
                &root,
                &folder
                    .join(SERVER_DATA)
                    .join(crate::ide::git::PRIVATE_INDEX)
            ))
        )
        .unwrap()
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
    servers.open(&project, &root, &build).await.unwrap();
    assert_eq!(
        crate::research::runner::SANDBOXED_GIT_LOOKUPS.get(),
        0,
        "the async thread looked up git itself"
    );
    let folder = servers.project_folder(&project).unwrap();
    let shim = settings::machine_folder(&folder.join(SERVER_DATA)).join(crate::ide::git::SHIM);
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        crate::ide::git::shim_text(
            None,
            crate::research::runner::sandboxed_git(),
            Some((
                &root,
                &folder
                    .join(SERVER_DATA)
                    .join(crate::ide::git::PRIVATE_INDEX)
            ))
        )
        .unwrap()
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
        .open(&project, checkout.path(), &cached_build())
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
    let open = servers.open(project, checkout, build);
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
        .open(&project, checkout.path(), &build)
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
        .open(&project, checkout.path(), &build)
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
        None,
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
    let open = servers.open(&project, checkout.path(), &build);
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
        .open(&project, checkout.path(), &cached_build())
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
            .block_on(crashed.open(&name, &opened, &path))
            .unwrap();
        runtime
            .block_on(crashed.open(&other, &other_opened, &path))
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
        .open(&project, checkout.path(), &build)
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
        .open(&project, checkout.path(), &cached_build())
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
        .open(&project, checkout.path(), &build)
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
        .open(&project, checkout.path(), &cached_build())
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
    (
        server_command(servers, project, root, &folder).unwrap(),
        folder,
    )
}

/// One project's server command as [`command`] builds it at a launch, refusal included.
/// Inputs: the register, the project, its folder and its prepared IDE folder. Output: the
/// command, or the refusal that stops the start.
fn server_command(
    servers: &Servers,
    project: &ProjectId,
    root: &Path,
    folder: &Path,
) -> Result<Command, EngineError> {
    let (token_path, _) = token::issue(folder).unwrap();
    let info = ServerInfo {
        url: "http://127.0.0.1:47963/".to_owned(),
        port: 47_963,
        server_port: 47_961,
        token_path,
        capability_path: folder.join(CAPABILITY),
        workspace: folder.join(WORKSPACE),
    };
    let (_reader, writer) = crate::spawn_gate::guarded(std::io::pipe).unwrap();
    command(
        Launch {
            project,
            build: &cached_build(),
            folder,
            root,
            socket: &servers.socket,
            policy: ide_policy(servers.folder.parent().unwrap(), project).unwrap(),
            kept_ports: &[],
        },
        &info,
        writer,
    )
}

/// A `.git` a server of an older profile left in its data folder, as a folder, a gitdir file,
/// a link, a hard link or a deep folder in any letter case, stops a confined start with
/// `ide_server_data_git` and stays where it was, so no later server renames it into the
/// project, since a rename checks only the two paths it names (SA vid 101 residue).
#[test]
fn a_git_left_in_the_server_data_folder_stops_a_confined_start() {
    let (checkout, data, outside) = (scratch(), scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let plants: [(&str, fn(&Path, &Path)); 5] = [
        (".git/hooks", |home, _| {
            fs::create_dir_all(home.join(".git").join("hooks")).unwrap();
        }),
        (".Git", |home, _| {
            fs::write(home.join(".Git"), "gitdir: /private/tmp/x\n").unwrap();
        }),
        (".GIT", |home, outside| {
            std::os::unix::fs::symlink(outside, home.join(".GIT")).unwrap();
        }),
        ("r/sub/.gIt", |home, _| {
            fs::create_dir_all(home.join("r").join("sub").join(".gIt")).unwrap();
        }),
        (".git", |home, _| {
            fs::write(home.join("plain"), "x").unwrap();
            fs::hard_link(home.join("plain"), home.join(".git")).unwrap();
        }),
    ];
    for (at, (name, plant)) in plants.iter().enumerate() {
        let id = project(&format!("planted-{at}"));
        let folder = servers.prepare(&id).unwrap();
        let home = folder.join(SERVER_DATA);
        assert_ne!(
            ide_policy(data.path(), &id).unwrap().access,
            AgentAccess::Full
        );
        plant(&home, outside.path());
        let refused = server_command(&servers, &id, &root, &folder).unwrap_err();
        assert_eq!(refused.0, "ide_server_data_git", "{name}");
        assert!(
            fs::symlink_metadata(home.join(name)).is_ok(),
            "{name} was removed instead of refusing the start"
        );
    }
}

/// The check reads at most its limit of entries and refuses past it, so a padded server data
/// folder cannot hold a start on a walk without end.
#[test]
fn a_server_data_folder_past_the_entry_limit_stops_a_start() {
    let home = scratch();
    fs::create_dir(home.path().join("a")).unwrap();
    for name in ["1", "a/2", "a/3"] {
        fs::write(home.path().join(name), "").unwrap();
    }
    assert!(refuse_planted_git(home.path(), 4).is_ok());
    fs::write(home.path().join("a").join("4"), "").unwrap();
    assert_eq!(
        refuse_planted_git(home.path(), 4).unwrap_err().0,
        "ide_server_data_git"
    );
}

/// A folder in the server data folder the check cannot read stops a confined start, since a
/// `.git` can sit in it and the server can open it again once it runs.
#[test]
fn an_unreadable_folder_in_the_server_data_folder_stops_a_confined_start() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let id = project("unreadable");
    let folder = servers.prepare(&id).unwrap();
    let shut = folder.join(SERVER_DATA).join("x");
    fs::create_dir_all(shut.join(".git")).unwrap();
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o000)).unwrap();
    let started = server_command(&servers, &id, &root, &folder);
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(started.unwrap_err().0, "ide_server_data_git");
}

/// Names near `.git` and a link to an outside folder that holds one leave a confined start
/// alone, as links are never followed, and a Full access start, which no profile confines,
/// is not checked.
#[test]
fn names_near_git_and_links_out_of_the_server_data_folder_leave_a_start_alone() {
    let (checkout, data, outside) = (scratch(), scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let id = project("near-git");
    let folder = servers.prepare(&id).unwrap();
    let home = folder.join(SERVER_DATA);
    fs::create_dir_all(home.join(".github")).unwrap();
    fs::create_dir_all(home.join("git")).unwrap();
    fs::write(home.join(".gitignore"), "x\n").unwrap();
    fs::write(home.join(".git.bak"), "x").unwrap();
    fs::create_dir_all(outside.path().join(".git")).unwrap();
    std::os::unix::fs::symlink(outside.path(), home.join("linked")).unwrap();
    assert!(server_command(&servers, &id, &root, &folder).is_ok());

    let full = project("full-access");
    set_level(data.path(), &full, AgentAccess::Full);
    let folder = servers.prepare(&full).unwrap();
    fs::create_dir_all(folder.join(SERVER_DATA).join(".git")).unwrap();
    assert!(server_command(&servers, &full, &root, &folder).is_ok());
}

/// A repository a server of an older profile moved from its data folder into the temp
/// folder every start of the project used to share is outside the next confined start: that
/// start gets a temp folder of its own, its profile never names the old one, so no server
/// renames the repository from there into the project, and nothing is deleted (SA vid 101
/// residue one rename further, rv-sec2 r1 F1, rv-sec3 r4 addendum).
#[test]
fn a_git_moved_into_the_old_server_temp_folder_is_outside_the_next_confined_start() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let id = project("moved-to-tmp");
    let folder = servers.prepare(&id).unwrap();
    let tmp = crate::sandbox::toolchains::make_tmp(&folder.join(SERVER_DATA)).unwrap();
    let planted = tmp.join("x").join(".git");
    fs::create_dir_all(planted.join("hooks")).unwrap();
    let started = server_command(&servers, &id, &root, &folder).map(|built| server_tmp(&built));
    let profile = fs::read_to_string(folder.join(sandbox::PROFILE)).unwrap_or_default();
    let kept = planted.is_dir();
    for made in [Some(&tmp), started.as_ref().ok()].into_iter().flatten() {
        let _ = fs::remove_dir_all(made);
    }
    let started = started.expect("a .git in the old temp folder refused a confined start");
    assert_ne!(started, tmp);
    assert!(
        !profile.contains(tmp.to_str().unwrap()),
        "the confined start's profile names the old temp folder"
    );
    assert!(kept, "the planted .git was removed");
}

/// A confined session that leaves its temp folder unusable for the walk, with a `.git`, a
/// folder it shut with mode 000 or more entries than any check reads, never stops the next
/// confined start of the project: each confined start gets a temp folder no earlier start
/// used, and the old one is outside its profile (rv-sec3 r4 addendum).
#[test]
fn a_confined_start_never_reuses_an_earlier_start_s_temp_folder() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let id = project("left-its-tmp");
    let folder = servers.prepare(&id).unwrap();
    let first = server_tmp(&server_command(&servers, &id, &root, &folder).unwrap());
    let shut = first.join("shut");
    fs::create_dir_all(first.join("clone").join(".git")).unwrap();
    fs::create_dir_all(&shut).unwrap();
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o000)).unwrap();
    let next = server_command(&servers, &id, &root, &folder).map(|built| server_tmp(&built));
    let profile = fs::read_to_string(folder.join(sandbox::PROFILE)).unwrap_or_default();
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o700)).unwrap();
    for made in [Some(&first), next.as_ref().ok()].into_iter().flatten() {
        let _ = fs::remove_dir_all(made);
    }
    let next = next.expect("an earlier start's temp folder refused the next confined start");
    assert_ne!(next, first);
    assert!(
        !profile.contains(first.to_str().unwrap()),
        "the next start's profile names the earlier start's temp folder"
    );
}

/// The temp folder a built server command gives the server as `TMPDIR`. Input: the command.
/// Output: the folder.
fn server_tmp(built: &Command) -> PathBuf {
    PathBuf::from(
        built
            .get_envs()
            .find(|(key, _)| key.to_str() == Some("TMPDIR"))
            .and_then(|(_, value)| value)
            .unwrap(),
    )
}

/// A `.git` a Full access session left in its temp folder, as a temporary clone or a git
/// build leaves one, never refuses a later confined start of the project, which gets a temp
/// folder of its own (rv-sec3 r2+r3 F1).
#[test]
fn a_git_a_full_access_session_left_in_its_temp_folder_leaves_a_confined_start_alone() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let id = project("full-then-confined");
    set_level(data.path(), &id, AgentAccess::Full);
    let folder = servers.prepare(&id).unwrap();
    let full = server_tmp(&server_command(&servers, &id, &root, &folder).unwrap());
    fs::create_dir_all(full.join("clone").join(".git").join("hooks")).unwrap();
    set_level(data.path(), &id, AgentAccess::Sandboxed);
    let confined = server_command(&servers, &id, &root, &folder).map(|built| server_tmp(&built));
    for made in [Some(&full), confined.as_ref().ok()].into_iter().flatten() {
        let _ = fs::remove_dir_all(made);
    }
    let confined =
        confined.expect("a .git in the Full access temp folder refused a confined start");
    assert_ne!(confined, full);
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
        .envs(crate::ide::bridge::engine_step::forwarder_env(
            &folder.join(SERVER_DATA),
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::ide::bridge::engine_step::output_with_engine(&mut git, &folder.join(SERVER_DATA), cwd)
}

/// An owner home holding git config files the IDE profile denies: `.gitconfig` with an
/// identity whose name needs quoting, and git's default ignore and attributes files. Output:
/// the folder.
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
    fs::write(xdg.join("attributes"), "*.denied binary\n").unwrap();
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

/// The git config the engine writes for the server is read only to it: neither git nor a
/// direct write from inside the server's profile changes the file.
#[test]
fn the_server_cannot_change_the_git_config_the_engine_wrote_for_it() {
    let (checkout, data, owner) = (scratch(), scratch(), owner_home_with_git_config());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    owner_repository(&root);
    let (built, folder) = built_server(&servers, &project("config-read-only"), &root);
    crate::ide::git::write_global(&folder, &root, Some(owner.path())).unwrap();
    let config = folder.join(crate::ide::git::GLOBAL_CONFIG);
    let before = fs::read(&config).unwrap();
    let git = server_git(
        &built,
        &folder,
        owner.path(),
        &root,
        &["config", "--global", "core.editor", "changed"],
    );
    assert!(
        !git.status.success(),
        "{}",
        String::from_utf8_lossy(&git.stderr)
    );
    let mut shell = sandbox::command(&folder.join(sandbox::PROFILE), Path::new("/bin/sh"));
    shell
        .args(["-c", &format!("printf x >> '{}'", config.display())])
        .current_dir(&root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let write = crate::spawn_gate::output(&mut shell).unwrap();
    let said = String::from_utf8_lossy(&write.stderr).into_owned();
    assert!(!write.status.success(), "{said}");
    assert!(said.contains("Operation not permitted"), "{said}");
    assert_eq!(fs::read(&config).unwrap(), before);
}

/// Git's default attributes file in the owner's home, which the server's profile denies,
/// draws no warning from the server's git. The same git pointed back at that file does warn,
/// so the row fails if the engine's config stops naming `/dev/null`.
#[test]
fn the_servers_git_draws_no_warning_from_the_owners_denied_attributes_file() {
    let (checkout, data, owner) = (scratch(), scratch(), owner_home_with_git_config());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    owner_repository(&root);
    let (built, folder) = built_server(&servers, &project("denied-attributes"), &root);
    crate::ide::git::write_global(&folder, &root, Some(owner.path())).unwrap();
    let run = |args: &[&str]| server_git(&built, &folder, owner.path(), &root, args);
    let denied = owner.path().join(".config").join("git").join("attributes");
    let default = format!("core.attributesFile={}", denied.display());
    let control = run(&["-c", &default, "check-attr", "-a", "a.txt"]);
    assert!(
        String::from_utf8_lossy(&control.stderr).contains("unable to access"),
        "{}",
        String::from_utf8_lossy(&control.stderr)
    );
    let quiet = run(&["check-attr", "-a", "a.txt"]);
    let said = String::from_utf8_lossy(&quiet.stderr).into_owned();
    assert!(quiet.status.success(), "{said}");
    assert!(!said.contains("unable to access"), "{said}");
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
            policy: IdePolicy {
                access: AgentAccess::Sandboxed,
                network: AgentNetwork::Open,
                hooks: false,
                agent_files: Default::default(),
            },
            kept_ports: &[],
        },
        &info,
        writer,
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
            policy: IdePolicy {
                access: AgentAccess::Sandboxed,
                network: AgentNetwork::Open,
                hooks: false,
                agent_files: Default::default(),
            },
            kept_ports: &[],
        },
        &info,
        writer,
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
        None,
    );
    let project = project("built-in");
    let info = servers
        .open(&project, checkout.path(), &cached_build())
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
        .open(&project, checkout.path(), &cached_build())
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
    let open = || servers.open(&held, folder.path(), folder.path());
    let first = servers.hold_for_delete(&held).await;
    let second = servers.hold_for_delete(&held).await;
    drop(first);
    assert_eq!(open().await.unwrap_err().0, "ide_project_deleting");
    drop(second);
    assert_eq!(open().await.unwrap_err().0, "ide_build_invalid");
}

/// A Delete everything counts its hold before its stop waits for the start lock, so an open
/// that arrives while the stop waits is refused.
#[tokio::test]
async fn a_delete_hold_counts_before_its_stop_waits_for_the_start_lock() {
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let id = project("held-open");
    let held = Arc::clone(&servers.slot(&id).unwrap().start)
        .lock_owned()
        .await;
    let mut holding = std::pin::pin!(servers.hold_for_delete(&id));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut holding)
            .await
            .is_err(),
        "the stop must wait for the start lock"
    );
    assert!(
        servers.deleting(&id),
        "an open arriving while the stop waits must be refused"
    );
    drop(held);
    drop(holding.await);
    assert!(!servers.deleting(&id));
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
    let opening = servers.open(&starting, checkout.path(), &build);
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
            .open(&starting, checkout.path(), &build)
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
        .open(&id, checkout.path(), &cached_build())
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

/// Saves a project's access level in the engine database of a test data root, as the owner's
/// Save does. Inputs: the data root, the project and the level.
fn set_level(data: &Path, project: &ProjectId, access: AgentAccess) {
    let db = crate::landing::open(data).unwrap();
    let policy = AgentPolicy {
        access,
        ..AgentPolicy::default()
    };
    crate::sandbox::policy::save(&db, project.as_str(), &policy).unwrap();
}

/// Saves a project's access level and IDE hooks switch in the engine database of a test
/// data root, as the owner's Save does. Inputs: the data root, the project, the level and
/// the switch.
fn set_policy(data: &Path, project: &ProjectId, access: AgentAccess, hooks: bool) {
    let db = crate::landing::open(data).unwrap();
    let policy = AgentPolicy {
        access,
        ide_git_hooks: hooks,
        ..AgentPolicy::default()
    };
    crate::sandbox::policy::save(&db, project.as_str(), &policy).unwrap();
}

/// Saves a project's access level and agent folders choice in the engine database of a test
/// data root, as the owner's Save does. Inputs: the data root, the project, the level and
/// the choice.
fn set_agent_files(data: &Path, project: &ProjectId, access: AgentAccess, choice: IdeAgentFiles) {
    let db = crate::landing::open(data).unwrap();
    let policy = AgentPolicy {
        access,
        ide_agent_files: choice,
        ..AgentPolicy::default()
    };
    crate::sandbox::policy::save(&db, project.as_str(), &policy).unwrap();
}

/// Changes only the agent folders choice of a project's saved policy in a test data root.
/// Inputs: the data root, the project and the choice.
fn change_agent_files(data: &Path, project: &ProjectId, choice: IdeAgentFiles) {
    let db = crate::landing::open(data).unwrap();
    let mut policy = crate::sandbox::policy::load(&db, project.as_str()).unwrap();
    policy.ide_agent_files = choice;
    crate::sandbox::policy::save(&db, project.as_str(), &policy).unwrap();
}

/// Changes only the network choice of a project's saved policy in a test data root. Inputs:
/// the data root, the project and the choice.
fn set_network(data: &Path, project: &ProjectId, network: AgentNetwork) {
    let db = crate::landing::open(data).unwrap();
    let mut policy = crate::sandbox::policy::load(&db, project.as_str()).unwrap();
    policy.network = network;
    crate::sandbox::policy::save(&db, project.as_str(), &policy).unwrap();
}

/// Opens a project's server in a new checkout and data root at a saved level and hooks
/// switch. Inputs: the project's name, the level and the switch. Output: the folders, the
/// servers and the project.
async fn open_at(
    name: &str,
    access: AgentAccess,
    hooks: bool,
) -> (tempfile::TempDir, tempfile::TempDir, Servers, ProjectId) {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let id = project(name);
    set_policy(data.path(), &id, access, hooks);
    servers.open(&id, &root, &cached_build()).await.unwrap();
    assert!(servers.group(&id).is_some());
    (checkout, data, servers, id)
}

/// Runs git as a Full access server starts it: the engine's shim under no profile, with the
/// built server's environment and a test owner's home as `HOME`. Inputs: the built command,
/// the IDE folder, the owner's home, the working folder and git's arguments. Output: git's
/// output.
fn full_access_git(
    built: &Command,
    folder: &Path,
    owner: &Path,
    cwd: &Path,
    args: &[&str],
) -> std::process::Output {
    let shim = settings::machine_folder(&folder.join(SERVER_DATA)).join(crate::ide::git::SHIM);
    let mut git = Command::new(&shim);
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

#[test]
fn a_project_with_no_saved_policy_opens_sandboxed() {
    let data = scratch();
    let policy = ide_policy(data.path(), &project("none")).unwrap();
    assert_eq!(policy.access, AgentAccess::Sandboxed);
}

/// A saved policy that cannot be read stops the open and records no launch. The checkout
/// stands in for the build, since the policy is read before a build is looked at.
#[tokio::test]
async fn a_saved_policy_that_cannot_be_read_never_reaches_a_launch() {
    let (checkout, data) = (scratch(), scratch());
    let bad = project("bad");
    set_level(data.path(), &bad, AgentAccess::Full);
    let db = crate::landing::open(data.path()).unwrap();
    db.execute(
        "UPDATE project_agent_policy SET body='tampered' WHERE project='bad'",
        [],
    )
    .unwrap();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let refused = servers
        .open(&bad, checkout.path(), checkout.path())
        .await
        .unwrap_err();
    assert_eq!(refused.0, "ide_workspace_unavailable");
    assert_eq!(servers.group(&bad), None);
}

/// A port on this machine that the settings rows name as the owner's local model.
const LOCAL_MODEL_PORT: u16 = 47_011;

/// The global settings of an owner with one local model connection on a port of this machine.
/// Input: the port. Output: the file's text.
fn local_model_settings(port: u16) -> String {
    format!(
        r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [{{ id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = {{ kind = "http", url = "http://127.0.0.1:{port}/v1" }}, model = {{ id = "qwen2.5:0.5b", author = "openai-compatible" }}, auth = {{ method = "none" }} }}]
pools = [{{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }}]
"#
    )
}

/// The servers of an engine whose global settings hold `text`. Inputs: the data folder and the
/// text. Output: the register.
fn servers_reading(data: &Path, text: &str) -> Servers {
    let global = data.join("global.toml");
    fs::write(&global, text).unwrap();
    Servers::new(
        data,
        IDLE_LIMIT,
        START_LIMIT,
        Arc::default(),
        data.join("engine").join("owner.sock"),
        Some((global, data.join("project.toml"))),
    )
}

/// The owner's local model ports come from the engine's settings files.
#[test]
fn the_owners_local_model_ports_are_read_from_the_settings_files() {
    let data = scratch();
    let global = data.path().join("global.toml");
    fs::write(&global, local_model_settings(LOCAL_MODEL_PORT)).unwrap();
    let files = (global, data.path().join("project.toml"));
    assert_eq!(
        crate::ide::workspace::kept_ports(Some(&files)).unwrap(),
        vec![LOCAL_MODEL_PORT]
    );
}

/// Settings that cannot be read stop the start, which never goes on with no port kept.
#[tokio::test]
async fn a_settings_file_that_cannot_be_read_stops_the_start() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_reading(data.path(), "not = = toml");
    let id = project("broken-settings");
    let refused = servers
        .open(&id, checkout.path(), checkout.path())
        .await
        .unwrap_err();
    assert_eq!(refused.0, "ide_settings_unavailable");
    assert_eq!(servers.group(&id), None);
}

/// A real open of an Own branch project keeps the owner's local model port in the profile the
/// server runs under.
#[tokio::test]
async fn an_own_branch_server_keeps_the_owners_local_model_port_in_its_profile() {
    let (checkout, data) = (scratch(), scratch());
    let build = cached_build();
    let id = project("kept");
    set_level(data.path(), &id, AgentAccess::OwnBranch);
    let servers = servers_reading(data.path(), &local_model_settings(LOCAL_MODEL_PORT));
    let root = fs::canonicalize(checkout.path()).unwrap();
    servers.open(&id, &root, &build).await.unwrap();
    let folder = servers.project_folder(&id).unwrap();
    let profile = fs::read_to_string(folder.join(sandbox::PROFILE)).unwrap();
    servers.stop_all().await;
    let rule = format!("(local ip \"*:{LOCAL_MODEL_PORT}\")");
    assert!(profile.contains(&rule), "{profile}");
}

#[test]
fn a_saved_network_change_only_matters_at_own_branch() {
    let at = |access, network| IdePolicy {
        access,
        network,
        hooks: false,
        agent_files: Default::default(),
    };
    let own = at(AgentAccess::OwnBranch, AgentNetwork::Open);
    let sandboxed = at(AgentAccess::Sandboxed, AgentNetwork::Open);
    let full = at(AgentAccess::Full, AgentNetwork::Open);
    assert!(own.holds(own));
    assert!(!own.holds(at(AgentAccess::OwnBranch, AgentNetwork::Https)));
    assert!(!own.holds(sandboxed));
    assert!(sandboxed.holds(at(AgentAccess::Sandboxed, AgentNetwork::None)));
    assert!(full.holds(at(AgentAccess::Full, AgentNetwork::Https)));
}

/// DEC-124: a server launched under one agent folders choice no longer holds once the owner
/// saves another below Full access, so the save restarts the project's IDE; at Full access,
/// where no profile runs, the choice never restarts it.
#[test]
fn a_saved_agent_folders_change_only_matters_below_full_access() {
    let at = |access, agent_files| IdePolicy {
        access,
        network: AgentNetwork::Open,
        hooks: false,
        agent_files,
    };
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        for running in IdeAgentFiles::ALL {
            for saved in IdeAgentFiles::ALL {
                assert_eq!(
                    at(access, running).holds(at(access, saved)),
                    running == saved,
                    "{access:?} {running:?} {saved:?}"
                );
            }
        }
    }
    assert!(
        at(AgentAccess::Full, IdeAgentFiles::AllowAll)
            .holds(at(AgentAccess::Full, IdeAgentFiles::ReadOnly))
    );
}

/// DEC-124: the agent folders choice the owner saved reaches the profile each launch
/// installs below Full access, read from the engine database the way an open reads it;
/// Allow all changes adds no rule. A project with a `.crazyagent` folder holds its agents
/// root there, and one without, made before the projects home, also holds `<folder>/worktrees`,
/// where the engine puts its agents, before any agent ran there (DEC-124 r1 M1).
#[test]
fn a_launch_profile_holds_the_saved_agent_folders_choice() {
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let protecting = [IdeAgentFiles::NoDeleteOrRename, IdeAgentFiles::ReadOnly];
    for (layout, older_agents) in [("home", false), ("legacy", true)] {
        let checkout = scratch();
        let root = fs::canonicalize(checkout.path()).unwrap();
        if !older_agents {
            fs::create_dir(root.join(".crazyagent")).unwrap();
        }
        let rule = |choice| {
            AgentFilesRule {
                choice,
                older_agents,
            }
            .seatbelt(&root)
        };
        for (index, choice) in IdeAgentFiles::ALL.into_iter().enumerate() {
            for (level, access) in [("s", AgentAccess::Sandboxed), ("o", AgentAccess::OwnBranch)] {
                let id = project(&format!("files-{layout}-{index}-{level}"));
                set_agent_files(data.path(), &id, access, choice);
                assert_eq!(ide_policy(data.path(), &id).unwrap().agent_files, choice);
                let (_, folder) = built_server(&servers, &id, &root);
                let profile = fs::read_to_string(folder.join(sandbox::PROFILE)).unwrap();
                for held in protecting {
                    assert!(!rule(held).is_empty());
                    assert_eq!(
                        profile.contains(&rule(held)),
                        held == choice,
                        "{layout} {choice:?} {access:?} {held:?}"
                    );
                }
            }
        }
    }
}

/// DEC-124 r1 M1: in a project with no `.crazyagent` folder, an agent folder the engine makes
/// under `<folder>/worktrees` after the IDE opened keeps every file under No delete or rename,
/// while the owner's own files stay removable.
#[test]
fn a_project_without_a_crazyagent_folder_holds_agents_started_after_its_ide_opened() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    assert_eq!(crate::projects_home::state_folder(&root), root);
    let id = project("legacy-later-agent");
    set_agent_files(
        data.path(),
        &id,
        AgentAccess::Sandboxed,
        IdeAgentFiles::NoDeleteOrRename,
    );
    let (_, folder) = built_server(&servers, &id, &root);
    let agent = root.join("worktrees").join("agent-1");
    fs::create_dir_all(&agent).unwrap();
    let file = agent.join("a.txt");
    fs::write(&file, "agent\n").unwrap();
    let own = root.join("own.txt");
    fs::write(&own, "owner\n").unwrap();
    let removes = |path: &Path| {
        let mut rm = sandbox::command(&folder.join(sandbox::PROFILE), Path::new("/bin/rm"));
        rm.arg("-f")
            .arg(path)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::spawn_gate::output(&mut rm).unwrap().status.success()
    };
    assert!(!removes(&file));
    assert_eq!(fs::read_to_string(&file).unwrap(), "agent\n");
    assert!(removes(&own));
    assert!(!own.exists());
}

#[test]
fn a_full_access_launch_runs_the_launcher_with_no_seatbelt() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    set_level(data.path(), &project("full"), AgentAccess::Full);
    let (built, _) = built_server(&servers, &project("full"), &root);
    assert_ne!(built.get_program(), crate::sandbox::SANDBOX_EXEC);
    assert!(built.get_program().to_string_lossy().ends_with(LAUNCHER));
}

/// A launch's profile follows the project's saved level: on the same Open choice a Sandboxed
/// launch cannot reach a listener on this machine and an Own branch launch can.
#[test]
fn a_launch_builds_its_profile_at_the_projects_level() {
    let (checkout, data) = (scratch(), scratch());
    let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let reaches = |level, name: &str| {
        let id = project(name);
        set_level(data.path(), &id, level);
        let (_, folder) = built_server(&servers, &id, &root);
        let mut probe = sandbox::command(&folder.join(sandbox::PROFILE), Path::new("/usr/bin/nc"));
        probe
            .args(["-z", "-G", "5", "127.0.0.1", &port])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::spawn_gate::output(&mut probe)
            .unwrap()
            .status
            .success()
    };
    assert!(!reaches(AgentAccess::Sandboxed, "at-sandboxed"));
    assert!(reaches(AgentAccess::OwnBranch, "at-own-branch"));
}

/// A launch tells the extension the project's saved level, in the words the terminal menu reads.
#[test]
fn a_launch_tells_the_extension_its_access_level() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let told = |level, name: &str| {
        let id = project(name);
        set_level(data.path(), &id, level);
        let (built, _) = built_server(&servers, &id, &root);
        built
            .get_envs()
            .find(|(variable, _)| *variable == env::ACCESS_VARIABLE)
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
    };
    assert_eq!(
        told(AgentAccess::Sandboxed, "told-sandboxed").as_deref(),
        Some("sandboxed")
    );
    assert_eq!(
        told(AgentAccess::OwnBranch, "told-own").as_deref(),
        Some("own_branch")
    );
    assert_eq!(
        told(AgentAccess::Full, "told-full").as_deref(),
        Some("full")
    );
}

/// Full access git is the owner's own: no forced key stops the project's hook or hides the
/// owner's identity, and the shim's count line, which needs those keys, is not written.
#[test]
fn a_full_access_git_runs_the_projects_hook_with_the_owners_identity() {
    let (checkout, data, owner) = (scratch(), scratch(), owner_home_with_git_config());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    owner_repository(&root);
    let hook = root.join(".git").join("hooks").join("pre-commit");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\n: > hook-ran\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    set_level(data.path(), &project("full-git"), AgentAccess::Full);
    let (built, folder) = built_server(&servers, &project("full-git"), &root);
    let added = full_access_git(&built, &folder, owner.path(), &root, &["add", "a.txt"]);
    assert!(added.status.success(), "{added:?}");
    let args = ["commit", "-q", "-m", "owner"];
    let committed = full_access_git(&built, &folder, owner.path(), &root, &args);
    assert!(committed.status.success(), "{committed:?}");
    assert!(root.join("hook-ran").exists());
    let args = ["log", "-1", "--format=%an"];
    let author = full_access_git(&built, &folder, owner.path(), &root, &args);
    assert_eq!(
        String::from_utf8_lossy(&author.stdout).trim(),
        "Dana \"Q\" O\\wner"
    );
}

/// A level change restarts the server at the next open. The row moves to Own branch so no
/// lib test runs a real unconfined server under the test process's own home.
#[tokio::test]
async fn an_open_after_a_level_change_starts_a_new_server() {
    let (checkout, data) = (scratch(), scratch());
    let build = cached_build();
    let moved = project("moved");
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let first = servers.open(&moved, &root, &build).await.unwrap();
    let token = fs::read_to_string(&first.token_path).unwrap();
    let group = recorded_launch(&servers.project_folder(&moved).unwrap());
    set_level(data.path(), &moved, AgentAccess::OwnBranch);
    let second = servers.open(&moved, &root, &build).await.unwrap();
    assert_ne!(token, fs::read_to_string(&second.token_path).unwrap());
    gone(group).await;
    servers.stop_all().await;
}

#[tokio::test]
async fn a_saved_level_change_stops_the_running_server() {
    let (_checkout, data, servers, id) = open_at("level", AgentAccess::Sandboxed, false).await;
    set_level(data.path(), &id, AgentAccess::Full);
    assert!(servers.follow_level(&id).await);
    assert!(servers.group(&id).is_none());
}

/// A level saved while an open is still starting waits for that open under the start lock,
/// so a launch that read the older level is judged once it is up and stopped
/// (v21-ide-level-3 r1 F2).
#[tokio::test]
async fn a_level_saved_while_an_open_is_starting_stops_what_it_started() {
    let checkout = scratch();
    let data = scratch();
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let starting = project("starting");
    set_policy(data.path(), &starting, AgentAccess::Sandboxed, false);
    let recorded = data.path().join("ide").join("starting").join(GROUP);
    let build = cached_build();
    let opening = servers.open(&starting, checkout.path(), &build);
    let following = async {
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
        set_level(data.path(), &starting, AgentAccess::Full);
        (group, servers.follow_level(&starting).await)
    };
    let (opened, (group, stopped)) = tokio::join!(opening, following);
    assert!(opened.is_ok(), "{opened:?}");
    assert!(stopped, "the launch that read the older level kept running");
    gone(group).await;
    assert!(servers.group(&starting).is_none());
    servers.stop_all().await;
}

#[tokio::test]
async fn a_save_that_keeps_the_level_leaves_the_server_running() {
    let (_checkout, data, servers, id) = open_at("same", AgentAccess::Sandboxed, false).await;
    set_level(data.path(), &id, AgentAccess::Sandboxed);
    assert!(!servers.follow_level(&id).await);
    assert!(servers.group(&id).is_some());
    servers.stop_all().await;
}

#[tokio::test]
async fn saving_hooks_off_stops_an_open_sandboxed_ide() {
    let (_checkout, data, servers, id) = open_at("hooks", AgentAccess::Sandboxed, true).await;
    set_policy(data.path(), &id, AgentAccess::Sandboxed, false);
    assert!(servers.follow_level(&id).await);
    assert!(servers.group(&id).is_none());
}

#[tokio::test]
async fn saving_hooks_off_stops_an_open_own_branch_ide() {
    let (_checkout, data, servers, id) = open_at("hooks", AgentAccess::OwnBranch, true).await;
    set_policy(data.path(), &id, AgentAccess::OwnBranch, false);
    assert!(servers.follow_level(&id).await);
    assert!(servers.group(&id).is_none());
}

#[tokio::test]
async fn a_hooks_change_leaves_a_full_access_ide_running() {
    let (_checkout, data, servers, id) = open_at("wide", AgentAccess::Full, true).await;
    set_policy(data.path(), &id, AgentAccess::Full, false);
    assert!(!servers.follow_level(&id).await);
    assert!(servers.group(&id).is_some());
    servers.stop_all().await;
}

/// DEC-124 r1 L2: saving only another agent folders choice stops an open Sandboxed or Own
/// branch IDE on a real server, its process group ends, and the next open installs a
/// profile holding the new rule, which the first one did not.
#[tokio::test]
async fn an_agent_folders_change_restarts_an_open_ide_under_the_new_rule() {
    for access in [AgentAccess::Sandboxed, AgentAccess::OwnBranch] {
        let (checkout, data, servers, id) = open_at("files", access, false).await;
        let root = fs::canonicalize(checkout.path()).unwrap();
        let folder = servers.project_folder(&id).unwrap();
        let group = recorded_launch(&folder);
        let rule = crate::ide::workspace::agent_files_rule(
            data.path(),
            &id,
            &root,
            access,
            IdeAgentFiles::ReadOnly,
        )
        .unwrap()
        .seatbelt(&root);
        assert!(!rule.is_empty());
        let before = fs::read_to_string(folder.join(sandbox::PROFILE)).unwrap();
        assert!(!before.contains(&rule), "{access:?}");
        change_agent_files(data.path(), &id, IdeAgentFiles::ReadOnly);
        assert!(servers.follow_level(&id).await, "{access:?}");
        assert!(servers.group(&id).is_none(), "{access:?}");
        gone(group).await;
        servers.open(&id, &root, &cached_build()).await.unwrap();
        assert_ne!(recorded_launch(&folder), group, "{access:?}");
        let after = fs::read_to_string(folder.join(sandbox::PROFILE)).unwrap();
        assert!(after.contains(&rule), "{access:?}");
        servers.stop_all().await;
    }
}

#[tokio::test]
async fn a_network_change_stops_an_own_branch_ide() {
    let (_checkout, data, servers, id) = open_at("net", AgentAccess::OwnBranch, false).await;
    set_network(data.path(), &id, AgentNetwork::None);
    assert!(servers.follow_level(&id).await);
    assert!(servers.group(&id).is_none());
}

#[tokio::test]
async fn a_network_change_leaves_a_sandboxed_ide_running() {
    let (_checkout, data, servers, id) = open_at("net", AgentAccess::Sandboxed, false).await;
    set_network(data.path(), &id, AgentNetwork::None);
    assert!(!servers.follow_level(&id).await);
    assert!(servers.group(&id).is_some());
    servers.stop_all().await;
}

#[tokio::test]
async fn a_network_change_leaves_a_full_access_ide_running() {
    let (_checkout, data, servers, id) = open_at("net", AgentAccess::Full, false).await;
    set_network(data.path(), &id, AgentNetwork::None);
    assert!(!servers.follow_level(&id).await);
    assert!(servers.group(&id).is_some());
    servers.stop_all().await;
}

/// A saved policy that can no longer be read says nothing about what the server may do, so
/// the server stops rather than keep the level it started under.
#[tokio::test]
async fn a_saved_policy_that_cannot_be_read_stops_the_running_server() {
    let (_checkout, data, servers, id) = open_at("bad", AgentAccess::Sandboxed, false).await;
    crate::landing::open(data.path())
        .unwrap()
        .execute(
            "UPDATE project_agent_policy SET body='tampered' WHERE project=?1",
            [id.as_str()],
        )
        .unwrap();
    assert!(servers.follow_level(&id).await);
    assert!(servers.group(&id).is_none());
}

/// Workspace trust stays off below Full access, where the sandbox limits the project's code,
/// and is on at Full access, where nothing else does.
#[test]
fn only_a_full_access_launch_has_workspace_trust_on() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    let (sandboxed, _) = built_server(&servers, &project("kept"), &root);
    set_level(data.path(), &project("wide"), AgentAccess::Full);
    let (full, _) = built_server(&servers, &project("wide"), &root);
    assert!(
        sandboxed
            .get_args()
            .any(|arg| arg == "--disable-workspace-trust")
    );
    assert!(
        !full
            .get_args()
            .any(|arg| arg == "--disable-workspace-trust")
    );
}

/// Only a Full access server gets a lineage token, the one its command carries; a confined
/// server's processes stay in its sandbox.
#[test]
fn only_a_full_access_server_carries_a_lineage_token() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let root = fs::canonicalize(checkout.path()).unwrap();
    set_level(data.path(), &project("wide"), AgentAccess::Full);
    for (name, access) in [
        ("kept", AgentAccess::Sandboxed),
        ("wide", AgentAccess::Full),
    ] {
        let (mut built, _) = built_server(&servers, &project(name), &root);
        let token = lineage_env(&mut built, access).unwrap();
        assert_eq!(token.is_some(), access == AgentAccess::Full, "{name}");
        let carried = built
            .get_envs()
            .find(|(key, _)| *key == LINEAGE_VARIABLE)
            .and_then(|(_, value)| value)
            .map(std::ffi::OsStr::to_owned);
        assert_eq!(
            carried,
            token.map(|token| OsString::from(token.as_str())),
            "{name}"
        );
    }
}

/// A Full access launch whose lineage could not be read, as a first follow whose listing
/// failed keeps it, fails its stop's read and leaves UNLISTED for the next open, never
/// none; a clean end leaves none (5b r1 rv-secreview L1).
#[test]
fn a_stop_whose_lineage_cannot_be_read_leaves_unlisted() {
    let followed = Followed {
        leader: std::process::id(),
        token: LineageToken::new().unwrap(),
        lineage: Mutex::new(Some(Err(Unstopped::Uncertain))),
    };
    let settled = followed.before_stop().and_then(Lineage::settle);
    assert!(matches!(settled, Err(Unstopped::Uncertain)));
    assert_eq!(left_names(settled).as_deref(), Some(leftovers::UNLISTED));
    assert_eq!(left_names(Ok(())), None);
}

/// A process that carries a Full access launch's token but left its group, parent and
/// session is named after the stop, for the next open only.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_full_access_stop_names_a_process_that_left_its_group() {
    let (checkout, data) = (scratch(), scratch());
    let servers = servers_in(data.path(), IDLE_LIMIT);
    let wide = project("left");
    set_level(data.path(), &wide, AgentAccess::Full);
    servers
        .open(&wide, checkout.path(), &cached_build())
        .await
        .unwrap();
    let token = servers
        .lineage_token(&wide)
        .expect("a Full access launch has a lineage token");
    let (_, sleeper) = crate::harnesses::leftovers::tests::detached(
        data.path(),
        "left-sleeper",
        &token,
        ("CRAZYAGENT_TEST_IDE", "left"),
    );
    assert!(servers.stop(&wide).await);
    let left = servers.left(&wide);
    let _ = rustix::process::kill_process(Pid::from_raw(sleeper).unwrap(), Signal::KILL);
    let left = left.expect("a process carrying the launch's token outlived the stop unnamed");
    assert!(
        left.split(", ")
            .any(|named| named.starts_with(&format!("{sleeper} "))),
        "{left}"
    );
    assert_eq!(servers.left(&wide), None, "the names are handed out once");
}
