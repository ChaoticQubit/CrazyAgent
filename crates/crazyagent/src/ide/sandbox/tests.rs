use super::*;
use crate::ide::bridge::engine_step::{FORWARD_REQUEST, FORWARDER_VARIABLES};
use kernel_sandbox::policy::{
    AgentAccess, AgentFilesRule, AgentNetwork, IdeAgentFiles, ide_network,
};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

const PORT: u16 = 45_731;

/// The network of a server that reaches nothing, as a Sandboxed one has.
fn closed() -> SandboxNetwork {
    SandboxNetwork::Only(Vec::new())
}

fn folder(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Runs `program` with `args` the way the IDE server starts: under the installed profile
/// through [`command`], from a cleared environment. Output: whether it exited 0, and its
/// standard output.
fn run(profile: &Path, program: &str, args: &[&std::ffi::OsStr]) -> (bool, String) {
    let mut command = super::command(profile, Path::new(program));
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = kernel_processes::spawn_gate::output(&mut command).unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// Moves `from` to `to` under the profile, the way a server would rename one of its data
/// folders into the project it can write. Inputs: the profile, the folder and its new path.
/// Output: whether the move succeeded.
fn renamed(profile: &Path, from: &Path, to: &Path) -> bool {
    run(profile, "/bin/mv", &[from.as_os_str(), to.as_os_str()]).0
}

/// Runs one `/bin/sh` script under the profile with `path` as `$1`. Output: whether it
/// exited 0.
fn sh(profile: &Path, script: &str, path: &Path) -> bool {
    run(
        profile,
        "/bin/sh",
        &[
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new(script),
            std::ffi::OsStr::new("sh"),
            path.as_os_str(),
        ],
    )
    .0
}

const REAL_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// Runs real git for a test's setup, outside any sandbox and with no owner or system
/// config, plus the given variables. Inputs: the folder, the variables and the arguments.
/// Output: whether git exited 0, with what it printed on standard output.
fn setup_git(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (bool, String) {
    let output = kernel_processes::spawn_gate::output(
        Command::new(REAL_GIT)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .envs(env.iter().copied())
            .args([
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
            ])
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgSign=false",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn setup(dir: &Path, args: &[&str]) -> String {
    let (ran, printed) = setup_git(dir, &[], args);
    assert!(ran, "git {args:?} in {}", dir.display());
    printed
}

/// Runs git the way the IDE server's git does: the engine's shim under the installed
/// profile, in the server's environment ([`crate::ide::env::server_env`], with the
/// project's git ceiling and configuration), real git first on the path and the server data
/// folder as home. Inputs: the profile, the shim, the home, the project folder, the folder
/// git runs in and the arguments. Output: whether git exited 0, with what it printed on
/// standard error.
fn ide_git(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    dir: &Path,
    args: &[&str],
) -> (bool, String) {
    let output = ide_git_output(profile, shim, home, root, dir, args);
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// [`ide_git`] with everything git printed. Inputs: as [`ide_git`]. Output: git's output.
fn ide_git_output(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    dir: &Path,
    args: &[&str],
) -> std::process::Output {
    ide_git_recorded(profile, shim, home, root, false, dir, args)
}

/// [`ide_git_output`] with the ceiling the engine builds when it records an agent in the
/// project's older agents root or not ([`crate::ide::git::ceiling`]). Inputs: as
/// [`ide_git`], with that answer. Output: git's output.
fn ide_git_recorded(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    older_agents: bool,
    dir: &Path,
    args: &[&str],
) -> std::process::Output {
    let mut command = server_git(
        profile,
        shim,
        home,
        root,
        older_agents,
        "/Library/Developer/CommandLineTools/usr/bin:/usr/bin:/bin",
    );
    command.args(args).current_dir(dir);
    crate::ide::bridge::engine_step::output_with_engine(&mut command, home, root)
}

/// The IDE server's git as VS Code starts it: the shim under the installed profile, in the
/// server's environment built from this PATH and the home, with the project's ceiling for
/// an agent recorded in the older agents root or not, and an owner identity. Inputs: the
/// profile, the shim, the home, the project folder, that answer and the PATH. Output: the
/// command, ready for its arguments and working folder.
fn server_git(
    profile: &Path,
    shim: &Path,
    home: &Path,
    root: &Path,
    older_agents: bool,
    path: &str,
) -> Command {
    let source = [
        (
            std::ffi::OsString::from("PATH"),
            std::ffi::OsString::from(path),
        ),
        (
            std::ffi::OsString::from("HOME"),
            home.as_os_str().to_owned(),
        ),
    ];
    let env = crate::ide::env::server_env(
        source,
        &home.join("capability"),
        &home.join("owner.sock"),
        &home.join("locked-settings.json"),
        AgentAccess::Sandboxed,
        Some(crate::ide::env::ServerGit {
            ceiling: &crate::ide::git::ceiling(root, older_agents).unwrap(),
            config: &home.join("gitconfig"),
        }),
    );
    let mut command = super::command(profile, shim);
    command
        .env_clear()
        .envs(env)
        .envs([
            ("GIT_AUTHOR_NAME", "Owner"),
            ("GIT_AUTHOR_EMAIL", "owner@example.invalid"),
            ("GIT_COMMITTER_NAME", "Owner"),
            ("GIT_COMMITTER_EMAIL", "owner@example.invalid"),
        ])
        .envs(crate::ide::bridge::engine_step::forwarder_env(home))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Writes an executable script that records that it ran by creating `marker`, then passes
/// its input through. Inputs: the script path and the marker. Output: none.
fn planted(script: &Path, marker: &Path) {
    fs::write(
        script,
        format!("#!/bin/sh\n: > '{}'\nexec cat\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
}

/// One project with one commit under an installed IDE profile, and the engine's shim and
/// private index folder as a launch writes them.
struct Server {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    profile: PathBuf,
    shim: PathBuf,
}

/// Makes a [`Server`]. Output: the server.
fn server() -> Server {
    server_with(AgentFilesRule::default())
}

/// Makes a [`Server`] whose profile holds an agent folders rule. Input: the rule. Output:
/// the server.
fn server_with(agent_files: AgentFilesRule) -> Server {
    let scratch = tempfile::Builder::new()
        .prefix("ca-ides-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join(crate::ide::FOLDER).join("p1");
    let build = data.join(crate::ide::FOLDER).join("builds").join("v1");
    let root = base.join("project");
    let home = mine.join("server");
    for path in [&mine, &home, &build, &root] {
        folder(path);
    }
    setup(&root, &["init", "-q", "-b", "main"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    setup(&root, &["add", "a.txt"]);
    setup(&root, &["commit", "-q", "-m", "base"]);
    let tmp =
        kernel_sandbox::toolchains::make_tmp(&crate::sandbox::TOOLCHAIN_HOOKS, &home).unwrap();
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    let profile = install(&mine, &grants, PORT, closed(), &[], agent_files).unwrap();
    crate::ide::settings::write(
        &mine,
        &home,
        &data,
        None,
        kernel_sandbox::policy::AgentAccess::Sandboxed,
        Some(&root),
    )
    .unwrap();
    let shim = crate::ide::settings::machine_folder(&home).join(crate::ide::git::SHIM);
    Server {
        _scratch: scratch,
        root,
        home,
        profile,
        shim,
    }
}

impl Server {
    /// Git through the shim in the project folder, with the engine step behind it. Input:
    /// the arguments. Output: whether it exited 0 and what it printed on standard error.
    fn git(&self, args: &[&str]) -> (bool, String) {
        ide_git(
            &self.profile,
            &self.shim,
            &self.home,
            &self.root,
            &self.root,
            args,
        )
    }

    /// Git as a task or the IDE shell runs it, by name with the shim's folder first on the
    /// path and no forwarder in its environment. Input: the shell line. Output: its output.
    fn task(&self, line: &str) -> std::process::Output {
        let path = format!(
            "{}:/Library/Developer/CommandLineTools/usr/bin:/usr/bin:/bin",
            crate::ide::settings::machine_folder(&self.home).display()
        );
        let mut command = server_git(
            &self.profile,
            Path::new("/bin/sh"),
            &self.home,
            &self.root,
            false,
            &path,
        );
        for name in FORWARDER_VARIABLES {
            command.env_remove(name);
        }
        command.args(["-c", line]).current_dir(&self.root);
        kernel_processes::spawn_gate::output(&mut command).unwrap()
    }

    /// The real index's path. Output: the path.
    fn index(&self) -> PathBuf {
        self.root.join(".git").join("index")
    }

    /// Whether the private index folder is empty. Output: the answer.
    fn no_private_files(&self) -> bool {
        fs::read_dir(self.home.join(crate::ide::git::PRIVATE_INDEX))
            .unwrap()
            .next()
            .is_none()
    }

    /// The owner's own `git status --porcelain`, outside the sandbox. Output: the text.
    fn status(&self) -> String {
        setup(&self.root, &["status", "--porcelain"])
    }
}

mod agent_folder_rows;
mod git_rows;
mod ide_denies;
mod profile_rows;
mod server_rows;
