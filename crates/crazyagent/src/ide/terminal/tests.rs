use super::*;
use crate::sandbox::ide_shell::{self, COMMAND, UNAVAILABLE};
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;

const PROJECT: &str = "alpha";

fn arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

/// A real engine data root and project folder in a private temp folder, as the engine
/// lays them out: the pool's saved account keys, one agent's home with its relay key,
/// and the project's IDE folder holding its capability and server token.
#[cfg(target_os = "macos")]
struct Case {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    folder: PathBuf,
}

#[cfg(target_os = "macos")]
impl Case {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::Builder::new()
            .prefix("ca-ide-shell-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("engine");
        let folder = base.join("project");
        for path in [
            root.join("account-keys"),
            root.join("ide").join(PROJECT),
            root.join("team").join("agents").join("a").join("home"),
            folder.clone(),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::set_permissions(
            root.join("team").join("agents").join("a").join("home"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(folder.join("README"), "readable project file\n").unwrap();
        Self {
            _tmp: tmp,
            root,
            folder,
        }
    }
}

#[cfg(target_os = "macos")]
fn quoted(path: &std::path::Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

#[cfg(target_os = "macos")]
#[test]
fn sandboxed_ide_shell_cannot_read_pool_credentials_or_the_ide_token() {
    use crate::harnesses::launch::Secret;
    use crate::model_gateway::relay::TOKEN_PREFIX;
    let case = Case::new();
    let pool_text = "pool-account-key-7c41d9e0b2";
    let capability_text = "a1".repeat(32);
    let token_text = "b2".repeat(32);
    let relay_text = format!("{TOKEN_PREFIX}{}", "e".repeat(64));
    let pool = case.root.join("account-keys").join("pool-a");
    let ide = case.root.join("ide").join(PROJECT);
    let capability = ide.join("capability");
    let token = ide.join("token");
    std::fs::write(&pool, pool_text).unwrap();
    std::fs::write(&capability, &capability_text).unwrap();
    std::fs::write(&token, &token_text).unwrap();
    let agent_key = crate::harnesses::interactive::write_key(
        &case.root.join("team").join("agents").join("a").join("home"),
        &Secret::new(relay_text.clone()).unwrap(),
    )
    .unwrap();
    let planted = ide.join("planted");
    let own = case.folder.join("own.txt");
    let mut script = String::new();
    for (name, path) in [
        ("pool", &pool),
        ("capability", &capability),
        ("token", &token),
        ("agent-key", &agent_key),
    ] {
        script.push_str(&format!(
            "if cat {}; then echo READ-{name}; else echo DENIED-{name}; fi\n",
            quoted(path)
        ));
    }
    script.push_str(&format!(
        "echo planted > {} && echo PLANTED || echo UNPLANTED\n",
        quoted(&planted)
    ));
    script.push_str("cat README\n/usr/bin/env\n");
    script.push_str(&format!("echo mine > {} && echo WROTE\n", quoted(&own)));
    let project = ProjectId::new(PROJECT).unwrap();
    let shell_arguments = arguments(&["-c", &script]);
    let incoming = vec![
        (OsString::from("TERM"), OsString::from("xterm-256color")),
        (
            OsString::from(crate::ide::bridge::CAPABILITY_ENV),
            capability.clone().into_os_string(),
        ),
        (
            OsString::from(crate::ide::bridge::SOCKET_ENV),
            OsString::from("/tmp/owner.sock"),
        ),
        (
            OsString::from("GH_TOKEN"),
            OsString::from("gho_from_owner_env"),
        ),
    ];
    let launch = ide_shell::Launch {
        root: &case.root,
        project: &project,
        folder: &case.folder,
        cwd: &case.folder,
        shell: std::path::Path::new("/bin/sh"),
        arguments: &shell_arguments,
        incoming: &incoming,
        extra_reads: &[],
    };
    let mut command = ide_shell::command(&launch).unwrap();
    assert_eq!(command.get_program(), crate::sandbox::SANDBOX_EXEC);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = crate::spawn_gate::output(&mut command).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for name in ["pool", "capability", "token", "agent-key"] {
        assert!(
            stdout.contains(&format!("DENIED-{name}\n")),
            "{name}: {stdout}"
        );
        assert!(
            !stdout.contains(&format!("READ-{name}")),
            "{name}: {stdout}"
        );
    }
    for secret in [
        pool_text,
        capability_text.as_str(),
        token_text.as_str(),
        relay_text.as_str(),
        "gho_from_owner_env",
    ] {
        assert!(!stdout.contains(secret) && !stderr.contains(secret));
    }
    assert!(stderr.contains("Operation not permitted"), "{stderr}");
    assert!(
        stdout.contains("UNPLANTED\n") && !planted.exists(),
        "{stdout}"
    );
    assert!(stdout.contains("readable project file\n"), "{stdout}");
    assert!(stdout.contains("WROTE\n"), "{stdout}");
    assert_eq!(std::fs::read_to_string(&own).unwrap(), "mine\n");
    let home = case.root.join("ide-shell").join(PROJECT).join("home");
    let env: Vec<&str> = stdout.lines().filter(|line| line.contains('=')).collect();
    assert!(
        env.contains(&format!("HOME={}", home.display()).as_str()),
        "{stdout}"
    );
    assert!(env.contains(&"TERM=xterm-256color"), "{stdout}");
    assert!(
        env.iter()
            .all(|line| !line.starts_with("CRAZYAGENT_") && !line.starts_with("GH_")),
        "{stdout}"
    );
    assert!(!stdout.contains(capability.to_str().unwrap()), "{stdout}");
}

#[cfg(target_os = "linux")]
#[test]
fn ide_shell_reports_unavailable_where_the_sandbox_is_not_qualified() {
    let error = ide_shell::run(&arguments(&[COMMAND, "--project", PROJECT])).unwrap_err();
    assert_eq!(error, UNAVAILABLE);
    assert!(error.contains(YOUR_SHELL), "{error}");
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let shell_arguments = arguments(&["-c", "echo started"]);
    let launch = ide_shell::Launch {
        root: &base,
        project: &project,
        folder: &base,
        cwd: &base,
        shell: std::path::Path::new("/bin/sh"),
        arguments: &shell_arguments,
        incoming: &[],
        extra_reads: &[],
    };
    match ide_shell::command(&launch) {
        Err(crate::sandbox::SandboxError::Unavailable(reason)) => assert_eq!(reason, UNAVAILABLE),
        other => panic!("expected the unavailable refusal, got {other:?}"),
    }
    assert!(!base.join("ide-shell").exists());
}

#[test]
fn terminal_profiles_label_your_shell_and_default_to_sandboxed() {
    let project = ProjectId::new(PROJECT).unwrap();
    let executable = PathBuf::from("/Applications/CrazyAgent.app/Contents/MacOS/crazyagent");
    let owner_shell = PathBuf::from("/bin/zsh");
    let launch = ShellLaunch {
        executable: &executable,
        project: &project,
        owner_shell: &owner_shell,
    };
    let sandboxed = json!({
        "path": executable.to_str().unwrap(),
        "args": [COMMAND, "--project", PROJECT],
        "icon": "shield",
        "overrideName": true,
    });
    for (platform, os, your_shell) in [
        (
            Platform::MacOs,
            "osx",
            json!({"path": "/bin/zsh", "args": ["-l"], "icon": "terminal", "overrideName": true}),
        ),
        (
            Platform::Linux,
            "linux",
            json!({"path": "/bin/zsh", "args": ["-l"], "icon": "terminal", "overrideName": true}),
        ),
        (
            Platform::Windows,
            "windows",
            json!({"source": "PowerShell", "icon": "terminal", "overrideName": true}),
        ),
    ] {
        let settings = profiles(platform, &launch);
        assert_eq!(
            settings[&format!("terminal.integrated.defaultProfile.{os}")],
            json!(SANDBOXED)
        );
        assert_eq!(
            settings[&format!("terminal.integrated.automationProfile.{os}")],
            sandboxed
        );
        let listed = settings[&format!("terminal.integrated.profiles.{os}")]
            .as_object()
            .unwrap();
        assert_eq!(listed[SANDBOXED], sandboxed);
        assert_eq!(listed[YOUR_SHELL], your_shell);
        let offered: Vec<&String> = listed
            .iter()
            .filter(|(_, profile)| !profile.is_null())
            .map(|(name, _)| name)
            .collect();
        assert_eq!(offered.len(), 2, "{os}: {offered:?}");
        assert!(listed.len() > 2, "{os}: detected shells must be hidden");
        let keys = if platform == Platform::Windows { 4 } else { 3 };
        assert_eq!(settings.len(), keys, "{os}");
        for other in ["osx", "linux", "windows"]
            .iter()
            .filter(|other| **other != os)
        {
            assert!(
                settings
                    .keys()
                    .all(|key| !key.ends_with(&format!(".{other}"))),
                "{os} wrote {other} keys"
            );
        }
    }
    assert!(UNAVAILABLE.contains(YOUR_SHELL), "{UNAVAILABLE}");
    let windows = profiles(Platform::Windows, &launch);
    assert_eq!(windows["terminal.integrated.useWslProfiles"], json!(false));
}

#[test]
fn ide_shell_keeps_the_shell_arguments_after_its_own_options() {
    let parsed = ide_shell::parse(&arguments(&[
        COMMAND,
        "--project",
        PROJECT,
        "--cwd",
        "/tmp/work",
        "-c",
        "npm test --cwd x",
    ]))
    .unwrap();
    assert_eq!(parsed.project.as_str(), PROJECT);
    assert_eq!(parsed.cwd, Some(PathBuf::from("/tmp/work")));
    assert_eq!(parsed.shell, arguments(&["-c", "npm test --cwd x"]));
    let plain = ide_shell::parse(&arguments(&[COMMAND, "--project", PROJECT])).unwrap();
    assert_eq!(plain.cwd, None);
    assert!(plain.shell.is_empty());
    for refused in [
        vec![COMMAND],
        vec![COMMAND, "--project"],
        vec![COMMAND, "--project", "."],
        vec![COMMAND, "--project", "a/b"],
        vec![COMMAND, "--project", PROJECT, "--project", "beta"],
        vec![COMMAND, "--project", PROJECT, "--cwd", "relative"],
        vec![COMMAND, "--cwd", "/tmp", "-c", "true"],
        vec!["engine", "--project", PROJECT],
    ] {
        assert!(
            ide_shell::parse(&arguments(&refused)).is_err(),
            "{refused:?}"
        );
    }
    let mut long = vec![COMMAND, "--project", PROJECT];
    long.extend(std::iter::repeat_n("-x", 17));
    assert!(ide_shell::parse(&arguments(&long)).is_err());
}

#[cfg(unix)]
#[test]
fn ide_shell_finds_the_project_folder_and_its_read_folders_in_the_engine_database() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("engine");
    let folder = base.join("project");
    let extra = base.join("docs");
    for path in [&root, &folder, &extra] {
        std::fs::create_dir(path).unwrap();
    }
    {
        let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
        crate::sandbox::extra_paths::save(&db, PROJECT, std::slice::from_ref(&extra)).unwrap();
    }
    let before = std::fs::read(root.join("engine.db")).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let place = ide_shell::project_place(&root, &project).unwrap();
    assert_eq!(place.folder, folder);
    assert_eq!(place.extra_reads, vec![extra]);
    assert_eq!(std::fs::read(root.join("engine.db")).unwrap(), before);
    assert!(ide_shell::project_place(&root, &ProjectId::new("beta").unwrap()).is_err());
    assert!(ide_shell::project_place(&base.join("missing"), &project).is_err());
}
