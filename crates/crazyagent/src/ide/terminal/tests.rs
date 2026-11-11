use super::*;
#[cfg(unix)]
use crate::protocol::ProjectId;
#[cfg(unix)]
use crate::sandbox::ide_shell;
use crate::sandbox::policy::{AgentAccess, AgentNetwork, AgentPolicy};
#[cfg(target_os = "macos")]
use crate::sandbox::policy::{AgentFilesRule, IdeAgentFiles};
use serde_json::json;
#[cfg(unix)]
use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(unix)]
const PROJECT: &str = "alpha";

#[cfg(unix)]
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

/// Runs git outside every sandbox, as the row's own setup. Inputs: the folder and git's
/// arguments. Output: none; a git that fails fails the row.
#[cfg(target_os = "macos")]
fn setup_git(folder: &std::path::Path, args: &[&str]) {
    let mut git = std::process::Command::new(crate::research::runner::setup_git());
    git.args(args)
        .current_dir(folder)
        .env_clear()
        .env("HOME", folder)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    assert!(
        crate::spawn_gate::output(&mut git)
            .unwrap()
            .status
            .success()
    );
}

/// Runs a script in a Sandboxed terminal of the project. Inputs: the case and the script.
/// Output: the finished process.
#[cfg(target_os = "macos")]
fn in_terminal(case: &Case, script: &str) -> std::process::Output {
    in_terminal_at(case, script, AgentAccess::Sandboxed, AgentNetwork::Open)
}

/// Runs a script in a terminal of the project through the real launch and the real
/// `sandbox-exec`. Inputs: the case, the script, the level and the network choice. Output:
/// the finished process.
#[cfg(target_os = "macos")]
fn in_terminal_at(
    case: &Case,
    script: &str,
    level: AgentAccess,
    network: AgentNetwork,
) -> std::process::Output {
    in_terminal_holding(case, script, level, network, &[], AgentFilesRule::default())
}

/// Runs a script in a terminal of the project that keeps `kept_ports`, through the real launch
/// and the real `sandbox-exec`. Inputs: the case, the script, the level, the network choice and
/// the ports. Output: the finished process.
#[cfg(target_os = "macos")]
fn in_terminal_keeping(
    case: &Case,
    script: &str,
    level: AgentAccess,
    network: AgentNetwork,
    kept_ports: &[u16],
) -> std::process::Output {
    in_terminal_holding(
        case,
        script,
        level,
        network,
        kept_ports,
        AgentFilesRule::default(),
    )
}

/// Runs a script in a terminal of the project that keeps `kept_ports` and holds an agent
/// folders rule, through the real launch and the real `sandbox-exec`. Inputs: the case, the
/// script, the level, the network choice, the ports and the rule. Output: the finished
/// process.
#[cfg(target_os = "macos")]
fn in_terminal_holding(
    case: &Case,
    script: &str,
    level: AgentAccess,
    network: AgentNetwork,
    kept_ports: &[u16],
    agent_files: AgentFilesRule,
) -> std::process::Output {
    let project = ProjectId::new(PROJECT).unwrap();
    let shell_arguments = arguments(&["-c", script]);
    let launch = ide_shell::Launch {
        root: &case.root,
        project: &project,
        folder: &case.folder,
        cwd: &case.folder,
        shell: std::path::Path::new("/bin/sh"),
        arguments: &shell_arguments,
        incoming: &[],
        extra_reads: &[],
        level,
        network,
        kept_ports,
        agent_files,
    };
    let mut command = ide_shell::command(&launch).unwrap();
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::spawn_gate::output(&mut command).unwrap()
}

/// Listens on a free port of this machine for the network rows. Output: the listener, which
/// must stay alive, and its port.
#[cfg(target_os = "macos")]
fn local_listener() -> (std::net::TcpListener, u16) {
    let listener = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// The script that connects to a port of this machine and exits 0 only if it could. Input:
/// the port. Output: the script.
#[cfg(target_os = "macos")]
fn connects(port: u16) -> String {
    format!("/usr/bin/nc -z -G 5 127.0.0.1 {port}")
}

/// An Own branch terminal on the Open choice reaches a listener on this machine.
#[cfg(target_os = "macos")]
#[test]
fn an_own_branch_terminal_on_open_reaches_a_local_listener() {
    let case = Case::new();
    let (_held, port) = local_listener();
    let output = in_terminal_at(
        &case,
        &connects(port),
        AgentAccess::OwnBranch,
        AgentNetwork::Open,
    );
    assert!(output.status.success(), "{output:?}");
}

/// An Own branch terminal on Port 443 only is refused any other port and holds the 443 rule.
#[cfg(target_os = "macos")]
#[test]
fn an_own_branch_terminal_on_port_443_refuses_another_port_and_renders_the_443_rule() {
    let case = Case::new();
    let (_held, port) = local_listener();
    let output = in_terminal_at(
        &case,
        &connects(port),
        AgentAccess::OwnBranch,
        AgentNetwork::Https,
    );
    assert!(!output.status.success(), "{output:?}");
    let profile = case.root.join("ide-shell").join(PROJECT).join("shell.sb");
    let rules = std::fs::read_to_string(profile).unwrap();
    assert!(rules.contains(r#"(remote ip "*:443")"#), "{rules}");
}

/// An Own branch terminal on None has no network.
#[cfg(target_os = "macos")]
#[test]
fn an_own_branch_terminal_on_none_has_no_network() {
    let case = Case::new();
    let (_held, port) = local_listener();
    let output = in_terminal_at(
        &case,
        &connects(port),
        AgentAccess::OwnBranch,
        AgentNetwork::None,
    );
    assert!(!output.status.success(), "{output:?}");
}

/// A Sandboxed terminal has no network, whatever network choice the project saved.
#[cfg(target_os = "macos")]
#[test]
fn a_sandboxed_terminal_has_no_network_even_on_open() {
    let case = Case::new();
    let (_held, port) = local_listener();
    let output = in_terminal_at(
        &case,
        &connects(port),
        AgentAccess::Sandboxed,
        AgentNetwork::Open,
    );
    assert!(!output.status.success(), "{output:?}");
}

/// The script that listens on a port of this machine and prints `bound` only if it could.
/// Input: the port. Output: the script.
#[cfg(target_os = "macos")]
fn binds(port: u16) -> String {
    format!(
        "/usr/bin/nc -l 127.0.0.1 {port} & p=$!; /bin/sleep 1; \
         if kill -0 $p 2>/dev/null; then kill $p; echo bound; fi"
    )
}

/// An Own branch terminal on the Open choice cannot listen on a port the owner's local model
/// uses, and still can listen on any other.
#[cfg(target_os = "macos")]
#[test]
fn an_own_branch_terminal_on_open_cannot_bind_a_configured_local_model_port() {
    let case = Case::new();
    let (first, kept) = local_listener();
    let (second, free) = local_listener();
    drop((first, second));
    let at = |port| {
        let output = in_terminal_keeping(
            &case,
            &binds(port),
            AgentAccess::OwnBranch,
            AgentNetwork::Open,
            &[kept],
        );
        String::from_utf8_lossy(&output.stdout).contains("bound")
    };
    assert!(!at(kept), "the local model's port is refused");
    assert!(at(free), "any other port still binds");
}

/// The engine never runs a Full access terminal: that one is the IDE server's own shell.
#[cfg(target_os = "macos")]
#[test]
fn an_engine_terminal_launch_at_full_access_is_refused() {
    let case = Case::new();
    let project = ProjectId::new(PROJECT).unwrap();
    let launch = ide_shell::Launch {
        root: &case.root,
        project: &project,
        folder: &case.folder,
        cwd: &case.folder,
        shell: std::path::Path::new("/bin/sh"),
        arguments: &[],
        incoming: &[],
        extra_reads: &[],
        level: AgentAccess::Full,
        network: AgentNetwork::Open,
        kept_ports: &[],
        agent_files: Default::default(),
    };
    let refused = ide_shell::command(&launch).unwrap_err();
    assert!(
        matches!(refused, crate::sandbox::SandboxError::Invalid(_)),
        "{refused:?}"
    );
    assert!(!case.root.join("ide-shell").exists());
}

/// DEC-124: an engine-run IDE terminal keeps to the project's agent folders choice at
/// Sandboxed and Own branch, like the IDE server: Allow all changes deletes, No delete or
/// rename keeps an agent file and folder but lets an edit land, Read only refuses the edit
/// too. At every choice the base profile keeps the agent's checkout from being moved aside.
/// The owner's own files stay changeable.
#[cfg(target_os = "macos")]
#[test]
fn an_ide_terminal_keeps_to_the_agent_folders_choice() {
    let case = Case::new();
    let agent = case
        .folder
        .join(".crazyagent")
        .join("worktrees")
        .join("agent-1");
    let teammate = agent.join(crate::agent_git::TEAMMATES_FOLDER).join("t1");
    for (index, (choice, level, deletes, edits)) in [
        (IdeAgentFiles::AllowAll, AgentAccess::Sandboxed, true, true),
        (
            IdeAgentFiles::NoDeleteOrRename,
            AgentAccess::Sandboxed,
            false,
            true,
        ),
        (
            IdeAgentFiles::NoDeleteOrRename,
            AgentAccess::OwnBranch,
            false,
            true,
        ),
        (
            IdeAgentFiles::ReadOnly,
            AgentAccess::Sandboxed,
            false,
            false,
        ),
        (
            IdeAgentFiles::ReadOnly,
            AgentAccess::OwnBranch,
            false,
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::create_dir_all(&teammate).unwrap();
        let file = teammate.join(format!("a{index}.txt"));
        std::fs::write(&file, "agent\n").unwrap();
        let own = case.folder.join(format!("own{index}.txt"));
        std::fs::write(&own, "owner\n").unwrap();
        let rule = AgentFilesRule {
            choice,
            older_agents: false,
        };
        let edit = in_terminal_holding(
            &case,
            &format!("printf edited > {}", quoted(&file)),
            level,
            AgentNetwork::Open,
            &[],
            rule,
        );
        assert_eq!(edit.status.success(), edits, "{choice:?} {level:?}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap() == "edited",
            edits,
            "{choice:?} {level:?}"
        );
        let delete = in_terminal_holding(
            &case,
            &format!("rm -f {}", quoted(&file)),
            level,
            AgentNetwork::Open,
            &[],
            rule,
        );
        assert_eq!(delete.status.success(), deletes, "{choice:?} {level:?}");
        assert_eq!(file.exists(), !deletes, "{choice:?} {level:?}");
        let aside = agent.with_file_name("agent-1-aside");
        let moved = in_terminal_holding(
            &case,
            &format!("mv {} {}", quoted(&agent), quoted(&aside)),
            level,
            AgentNetwork::Open,
            &[],
            rule,
        );
        assert!(!moved.status.success(), "{choice:?} {level:?}");
        assert!(agent.is_dir(), "{choice:?} {level:?}");
        assert!(!aside.exists(), "{choice:?} {level:?}");
        let owner = in_terminal_holding(
            &case,
            &format!(
                "printf changed > {} && rm -f {}",
                quoted(&own),
                quoted(&own)
            ),
            level,
            AgentNetwork::Open,
            &[],
            rule,
        );
        assert!(owner.status.success(), "{choice:?} {level:?}");
        assert!(!own.exists());
    }
}

/// A Sandboxed terminal is refused a new file in the project's git hooks.
#[cfg(target_os = "macos")]
#[test]
fn the_sandboxed_terminal_cannot_write_the_projects_git_hooks() {
    let case = Case::new();
    setup_git(&case.folder, &["init", "-q"]);
    let hooks = case.folder.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let output = in_terminal(&case, "printf x > .git/hooks/x");
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("Operation not permitted"), "{said}");
    assert!(!hooks.join("x").exists(), "{said}");
}

/// A Sandboxed terminal is refused a change to the project's git config.
#[cfg(target_os = "macos")]
#[test]
fn the_sandboxed_terminal_cannot_write_the_projects_git_config() {
    let case = Case::new();
    setup_git(&case.folder, &["init", "-q"]);
    let config = case.folder.join(".git").join("config");
    let before = std::fs::read(&config).unwrap();
    let output = in_terminal(&case, "printf x >> .git/config");
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("Operation not permitted"), "{said}");
    assert_eq!(std::fs::read(&config).unwrap(), before, "{said}");
}

/// Every file under the project's `.git` and its bytes, leaving out `objects`, where a
/// refused `git add` still leaves the blob it wrote before the index. Input: the project
/// folder. Output: the files.
#[cfg(target_os = "macos")]
fn git_files(folder: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![folder.join(".git")];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_dir() {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            } else if !path.ends_with("objects") {
                pending.push(path);
            }
        }
    }
    files
}

/// `git add` is refused in a Sandboxed terminal: a crafted index is what lets a process in
/// it write outside the project through the owner's next `git restore .` (rv-code H1).
#[cfg(target_os = "macos")]
#[test]
fn the_sandboxed_terminal_cannot_stage() {
    let case = Case::new();
    setup_git(&case.folder, &["init", "-q"]);
    let before = git_files(&case.folder);
    let git = quoted(crate::research::runner::sandboxed_git());
    let output = in_terminal(&case, &format!("{git} add README"));
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{said}");
    assert!(
        said.contains("index.lock': Operation not permitted"),
        "{said}"
    );
    assert_eq!(git_files(&case.folder), before, "{said}");
}

/// `git commit` of what the owner staged is refused in a Sandboxed terminal. The row gives a
/// name and email, so the index is the only thing that stops it.
#[cfg(target_os = "macos")]
#[test]
fn the_sandboxed_terminal_cannot_commit() {
    let case = Case::new();
    setup_git(&case.folder, &["init", "-q"]);
    setup_git(&case.folder, &["add", "README"]);
    let before = git_files(&case.folder);
    let git = quoted(crate::research::runner::sandboxed_git());
    let identity = "-c user.name=Owner -c user.email=owner@example.invalid";
    let script = format!("{git} {identity} commit -q -m first");
    let output = in_terminal(&case, &script);
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{said}");
    assert!(
        said.contains("index.lock': Operation not permitted"),
        "{said}"
    );
    assert_eq!(git_files(&case.folder), before, "{said}");
}

/// Plants with `script` in a Sandboxed terminal of a fresh repository, `@OWNER@` naming a
/// folder of the owner's outside the project, then runs the owner's own `git worktree
/// prune`, which a commit also runs as maintenance. Inputs: whether the repository has a
/// `.git/worktrees` folder and the script. Output: what the terminal printed and how many of
/// the owner's two files are still there.
#[cfg(target_os = "macos")]
fn plant_then_prune(worktrees: bool, script: &str) -> (String, usize) {
    let case = Case::new();
    setup_git(&case.folder, &["init", "-q"]);
    if worktrees {
        std::fs::create_dir(case.folder.join(".git").join("worktrees")).unwrap();
    }
    let owner = case.folder.parent().unwrap().join("owner-files");
    let files = [owner.join("keep.txt"), owner.join("sub").join("keep.txt")];
    std::fs::create_dir_all(owner.join("sub")).unwrap();
    for file in &files {
        std::fs::write(file, "owner file\n").unwrap();
    }
    let output = in_terminal(&case, &script.replace("@OWNER@", &quoted(&owner)));
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    setup_git(&case.folder, &["worktree", "prune"]);
    (said, files.iter().filter(|file| file.is_file()).count())
}

/// A Sandboxed terminal cannot make `.git/worktrees`, or an entry in it, a link, in any
/// letter case, by renaming or clone-copying one in either, while it still makes a real
/// admin folder there as `git worktree add` does and links in the project itself. Git's
/// `worktree prune` empties the folder such a link names, outside the project
/// (SECAUDIT-WORKTREE-ADMIN-PLANT).
#[cfg(target_os = "macos")]
#[test]
fn the_sandboxed_terminal_cannot_link_git_worktrees_to_a_folder_the_owners_prune_empties() {
    let (said, kept) = plant_then_prune(false, "ln -s @OWNER@ .git/worktrees; echo folder=$?");
    assert!(said.contains("folder=1"), "{said}");
    assert_eq!(kept, 2, "{said}");
    let (said, kept) = plant_then_prune(
        true,
        "ln -s @OWNER@ .git/worktrees/planted; echo admin=$?; \
         ln -s @OWNER@ .git/WORKTREES/cased; echo cased=$?; \
         mkdir .git/worktrees/real; echo real=$?; \
         ln -s @OWNER@ t && ln -s @OWNER@ u; echo linked=$?; \
         mv t .git/worktrees/moved; echo moved=$?; \
         cp -c -P u .git/worktrees/cloned; echo cloned=$?; \
         cp -c -P u copied; echo copied=$?",
    );
    for code in [
        "admin=1", "cased=1", "real=0", "linked=0", "moved=1", "cloned=1", "copied=0",
    ] {
        assert!(said.contains(code), "{code}: {said}");
    }
    assert_eq!(kept, 2, "{said}");
}

/// Runs a confined IDE terminal's pane as the host would, pins that its shell then starts,
/// and answers what it says first, which is printed inside the pane because the pane host
/// repaints the terminal as it attaches and wipes anything written from outside. Inputs: the
/// project's level and the terminal's. Output: the pane's first line.
#[cfg(target_os = "macos")]
fn pane_first_line(project_access: AgentAccess, level: AgentAccess) -> String {
    let case = Case::new();
    let project = ProjectId::new(PROJECT).unwrap();
    let pane = ide_shell::interactive(
        &ide_shell::Launch {
            root: &case.root,
            project: &project,
            folder: &case.folder,
            cwd: &case.folder,
            shell: std::path::Path::new("/bin/sh"),
            arguments: &[],
            incoming: &[],
            extra_reads: &[],
            level,
            network: AgentNetwork::Open,
            kept_ports: &[],
            agent_files: Default::default(),
        },
        project_access,
    )
    .unwrap();
    let typed = case.root.join("typed");
    std::fs::write(&typed, "echo started\n").unwrap();
    let mut command = std::process::Command::new(&pane.argv[0]);
    command
        .args(&pane.argv[1..])
        .current_dir(&pane.cwd)
        .env_clear()
        .envs(pane.env.iter().map(|(name, value)| (name, value)))
        .stdin(std::fs::File::open(&typed).unwrap())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = crate::spawn_gate::output(&mut command).unwrap();
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(said.lines().nth(1), Some("started"), "{output:?}");
    said.lines().next().unwrap_or_default().to_owned()
}

/// A Sandboxed project's terminal says it cannot stage or commit, and that a Full access
/// terminal needs the project's access raised first.
#[cfg(target_os = "macos")]
#[test]
fn the_sandboxed_terminal_pane_says_it_cannot_stage_before_its_shell_starts() {
    assert_eq!(
        pane_first_line(AgentAccess::Sandboxed, AgentAccess::Sandboxed),
        "This terminal can't stage or commit. Use Source Control, or a Full access terminal after raising project access."
    );
}

/// An Own branch project's terminal is told the same: it has no Full access terminal yet.
#[cfg(target_os = "macos")]
#[test]
fn an_own_branch_terminal_pane_says_to_raise_project_access() {
    assert_eq!(
        pane_first_line(AgentAccess::OwnBranch, AgentAccess::OwnBranch),
        "This terminal can't stage or commit. Use Source Control, or a Full access terminal after raising project access."
    );
}

/// A narrower terminal in a Full access project names a Full access terminal plainly.
#[cfg(target_os = "macos")]
#[test]
fn a_full_access_projects_narrower_pane_is_not_told_to_raise_access() {
    assert_eq!(
        pane_first_line(AgentAccess::Full, AgentAccess::Sandboxed),
        "This terminal can't stage or commit. Use Source Control, or a Full access terminal."
    );
}

/// Git answers in a Sandboxed IDE terminal, whose home is its own, so no config the sandbox
/// denies stands in its way as the owner's home does for the IDE server's git.
#[cfg(target_os = "macos")]
#[test]
fn the_sandboxed_terminals_git_answers_in_the_project() {
    let case = Case::new();
    setup_git(&case.folder, &["init", "-q"]);
    let script = format!(
        "{git} --version && {git} status --porcelain",
        git = quoted(crate::research::runner::sandboxed_git())
    );
    let output = in_terminal(&case, &script);
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{said}");
    assert!(!said.contains("unable to access"), "{said}");
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("git version "),
        "{said}"
    );
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
        level: AgentAccess::Sandboxed,
        network: AgentNetwork::Open,
        kept_ports: &[],
        agent_files: Default::default(),
    };
    let mut command = ide_shell::command(&launch).unwrap();
    assert_eq!(command.get_program(), crate::sandbox::SANDBOX_EXEC);
    let pane = ide_shell::interactive(&launch, AgentAccess::Sandboxed).unwrap();
    let argv: Vec<&std::ffi::OsStr> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .collect();
    assert_eq!(
        pane.argv
            .iter()
            .map(std::ffi::OsStr::new)
            .collect::<Vec<_>>(),
        argv
    );
    assert_eq!(Some(pane.cwd.as_path()), command.get_current_dir());
    assert_eq!(pane.env.len(), command.get_envs().count());
    assert!(
        pane.env
            .iter()
            .all(|(name, _)| !name.starts_with("CRAZYAGENT_") && name != "GH_TOKEN")
    );
    assert_eq!(
        pane.transcript,
        crate::harnesses::interactive::Transcript::Unavailable
    );
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
        level: AgentAccess::Sandboxed,
        network: AgentNetwork::Open,
        kept_ports: &[],
        agent_files: Default::default(),
    };
    match ide_shell::command(&launch) {
        Err(crate::sandbox::SandboxError::Unavailable(reason)) => {
            assert_eq!(reason, ide_shell::UNAVAILABLE)
        }
        other => panic!("expected the unavailable refusal, got {other:?}"),
    }
    match ide_shell::interactive(&launch, AgentAccess::Sandboxed) {
        Err(crate::sandbox::SandboxError::Unavailable(reason)) => {
            assert_eq!(reason, ide_shell::UNAVAILABLE)
        }
        other => panic!("expected the unavailable refusal, got {other:?}"),
    }
    assert!(!base.join("ide-shell").exists());
}

/// A Sandboxed or Own branch project offers no shell hosted in the IDE server, which holds
/// the git index grant, and still runs tasks there. A Full access project's own shell is the
/// owner's login shell.
#[test]
fn only_a_full_access_project_offers_a_shell_hosted_in_the_ide_server() {
    let owner_shell = PathBuf::from("/bin/zsh");
    for (access, default) in [
        (AgentAccess::Sandboxed, SANDBOXED),
        (AgentAccess::OwnBranch, OWN_BRANCH),
    ] {
        let settings = profiles(Platform::MacOs, &owner_shell, access);
        let listed = settings["terminal.integrated.profiles.osx"]
            .as_object()
            .unwrap();
        assert!(
            listed.values().all(Value::is_null),
            "{access:?}: {listed:?}"
        );
        assert_eq!(
            settings["terminal.integrated.defaultProfile.osx"],
            json!(default)
        );
        assert_eq!(
            settings["terminal.integrated.automationProfile.osx"]["path"],
            json!("/bin/zsh")
        );
        assert_eq!(shell_label(access), None);
    }
    let settings = profiles(Platform::MacOs, &owner_shell, AgentAccess::Full);
    let label = shell_label(AgentAccess::Full).unwrap();
    assert_eq!(label, "Full access");
    let shell = &settings["terminal.integrated.profiles.osx"][label];
    assert_eq!(shell["path"], json!("/bin/zsh"));
    assert_eq!(shell["args"], json!(["-l"]));
    assert_eq!(
        settings["terminal.integrated.defaultProfile.osx"],
        json!(label)
    );
}

/// At Sandboxed the terminal settings hide every shell, run tasks in the server's own shell,
/// start no task on their own, and default to the extension's Sandboxed profile. The built-in
/// extension's manifest contributes both its profiles under the same ids and titles and
/// activates on them.
#[test]
fn terminal_profiles_hide_every_shell_and_default_to_the_extension_sandboxed_profile() {
    let owner_shell = PathBuf::from("/bin/zsh");
    for (platform, os, ide_shell) in [
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
        let settings = profiles(platform, &owner_shell, AgentAccess::Sandboxed);
        assert_eq!(
            settings[&format!("terminal.integrated.defaultProfile.{os}")],
            json!(SANDBOXED)
        );
        assert_eq!(
            settings[&format!("terminal.integrated.automationProfile.{os}")],
            ide_shell
        );
        assert_eq!(settings["task.allowAutomaticTasks"], json!("off"));
        let listed = settings[&format!("terminal.integrated.profiles.{os}")]
            .as_object()
            .unwrap();
        assert!(!listed.contains_key(SANDBOXED), "{os}");
        let offered: Vec<&String> = listed
            .iter()
            .filter(|(_, profile)| !profile.is_null())
            .map(|(name, _)| name)
            .collect();
        assert!(offered.is_empty(), "{os}: {offered:?}");
        assert!(listed.len() > 1, "{os}: detected shells must be hidden");
        let keys = if platform == Platform::Windows { 5 } else { 4 };
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
    let unavailable = crate::sandbox::ide_shell::UNAVAILABLE;
    assert!(!unavailable.contains("IDE shell"), "{unavailable}");
    assert_eq!(
        unavailable,
        "This terminal is unavailable: this system has no qualified sandbox for it."
    );
    let windows = profiles(Platform::Windows, &owner_shell, AgentAccess::Sandboxed);
    assert_eq!(windows["terminal.integrated.useWslProfiles"], json!(false));
    let (name, manifest) = crate::ide::bridge::EXTENSION_FILES[0];
    assert_eq!(name, "package.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest).unwrap();
    assert_eq!(
        manifest["contributes"]["terminal"]["profiles"],
        json!([
            { "id": SANDBOXED_PROFILE, "title": SANDBOXED, "icon": "shield" },
            { "id": OWN_BRANCH_PROFILE, "title": OWN_BRANCH, "icon": "shield" },
        ])
    );
    let events = manifest["activationEvents"].as_array().unwrap();
    assert!(events.contains(&json!(format!("onTerminalProfile:{SANDBOXED_PROFILE}"))));
    assert!(events.contains(&json!(format!("onTerminalProfile:{OWN_BRANCH_PROFILE}"))));
    let embedded = |wanted: &str| {
        crate::ide::bridge::EXTENSION_FILES
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .unwrap()
    };
    let pty = embedded("out/pty.js");
    assert!(pty.contains(&format!(
        "exports.UNAVAILABLE = {};",
        serde_json::to_string(unavailable).unwrap()
    )));
    let provider = embedded("out/terminal.js");
    for (export, literal) in [
        ("PROFILE", SANDBOXED_PROFILE),
        ("TITLE", SANDBOXED),
        ("OWN_BRANCH_PROFILE", OWN_BRANCH_PROFILE),
        ("OWN_BRANCH_TITLE", OWN_BRANCH),
        ("FULL_TITLE", FULL_ACCESS),
    ] {
        let line = format!(
            "exports.{export} = {};",
            serde_json::to_string(literal).unwrap()
        );
        assert!(provider.contains(&line), "{line}");
    }
}

/// The settings the engine writes for this machine are the current platform's terminal
/// profiles with the owner's login shell from the real account database as the IDE shell
/// and the one tasks run in, and the extension's Sandboxed terminal as the default.
#[cfg(unix)]
#[test]
fn machine_profiles_use_this_platform_and_the_owners_login_shell() {
    let os = if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    };
    let shell = ide_shell::owner_shell().unwrap();
    assert!(shell.is_absolute(), "{}", shell.display());

    let settings = machine_profiles(AgentAccess::Sandboxed).unwrap();

    assert_eq!(
        settings,
        profiles(Platform::CURRENT, &shell, AgentAccess::Sandboxed)
    );
    assert_eq!(
        settings[&format!("terminal.integrated.defaultProfile.{os}")],
        json!(SANDBOXED)
    );
    let automation = &settings[&format!("terminal.integrated.automationProfile.{os}")];
    assert_eq!(automation["path"], json!(shell.to_string_lossy()));
    let full = machine_profiles(AgentAccess::Full).unwrap();
    assert_eq!(
        &full[&format!("terminal.integrated.profiles.{os}")]["Full access"],
        automation
    );
}

/// Makes an engine root with one project whose saved policy is the given one, for the rows
/// that read a terminal's level. Input: the folder to build in and the policy. Output: the
/// engine root.
#[cfg(unix)]
fn saved_policy(base: &std::path::Path, policy: &AgentPolicy) -> PathBuf {
    let root = base.join("engine");
    let folder = base.join("project");
    for path in [&root, &folder] {
        std::fs::create_dir(path).unwrap();
    }
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    crate::ide::workspace::tests::add_project(&db, PROJECT, &folder);
    crate::sandbox::policy::save(&db, PROJECT, policy).unwrap();
    root
}

/// A terminal's level and network choice are the ones the owner saved for the project.
#[cfg(unix)]
#[test]
fn ide_shell_reads_the_level_and_network_the_owner_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let policy = AgentPolicy {
        access: AgentAccess::OwnBranch,
        network: AgentNetwork::Https,
        ..AgentPolicy::default()
    };
    let root = saved_policy(&base, &policy);
    let place = ide_shell::project_place(&root, &ProjectId::new(PROJECT).unwrap()).unwrap();
    assert_eq!(place.access, AgentAccess::OwnBranch);
    assert_eq!(place.network, AgentNetwork::Https);
}

/// A saved policy that cannot be read stops the terminal instead of picking a level.
#[cfg(unix)]
#[test]
fn a_saved_policy_that_cannot_be_read_stops_the_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = saved_policy(&base, &AgentPolicy::default());
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let tampered = db
        .execute(
            "UPDATE project_agent_policy SET body='tampered' WHERE project=?1",
            [PROJECT],
        )
        .unwrap();
    assert_eq!(tampered, 1);
    drop(db);
    assert!(ide_shell::project_place(&root, &ProjectId::new(PROJECT).unwrap()).is_err());
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

/// The built-in extension keeps working in Restricted Mode, where a Full access IDE starts
/// until the owner trusts the folder: it runs none of the project's code.
#[test]
fn the_extension_declares_it_runs_in_restricted_mode() {
    let (name, manifest) = crate::ide::bridge::EXTENSION_FILES[0];
    assert_eq!(name, "package.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest).unwrap();
    assert_eq!(
        manifest["capabilities"]["untrustedWorkspaces"]["supported"],
        json!(true)
    );
}

/// The command palette offers the terminal menu's levels with the menu's state and words. It
/// drops a greyed command rather than greying it, so a level above the project's is its own
/// palette command, titled as the menu's greyed row, that only says to raise project access.
#[test]
fn the_palette_offers_the_menus_levels_and_words() {
    let (name, manifest) = crate::ide::bridge::EXTENSION_FILES[0];
    assert_eq!(name, "package.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest).unwrap();
    let contributes = &manifest["contributes"];
    let category = "New terminal at access level";
    assert_eq!(contributes["submenus"][0]["label"], json!(category));
    let command = |id: &str| {
        contributes["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["command"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("{id} is not contributed"))
    };
    for (id, title) in [
        ("crazyagent.terminal.sandboxed", "Sandboxed"),
        ("crazyagent.terminal.ownBranch", "Own branch"),
        ("crazyagent.terminal.full", "Full access"),
        (
            "crazyagent.terminal.ownBranchAbove",
            "Own branch - Raise project access to use this",
        ),
        (
            "crazyagent.terminal.fullAbove",
            "Full access - Raise project access to use this",
        ),
    ] {
        assert_eq!(
            command(id),
            json!({"command": id, "title": title, "category": category})
        );
    }
    for (raise, above) in [
        (
            "crazyagent.terminal.ownBranchRaise",
            "crazyagent.terminal.ownBranchAbove",
        ),
        (
            "crazyagent.terminal.fullRaise",
            "crazyagent.terminal.fullAbove",
        ),
    ] {
        assert_eq!(command(raise)["enablement"], json!("false"));
        assert_eq!(command(raise)["title"], command(above)["title"]);
    }
    let menu = contributes["menus"]["crazyagent.terminalLevels"]
        .as_array()
        .unwrap();
    let menu_when = |id: &str| {
        menu.iter()
            .find(|entry| entry["command"] == json!(id))
            .map(|entry| entry["when"].clone())
            .unwrap_or_else(|| panic!("{id} is not in the menu"))
    };
    let palette = contributes["menus"]["commandPalette"].as_array().unwrap();
    let palette_when = |id: &str| {
        palette
            .iter()
            .filter(|entry| entry["command"] == json!(id))
            .map(|entry| entry["when"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(menu_when("crazyagent.terminal.sandboxed"), json!(null));
    assert!(palette_when("crazyagent.terminal.sandboxed").is_empty());
    for (shown, in_menu) in [
        (
            "crazyagent.terminal.ownBranch",
            "crazyagent.terminal.ownBranch",
        ),
        (
            "crazyagent.terminal.ownBranchAbove",
            "crazyagent.terminal.ownBranchRaise",
        ),
        ("crazyagent.terminal.full", "crazyagent.terminal.full"),
        (
            "crazyagent.terminal.fullAbove",
            "crazyagent.terminal.fullRaise",
        ),
    ] {
        assert_eq!(palette_when(shown), [menu_when(in_menu)], "{shown}");
    }
    assert_eq!(
        palette_when("crazyagent.terminal.ownBranchAbove"),
        [json!("crazyagent.ideAccess == sandboxed")]
    );
    assert_eq!(
        palette_when("crazyagent.terminal.fullAbove"),
        [json!("crazyagent.ideAccess != full")]
    );
    for raise in [
        "crazyagent.terminal.ownBranchRaise",
        "crazyagent.terminal.fullRaise",
    ] {
        assert_eq!(palette_when(raise), [json!("false")], "{raise}");
    }
    assert!(
        menu.iter()
            .all(|entry| !entry["command"].as_str().unwrap().ends_with("Above"))
    );
    let embedded = |wanted: &str| {
        crate::ide::bridge::EXTENSION_FILES
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .unwrap()
    };
    assert!(
        embedded("out/pty.js")
            .contains(r#"exports.ABOVE_PROJECT = "Raise project access to use this";"#)
    );
    let terminal = embedded("out/terminal.js");
    for above in [
        "crazyagent.terminal.ownBranchAbove",
        "crazyagent.terminal.fullAbove",
    ] {
        assert!(terminal.contains(&format!("{above:?}")), "{above}");
    }
    assert!(terminal.contains("showInformationMessage(pty_js_1.ABOVE_PROJECT)"));
}
