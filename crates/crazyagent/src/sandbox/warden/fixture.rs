use super::super::{ProjectSandbox, SandboxNetwork, SandboxRemote, agent_command};
use rustix::process::Pid;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A run that leaves what a Claude Code Bash tool shell can leave: a process in its own
/// session with its own stdio, and a `nohup` command whose setsid shell already exited. It
/// writes their ids and its own into its working folder, then sleeps.
const DETACHING_RUN: &str = r#"/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; open(my $file, ">", "descendant.pid") or exit 1; print $file "$$\n"; close($file); sleep 30' < /dev/null > /dev/null 2>&1 &
/usr/bin/perl -mPOSIX -e 'POSIX::setsid() > 0 or exit 1; exec "/bin/sh", "-c", q(/usr/bin/nohup /bin/sleep 30 < /dev/null > /dev/null 2>&1 & echo $! > orphan.pid)' < /dev/null > /dev/null 2>&1
echo $$ > leader.pid
exec /bin/sleep 30
"#;

/// A private project, home and temp folder under the canonical `/tmp` with the agent profile every
/// project agent gets installed for them, for tests that start a detaching run in it.
pub(crate) struct DetachingRun {
    _root: Option<tempfile::TempDir>,
    pub(crate) project: PathBuf,
    pub(crate) profile: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
}

impl DetachingRun {
    /// Makes the folders and installs the profile. Takes nothing. Returns the run.
    pub(crate) fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("crazyagent-warden-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let folder = |name: &str| {
            let folder = root.path().join(name);
            std::fs::create_dir(&folder).unwrap();
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::canonicalize(folder).unwrap()
        };
        let (project, home, tmp) = (folder("project"), folder("home"), folder("tmp"));
        let profile = root.path().join("agent.sb");
        ProjectSandbox::new(
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        )
        .unwrap()
        .install(&profile)
        .unwrap();
        Self {
            _root: Some(root),
            project,
            profile,
            home,
            tmp,
        }
    }

    /// A second run under this run's profile, home and temp folder, working in a new folder
    /// `name` inside this run's project, as two agents launched from one profile file are.
    /// Takes the folder name. Returns the run, whose folders are removed with this run's.
    pub(crate) fn beside(&self, name: &str) -> Self {
        let project = self.project.join(name);
        std::fs::create_dir(&project).unwrap();
        Self {
            _root: None,
            project,
            profile: self.profile.clone(),
            home: self.home.clone(),
            tmp: self.tmp.clone(),
        }
    }

    /// The detaching run as the one sandboxed agent command, under `profile`, with null stdio.
    /// Takes the profile. Returns the command.
    pub(crate) fn command(&self, profile: &Path) -> Command {
        let mut command = agent_command(
            profile,
            Path::new("/bin/sh"),
            &[
                ("HOME", self.home.as_os_str()),
                ("TMPDIR", self.tmp.as_os_str()),
                ("PATH", OsStr::new("/usr/bin:/bin")),
            ],
            std::iter::empty::<(&OsStr, Option<&OsStr>)>(),
        )
        .unwrap();
        command
            .args(["-c", DETACHING_RUN])
            .current_dir(&self.project)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    /// Waits up to ten seconds for a process id file in the project. Takes its name.
    /// Returns the process.
    pub(crate) fn written(&self, name: &str) -> Pid {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(process) = self.read(name) {
                return process;
            }
            assert!(Instant::now() < deadline, "{name} never written");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Reads a process id file in the project. Takes its name. Returns the process, if any.
    fn read(&self, name: &str) -> Option<Pid> {
        std::fs::read_to_string(self.project.join(name))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            .and_then(Pid::from_raw)
    }

    /// Waits up to ten seconds for the run's three ids and checks the two detached processes
    /// left its group and are alive. Takes nothing. Returns the harness, session and orphan.
    pub(crate) fn processes(&self) -> [Pid; 3] {
        let read = |name: &str| self.read(name);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let (Some(leader), Some(session), Some(orphan)) = (
                read("leader.pid"),
                read("descendant.pid"),
                read("orphan.pid"),
            ) {
                for detached in [session, orphan] {
                    assert_ne!(rustix::process::getpgid(Some(detached)), Ok(leader));
                    assert_eq!(rustix::process::test_kill_process(detached), Ok(()));
                }
                return [leader, session, orphan];
            }
            assert!(Instant::now() < deadline, "the run never started");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Waits up to `wait` for a child to be reaped. Takes the child and the wait. Returns true
/// once it is.
pub(crate) fn reaped_within(child: &mut Child, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits up to `wait` for processes to be gone, then kills any left. Takes the processes and
/// the wait. Returns those that were still alive.
pub(crate) fn survivors(processes: [Pid; 3], wait: Duration) -> Vec<Pid> {
    let deadline = Instant::now() + wait;
    loop {
        let alive: Vec<Pid> = processes
            .into_iter()
            .filter(|process| {
                rustix::process::test_kill_process(*process) != Err(rustix::io::Errno::SRCH)
            })
            .collect();
        if alive.is_empty() || Instant::now() >= deadline {
            for process in &alive {
                let _ = rustix::process::kill_process(*process, rustix::process::Signal::KILL);
            }
            return alive;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// This user's processes whose command line contains `needle`, read with `ps`. Input: the
/// text. Output: their ids and command lines.
pub(crate) fn processes_naming(needle: &str) -> Vec<(i32, String)> {
    let listing = crate::spawn_gate::output(
        std::process::Command::new("/bin/ps")
            .args(["-A", "-ww", "-o", "pid=,uid=,command="])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    assert!(!listing.stdout.is_empty());
    naming(
        &String::from_utf8_lossy(&listing.stdout),
        &rustix::process::getuid().as_raw().to_string(),
        i32::try_from(std::process::id()).unwrap(),
        needle,
    )
}

/// Picks from a `ps -o pid=,uid=,command=` listing the processes of `user` other than `own`
/// whose command line contains `needle`. Inputs: the listing, the user id, this process's id
/// and the text. Output: their ids and command lines.
pub(crate) fn naming(listing: &str, user: &str, own: i32, needle: &str) -> Vec<(i32, String)> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: i32 = fields.next()?.parse().ok()?;
            let owner = fields.next()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            (owner == user && pid != own && command.contains(needle)).then_some((pid, command))
        })
        .collect()
}

/// Waits until `deadline` for no process of this user to name any of `needles`, then kills
/// by id whatever still does, so a failing run leaves nothing behind. Inputs: the deadline
/// and the texts. Output: the processes that outlived it.
pub(crate) fn detached_survivors(
    deadline: std::time::Instant,
    needles: &[&str],
) -> Vec<(i32, String)> {
    loop {
        let left: Vec<(i32, String)> = needles
            .iter()
            .flat_map(|needle| processes_naming(needle))
            .collect();
        if left.is_empty() || std::time::Instant::now() >= deadline {
            for (pid, _) in &left {
                if let Some(pid) = rustix::process::Pid::from_raw(*pid) {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
            }
            return left;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
