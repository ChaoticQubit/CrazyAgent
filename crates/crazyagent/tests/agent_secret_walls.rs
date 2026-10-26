#![cfg(target_os = "macos")]

use crazyagent::credentials::{Keychain, SecretString, Store, reference};
use crazyagent::model_gateway::relay::{LOCAL_PLACEHOLDER, TOKEN_PREFIX};
use crazyagent::sandbox::{
    AGENT_GIT_ENV, ProjectSandbox, SECRET_ROOT_REFUSED, SandboxError, SandboxNetwork,
    agent_command, agent_env_refused, agent_variable_allowed,
};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Case {
    _temp: tempfile::TempDir,
    runtime: PathBuf,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    profiles: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let runtime = root.join("runtime");
    let project = root.join("project");
    let home = runtime.join("team").join("agents").join("a").join("home");
    let tmp = runtime.join("team").join("agents").join("a").join("tmp");
    let profiles = root.join("profiles");
    for directory in [&project, &home, &tmp, &profiles] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::create_dir_all(runtime.join("account-keys")).unwrap();
    Case {
        _temp: temp,
        runtime,
        project,
        home,
        tmp,
        profiles,
    }
}

fn sandbox(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
}

fn run(sandbox: &ProjectSandbox, profile: &Path, program: &str, args: &[&str]) -> Output {
    sandbox.install(profile).unwrap();
    let mut command = sandbox.command(profile, Path::new(program));
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crazyagent::spawn_gate::output(&mut command).unwrap()
}

fn shell(sandbox: &ProjectSandbox, profile: &Path, script: &str) -> Output {
    run(sandbox, profile, "/bin/sh", &["-c", script])
}

fn quoted(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

fn owner_home() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    home.canonicalize().unwrap_or(home)
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer).unwrap();
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn default_keychain() -> PathBuf {
    let output = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/security")
            .args(["default-keychain", "-d", "user"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    PathBuf::from(text.trim().trim_matches('"'))
}

fn walled(result: Result<ProjectSandbox, SandboxError>) {
    match result {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, SECRET_ROOT_REFUSED),
        other => panic!("expected the secret wall refusal, got {other:?}"),
    }
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

struct ThrowawayItem {
    store: Keychain,
    service: String,
}

impl Drop for ThrowawayItem {
    fn drop(&mut self) {
        let item = reference(&self.service, "token").unwrap();
        let _ = self.store.delete(&item);
    }
}

#[test]
fn agent_profile_denies_the_keychain_daemons_and_the_keychain_files() {
    let case = case();
    let profile = sandbox(&case).profile();
    let (allowed, denied) = profile.split_once("(deny mach-lookup").unwrap();
    assert!(!allowed.contains("com.apple.SecurityServer"));
    assert!(!allowed.contains("com.apple.securityd"));
    for service in [
        "com.apple.SecurityServer",
        "com.apple.securityd",
        "com.apple.secd",
        "com.apple.security.agent",
    ] {
        assert!(
            denied.contains(&format!("(global-name \"{service}\")")),
            "{service}"
        );
    }
    let home = owner_home();
    for secret in [
        home.join("Library/Keychains"),
        home.join(".ssh"),
        home.join(".config/gh"),
        home.join(".git-credentials"),
        PathBuf::from("/Library/Keychains"),
    ] {
        assert!(
            profile.contains(&format!("(subpath \"{}\")", secret.display())),
            "{}",
            secret.display()
        );
    }
}

#[test]
fn sandboxed_agent_cannot_read_an_engine_keychain_item_through_security() {
    let namespace = format!("dev.crazyagent.test-{}", random_hex(6));
    let item = ThrowawayItem {
        store: Keychain::new(&namespace).unwrap(),
        service: format!("{namespace}.github"),
    };
    let secret_text = format!("gho_test{}", random_hex(18));
    let stored = item
        .store
        .put(
            &item.service,
            "token",
            &SecretString::new(secret_text.as_bytes().to_vec()).unwrap(),
        )
        .unwrap();
    assert_eq!(item.store.get(&stored).unwrap().expose(), secret_text);
    let case = case();
    let sandbox = sandbox(&case);
    let profile = case.profiles.join("keychain.sb");
    let keychain = default_keychain();
    let keychain_text = keychain.to_str().unwrap();
    for args in [
        vec![
            "find-generic-password",
            "-s",
            item.service.as_str(),
            "-a",
            "token",
            "-w",
            keychain_text,
        ],
        vec![
            "find-generic-password",
            "-s",
            item.service.as_str(),
            "-a",
            "token",
            "-w",
        ],
    ] {
        let output = run(&sandbox, &profile, "/usr/bin/security", &args);
        assert!(!output.status.success(), "{args:?}");
        assert!(!holds(&output.stdout, &secret_text), "{args:?}");
        assert!(!holds(&output.stderr, &secret_text), "{args:?}");
    }
    let output = shell(
        &sandbox,
        &profile,
        &format!(
            "cat {} > /dev/null 2>&1 && exit 10; ls {} > /dev/null 2>&1 && exit 11; exit 0",
            quoted(&keychain),
            quoted(keychain.parent().unwrap())
        ),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(item.store.get(&stored).unwrap().expose(), secret_text);
}

#[test]
fn guarded_engine_runtime_secrets_stay_unreadable_and_no_read_root_may_hold_them() {
    let case = case();
    let secret_text = format!("gho_test{}", random_hex(18));
    for name in ["github.json", "github.toml", "engine.db", "engine.db-wal"] {
        fs::write(case.runtime.join(name), &secret_text).unwrap();
    }
    fs::write(
        case.runtime.join("account-keys").join("key-a"),
        &secret_text,
    )
    .unwrap();
    walled(
        sandbox(&case)
            .allow_read(&case.runtime)
            .unwrap()
            .guard_engine(&case.runtime),
    );
    walled(
        sandbox(&case)
            .guard_engine(&case.runtime)
            .unwrap()
            .allow_read(&case.runtime),
    );
    let sandbox = sandbox(&case).guard_engine(&case.runtime).unwrap();
    let profile = case.profiles.join("runtime.sb");
    for secret in [
        case.runtime.join("github.json"),
        case.runtime.join("github.toml"),
        case.runtime.join("engine.db"),
        case.runtime.join("engine.db-wal"),
        case.runtime.join("account-keys").join("key-a"),
    ] {
        let output = shell(&sandbox, &profile, &format!("cat {}", quoted(&secret)));
        assert!(!output.status.success(), "{}", secret.display());
        assert!(!holds(&output.stdout, &secret_text), "{}", secret.display());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
            "{}",
            secret.display()
        );
    }
    let write = shell(
        &sandbox,
        &profile,
        &format!(
            "echo x > {}",
            quoted(&case.runtime.join("account-keys").join("planted"))
        ),
    );
    assert!(!write.status.success());
    assert!(!case.runtime.join("account-keys").join("planted").exists());
    let home_write = shell(
        &sandbox,
        &profile,
        &format!("echo ok > {}", quoted(&case.home.join("own.txt"))),
    );
    assert!(home_write.status.success());
}

#[test]
fn guard_refuses_agent_roots_inside_engine_secrets() {
    let case = case();
    let inside = case.runtime.join("account-keys").join("home");
    fs::create_dir_all(&inside).unwrap();
    let sandbox = ProjectSandbox::new(
        &case.project,
        &inside,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    assert!(sandbox.guard_engine(&case.runtime).is_err());
}

#[test]
fn owner_library_is_refused_as_a_read_root_and_its_keychains_stay_hidden() {
    let home = owner_home();
    let library = home.join("Library");
    let case = case();
    walled(sandbox(&case).allow_read(&library));
    let sandbox = sandbox(&case)
        .allow_read(&library.join("Preferences"))
        .unwrap();
    let profile = case.profiles.join("library.sb");
    let output = shell(
        &sandbox,
        &profile,
        &format!(
            "ls -d {} > /dev/null || exit 9; ls {} > /dev/null 2>&1 && exit 10; exit 0",
            quoted(&library.join("Preferences")),
            quoted(&library.join("Keychains"))
        ),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn shared_builder_refuses_owner_secrets_and_keeps_the_agent_model_key() {
    for name in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_CONFIG_DIR",
        "GH_ENTERPRISE_TOKEN",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
        "SSH_AUTH_SOCK",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_AUTH_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "CRAZYAGENT_OWNER_TOKEN",
        "crazyagent_debug_custodian",
        "npm_token",
        "ANTHROPIC_API_KEY",
        "CRAZYAGENT_API_KEY",
    ] {
        assert!(agent_env_refused(OsStr::new(name)), "{name}");
    }
    let token = format!("{TOKEN_PREFIX}{}", "c".repeat(64));
    for name in ["ANTHROPIC_API_KEY", "CRAZYAGENT_API_KEY"] {
        let name = OsStr::new(name);
        assert!(agent_variable_allowed(name, OsStr::new(LOCAL_PLACEHOLDER)));
        assert!(agent_variable_allowed(name, OsStr::new(&token)));
        assert!(!agent_variable_allowed(
            name,
            OsStr::new("sk-ant-api03-planted")
        ));
        assert!(!agent_variable_allowed(name, OsStr::new("")));
    }
    for (name, value) in AGENT_GIT_ENV {
        assert!(agent_variable_allowed(OsStr::new(name), OsStr::new(value)));
    }
    assert!(!agent_variable_allowed(
        OsStr::new("GIT_CONFIG_VALUE_0"),
        OsStr::new("osxkeychain")
    ));
    assert!(!agent_variable_allowed(
        OsStr::new("GH_TOKEN"),
        OsStr::new(LOCAL_PLACEHOLDER)
    ));
    for name in [
        "ANTHROPIC_BASE_URL",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
        "TERM",
        "SHELL",
        "PATH",
        "HOME",
    ] {
        assert!(!agent_env_refused(OsStr::new(name)), "{name}");
    }
    let case = case();
    let profile = case.profiles.join("env.sb");
    let sandbox = sandbox(&case);
    sandbox.install(&profile).unwrap();
    for name in [
        "GH_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CRAZYAGENT_API_KEY",
        "SSH_AUTH_SOCK",
        "GIT_CONFIG_COUNT",
    ] {
        let mut harness = Command::new("/usr/bin/env");
        harness
            .env(name, "planted-value")
            .current_dir(&case.project);
        assert!(sandbox.wrap(&profile, &harness).is_err(), "{name}");
        let refused = agent_command(
            &profile,
            Path::new("/usr/bin/env"),
            &[("HOME", case.home.as_os_str())],
            [(OsStr::new(name), Some(OsStr::new("planted-value")))],
        );
        assert!(refused.is_err(), "{name}");
    }
}

#[test]
fn sandboxed_agent_env_holds_only_the_builder_variables() {
    let case = case();
    let profile = case.profiles.join("env-run.sb");
    let sandbox = sandbox(&case);
    sandbox.install(&profile).unwrap();
    let mut harness = Command::new("/usr/bin/env");
    harness
        .env("ANTHROPIC_API_KEY", "crazyagent-local")
        .env("TERM", "dumb")
        .current_dir(&case.project);
    let output = crazyagent::spawn_gate::output(
        sandbox
            .wrap(&profile, &harness)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen: BTreeMap<String, String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| *name != "__CF_USER_TEXT_ENCODING")
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    let expected: BTreeMap<String, String> = [
        ("HOME", case.home.to_str().unwrap()),
        ("TMPDIR", case.tmp.to_str().unwrap()),
        (
            "PATH",
            "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
        ),
        ("LANG", "C"),
        ("LC_ALL", "C"),
        ("ANTHROPIC_API_KEY", "crazyagent-local"),
        ("TERM", "dumb"),
    ]
    .into_iter()
    .chain(AGENT_GIT_ENV)
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    assert_eq!(seen, expected);
    assert!(
        seen.iter()
            .all(|(name, value)| agent_variable_allowed(OsStr::new(name), OsStr::new(value)))
    );
}

#[test]
fn sandboxed_agent_git_has_no_credential_helper_and_gets_no_password() {
    let case = case();
    let profile = case.profiles.join("git.sb");
    let sandbox = sandbox(&case);
    let helpers = run(
        &sandbox,
        &profile,
        "/usr/bin/git",
        &["config", "--get-all", "credential.helper"],
    );
    assert_eq!(String::from_utf8_lossy(&helpers.stdout).trim(), "");
    sandbox.install(&profile).unwrap();
    let mut fill = sandbox.command(&profile, Path::new("/usr/bin/git"));
    let mut child = crazyagent::spawn_gate::spawn(
        fill.args(["credential", "fill"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=github.com\n\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(!holds(&output.stdout, "password="));
}

#[test]
fn tools_on_agent_reaches_ip_endpoints_but_no_engine_unix_socket() {
    let case = case();
    let generation = case.runtime.join("o1a2b3c4d");
    let runner = case.runtime.join("r00");
    let terminal = case.runtime.join("terminal");
    for folder in [&generation, &runner, &terminal] {
        fs::create_dir_all(folder).unwrap();
    }
    let sockets = [
        terminal.join("tmux.sock"),
        case.runtime.join("ordinary-engine.sock"),
        generation.join("owner.sock"),
        runner.join("control.sock"),
    ];
    let _listeners: Vec<_> = sockets
        .iter()
        .map(|path| crazyagent::spawn_gate::bind_unix_std(path).unwrap())
        .collect();
    let endpoint = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let free = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let served = free.local_addr().unwrap().port();
    drop(free);
    let open = ProjectSandbox::new(&case.project, &case.home, &case.tmp, SandboxNetwork::Open)
        .unwrap()
        .guard_engine(&case.runtime)
        .unwrap();
    let guarded = open.secret_paths();
    for place in [&terminal, &sockets[1], &generation, &runner] {
        assert!(
            guarded.contains(place),
            "{} is not guarded",
            place.display()
        );
    }
    walled(
        sandbox(&case)
            .allow_read(&runner)
            .unwrap()
            .guard_engine(&case.runtime),
    );
    let closed = sandbox(&case).guard_engine(&case.runtime).unwrap();
    for (sandbox, name) in [(&open, "open.sb"), (&closed, "closed.sb")] {
        let profile = case.profiles.join(name);
        for socket in &sockets {
            let output = shell(
                sandbox,
                &profile,
                &format!("/usr/bin/nc -U -w 2 {} </dev/null", quoted(socket)),
            );
            assert!(
                !output.status.success(),
                "{name}: the agent connected to {}",
                socket.display()
            );
        }
        let listing = shell(sandbox, &profile, &format!("ls {}", quoted(&terminal)));
        assert!(
            !listing.status.success(),
            "{name}: the agent listed the terminal folder"
        );
    }
    let profile = case.profiles.join("open-ip.sb");
    let reach = shell(
        &open,
        &profile,
        &format!("/usr/bin/nc -z -w 2 127.0.0.1 {port}"),
    );
    assert!(
        reach.status.success(),
        "{}",
        String::from_utf8_lossy(&reach.stderr)
    );
    let serve = shell(
        &open,
        &profile,
        &format!(
            "/usr/bin/nc -l 127.0.0.1 {served} >/dev/null & sleep 1; /usr/bin/nc -z -w 2 127.0.0.1 {served}; s=$?; kill $! 2>/dev/null; exit $s"
        ),
    );
    assert!(
        serve.status.success(),
        "{}",
        String::from_utf8_lossy(&serve.stderr)
    );
    assert!(!open.profile().contains("network*"));
}

#[test]
fn agent_opens_only_its_own_terminal_and_ptys_it_creates() {
    let case = case();
    let sandbox = sandbox(&case);
    let profile = case.profiles.join("tty.sb");
    sandbox.install(&profile).unwrap();
    let named = case.profiles.join("other-tty");
    let mut holder = crazyagent::spawn_gate::spawn(
        Command::new("/usr/bin/script")
            .args(["-q", "/dev/null", "/bin/sh", "-c"])
            .arg("/usr/bin/tty > \"$0\"; exec /bin/sleep 30")
            .arg(&named)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let other = loop {
        if let Ok(text) = fs::read_to_string(&named)
            && text.ends_with('\n')
        {
            break text.trim().to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the holder pty never named itself"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(other.starts_with("/dev/ttys"), "{other}");
    let other = quoted(Path::new(&other));
    let probe = format!(
        "own=$(/usr/bin/tty) || own=none; echo \"own=$own\"\n\
         /bin/stty -g >/dev/null 2>&1 && echo raw=ok || echo raw=denied\n\
         ( exec 3<>/dev/tty ) 2>/dev/null && echo devtty=ok || echo devtty=denied\n\
         ( exec 3<{other} ) 2>/dev/null && echo read_other=ok || echo read_other=denied\n\
         ( exec 3>{other} ) 2>/dev/null && echo write_other=ok || echo write_other=denied\n\
         n=$(/usr/bin/script -q /dev/null /usr/bin/tty </dev/null 2>&1)\n\
         case $n in *\"/dev/ttys\"*) echo nested=ok ;; *) echo \"nested=failed $n\" ;; esac\n\
         ( exec 3</dev/console ) 2>/dev/null && echo console=ok || echo console=denied\n\
         ( exec 3</dev/auditsessions ) 2>/dev/null && echo device=ok || echo device=denied\n\
         /bin/ls /dev >/dev/null 2>&1 && echo listing=ok || echo listing=denied\n\
         /usr/bin/head -c 4 /dev/urandom >/dev/null 2>&1 && echo random=ok || echo random=denied\n\
         ( echo x > /dev/stdout ) >/dev/null 2>&1 && echo stdout=ok || echo stdout=denied\n"
    );
    let control = crazyagent::spawn_gate::status(
        Command::new("/bin/sh")
            .args(["-c", "exec 3</dev/auditsessions"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(
        control.success(),
        "this machine must let its user open /dev/auditsessions"
    );
    let inner = sandbox.command(&profile, Path::new("/bin/sh"));
    let mut command = Command::new("/usr/bin/script");
    command
        .args(["-q", "/dev/null", "/usr/bin/sandbox-exec", "-f"])
        .arg(&profile)
        .args(["/bin/sh", "-c"])
        .arg(&probe)
        .env_clear()
        .envs(
            inner
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
        .current_dir(&case.project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut command).unwrap();
    let _ = holder.kill();
    let _ = holder.wait();
    let text = String::from_utf8_lossy(&output.stdout).replace('\r', "");
    for expected in [
        "own=/dev/ttys",
        "raw=ok",
        "devtty=ok",
        "nested=ok",
        "read_other=denied",
        "write_other=denied",
        "console=denied",
        "device=denied",
        "listing=ok",
        "random=ok",
        "stdout=ok",
    ] {
        assert!(text.contains(expected), "missing {expected} in {text}");
    }
}

const ENGINE_TERMINAL: &str = "CRAZYAGENT_TEST_ENGINE_TERMINAL";

fn engine_with_a_terminal() {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .expect("the engine role runs with a controlling terminal");
    println!("engine=open");
    let case = case();
    let sandbox = sandbox(&case);
    let profile = case.profiles.join("session.sb");
    let output = shell(
        &sandbox,
        &profile,
        "msg=$( ( exec 3</dev/tty ) 2>&1 ) && msg=opened; echo \"agent=$msg\"\n\
         /usr/bin/perl -e 'print getpgrp(0) == $ARGV[0] ? \"group=own\\n\" : \"group=other\\n\"' \"$$\"\n",
    );
    print!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_headless_agent_cannot_open_the_engine_terminal() {
    if std::env::var_os(ENGINE_TERMINAL).is_some() {
        engine_with_a_terminal();
        return;
    }
    let output = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/script")
            .args(["-q", "/dev/null"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "a_headless_agent_cannot_open_the_engine_terminal",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ENGINE_TERMINAL, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).replace('\r', "");
    for expected in [
        "engine=open",
        "agent=/bin/sh: /dev/tty: Device not configured",
        "group=own",
        "test result: ok. 1 passed",
    ] {
        assert!(text.contains(expected), "missing {expected} in {text}");
    }
}

struct TmuxServer {
    tmux: PathBuf,
    socket: PathBuf,
}

impl TmuxServer {
    fn tmux(&self, arguments: &[&str]) -> Output {
        crazyagent::spawn_gate::output(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .args(arguments)
                .env_remove("TMUX")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap()
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

fn appears(path: &Path) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[test]
fn a_tools_on_agent_cannot_drive_the_engine_tmux_server() {
    let case = case();
    let tmux = ["/opt/homebrew/bin/tmux", "/usr/local/bin/tmux"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .expect("macOS test machines need tmux, as the engine does");
    let terminal = case.runtime.join("terminal");
    fs::create_dir_all(&terminal).unwrap();
    let server = TmuxServer {
        tmux,
        socket: terminal.join("tmux.sock"),
    };
    let started = server.tmux(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "crazyagent",
        "/bin/sleep 120",
    ]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let outside = case.profiles.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let control = outside.join("control");
    let window = format!("/usr/bin/touch {}", quoted(&control));
    assert!(server.tmux(&["new-window", "-d", &window]).status.success());
    assert!(
        appears(&control),
        "the unsandboxed control must run a command through the server"
    );
    let open = ProjectSandbox::new(&case.project, &case.home, &case.tmp, SandboxNetwork::Open)
        .unwrap()
        .guard_engine(&case.runtime)
        .unwrap();
    let escaped = outside.join("escaped");
    let output = shell(
        &open,
        &case.profiles.join("open.sb"),
        &format!(
            "cd \"$TMPDIR\" && {} -S ../../../../terminal/tmux.sock new-window -d \"/usr/bin/touch {}\"",
            quoted(&server.tmux),
            quoted(&escaped)
        ),
    );
    let after = outside.join("after");
    let window = format!("/usr/bin/touch {}", quoted(&after));
    assert!(server.tmux(&["new-window", "-d", &window]).status.success());
    assert!(appears(&after));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("Operation not permitted"), "{stderr}");
    assert!(
        !escaped.exists(),
        "the agent ran a command outside its sandbox"
    );
}
