use super::login_env::{login_child, login_env};
use crazyagent_win_sandbox::ProcessTree;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CONFIGS: [&str; 2] = ["CODEX_HOME", "CLAUDE_CONFIG_DIR"];
const SUSPENDED_WAIT: Duration = Duration::from_secs(2);
const OUTPUT_BOUND: Duration = Duration::from_secs(20);

/// The real path of a fresh profile home in the plain `C:\...` spelling, removed when
/// dropped. Output: the folder and its path.
fn profile_home() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(folder.path()).unwrap();
    let text = real.to_str().unwrap();
    let plain = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text));
    (folder, plain)
}

/// `cmd.exe` by its absolute path, as a login child's program always is.
fn cmd() -> PathBuf {
    let system = std::env::var_os("SystemRoot").expect("Windows sets SystemRoot");
    Path::new(&system).join("System32").join("cmd.exe")
}

/// N4 row 5, #729 C3 and C4: the env a login child is built with is the boundary's env at the
/// profile home, with the harness's config home and the C locale, and nothing else; its
/// `SystemRoot` is the checked one the boundary uses.
#[test]
fn a_login_child_env_is_the_boundary_env_with_its_config_home_and_the_c_locale() {
    let (_folder, home) = profile_home();
    for config in CONFIGS {
        let mut wanted: Vec<(&str, OsString)> =
            crate::sandbox::windows::windows_base_env(&home, home.as_os_str());
        wanted.push((config, home.clone().into_os_string()));
        wanted.push(("LANG", "C".into()));
        wanted.push(("LC_ALL", "C".into()));
        assert_eq!(login_env(&home, config), wanted, "{config}");
        let mut command = Command::new(cmd());
        login_child(&mut command, &home, config);
        let mut set: Vec<(OsString, OsString)> = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.unwrap_or_default().to_owned()))
            .collect();
        set.sort();
        let mut wanted: Vec<(OsString, OsString)> = wanted
            .into_iter()
            .map(|(name, value)| (OsString::from(name), value))
            .collect();
        wanted.sort();
        assert_eq!(set, wanted, "{config}");
        assert_eq!(command.get_current_dir(), Some(home.as_path()));
    }
}

/// N4 row 5 and #729 C2: a login child built by the one builder is created suspended, so it
/// runs nothing until its tree holds it, and then sees only its own env: the profile home,
/// the config home and the checked system folder, and no variable of the engine's.
#[test]
fn a_login_child_runs_only_once_held_and_sees_only_its_own_env() {
    let (_folder, home) = profile_home();
    let out = home.join("seen.txt");
    let mut command = Command::new(cmd());
    command.args([
        "/c",
        "echo %USERPROFILE%^|%CODEX_HOME%^|%SystemRoot%^|%USERNAME%^|%TEMP%> seen.txt",
    ]);
    login_child(&mut command, &home, "CODEX_HOME");
    command.stdin(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    std::thread::sleep(SUSPENDED_WAIT);
    let early = out.exists();
    let tree = ProcessTree::hold(&child);
    if early || tree.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(!early, "the login child ran before its tree held it");
    let _tree = tree.unwrap();
    assert!(child.wait().unwrap().success());
    let started = Instant::now();
    while !out.exists() {
        assert!(started.elapsed() < OUTPUT_BOUND);
        std::thread::sleep(Duration::from_millis(50));
    }
    let seen = std::fs::read_to_string(&out).unwrap();
    let system = crate::sandbox::windows::windows_base_env(&home, home.as_os_str())
        .into_iter()
        .find(|(name, _)| *name == "SystemRoot")
        .map(|(_, value)| value.into_string().unwrap())
        .unwrap();
    let home_text = home.to_str().unwrap();
    assert_eq!(
        seen.trim_end(),
        format!("{home_text}|{home_text}|{system}|%USERNAME%|{home_text}")
    );
}
