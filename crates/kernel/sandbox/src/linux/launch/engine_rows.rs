use crazyagent_linux_supervisor::rules::{GitOwn, Held};
use crate::error::SandboxError;
use crate::host::LinuxHost;
use crate::lines::{
    LINUX_PORTS_ANY_HOST_LINE, LINUX_UNROUTED_ANY_HOST_LINE, LINUX_UNROUTED_LINE,
    linux_unbridged_line,
};
use crate::linux::helper::bridge::Bridge;
use crate::linux::helper::landlock::*;
use crate::linux::helper::*;
use crate::linux::launch::host::*;
use crate::linux::launch::*;
use crate::net::{SandboxNetwork, SandboxRemote};
use crate::ports::PICKED_PORTS_START;
use crate::ports::PROXY_PORTS;
use crate::profile::consts::LINUX_OPEN_NETWORK_NEEDS_LANDLOCK;
use crate::profile::{ProjectSandbox, command::Sandbox, linux_unbridged_notice_on};
use crate::settings::{LinuxSockets, LinuxSupervisor};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

fn helper() -> PathBuf {
    Path::new("/opt/crazyagent").join(HELPER_NAME)
}

fn project(network: SandboxNetwork) -> (tempfile::TempDir, ProjectSandbox) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let folders = ["project", "home", "tmp"].map(|name| root.join(name));
    for folder in &folders {
        fs::create_dir(folder).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &crate::test_hooks::STAND_IN,
        &folders[0],
        &folders[1],
        &folders[2],
        network,
    )
    .unwrap();
    (temp, sandbox)
}

fn git(folder: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(folder)
        .args([
            "-c",
            "user.name=row",
            "-c",
            "user.email=row@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdout(Stdio::null());
    let status = kernel_processes::spawn_gate::status(&mut command).unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A project with one commit and a linked worktree in its worktrees folder, then the
/// sandbox over it with the agent's home and temp folders. No input. Output: the temp
/// folder, the project, the sandbox and the worktree.
fn linked() -> (tempfile::TempDir, PathBuf, ProjectSandbox, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let folders = ["project", "home", "tmp"].map(|name| root.join(name));
    for folder in &folders {
        fs::create_dir(folder).unwrap();
    }
    let project = folders[0].clone();
    fs::write(project.join("owner.txt"), "owner\n").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "owner.txt"]);
    git(&project, &["commit", "-q", "-m", "start"]);
    fs::create_dir(project.join("worktrees")).unwrap();
    let worktree = project.join("worktrees").join("agent-1");
    let path = worktree.to_str().unwrap();
    git(
        &project,
        &["worktree", "add", "-q", "--detach", path, "HEAD"],
    );
    let sandbox = ProjectSandbox::new(
        &crate::test_hooks::STAND_IN,
        &project,
        &folders[1],
        &folders[2],
        SandboxNetwork::Open,
    )
    .unwrap();
    (temp, project, sandbox, worktree)
}

#[test]
fn a_linked_worktree_writes_its_tree_its_admin_folder_without_links_and_no_object() {
    let (_temp, project, sandbox, worktree) = linked();
    let confined = sandbox.confine_to_worktree(&worktree).unwrap();
    let git = project.join(".git");
    let admin = git.join("worktrees").join("agent-1");
    let objects = git.join("objects");
    let granted = Grants::of(&confined);
    assert!(granted.is_ok(), "{:?}", granted.as_ref().err());
    let grants = granted.unwrap();
    assert!(grants.write.contains(&worktree));
    assert_eq!(grants.git_write, vec![admin.clone()]);
    assert!(
        !grants.write.contains(&admin),
        "the admin folder has the git tier only"
    );
    for path in grants.write.iter().chain(&grants.git_write) {
        assert!(
            !path.starts_with(&objects) && !objects.starts_with(path),
            "{} reaches the object store",
            path.display()
        );
        assert!(
            *path == admin || !path.starts_with(&git),
            "{} is in the project's git folder",
            path.display()
        );
    }
    assert!(
        grants.read.contains(&project),
        "the object store is read through the project"
    );
    assert!(grants.guarded.entries.is_empty() && grants.guarded.stores.is_empty());
    let mut guarded = grants.guarded.trees.clone();
    guarded.sort();
    let mut held = ["HEAD", "commondir", "gitdir", "config.worktree", "modules"]
        .map(|name| admin.join(name))
        .to_vec();
    held.push(worktree.join(".git"));
    held.sort();
    assert_eq!(
        guarded, held,
        "every entry macOS keeps by name under the two grants is left to the supervisor and the binds"
    );
}

#[test]
fn a_confined_launch_on_the_bubblewrap_backend_is_refused_with_its_own_line() {
    let (_temp, _project, sandbox, worktree) = linked();
    let launch = LinuxSandbox::assembled(
        sandbox.confine_to_worktree(&worktree).unwrap(),
        LinuxBackend::Bubblewrap {
            bwrap: PathBuf::from(BWRAP_PATHS[0]),
            helper: helper(),
        },
        None,
        None,
    );
    let mut harness = Command::new("/bin/true");
    harness.current_dir(&worktree);
    let refused = launch.wrap(&harness).err();
    assert!(
        matches!(&refused, Some(SandboxError::Unavailable(reason))
            if reason == CONFINED_NEEDS_LANDLOCK),
        "{refused:?}"
    );
}

#[test]
fn a_launch_whose_git_grant_covers_a_protected_path_nothing_holds_is_refused() {
    let (_temp, _project, sandbox, worktree) = linked();
    let launch = LinuxSandbox::assembled(
        sandbox.confine_to_worktree(&worktree).unwrap(),
        LinuxBackend::Landlock { helper: helper() },
        None,
        None,
    );
    let mut harness = Command::new("/bin/true");
    harness.current_dir(&worktree);
    let refused = launch.wrap(&harness).err();
    assert!(
        matches!(&refused, Some(SandboxError::Unavailable(reason))
            if reason == CONFINED_UNAVAILABLE),
        "{refused:?}"
    );
}

/// LINUX-KILL-GROUP: only the plain Landlock backend leaves a process in a session of its
/// own running after a stop's group kill, so only its runs have their lineage checked;
/// both bubblewrap backends end every process with their process namespace.
#[test]
fn only_the_plain_landlock_backend_leaves_a_session_of_its_own_running() {
    let (_temp, _project, sandbox, _worktree) = linked();
    let ends = |backend: LinuxBackend| {
        LinuxSandbox::assembled(sandbox.clone(), backend, None, None).ends_every_session()
    };
    let bwrap = PathBuf::from("/usr/bin/bwrap");

    assert!(!ends(LinuxBackend::Landlock { helper: helper() }));
    assert!(ends(LinuxBackend::Nested {
        bwrap: bwrap.clone(),
        helper: helper(),
    }));
    assert!(ends(LinuxBackend::Bubblewrap {
        bwrap,
        helper: helper(),
    }));
}

fn supervised_launch(sandbox: ProjectSandbox) -> LinuxSandbox {
    LinuxSandbox::assembled(
        sandbox,
        LinuxBackend::Landlock { helper: helper() },
        Some(Supervision { signals: false }),
        None,
    )
}

fn flag_values(words: &[OsString], flag: &str) -> Vec<PathBuf> {
    words
        .windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| PathBuf::from(&pair[1]))
        .collect()
}

#[test]
fn a_supervised_linked_worktree_launches_with_every_guarded_entry_held() {
    let (_temp, project, sandbox, worktree) = linked();
    let confined = sandbox.confine_to_worktree(&worktree).unwrap();
    let mut guarded = Grants::of(&confined).unwrap().guarded.trees;
    guarded.sort();
    let launch = supervised_launch(confined);
    let mut harness = Command::new("/bin/true");
    harness.current_dir(&worktree);
    let wrapped = launch.wrap(&harness);
    assert!(wrapped.is_ok(), "{:?}", wrapped.as_ref().err());
    let words: Vec<OsString> = wrapped
        .unwrap()
        .get_args()
        .map(OsStr::to_os_string)
        .collect();
    let mut held = flag_values(&words, GUARD_HELD_TREE);
    held.extend(flag_values(&words, GUARD_HELD));
    held.sort();
    assert_eq!(
        held, guarded,
        "the supervisor holds the list the tripwire reads"
    );
    assert_eq!(flag_values(&words, OWN_TREE), vec![worktree.clone()]);
    assert_eq!(flag_values(&words, OWN_GIT), vec![project.join(".git")]);
}

#[test]
fn a_linked_worktree_agent_makes_entries_in_its_admin_folder_and_no_other_git() {
    let (_temp, project, sandbox, worktree) = linked();
    let confined = sandbox.confine_to_worktree(&worktree).unwrap();
    let own = git_own(&confined);
    let admin = project.join(".git").join("worktrees").join("agent-1");
    for made in [
        admin.join("index.lock"),
        admin.join("logs").join("HEAD"),
        worktree.join("src").join("main.rs"),
    ] {
        assert!(own.allows(&made), "{}", made.display());
    }
    for refused in [
        worktree.join("vendor").join(".git"),
        admin.join("modules").join("sub").join(".git"),
        project.join("other").join(".git").join("config"),
    ] {
        assert!(!own.allows(&refused), "{}", refused.display());
    }
}

#[test]
fn a_supervised_clone_launches_with_its_store_held() {
    let (_temp, project, sandbox, _) = linked();
    let clone = project.join("worktrees").join("agent-2");
    fs::create_dir(&clone).unwrap();
    git(&clone, &["init", "-q"]);
    let confined = sandbox.confine_to_clone(&clone).unwrap();
    let guarded = Grants::of(&confined).unwrap().guarded;
    let launch = supervised_launch(confined);
    let mut harness = Command::new("/bin/true");
    harness.current_dir(&clone);
    let wrapped = launch.wrap(&harness);
    assert!(wrapped.is_ok(), "{:?}", wrapped.as_ref().err());
    let words: Vec<OsString> = wrapped
        .unwrap()
        .get_args()
        .map(OsStr::to_os_string)
        .collect();
    assert_eq!(flag_values(&words, GUARD_HELD), guarded.entries);
    assert_eq!(flag_values(&words, GUARD_HELD_TREE), guarded.trees);
    assert_eq!(flag_values(&words, GUARD_HELD_STORE), guarded.stores);
    assert!(
        flag_values(&words, OWN_TREE).contains(&clone),
        "the clone's own top-level .git is the agent's"
    );
    let parsed = match HelperRequest::parse(words.clone()) {
        Ok(HelperRequest::Run { policy, .. }) => policy.guard.held,
        other => panic!("the wrapped words do not read back: {other:?}"),
    };
    assert_eq!(
        parsed.as_ref(),
        Some(&guarded),
        "every held place reads back"
    );
    let objects = clone.join(".git").join("objects");
    assert_eq!(guarded.stores, vec![objects.clone()]);
    assert!(guarded.entries.contains(&objects.join("ff")));
    let unsupervised = launch.with_supervision(None);
    let refused = unsupervised.wrap(&harness).err();
    assert!(
        matches!(&refused, Some(SandboxError::Unavailable(reason))
            if reason == CONFINED_UNAVAILABLE),
        "an unsupervised clone holds nothing: {refused:?}"
    );
}

#[test]
fn a_clone_writes_its_whole_clone_and_has_no_git_folder_grant() {
    let (_temp, project, sandbox, _) = linked();
    let clone = project.join("worktrees").join("agent-2");
    fs::create_dir(&clone).unwrap();
    git(&clone, &["init", "-q"]);
    let confined = sandbox.confine_to_clone(&clone).unwrap();
    let grants = Grants::of(&confined).unwrap();
    assert!(grants.write.contains(&clone));
    assert!(grants.git_write.is_empty());
    let git_folder = clone.join(".git");
    let objects = git_folder.join("objects");
    let (info, pack) = (objects.join("info"), objects.join("pack"));
    let mut entries = vec![
        git_folder.clone(),
        git_folder.join("commondir"),
        objects.clone(),
        info.clone(),
        info.join("alternates"),
        info.join("http-alternates"),
        pack.clone(),
        pack.join("multi-pack-index"),
    ];
    entries.extend((0..=u8::MAX).map(|byte| objects.join(format!("{byte:02x}"))));
    entries.sort();
    let mut trees = vec![clone.join(".git.git"), pack.join("multi-pack-index.d")];
    trees.sort();
    let mut guarded = grants.guarded.clone();
    guarded.entries.sort();
    guarded.trees.sort();
    assert_eq!(guarded.entries, entries, "each store name macOS keeps");
    assert_eq!(guarded.trees, trees);
    assert_eq!(
        guarded.stores,
        vec![objects],
        "no symlink or folder put in the store"
    );
    let git = project.join(".git");
    assert!(grants.write.iter().all(|path| !path.starts_with(&git)));
}

#[test]
fn a_clone_on_the_bubblewrap_backend_is_refused_with_the_landlock_line() {
    let (_temp, project, sandbox, _) = linked();
    let clone = project.join("worktrees").join("agent-2");
    fs::create_dir(&clone).unwrap();
    git(&clone, &["init", "-q"]);
    let launch = LinuxSandbox::assembled(
        sandbox.confine_to_clone(&clone).unwrap(),
        LinuxBackend::Bubblewrap {
            bwrap: PathBuf::from(BWRAP_PATHS[0]),
            helper: helper(),
        },
        None,
        None,
    );
    let mut harness = Command::new("/bin/true");
    harness.current_dir(&clone);
    let refused = launch.wrap(&harness).err();
    assert!(
        matches!(&refused, Some(SandboxError::Unavailable(reason))
            if reason == CONFINED_NEEDS_LANDLOCK),
        "{refused:?}"
    );
}

#[test]
fn an_open_network_policy_keeps_the_owner_model_and_proxy_ports_free() {
    let (_temp, sandbox) = project(SandboxNetwork::Open);
    let sandbox = sandbox.keep_ports(&[8080, 11434, 1234]).unwrap();
    let grants = Grants {
        read: Vec::new(),
        write: Vec::new(),
        git_write: Vec::new(),
        guarded: Held::default(),
        hidden: Vec::new(),
    };
    let policy = landlock_policy(&grants, &sandbox);
    assert!(!policy.limit_network);
    let mut expected = vec![1234, 1455, 8080, 11434];
    expected.extend(PROXY_PORTS);
    expected.extend(crate::ports::RELAY_PORTS);
    assert_eq!(policy.keep_free, expected);
    assert_eq!(policy.keep_free.len(), 4 + 256);
    assert_eq!(
        HelperRequest::parse(
            policy
                .args()
                .into_iter()
                .chain(["--".into(), "/bin/true".into()])
                .collect()
        ),
        Ok(HelperRequest::Run {
            policy: Box::new(policy),
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        })
    );
}

#[test]
fn an_open_network_is_refused_when_linux_could_hand_out_a_kept_port() {
    let (_temp, sandbox) = project(SandboxNetwork::Open);
    let usual = PICKED_PORTS_START..=60_999;
    let kept = kept_free(&sandbox.clone().keep_ports(&[8080, 18_080]).unwrap());
    assert_eq!(
        kept_outside_picked(&kept, Some(&usual)),
        Ok(()),
        "the owner ports and the whole proxy block lie below the ports Linux hands out"
    );
    let inside = kept_free(&sandbox.clone().keep_ports(&[40_000]).unwrap());
    let reason = kept_outside_picked(&inside, Some(&usual)).unwrap_err();
    assert!(reason.starts_with("port 40000 is kept free"), "{reason}");
    assert!(reason.contains("ports 32768 to 60999"), "{reason}");
    let moved = kept_outside_picked(&kept, Some(&(1024..=65_535))).unwrap_err();
    assert!(moved.starts_with("port 1455 is kept free"), "{moved}");
    assert_eq!(
        kept_outside_picked(&kept, None),
        Err(PICKED_PORTS_UNREADABLE.to_owned()),
        "a range that cannot be read starts no open-network agent"
    );
    assert!(kept_outside_picked(&[], None).is_err());
}

#[test]
fn a_kept_port_inside_this_machines_picked_ports_refuses_every_landlock_launch_only() {
    let picked = read_picked_ports().expect("Linux test machines show their port range");
    let inside = (*picked.start()).max(1);
    let helper = std::env::current_exe().unwrap();
    let landlock = || LinuxBackend::Landlock {
        helper: helper.clone(),
    };
    let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
    let closed = sandbox.keep_ports(&[inside]).unwrap();
    let bubblewrap = LinuxBackend::Bubblewrap {
        bwrap: helper.clone(),
        helper: helper.clone(),
    };
    assert!(
        LinuxSandbox::new(closed, bubblewrap).is_ok(),
        "bubblewrap starts a closed agent in a network of its own"
    );
    for network in [
        SandboxNetwork::Open,
        SandboxNetwork::Only(Vec::new()),
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
    ] {
        let (_kept, sandbox) = project(network.clone());
        let sandbox = sandbox.keep_ports(&[inside]).unwrap();
        match LinuxSandbox::new(sandbox, landlock()) {
            Err(SandboxError::Unavailable(reason)) => {
                assert!(reason.contains(&format!("port {inside} ")), "{reason}");
            }
            other => panic!(
                "a kept port Linux could pick must refuse the launch on {network:?}: {other:?}"
            ),
        }
    }
}

#[test]
fn a_limited_network_policy_names_the_kept_ports_for_the_helper_to_check() {
    let (_temp, sandbox) = project(SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]));
    let sandbox = sandbox.keep_ports(&[8080]).unwrap();
    let grants = Grants {
        read: Vec::new(),
        write: Vec::new(),
        git_write: Vec::new(),
        guarded: Held::default(),
        hidden: Vec::new(),
    };
    let policy = landlock_policy(&grants, &sandbox);
    assert!(policy.limit_network);
    let mut expected = vec![1455, 8080, 11434];
    expected.extend(PROXY_PORTS);
    expected.extend(crate::ports::RELAY_PORTS);
    assert_eq!(policy.keep_free, expected);
    assert_eq!(
        HelperRequest::parse(
            policy
                .args()
                .into_iter()
                .chain(["--".into(), "/bin/true".into()])
                .collect()
        ),
        Ok(HelperRequest::Run {
            policy: Box::new(policy),
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        })
    );
}

#[test]
fn bubblewrap_starts_no_open_network_agent_since_it_keeps_no_port_free() {
    let bwrap = std::env::current_exe().unwrap();
    let backend = || LinuxBackend::Bubblewrap {
        bwrap: bwrap.clone(),
        helper: bwrap.clone(),
    };
    let (_open, sandbox) = project(SandboxNetwork::Open);
    match LinuxSandbox::new(sandbox, backend()) {
        Err(SandboxError::Unavailable(reason)) => {
            assert_eq!(reason, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK);
        }
        other => panic!("an open network on bubblewrap must be refused: {other:?}"),
    }
    let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
    assert!(LinuxSandbox::new(sandbox, backend()).is_ok());
}

/// The refusal a detection gives, for the rows that expect one. Input: the detection.
/// Output: its reason; a sandbox or another error fails the row.
fn refusal(detected: Result<LinuxSandbox, SandboxError>) -> String {
    match detected {
        Err(SandboxError::Unavailable(reason)) => reason,
        other => panic!("a refusal was expected: {other:?}"),
    }
}

/// V243-LINUX-KEEP-FREE-REFUSAL-NAME: a probe that says the kernel cannot hold the
/// boundary keeps the kernel refusals; any other failure is named, its first line cut to
/// 200 bytes with no control characters. A network with ports never widens to open; a
/// closed network keeps its bubblewrap fallback, and without bubblewrap names the reason.
#[test]
fn a_failed_probe_names_its_own_reason_and_only_an_enforcement_answer_names_the_kernel() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let bwrap = std::env::current_exe().unwrap();
    let fake = |name: &str, said: &str| {
        let path = root.join(name);
        fs::write(&path, format!("#!/bin/sh\nprintf '{said}' >&2\nexit 3\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let detected = |helper: &Path, network: SandboxNetwork| {
        let (_folders, sandbox) = project(network);
        LinuxSandbox::detect_among(sandbox, helper, &[bwrap.as_path()])
    };
    let ports = || SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]);
    let falls_back = |helper: &Path| {
        let closed = detected(helper, SandboxNetwork::Only(Vec::new()));
        matches!(
            closed.as_ref().map(LinuxSandbox::backend),
            Ok(LinuxBackend::Bubblewrap { .. })
        )
    };
    let no_bubblewrap = |helper: &Path| {
        let (_folders, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        refusal(LinuxSandbox::detect_among(sandbox, helper, &[]))
    };
    for status in ["PartiallyEnforced", "NotEnforced"] {
        let said =
            format!("crazyagent-landlock: this kernel cannot hold the boundary ({status})\\n");
        let helper = fake(status, &said);
        assert_eq!(
            refusal(detected(&helper, SandboxNetwork::Open)),
            LINUX_OPEN_NETWORK_NEEDS_LANDLOCK
        );
        let limited = refusal(detected(&helper, ports()));
        assert!(
            limited.starts_with("limiting an agent's network to a few ports needs Landlock"),
            "{limited}"
        );
        assert!(falls_back(&helper), "{status}");
        let pinned = no_bubblewrap(&helper);
        assert!(
            pinned.starts_with("this Linux cannot fully enforce Landlock file rules"),
            "{pinned}"
        );
    }
    let other = fake(
        "other",
        "crazyagent-landlock: seccomp\\007 filter refused\\nsecond line\\n",
    );
    let named = "the sandbox check failed: crazyagent-landlock: seccomp filter refused";
    assert_eq!(refusal(detected(&other, SandboxNetwork::Open)), named);
    assert_eq!(refusal(detected(&other, ports())), named);
    assert!(falls_back(&other));
    assert_eq!(no_bubblewrap(&other), named);
    let silent = fake("silent", "");
    assert_eq!(
        refusal(detected(&silent, SandboxNetwork::Open)),
        "the sandbox check failed: exit status: 3"
    );
    let wide = fake("wide", &"é".repeat(150));
    let cut = refusal(detected(&wide, SandboxNetwork::Open));
    let reason = cut.strip_prefix("the sandbox check failed: ").unwrap();
    assert!(
        reason.len() <= 200 && reason.len() > 190,
        "{}",
        reason.len()
    );
    assert!(reason.chars().all(|letter| letter == 'é'));
}

#[test]
fn a_nested_agent_starts_through_the_helper_then_bubblewrap_then_the_helper_again() {
    let (_temp, sandbox) = project(SandboxNetwork::Open);
    let cwd = sandbox.project_root().to_path_buf();
    let helper = std::env::current_exe().unwrap();
    let nested = LinuxSandbox::new(
        sandbox,
        LinuxBackend::Nested {
            bwrap: helper.clone(),
            helper: helper.clone(),
        },
    )
    .unwrap();
    let mut harness = Command::new("/bin/sh");
    harness.args(["-c", "true"]).current_dir(&cwd);
    let command = nested.wrap(&harness).unwrap();
    assert_eq!(command.get_program(), helper.as_os_str());
    let args: Vec<&OsStr> = command.get_args().collect();
    let mut head = vec![OsStr::new(NEST), OsStr::new("--"), helper.as_os_str()];
    head.extend(NEST_ARGS.map(OsStr::new));
    head.extend([
        OsStr::new("--chdir"),
        cwd.as_os_str(),
        OsStr::new("--"),
        helper.as_os_str(),
    ]);
    assert_eq!(args.get(..head.len()), Some(&head[..]), "{args:?}");
    let [policy @ .., split, program, flag, script] = &args[head.len()..] else {
        panic!("{args:?}");
    };
    assert_eq!(*split, "--", "{args:?}");
    assert_eq!(*program, "/bin/sh", "{args:?}");
    assert_eq!(*flag, "-c", "{args:?}");
    assert_eq!(*script, "true", "{args:?}");
    let grants = Grants::of(nested.sandbox()).unwrap();
    let mut expected = landlock_policy(&grants, nested.sandbox());
    expected.nested = true;
    let written: Vec<OsString> = policy.iter().map(|arg| arg.to_os_string()).collect();
    assert_eq!(written, expected.args(), "{args:?}");
    assert!(written.contains(&OsString::from(NESTED)), "{args:?}");
    for absent in ["--new-session", "--unshare-net"] {
        assert!(!args.contains(&OsStr::new(absent)), "{absent}: {args:?}");
    }
    assert_eq!(args.iter().filter(|arg| **arg == NEST).count(), 1);
}

#[test]
fn only_a_loopback_only_tcp_only_agent_with_ports_from_1024_up_is_bridged() {
    use SandboxRemote::{AnyHost, Loopback};
    let bridged = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
        let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
        sandbox.limit_sockets(sockets).bridged_ports()
    };
    let default = LinuxSockets::TcpOnly;
    assert_eq!(
        bridged(&[Loopback(11434), Loopback(8080)], default),
        Some(vec![8080, 11434])
    );
    assert_eq!(bridged(&[Loopback(1024)], default), Some(vec![1024]));
    assert_eq!(bridged(&[Loopback(8080), AnyHost(443)], default), None);
    assert_eq!(bridged(&[Loopback(8080), Loopback(1023)], default), None);
    assert_eq!(bridged(&[], default), None);
    assert_eq!(bridged(&[Loopback(8080)], LinuxSockets::TcpPorts), None);
    let (_temp, open) = project(SandboxNetwork::Open);
    assert_eq!(open.bridged_ports(), None);
}

#[test]
fn only_a_tcp_only_agent_with_a_port_on_any_host_and_loopback_ports_from_1024_up_is_routed() {
    use SandboxRemote::{AnyHost, Loopback};
    let routed = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
        let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
        let sandbox = sandbox.limit_sockets(sockets);
        let routed = sandbox.routed();
        assert!(routed.is_none() || sandbox.bridged_ports().is_none());
        routed.map(|routed| (routed.any, routed.bridged))
    };
    let default = LinuxSockets::TcpOnly;
    assert_eq!(routed(&[AnyHost(443)], default), Some((vec![443], vec![])));
    assert_eq!(
        routed(&[Loopback(18080), AnyHost(8443), AnyHost(443)], default),
        Some((vec![443, 8443], vec![18080]))
    );
    assert_eq!(
        routed(&[AnyHost(443), Loopback(1024)], default),
        Some((vec![443], vec![1024]))
    );
    assert_eq!(routed(&[AnyHost(443), Loopback(1023)], default), None);
    assert_eq!(routed(&[AnyHost(443), Loopback(53)], default), None);
    assert_eq!(
        routed(&[Loopback(8080)], default),
        None,
        "the bridge's case"
    );
    assert_eq!(routed(&[], default), None);
    assert_eq!(routed(&[AnyHost(443)], LinuxSockets::TcpPorts), None);
    assert_eq!(routed(&[AnyHost(443)], LinuxSockets::Refuse), None);
    let (_temp, open) = project(SandboxNetwork::Open);
    assert_eq!(open.routed(), None);
}

#[test]
fn a_routed_agent_starts_with_its_route_flags_first_and_a_network_of_its_own() {
    use bridge::{BRIDGE, UNSHARE_NET};
    let (_temp, sandbox) = project(SandboxNetwork::Only(vec![
        SandboxRemote::AnyHost(443),
        SandboxRemote::Loopback(18080),
    ]));
    let cwd = sandbox.project_root().to_path_buf();
    let grants = Grants::of(&sandbox).unwrap();
    let policy = LandlockPolicy {
        nested: true,
        ..landlock_policy(&grants, &sandbox)
    };
    let bwrap = Path::new(BWRAP_PATHS[0]);
    let helper = helper();
    let words = |bridged: &[u16], routes: &[u16]| -> Vec<OsString> {
        nest_command(bwrap, &helper, &policy, &[], &cwd, bridged, routes)
            .get_args()
            .map(OsStr::to_os_string)
            .collect()
    };
    let expected = |head: &[&str]| -> Vec<OsString> {
        let mut expected: Vec<OsString> = head.iter().map(OsString::from).collect();
        expected.extend([NEST, "--"].map(OsString::from));
        expected.push(bwrap.into());
        expected.extend(NEST_ARGS.map(OsString::from));
        expected.extend([UNSHARE_NET, "--chdir"].map(OsString::from));
        expected.push(cwd.clone().into());
        expected.push("--".into());
        expected.push(helper.clone().into());
        expected.extend(policy.args());
        expected
    };
    let both = [egress::ROUTE, "443", BRIDGE, "18080"];
    assert_eq!(words(&[18080], &[443]), expected(&both));
    let routes = [egress::ROUTE, "443", egress::ROUTE, "8443"];
    assert_eq!(words(&[], &[443, 8443]), expected(&routes));
    for routed in [words(&[18080], &[443]), words(&[], &[443])] {
        for flag in egress::DESCRIPTOR_FLAGS {
            assert!(
                !routed.iter().any(|word| word == flag),
                "{flag}: {routed:?}"
            );
        }
    }
}

#[test]
fn a_set_user_or_set_group_id_bubblewrap_is_never_nested_under() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let moded = |name: &str, mode: u32| {
        let path = root.join(name);
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    };
    let plain = moded("plain", 0o755);
    let marked = [moded("setuid", 0o4755), moded("setgid", 0o2755)];
    let agreeing = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|folder| folder.join("true"))
        .find(|path| path.is_file())
        .unwrap();
    let probe = [OsString::from(PROBE)];
    assert!(nests(&plain, &agreeing, &probe), "the control");
    assert!(trusted(&plain));
    assert!(trusted(Path::new("/bin/sh")), "a program root owns");
    assert!(!trusted(&root.join("absent")));
    assert!(!trusted(&root), "a folder");
    for bwrap in &marked {
        assert!(!trusted(bwrap), "{}", bwrap.display());
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let backend = LinuxBackend::Nested {
            bwrap: bwrap.clone(),
            helper: plain.clone(),
        };
        let refused = LinuxSandbox::new(sandbox, backend).is_err();
        assert!(refused, "{}", bwrap.display());
        assert!(!nests(bwrap, &agreeing, &probe), "{}", bwrap.display());
    }
}

/// rv-sa241 SBX-2-R2 LOW-1: where this computer's nest check passes, a launch whose own
/// nest probe fails is refused with words to try again or restart, instead of running on
/// the shared network with Landlock rules only while the Agent access page says it is
/// nested; where the nest check fails too, the launch is a Landlock one, as the page says.
#[test]
fn a_launch_whose_own_nest_probe_fails_where_this_computer_nests_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let place = temp.path().canonicalize().unwrap();
    let bwrap = place.join("bwrap");
    fs::write(&bwrap, b"").unwrap();
    fs::set_permissions(&bwrap, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(trusted(&bwrap));
    let fake = |name: &str, failing: &str| {
        let helper = place.join(name);
        let script = format!(
            "#!/bin/sh\nnest=0\nfailing=0\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    \
             {NEST}) nest=1 ;;\n    {failing}) failing=1 ;;\n  esac\ndone\n\
             [ \"$nest\" = 1 ] && [ \"$failing\" = 1 ] && exit 1\nexit 0\n"
        );
        fs::write(&helper, script).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        helper
    };
    let flagged = fake("flagged", KEEP_FREE);
    let neither = fake("neither", PROBE);
    assert!(
        nests(&bwrap, &flagged, &[PROBE.into()]),
        "this computer nests"
    );
    let (_project, sandbox) = project(SandboxNetwork::Open);
    let answer = LinuxSandbox::detect_among(sandbox.clone(), &flagged, &[bwrap.as_path()]);
    let shown = match &answer {
        Ok(sandbox) => format!("{:?}", sandbox.backend()),
        Err(error) => error.to_string(),
    };
    assert!(
        matches!(
            &answer,
            Err(SandboxError::Unavailable(reason)) if reason.contains("restart CrazyAgent")
        ),
        "a launch that cannot nest where this computer nests is refused, never run on \
         Landlock alone: {shown}"
    );
    assert!(
        !nests(&bwrap, &neither, &[PROBE.into()]),
        "this computer does not nest"
    );
    let landlock = LinuxSandbox::detect_among(sandbox, &neither, &[bwrap.as_path()]).unwrap();
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "where nothing nests the launch is a Landlock one, as the page says"
    );
}

#[test]
fn an_agent_kept_on_this_network_by_a_low_port_names_the_lowest_one() {
    use SandboxRemote::{AnyHost, Loopback};
    let low = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
        let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
        let sandbox = sandbox.limit_sockets(sockets);
        assert!(sandbox.unbridged_port().is_none() || sandbox.bridged_ports().is_none());
        sandbox.unbridged_port()
    };
    let default = LinuxSockets::TcpOnly;
    assert_eq!(
        low(&[Loopback(8080), Loopback(443), Loopback(80)], default),
        Some(80)
    );
    assert_eq!(low(&[Loopback(1023)], default), Some(1023));
    assert_eq!(low(&[Loopback(1024)], default), None, "bridged");
    assert_eq!(
        low(&[Loopback(80), AnyHost(443)], default),
        None,
        "a host port"
    );
    assert_eq!(low(&[], default), None);
    assert_eq!(
        low(&[Loopback(80)], LinuxSockets::TcpPorts),
        None,
        "the setting"
    );
    let (_temp, open) = project(SandboxNetwork::Open);
    assert_eq!(open.unbridged_port(), None);
}

#[test]
fn a_launch_left_on_this_network_by_a_low_port_says_so_with_the_port() {
    use SandboxRemote::{AnyHost, Loopback};
    use crate::policy::AgentNetwork;
    let host = |nested| {
        Some(LinuxHost {
            nested,
            scoped: false,
            supervised: false,
            root: false,
            armored: false,
            routed: false,
        })
    };
    let launched = |remotes: &[SandboxRemote]| {
        let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
        sandbox
    };
    let said = |nested, sandbox: &ProjectSandbox| {
        linux_unbridged_notice_on(host(nested), AgentNetwork::None, sandbox)
    };
    let low = launched(&[Loopback(11434), Loopback(80)]);
    assert_eq!(said(true, &low), Some(linux_unbridged_line(80)));
    assert!(said(true, &low).unwrap().contains("port 80 is below 1024"));
    assert_eq!(said(false, &low), None, "no nest");
    assert_eq!(
        linux_unbridged_notice_on(None, AgentNetwork::None, &low),
        None
    );
    assert_eq!(said(true, &launched(&[Loopback(11434)])), None, "bridged");
    let (_temp, open) = project(SandboxNetwork::Open);
    assert_eq!(said(true, &open), None);
    let plain = Some(LINUX_PORTS_ANY_HOST_LINE.to_owned());
    let unrouted = Some(LINUX_UNROUTED_LINE.to_owned());
    let hosted = launched(&[AnyHost(443), Loopback(8080)]);
    assert_eq!(
        said(true, &hosted),
        unrouted,
        "a model on a host, tools off"
    );
    assert_eq!(
        said(false, &hosted),
        None,
        "the page says it without a nest"
    );
    let ports = launched(&[Loopback(8080)]).limit_sockets(LinuxSockets::TcpPorts);
    assert_eq!(said(true, &ports), plain);
    let any_host = Some(LINUX_UNROUTED_ANY_HOST_LINE.to_owned());
    assert_eq!(
        said(true, &launched(&[AnyHost(443)])),
        any_host,
        "unrouted, any host"
    );
    assert_eq!(said(true, &launched(&[])), None);
    for choice in [AgentNetwork::Https, AgentNetwork::Open] {
        let said = |sandbox| linux_unbridged_notice_on(host(true), choice, sandbox);
        assert_eq!(said(&hosted), unrouted, "{choice:?}: no network of its own");
        assert_eq!(said(&ports), None, "{choice:?}: the page says it");
        assert_eq!(said(&low), Some(linux_unbridged_line(80)), "{choice:?}");
    }
}

#[test]
fn every_unrouted_launch_that_could_route_says_why_and_a_low_port_names_itself() {
    use SandboxRemote::{AnyHost, Loopback};
    use crate::policy::AgentNetwork;
    let host = |routed, supervised| {
        Some(LinuxHost {
            nested: true,
            scoped: false,
            supervised,
            root: false,
            armored: false,
            routed,
        })
    };
    let launched = |remotes: &[SandboxRemote]| {
        let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
        sandbox
    };
    let said = |held, sandbox: &ProjectSandbox| {
        linux_unbridged_notice_on(held, AgentNetwork::Https, sandbox)
    };
    let wide = Some(LINUX_UNROUTED_LINE.to_owned());
    let any_host = Some(LINUX_UNROUTED_ANY_HOST_LINE.to_owned());
    let model = launched(&[AnyHost(443), Loopback(18080)]);
    let remote = launched(&[AnyHost(443)]);
    assert!(model.routed().is_some() && remote.routed().is_some());
    assert_eq!(said(host(false, false), &model), wide);
    assert_eq!(
        said(host(false, true), &model),
        any_host,
        "loopback kept by the supervisor"
    );
    assert_eq!(
        said(host(false, false), &remote),
        any_host,
        "no loopback port"
    );
    assert_eq!(said(host(false, true), &remote), any_host);
    for supervised in [false, true] {
        assert_eq!(said(host(true, supervised), &model), None, "routed");
        assert_eq!(said(host(true, supervised), &remote), None, "routed");
    }
    for line in [LINUX_UNROUTED_LINE, LINUX_UNROUTED_ANY_HOST_LINE] {
        let why = "On this Linux, this agent has no network of its own: ";
        assert!(line.starts_with(why), "{line}");
        assert!(
            line.contains("AppArmor") && line.contains("as root"),
            "{line}"
        );
    }
    assert!(LINUX_UNROUTED_LINE.contains("on every host, this computer included."));
    assert!(LINUX_UNROUTED_ANY_HOST_LINE.contains("on any host include this computer"));
    let low = launched(&[AnyHost(443), Loopback(18080), Loopback(80), Loopback(53)]);
    assert!(low.routed().is_none());
    let named = Some(linux_unbridged_line(53));
    assert_eq!(
        said(host(true, false), &low),
        named,
        "the low port keeps it unrouted"
    );
    assert_eq!(said(host(true, true), &low), named);
    assert_eq!(
        said(host(false, false), &low),
        None,
        "the page says it on this host"
    );
}

#[test]
fn a_launch_that_could_have_a_network_of_its_own_says_why_it_has_none() {
    use SandboxRemote::{AnyHost, Loopback};
    use crate::policy::AgentNetwork;
    let host = |nested, routed| {
        Some(LinuxHost {
            nested,
            scoped: false,
            supervised: false,
            root: false,
            armored: false,
            routed,
        })
    };
    let (_temp, eligible) = project(SandboxNetwork::Only(vec![AnyHost(443), Loopback(18080)]));
    assert!(eligible.routed().is_some());
    let unrouted = Some(LINUX_UNROUTED_LINE.to_owned());
    for choice in AgentNetwork::ALL {
        let said = |held| linux_unbridged_notice_on(held, choice, &eligible);
        assert_eq!(said(host(true, false)), unrouted, "{choice:?}");
        assert_eq!(said(host(true, true)), None, "{choice:?}: routed");
        assert_eq!(said(host(false, false)), None, "{choice:?}: no nest");
    }
    assert!(LINUX_UNROUTED_LINE.contains("has no network of its own: "));
    let ports = eligible.limit_sockets(LinuxSockets::TcpPorts);
    assert!(ports.routed().is_none());
    let said = linux_unbridged_notice_on(host(true, false), AgentNetwork::Https, &ports);
    assert_eq!(said, None, "not routed-eligible, the page says it");
}

fn nested_words(network: SandboxNetwork) -> (PathBuf, PathBuf, Vec<OsString>) {
    let (_temp, sandbox) = project(network);
    let cwd = sandbox.project_root().to_path_buf();
    let helper = std::env::current_exe().unwrap();
    let backend = LinuxBackend::Nested {
        bwrap: helper.clone(),
        helper: helper.clone(),
    };
    let nested = LinuxSandbox::new(sandbox, backend).unwrap();
    let mut harness = Command::new("/bin/sh");
    harness.args(["-c", "true"]).current_dir(&cwd);
    let command = nested.wrap(&harness).unwrap();
    let words = command.get_args().map(OsStr::to_os_string).collect();
    (helper, cwd, words)
}

#[test]
fn a_loopback_only_nested_agent_starts_with_a_network_of_its_own_and_its_ports_bridged() {
    use bridge::{BRIDGE, BRIDGE_FD, UNSHARE_NET};
    let loopback = [
        SandboxRemote::Loopback(11434),
        SandboxRemote::Loopback(8080),
    ];
    let (helper, cwd, words) = nested_words(SandboxNetwork::Only(loopback.to_vec()));
    let mut head: Vec<OsString> = [BRIDGE, "8080", BRIDGE, "11434", NEST, "--"]
        .map(OsString::from)
        .to_vec();
    head.push(helper.clone().into());
    head.extend(NEST_ARGS.map(OsString::from));
    head.extend([UNSHARE_NET, "--chdir"].map(OsString::from));
    head.push(cwd.into());
    head.push("--".into());
    head.push(helper.into());
    assert_eq!(words.get(..head.len()), Some(&head[..]), "{words:?}");
    let inside = &words[head.len()..];
    let written = |word: &OsString| word == BRIDGE || word == BRIDGE_FD;
    assert!(
        !inside.iter().any(written),
        "the nest writes them: {words:?}"
    );
    let (bridge, rest) = Bridge::leading(words.clone()).unwrap();
    let asked = Bridge {
        fd: None,
        ports: vec![8080, 11434],
    };
    assert_eq!(bridge, Some(asked));
    assert_eq!(rest.first(), Some(&OsString::from(NEST)));
    let low = [SandboxRemote::Loopback(8080), SandboxRemote::Loopback(443)];
    for network in [
        SandboxNetwork::Only(low.to_vec()),
        SandboxNetwork::Only(Vec::new()),
        SandboxNetwork::Open,
    ] {
        let (_, _, words) = nested_words(network.clone());
        assert_eq!(words.first(), Some(&OsString::from(NEST)), "{network:?}");
        assert!(!words.iter().any(|word| word == UNSHARE_NET), "{network:?}");
    }
}

#[test]
fn a_nested_agent_may_have_an_open_network_and_keeps_the_kept_port_check() {
    let picked = read_picked_ports().expect("Linux test machines show their port range");
    let inside = (*picked.start()).max(1);
    let helper = std::env::current_exe().unwrap();
    let nested = || LinuxBackend::Nested {
        bwrap: helper.clone(),
        helper: helper.clone(),
    };
    let (_open, sandbox) = project(SandboxNetwork::Open);
    assert!(
        LinuxSandbox::new(sandbox, nested()).is_ok(),
        "a nested agent is a Landlock agent, which keeps ports free on an open network"
    );
    let (_kept, sandbox) = project(SandboxNetwork::Open);
    match LinuxSandbox::new(sandbox.keep_ports(&[inside]).unwrap(), nested()) {
        Err(SandboxError::Unavailable(reason)) => {
            assert!(reason.contains(&format!("port {inside} ")), "{reason}");
        }
        other => panic!("a kept port Linux could pick must refuse the nest: {other:?}"),
    }
}

/// A launch whose own probe fails with the setting On runs without the supervisor and
/// marks the kept host as running none, so the next Agent access page read shows the
/// wider lines; nothing kept stays nothing (rv-secreview linux-t4c F2).
#[test]
fn a_launch_whose_probe_fails_marks_the_kept_host_unsupervised() {
    let host = LinuxHost {
        nested: true,
        scoped: true,
        supervised: true,
        root: false,
        armored: false,
        routed: false,
    };
    let at = Instant::now();
    let kept: KeptHost = Mutex::new((0, Some((at, host))));
    forget_supervisor(&kept);
    let marked = LinuxHost {
        supervised: false,
        ..host
    };
    assert_eq!(kept.lock().unwrap().1, Some((at, marked)));
    let nothing: KeptHost = Mutex::new((0, None));
    forget_supervisor(&nothing);
    assert_eq!(nothing.lock().unwrap().1, None);
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().canonicalize().unwrap();
    let (_project, sandbox) = project(SandboxNetwork::Only(Vec::new()));
    let failing = LinuxBackend::Landlock {
        helper: PathBuf::from("/bin/false"),
    };
    let launch = LinuxSandbox::new(sandbox, failing).unwrap();
    let unsupervised = launch.supervise(LinuxSupervisor::On, Some(data.as_path()));
    assert_eq!(unsupervised.supervision(), None);
    assert_eq!(fs::read_dir(&data).unwrap().count(), 0);
}

/// A supervised launch guards the loopback ports of a limited network under TCP only,
/// but a port it may also reach on any host or look names up on, always its own git
/// folders, and signals only where the host holds nothing else (DEC-179 T4c).
#[test]
fn a_supervised_launch_carries_each_guard_its_policy_and_host_need() {
    use SandboxRemote::{AnyHost, Loopback};
    let remotes = vec![Loopback(8080), Loopback(443), AnyHost(443), Loopback(53)];
    let (_temp, sandbox) = project(SandboxNetwork::Only(remotes));
    assert!(sandbox.resolves_over_tcp());
    let own = GitOwn {
        trees: vec![sandbox.project_root().to_path_buf()],
        ..GitOwn::default()
    };
    let alone = Supervision { signals: true };
    let expected = Guard {
        loopback: vec![8080],
        git: Some(own),
        signals: true,
        held: None,
    };
    assert_eq!(guard(&sandbox, alone), expected);
    let nested = Guard {
        signals: false,
        ..expected.clone()
    };
    assert_eq!(guard(&sandbox, Supervision { signals: false }), nested);
    let ports = sandbox.clone().limit_sockets(LinuxSockets::TcpPorts);
    let any_host = Guard {
        loopback: Vec::new(),
        ..expected
    };
    assert_eq!(guard(&ports, alone), any_host);
    let (_open, open) = project(SandboxNetwork::Open);
    let held = guard(&open, alone);
    assert!(held.loopback.is_empty(), "{held:?}");
    assert!(
        held.git
            .is_some_and(|git| git.trees == [open.project_root()])
    );
}

/// The git folders a supervised agent owns are the ones the macOS boundary leaves it:
/// the top of each tree it works in and, for the engine's worktree step, each folder in
/// the two worktree folders (DEC-179 T4c, RV-DS241-85b).
#[test]
fn a_supervised_agent_owns_the_git_of_each_tree_it_works_in_and_of_each_engine_worktree() {
    let (temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
    let project = sandbox.project_root().to_path_buf();
    let alone = GitOwn {
        trees: vec![project.clone()],
        ..GitOwn::default()
    };
    assert_eq!(git_own(&sandbox), alone);
    let lease = temp.path().canonicalize().unwrap().join("lease");
    fs::create_dir(&lease).unwrap();
    let worked = sandbox.allow_worktree(&lease).unwrap().adding_worktrees();
    let state = project.join(kernel_common::names::CRAZYAGENT_DIRECTORY);
    let expected = GitOwn {
        trees: vec![project.clone(), lease],
        kids: vec![project.join("worktrees"), state.join("worktrees")],
        exact: Vec::new(),
    };
    assert_eq!(git_own(&worked), expected);
}

/// The helper gets guard flags only for a launch the call supervisor holds, and a
/// nested one carries no signal guard: its process namespace holds signals.
#[test]
fn only_a_supervised_launch_hands_the_helper_its_guards() {
    let (_temp, sandbox) = project(SandboxNetwork::Open);
    let cwd = sandbox.project_root().to_path_buf();
    let helper = std::env::current_exe().unwrap();
    let backend = LinuxBackend::Nested {
        bwrap: helper.clone(),
        helper,
    };
    let plain = LinuxSandbox::new(sandbox, backend).unwrap();
    let mut harness = Command::new("/bin/sh");
    harness.args(["-c", "true"]).current_dir(&cwd);
    let words = |sandbox: &LinuxSandbox| -> Vec<OsString> {
        sandbox
            .wrap(&harness)
            .unwrap()
            .get_args()
            .map(OsStr::to_os_string)
            .collect()
    };
    assert!(!words(&plain).contains(&GUARD_GIT.into()));
    let supervised = plain.with_supervision(Some(Supervision { signals: false }));
    let held = words(&supervised);
    let git = [GUARD_GIT, OWN_TREE].map(OsString::from);
    let at = held
        .windows(2)
        .position(|pair| pair == git)
        .expect("the git guard");
    assert_eq!(held[at + 2], cwd.as_os_str());
    assert!(!held.contains(&GUARD_SIGNALS.into()));
    assert!(!held.contains(&GUARD_LOOPBACK.into()));
}

#[test]
fn no_agent_is_granted_the_git_state_the_owners_git_reads_and_runs() {
    let (_temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
    let git = sandbox.project_root().join(".git");
    let trees = [
        "rebase-merge",
        "rebase-apply",
        "remotes",
        "branches",
        "sequencer",
        "refs",
    ];
    for name in trees {
        fs::create_dir_all(git.join(name)).unwrap();
    }
    for name in ["index", "index.lock", "HEAD"] {
        fs::write(git.join(name), "state\n").unwrap();
    }
    let grants = Grants::of(&sandbox).unwrap();
    for name in [
        "index",
        "index.lock",
        "rebase-merge",
        "rebase-apply",
        "remotes",
        "branches",
        "sequencer",
    ] {
        assert!(
            !grants.write.contains(&git.join(name)),
            "{name} is writable"
        );
    }
    for name in ["HEAD", "refs"] {
        assert!(
            grants.write.contains(&git.join(name)),
            "{name} is not writable"
        );
    }
}

mod helper_paths;
