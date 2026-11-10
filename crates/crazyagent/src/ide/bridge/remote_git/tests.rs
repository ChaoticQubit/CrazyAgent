use super::*;
use crate::protocol::{FRAME_BYTES_MAX, encode_frame};
use std::os::unix::fs::PermissionsExt;
use std::process::Output;

const LIMIT: Duration = Duration::from_secs(60);
const LISTEN_TIME: Duration = Duration::from_secs(20);
const LEASED: &str = "0123456789abcdef0123456789abcdef01234567";

/// A real `git daemon` on 127.0.0.1 serving every bare repository in one folder, pushes
/// included, in its own process group, which is killed on drop.
pub(crate) struct Daemon {
    child: std::process::Child,
    pub(crate) port: u16,
}

impl Daemon {
    /// Starts the daemon on a free loopback port and waits until it accepts connections.
    /// Input: the folder of bare repositories. Output: the running daemon.
    pub(crate) fn start(base: &Path) -> Self {
        let port = crate::spawn_gate::bind_tcp_std("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut command = Command::new(GIT);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", base)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args([
                "daemon",
                "--reuseaddr",
                "--listen=127.0.0.1",
                "--export-all",
                "--enable=receive-pack",
                "--informative-errors",
            ])
            .arg(format!("--port={port}"))
            .arg(format!("--base-path={}", base.display()))
            .arg(base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = crate::spawn_gate::spawn(&mut command).unwrap();
        let deadline = Instant::now() + LISTEN_TIME;
        while crate::spawn_gate::connect_tcp_std(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "git daemon never listened");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self { child, port }
    }

    /// The daemon's URL of one served repository. Input: its folder name. Output: the URL.
    pub(crate) fn url(&self, name: &str) -> String {
        format!("git://127.0.0.1:{}/{name}", self.port)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(Pid::from_child(&self.child), Signal::KILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs the system git with no system or global config and a fixed identity, as a test's
/// own setup step outside any engine path. Inputs: the folder and the arguments. Output:
/// what git printed and its status.
pub(crate) fn plain_git(dir: &Path, args: &[&str]) -> Output {
    crate::spawn_gate::output(
        Command::new(GIT)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

/// [`plain_git`] that must succeed. Output: its trimmed standard output.
pub(crate) fn ok(dir: &Path, args: &[&str]) -> String {
    let output = plain_git(dir, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Makes a repository on main with one commit in `folder`, `origin` set to `url` and main
/// pushed there and tracked. Inputs: the folder and the remote URL. Output: the commit.
pub(crate) fn owner_repository(folder: &Path, url: &str) -> String {
    std::fs::create_dir_all(folder).unwrap();
    ok(folder, &["init", "-q", "-b", "main"]);
    std::fs::write(folder.join("README.md"), "# Seed shop\n").unwrap();
    ok(folder, &["add", "README.md"]);
    ok(folder, &["commit", "-q", "-m", "Start the seed shop"]);
    ok(folder, &["remote", "add", "origin", url]);
    ok(folder, &["push", "-q", "-u", "origin", "main"]);
    ok(folder, &["rev-parse", "HEAD"])
}

/// A project folder whose `origin` is a bare repository a real git daemon serves on
/// 127.0.0.1, an owner home with only an identity in its git config, and the daemon, which
/// stops before the folders go.
struct Project {
    daemon: Daemon,
    root: PathBuf,
    folder: PathBuf,
    home: PathBuf,
    bare: PathBuf,
    _temp: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("ca-ide-git-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let served = root.join("served");
        let home = root.join("home");
        for folder in [&served, &home] {
            std::fs::create_dir(folder).unwrap();
        }
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.invalid\n[commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        let bare = served.join("remote.git");
        ok(
            &served,
            &["init", "-q", "--bare", "-b", "main", "remote.git"],
        );
        let daemon = Daemon::start(&served);
        let folder = root.join("project");
        owner_repository(&folder, &daemon.url("remote.git"));
        Self {
            daemon,
            root,
            folder,
            home,
            bare,
            _temp: temp,
        }
    }

    /// The owner environment the engine builds, from a home and path the test controls.
    fn env(&self) -> Vec<(OsString, OsString)> {
        owner_env(
            [
                (OsString::from("HOME"), self.home.clone().into_os_string()),
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            ],
            &[self.folder.as_path()],
        )
    }

    /// Plans and runs one IDE git command in the project folder as the engine does.
    fn git(&self, args: &[&str]) -> Result<GitOutcome, EngineError> {
        self.git_within(args, LIMIT)
    }

    fn git_within(&self, args: &[&str], limit: Duration) -> Result<GitOutcome, EngineError> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let plan = plan(&args)?;
        let folder = checked_folder(&self.folder, &self.folder)?;
        run(&folder, &self.env(), &plan, &args, limit)
    }

    /// Commits one more change in the project folder. Output: the commit.
    fn commit(&self, file: &str, text: &str) -> String {
        std::fs::write(self.folder.join(file), text).unwrap();
        ok(&self.folder, &["add", file]);
        ok(
            &self.folder,
            &["commit", "-q", "-m", &format!("Write {file}")],
        );
        ok(&self.folder, &["rev-parse", "HEAD"])
    }

    /// The commit a branch of the daemon's bare repository holds, or None.
    fn served(&self, branch: &str) -> Option<String> {
        let output = plain_git(
            &self.bare,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
        );
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned())
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn refused(result: Result<GitOutcome, EngineError>) -> String {
    match result {
        Err(error) => error.0,
        Ok(outcome) => panic!("git ran: {outcome:?}"),
    }
}

fn landed(result: Result<GitOutcome, EngineError>) -> GitOutcome {
    let outcome = result.unwrap();
    assert_eq!(outcome.code, 0, "{outcome:?}");
    outcome
}

#[test]
fn every_network_command_vs_code_sends_is_planned_and_nothing_else() {
    let named = |verb, name: &str| Plan {
        verb,
        target: Target::Named(name.to_owned()),
    };
    let lease = format!("--force-with-lease=refs/heads/main:{LEASED}");
    let accepted: Vec<(Vec<&str>, Plan)> = vec![
        (
            vec!["fetch"],
            Plan {
                verb: Verb::Fetch,
                target: Target::Default,
            },
        ),
        (
            vec!["fetch", "--all"],
            Plan {
                verb: Verb::Fetch,
                target: Target::All,
            },
        ),
        (
            vec!["fetch", "--prune"],
            Plan {
                verb: Verb::Fetch,
                target: Target::Default,
            },
        ),
        (
            vec!["fetch", "origin", "main", "--prune", "--depth=3"],
            named(Verb::Fetch, "origin"),
        ),
        (
            vec!["fetch", "origin", "refs/tags/v1:refs/tags/v1", "--force"],
            named(Verb::Fetch, "origin"),
        ),
        (
            vec!["pull"],
            Plan {
                verb: Verb::Pull,
                target: Target::Default,
            },
        ),
        (
            vec!["pull", "--tags", "--autostash", "-r", "origin", "main"],
            named(Verb::Pull, "origin"),
        ),
        (
            vec!["pull", "--unshallow", "origin", "main"],
            named(Verb::Pull, "origin"),
        ),
        (
            vec!["push", "origin", "main:main"],
            named(Verb::Push, "origin"),
        ),
        (
            vec!["push", "-u", "origin", "feature"],
            named(Verb::Push, "origin"),
        ),
        (
            vec![
                "push",
                lease.as_str(),
                "--force-if-includes",
                "origin",
                "main:main",
            ],
            named(Verb::Push, "origin"),
        ),
        (
            vec!["push", "--force", "origin", "main"],
            named(Verb::Push, "origin"),
        ),
        (
            vec!["push", "--follow-tags", "origin", "main"],
            named(Verb::Push, "origin"),
        ),
        (
            vec!["push", "--no-follow-tags", "upstream", "HEAD:refs/heads/x"],
            named(Verb::Push, "upstream"),
        ),
        (
            vec!["push", "--tags", "origin"],
            named(Verb::Push, "origin"),
        ),
        (
            vec!["push", "origin", "--delete", "old"],
            named(Verb::Push, "origin"),
        ),
        (
            vec!["push"],
            Plan {
                verb: Verb::Push,
                target: Target::Default,
            },
        ),
        (
            vec!["ls-remote", "--heads", "origin"],
            named(Verb::LsRemote, "origin"),
        ),
        (
            vec!["ls-remote", "--tags", "origin"],
            named(Verb::LsRemote, "origin"),
        ),
    ];
    for (args, expected) in accepted {
        assert_eq!(plan(&strings(&args)).unwrap(), expected, "{args:?}");
    }
    let many = vec!["fetch"; GIT_ARGS_MAX + 1];
    let long = "a".repeat(GIT_ARG_BYTES_MAX + 1);
    let refused: Vec<Vec<&str>> = vec![
        vec![],
        vec!["status"],
        vec!["clone", "git://127.0.0.1/r"],
        vec!["-c", "core.sshCommand=touch /tmp/owned", "push"],
        vec!["push", "-c", "core.sshCommand=evil", "origin"],
        vec!["push", "--receive-pack=evil", "origin"],
        vec!["fetch", "--upload-pack=evil", "origin"],
        vec!["fetch", "-u", "evil", "origin"],
        vec!["pull", "--upload-pack=evil"],
        vec!["push", "--exec=evil", "origin"],
        vec!["push", "--repo=evil"],
        vec!["push", "-o", "ci.skip", "origin"],
        vec!["push", "--", "origin", "main"],
        vec!["push", "-uf", "origin", "main"],
        vec!["push", "origin", "+main"],
        vec!["push", "origin", ":main"],
        vec!["push", "origin", "main:"],
        vec!["push", "origin", "a:b:c"],
        vec!["push", "origin", "a..b"],
        vec!["push", "origin", "main@{1}"],
        vec!["push", "origin", "main.lock"],
        vec!["push", "origin", ".hidden"],
        vec!["push", "origin", "has space"],
        vec!["push", "origin", long.as_str()],
        vec!["push", "ext::sh -c touch% /tmp/owned", "main"],
        vec!["push", "/tmp/elsewhere.git", "main"],
        vec!["push", "git://127.0.0.1/x", "main"],
        vec!["push", "--delete", "origin"],
        vec![
            "push",
            "--force-with-lease=refs/heads/main:nothex",
            "origin",
        ],
        vec!["push", "--force-with-lease=main:0123", "origin"],
        vec!["fetch", "--all", "origin"],
        vec!["fetch", "--depth=0"],
        vec!["fetch", "--depth=x"],
        vec!["fetch", "--depth=1234567890"],
        vec!["fetch", "--prune-tags"],
        vec!["pull", "origin", "a", "b"],
        vec!["pull", "--rebase=interactive"],
        vec!["ls-remote"],
        vec!["ls-remote", "origin", "main"],
        vec!["ls-remote", "--upload-pack=evil", "origin"],
        many,
    ];
    for args in refused {
        assert_eq!(
            plan(&strings(&args)).err().map(|error| error.0),
            Some("ide_git_refused".to_owned()),
            "{args:?}"
        );
    }
}

#[test]
fn allowed_transports_are_https_http_ssh_and_git_only() {
    for url in [
        "https://github.com/owner/shop.git",
        "HTTPS://github.com/owner/shop.git",
        "http://git.example.invalid/shop",
        "ssh://git@github.com/owner/shop.git",
        "git://127.0.0.1:9418/remote.git",
        "git+ssh://git@example.invalid/shop",
        "ssh+git://git@example.invalid/shop",
        "git@github.com:owner/shop.git",
    ] {
        assert!(transport_allowed(url), "{url}");
    }
    for url in [
        "",
        "ext::sh -c touch% /tmp/owned",
        "fd::3",
        "file:///tmp/remote.git",
        "FILE:///tmp/remote.git",
        "/tmp/remote.git",
        "./remote.git",
        "../remote.git",
        "remote.git",
        "~/remote.git",
        "-oProxyCommand=touch /tmp/owned:x",
        "ssh://-oProxyCommand=touch/x",
        "https:///remote.git",
        "rsync://example.invalid/shop",
        "foo://example.invalid/shop",
        "a/b:c",
        ":path",
        "host:",
        "https://example.invalid/shop\n",
        "[::1]:shop",
    ] {
        assert!(!transport_allowed(url), "{url:?}");
    }
}

#[test]
fn the_owner_environment_passes_only_the_basics_and_turns_every_prompt_off() {
    let env = owner_env(
        [
            ("HOME", "/Users/owner"),
            ("HOME", "/elsewhere"),
            ("PATH", "/usr/bin:/bin"),
            ("SSH_AUTH_SOCK", "/private/tmp/agent.sock"),
            ("OPENAI_API_KEY", "sk-never"),
            ("GIT_DIR", "/tmp/elsewhere"),
            ("GIT_SSH_COMMAND", "touch /tmp/owned"),
            ("GIT_CONFIG_GLOBAL", "/tmp/planted"),
            ("GIT_ALLOW_PROTOCOL", "ext:file"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        &[],
    );
    let value = |name: &str| {
        let found: Vec<&OsString> = env
            .iter()
            .filter(|(known, _)| known == name)
            .map(|(_, value)| value)
            .collect();
        assert!(found.len() <= 1, "{name} twice");
        found
            .first()
            .map(|value| value.to_string_lossy().into_owned())
    };
    assert_eq!(value("HOME").as_deref(), Some("/Users/owner"));
    assert_eq!(value("PATH").as_deref(), Some("/usr/bin:/bin"));
    assert_eq!(
        value("SSH_AUTH_SOCK").as_deref(),
        Some("/private/tmp/agent.sock")
    );
    for name in [
        "OPENAI_API_KEY",
        "GIT_DIR",
        "GIT_SSH_COMMAND",
        "GIT_CONFIG_GLOBAL",
    ] {
        assert_eq!(value(name), None, "{name}");
    }
    assert_eq!(value("GIT_ALLOW_PROTOCOL").as_deref(), Some(PROTOCOLS));
    assert_eq!(value("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
    assert_eq!(value("GIT_ASKPASS").as_deref(), Some(""));
    assert_eq!(value("SSH_ASKPASS_REQUIRE").as_deref(), Some("never"));
    assert_eq!(value("LC_ALL").as_deref(), Some("en_US.UTF-8"));
}

#[test]
fn the_owner_path_keeps_only_real_folders_the_ide_and_agents_cannot_write() {
    let project = Project::new();
    let engine = project.root.join("engine");
    for folder in [
        project.folder.join("bin"),
        engine.join("ide").join("home").join("bin"),
    ] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let linked = project.root.join("linked");
    std::os::unix::fs::symlink("/usr/bin", &linked).unwrap();
    let path = std::env::join_paths([
        PathBuf::from("."),
        PathBuf::from("bin"),
        PathBuf::new(),
        project.folder.join("bin"),
        project.folder.join("bin").join(".."),
        engine.join("ide").join("home").join("bin"),
        project.root.clone(),
        PathBuf::from("/private/var/folders"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/nowhere/ca-missing"),
        linked,
        PathBuf::from("/bin"),
    ])
    .unwrap();
    assert_eq!(
        owner_path(&path, &[project.folder.as_path(), engine.as_path()]),
        OsString::from("/usr/bin:/usr/bin:/bin")
    );
    assert_eq!(
        owner_path(&path, &[Path::new("/usr")]),
        OsString::from("/bin"),
        "a denied folder is left out by its real path, through a link too"
    );
    let env = owner_env(
        [(OsString::from("PATH"), path)],
        &[project.folder.as_path(), engine.as_path()],
    );
    assert_eq!(
        env.iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| value.clone()),
        Some(OsString::from("/usr/bin:/usr/bin:/bin"))
    );
    assert_eq!(owner_path(OsStr::new(".:bin"), &[]), OsString::new());
}

#[test]
fn a_pull_runs_only_its_fetch_and_a_command_naming_no_remote_names_the_checked_one() {
    let planned = |args: &[&str], default: Option<&str>| {
        let args = strings(args);
        engine_args(&plan(&args).unwrap(), &args, default)
    };
    assert_eq!(
        planned(
            &[
                "pull",
                "--tags",
                "--unshallow",
                "--autostash",
                "-r",
                "origin",
                "main"
            ],
            None
        ),
        strings(&["fetch", "--tags", "--unshallow", "origin", "main"])
    );
    assert_eq!(
        planned(&["pull"], Some("upstream")),
        strings(&["fetch", "upstream"])
    );
    assert_eq!(
        planned(&["pull", "-r"], Some("origin")),
        strings(&["fetch", "origin"])
    );
    assert_eq!(
        planned(&["fetch", "--prune"], Some("upstream")),
        strings(&["fetch", "--prune", "upstream"])
    );
    assert_eq!(
        planned(&["push", "-u", "--follow-tags"], Some("mirror")),
        strings(&["push", "-u", "--follow-tags", "mirror"])
    );
    assert_eq!(
        planned(&["fetch", "--all", "--prune"], None),
        strings(&["fetch", "--all", "--prune"])
    );
    assert_eq!(
        planned(&["push", "--force", "origin", "main:main"], None),
        strings(&["push", "--force", "origin", "main:main"])
    );
    assert_eq!(
        planned(&["ls-remote", "--heads", "origin"], None),
        strings(&["ls-remote", "--heads", "origin"])
    );
}

#[test]
fn what_a_git_command_printed_fits_one_bridge_frame() {
    let mut noisy = Vec::new();
    for _ in 0..(OUTPUT_BYTES_MAX * 4) {
        noisy.extend_from_slice(b"\"\\\xff\x1b\r");
    }
    noisy.extend_from_slice("Ende \u{e9}\n".as_bytes());
    let kept = text(&noisy, OUTPUT_BYTES_MAX);
    assert!(kept.len() <= OUTPUT_BYTES_MAX);
    assert!(kept.ends_with("Ende \u{e9}\n"), "the last bytes are kept");
    assert!(!kept.contains('\r') && !kept.contains('\u{1b}'));
    let outcome = GitOutcome {
        code: 1,
        stdout: kept.clone(),
        stderr: kept,
    };
    let reply = super::super::IdeReply::Ok {
        data: serde_json::to_value(&outcome).unwrap(),
    };
    assert!(encode_frame(&reply).unwrap().len() <= FRAME_BYTES_MAX + 4);
    assert_eq!(
        text(b"To git://h/r\r\n   abc..def  main -> main\n", 64),
        "To git://h/r\n\n   abc..def  main -> main\n"
    );
    let (tail_kept, cut) = tail(&b"0123456789"[..], 4);
    assert_eq!((tail_kept.as_slice(), cut), (&b"6789"[..], true));
    assert_eq!(tail(&b"0123"[..], 4), (b"0123".to_vec(), false));
}

/// land-19 r2 L1: the IDE's git reads no grafts file and prints no advice about the one it
/// is pointed at, so a push's error output, which the IDE shows, holds only what the push
/// itself says.
#[test]
fn a_push_from_the_project_folder_prints_no_grafts_advice() {
    let project = Project::new();
    let head = project.commit("notes.md", "Planting notes.\n");
    let pushed = landed(project.git(&["push", "origin", "main:main"]));
    assert_eq!(project.served("main").as_deref(), Some(head.as_str()));
    assert!(!pushed.stderr.contains("graft"), "{pushed:?}");
}

#[test]
fn a_push_from_the_project_folder_reaches_the_daemon_with_no_hook_running() {
    let project = Project::new();
    let head = project.commit("notes.md", "Planting notes.\n");
    let marker = project.root.join("hook-ran");
    let hook = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
    let planted = project.folder.join("hooks");
    std::fs::create_dir(&planted).unwrap();
    for folder in [project.folder.join(".git").join("hooks"), planted.clone()] {
        for name in ["pre-push", "reference-transaction", "post-update"] {
            let path = folder.join(name);
            std::fs::write(&path, &hook).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    ok(
        &project.folder,
        &["config", "core.hooksPath", planted.to_str().unwrap()],
    );
    let pushed = landed(project.git(&["push", "origin", "main:main"]));
    assert_eq!(project.served("main").as_deref(), Some(head.as_str()));
    assert!(pushed.stderr.contains("main -> main"), "{pushed:?}");
    assert!(!marker.exists(), "a hook ran outside the IDE sandbox");

    ok(&project.folder, &["checkout", "-q", "-b", "feature"]);
    let feature = project.commit("feature.md", "A new bed.\n");
    landed(project.git(&["push", "-u", "origin", "feature"]));
    assert_eq!(project.served("feature").as_deref(), Some(feature.as_str()));
    assert_eq!(
        ok(&project.folder, &["config", "branch.feature.remote"]),
        "origin",
        "the engine's push set the upstream the IDE sandbox cannot write"
    );
    landed(project.git(&["push", "origin", "--delete", "feature"]));
    assert_eq!(project.served("feature"), None);
    assert!(!marker.exists(), "a hook ran outside the IDE sandbox");
}

#[test]
fn fetch_pull_and_ls_remote_bring_the_daemon_commits_and_never_write_the_working_tree() {
    let project = Project::new();
    std::fs::create_dir(project.folder.join("context")).unwrap();
    let mine = project.commit("context/x", "Mine.\n");
    ok(&project.folder, &["push", "-q", "origin", "main"]);
    let other = project.root.join("other");
    ok(
        &project.root,
        &[
            "clone",
            "-q",
            &project.daemon.url("remote.git"),
            other.to_str().unwrap(),
        ],
    );
    std::fs::write(other.join("context").join("x"), "Theirs.\n").unwrap();
    std::fs::write(other.join("beds.md"), "Six beds.\n").unwrap();
    ok(&other, &["add", "context/x", "beds.md"]);
    ok(&other, &["commit", "-q", "-m", "Plan the beds"]);
    ok(&other, &["push", "-q", "origin", "main"]);
    let theirs = ok(&other, &["rev-parse", "HEAD"]);

    let listed = landed(project.git(&["ls-remote", "--heads", "origin"]));
    assert!(
        listed
            .stdout
            .contains(&format!("{theirs}\trefs/heads/main")),
        "{listed:?}"
    );
    landed(project.git(&["fetch"]));
    assert_eq!(
        ok(&project.folder, &["rev-parse", "refs/remotes/origin/main"]),
        theirs
    );
    landed(project.git(&["fetch", "--all", "--prune"]));
    let fetched =
        || std::fs::read_to_string(project.folder.join(".git").join("FETCH_HEAD")).unwrap();
    for pull in [
        &["pull", "--tags", "--autostash", "-r", "origin", "main"][..],
        &["pull"],
    ] {
        std::fs::remove_file(project.folder.join(".git").join("FETCH_HEAD")).unwrap();
        let pulled = landed(project.git(pull));
        assert!(
            fetched().starts_with(&format!("{theirs}\t\tbranch 'main' of ")),
            "{pull:?} fetched {:?}: {pulled:?}",
            fetched()
        );
        assert_eq!(
            ok(&project.folder, &["rev-parse", "HEAD"]),
            mine,
            "{pull:?} moved HEAD in the engine"
        );
        assert_eq!(
            std::fs::read_to_string(project.folder.join("context").join("x")).unwrap(),
            "Mine.\n",
            "{pull:?} wrote the working tree in the engine"
        );
        assert!(!project.folder.join("beds.md").exists(), "{pull:?}");
        assert_eq!(ok(&project.folder, &["status", "--porcelain"]), "");
    }
}

#[test]
fn only_the_project_folder_itself_runs_git_in_the_engine() {
    let project = Project::new();
    let inside = project.folder.join("src");
    std::fs::create_dir(&inside).unwrap();
    let agents = project.folder.join(".crazyagent").join("worktrees");
    std::fs::create_dir_all(&agents).unwrap();
    let agent = agents.join("builder");
    ok(
        &project.folder,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent",
            agent.to_str().unwrap(),
        ],
    );
    let elsewhere = project.root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    for asked in [
        inside.clone(),
        agent.clone(),
        elsewhere.clone(),
        project.root.clone(),
        PathBuf::from("project"),
        project.root.join("missing"),
    ] {
        assert_eq!(
            checked_folder(&asked, &project.folder)
                .err()
                .map(|error| error.0),
            Some("ide_git_folder_refused".to_owned()),
            "{}",
            asked.display()
        );
    }
    let linked = project.root.join("linked");
    std::os::unix::fs::symlink(&project.folder, &linked).unwrap();
    assert_eq!(
        checked_folder(&linked, &project.folder).unwrap(),
        project.folder,
        "the same folder through a link is the project folder"
    );
    assert_eq!(
        checked_folder(&agent, &agent).err().map(|error| error.0),
        Some("ide_git_repository_unsupported".to_owned()),
        "a linked worktree is never an IDE project folder"
    );
}

#[test]
fn remotes_must_be_configured_and_reach_only_allowed_transports() {
    let project = Project::new();
    let head = project.commit("notes.md", "Planting notes.\n");
    let marker = project.root.join("ext-ran");
    let bare = project.bare.to_str().unwrap().to_owned();
    assert_eq!(
        refused(project.git(&["push", "upstream", "main"])),
        "ide_git_remote_unknown"
    );

    let legacy = project.folder.join(".git").join("remotes");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(
        legacy.join("legacy"),
        format!(
            "URL: {}\nPush: refs/heads/main:refs/heads/legacy\n",
            project.daemon.url("remote.git")
        ),
    )
    .unwrap();
    assert_eq!(
        refused(project.git(&["push", "legacy", "main"])),
        "ide_git_remote_unknown",
        "a remote file the IDE could plant is not a configured remote"
    );

    let ext = format!("ext::sh -c touch% {}", marker.display());
    ok(&project.folder, &["remote", "add", "evil", &ext]);
    for args in [
        ["push", "evil", "main"].as_slice(),
        &["fetch", "evil"],
        &["ls-remote", "--heads", "evil"],
        &["fetch", "--all"],
    ] {
        assert_eq!(
            refused(project.git(args)),
            "ide_git_transport_refused",
            "{args:?}"
        );
    }
    ok(&project.folder, &["remote", "remove", "evil"]);
    ok(&project.folder, &["remote", "add", "local", &bare]);
    assert_eq!(
        refused(project.git(&["push", "local", "main"])),
        "ide_git_transport_refused"
    );
    ok(&project.folder, &["remote", "remove", "local"]);

    ok(
        &project.folder,
        &["remote", "set-url", "--push", "origin", &bare],
    );
    assert_eq!(
        refused(project.git(&["push", "origin", "main"])),
        "ide_git_transport_refused",
        "the push URL is checked for a push"
    );
    landed(project.git(&["fetch", "origin"]));
    ok(
        &project.folder,
        &["config", "--unset", "remote.origin.pushurl"],
    );

    let rewrite = format!("url.{bare}.insteadOf");
    ok(
        &project.folder,
        &["config", &rewrite, &project.daemon.url("remote.git")],
    );
    assert_eq!(
        refused(project.git(&["push", "origin", "main"])),
        "ide_git_transport_refused",
        "URLs are checked after insteadOf"
    );
    ok(&project.folder, &["config", "--unset", &rewrite]);

    ok(
        &project.folder,
        &[
            "config",
            "branch.main.remote",
            &project.daemon.url("remote.git"),
        ],
    );
    assert_eq!(
        refused(project.git(&["fetch"])),
        "ide_git_remote_unknown",
        "a URL in place of a branch's remote name is not a configured remote"
    );
    ok(&project.folder, &["config", "branch.main.remote", "origin"]);

    assert!(!marker.exists(), "an ext:: command ran");
    assert_ne!(project.served("main").as_deref(), Some(head.as_str()));
    assert_eq!(project.served("legacy"), None);
    landed(project.git(&["push", "origin", "main:main"]));
    assert_eq!(project.served("main").as_deref(), Some(head.as_str()));
}

#[test]
fn git_config_planted_inside_the_project_folder_is_refused() {
    let project = Project::new();
    let head = project.commit("notes.md", "Planting notes.\n");
    let marker = project.root.join("ssh-ran");
    std::fs::write(
        project.folder.join("shared.gitconfig"),
        format!("[core]\n\tgitProxy = touch {}\n", marker.display()),
    )
    .unwrap();
    ok(
        &project.folder,
        &["config", "include.path", "../shared.gitconfig"],
    );
    assert_eq!(
        refused(project.git(&["push", "origin", "main:main"])),
        "ide_git_config_refused"
    );
    ok(&project.folder, &["config", "--unset", "include.path"]);

    ok(
        &project.folder,
        &["config", "extensions.worktreeConfig", "true"],
    );
    ok(
        &project.folder,
        &["config", "--worktree", "push.default", "current"],
    );
    landed(project.git(&["push", "origin", "main:main"]));
    assert_eq!(project.served("main").as_deref(), Some(head.as_str()));
    assert!(!marker.exists());
}

#[test]
fn a_git_command_past_its_limit_is_killed_and_its_outcome_unknown() {
    let project = Project::new();
    let silent = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    ok(
        &project.folder,
        &[
            "remote",
            "add",
            "silent",
            &format!("git://127.0.0.1:{port}/remote.git"),
        ],
    );
    let started = Instant::now();
    assert_eq!(
        refused(project.git_within(&["fetch", "silent"], Duration::from_secs(2))),
        "ide_git_outcome_unknown"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    drop(silent);
}

/// SECAUDIT ide-git-commondir-case-variant-config-redirect: every later git the engine runs
/// names the project's git folder as its common folder, so a `commondir` planted after the
/// check redirects none of them.
#[test]
fn a_commondir_planted_after_the_check_redirects_no_later_git() {
    let project = Project::new();
    let store = project.root.join("store");
    for name in ["objects", "refs"] {
        std::fs::create_dir_all(store.join(name)).unwrap();
    }
    std::fs::write(
        store.join("config"),
        "[core]\n\trepositoryformatversion = 0\n[probe]\n\tvalue = planted\n",
    )
    .unwrap();
    ok(&project.folder, &["config", "probe.value", "project"]);
    std::fs::write(
        project.folder.join(".git").join("commondir"),
        format!("{}\n", store.display()),
    )
    .unwrap();
    let done = ran(
        git(
            &project.folder,
            &project.env(),
            &["config", "--get", "probe.value"],
        ),
        4096,
        LIMIT,
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&done.stdout).trim(), "project");
}

/// SECAUDIT ide-git-commondir-case-variant-config-redirect: a `commondir` planted in the
/// project's `.git`, in any spelling the volume folds together, cannot send the engine's git
/// to another folder's config and run a command from it.
#[test]
fn a_planted_commondir_runs_no_command_from_another_folder_s_config() {
    let project = Project::new();
    std::fs::write(project.root.join("CaseProbe"), "").unwrap();
    let names: &[&str] = if project.root.join("caseprobe").exists() {
        &["commondir", "COMMONDIR"]
    } else {
        &["commondir"]
    };
    let store = project.root.join("store");
    for name in ["objects", "refs"] {
        std::fs::create_dir_all(store.join(name)).unwrap();
    }
    let marker = project.root.join("ssh-ran");
    std::fs::write(
        store.join("config"),
        format!(
            "[core]\n\trepositoryformatversion = 0\n\tsshCommand = touch {}\n[remote \"origin\"]\n\turl = ssh://dummy.invalid/x.git\n",
            marker.display()
        ),
    )
    .unwrap();
    for name in names {
        let planted = project.folder.join(".git").join(name);
        std::fs::write(&planted, format!("{}\n", store.display())).unwrap();
        assert_eq!(
            refused(project.git(&["ls-remote", "origin"])),
            "ide_git_config_refused",
            "{name}"
        );
        assert!(!marker.exists(), "{name}");
        std::fs::remove_file(&planted).unwrap();
    }
}
