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
    command.output().unwrap()
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
    let output = Command::new("/usr/bin/security")
        .args(["default-keychain", "-d", "user"])
        .output()
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
    let output = sandbox
        .wrap(&profile, &harness)
        .unwrap()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
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
    let mut child = fill
        .args(["credential", "fill"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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
