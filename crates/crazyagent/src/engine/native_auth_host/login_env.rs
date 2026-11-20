use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Gives a sign-in login child its one shape, so every login child command is built the same
/// way: only [`login_env`] as its environment, the profile home as its folder, its output
/// piped, and its own process group on Unix; on Windows it is created suspended with no
/// window, so the job its custody holds is in place before it runs (#729 C2). The caller sets
/// its stdin. Inputs: the command, the profile home and the harness's config variable.
/// Output: none.
pub(super) fn login_child(command: &mut Command, home: &Path, config: &'static str) {
    assert!(home.is_absolute());
    command
        .env_clear()
        .envs(login_env(home, config))
        .current_dir(home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(
        command,
        CREATE_SUSPENDED | CREATE_NO_WINDOW,
    );
}

/// The environment a sign-in login child starts with on Unix: HOME, its config home and
/// TMPDIR at the profile home, the system PATH and the C locale. Inputs: the profile home and
/// the harness's config variable. Output: the names and values.
#[cfg(unix)]
pub(super) fn login_env(home: &Path, config: &'static str) -> Vec<(&'static str, OsString)> {
    let home = home.as_os_str().to_owned();
    vec![
        ("HOME", home.clone()),
        (config, home.clone()),
        ("TMPDIR", home),
        ("PATH", super::SIGN_IN_PATH.into()),
        ("LANG", "C".into()),
        ("LC_ALL", "C".into()),
    ]
}

/// The environment a sign-in login child starts with on Windows: the boundary's own env at
/// the profile home ([`crate::sandbox::windows::windows_base_env`], its checked `SystemRoot`
/// and system PATH included, #729 C3 and C4), its config home and the C locale. Inputs: the
/// profile home and the harness's config variable. Output: the names and values.
#[cfg(windows)]
pub(super) fn login_env(home: &Path, config: &'static str) -> Vec<(&'static str, OsString)> {
    let mut env = crate::sandbox::windows::windows_base_env(home, home.as_os_str());
    assert!(env.iter().all(|(name, _)| *name != config));
    env.extend([
        (config, home.as_os_str().to_owned()),
        ("LANG", "C".into()),
        ("LC_ALL", "C".into()),
    ]);
    env
}
