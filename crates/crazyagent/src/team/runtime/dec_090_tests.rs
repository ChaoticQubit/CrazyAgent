use super::managed_agent::{ManagedAgent, claude, codex};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const OWNED: [(&str, &str); 6] = [
    ("settings.json", "{\"owner\":true}\n"),
    ("config.toml", "owner = true\n"),
    ("AGENTS.md", "owner\n"),
    ("CLAUDE.md", "owner\n"),
    (
        "skills/owner/SKILL.md",
        "---\nname: owner\ndescription: owner\n---\n",
    ),
    (".claude/settings.json", "{\"owner\":true}\n"),
];

const ATTEMPTS: [(&str, &str); 12] = [
    ("overwrite", "printf planted > {p}/settings.json"),
    ("append", "printf planted >> {p}/config.toml"),
    ("create", "printf planted > {p}/AGENTS.override.md"),
    ("create_folder", "mkdir {p}/skills/planted"),
    (
        "create_in_skill",
        "printf planted > {p}/skills/owner/extra.md",
    ),
    (
        "create_in_claude",
        "printf planted > {p}/.claude/settings.local.json",
    ),
    (
        "rename_over",
        "printf planted > \"$TMPDIR/claude.md\" && mv \"$TMPDIR/claude.md\" {p}/CLAUDE.md",
    ),
    ("remove", "rm {p}/AGENTS.md"),
    ("link", "ln -s /etc/hosts {p}/skills/owner/linked.md"),
    ("move_folder", "mv {p}/skills {p}/skills-moved"),
    ("chmod", "chmod 644 {p}/settings.json"),
    ("touch", "touch {p}/config.toml"),
];

const ALLOWED: [(&str, &str); 2] = [
    ("own_home", "printf ok > \"$HOME/dec-090-note\""),
    ("worktree", "printf ok > dec-090-note"),
];

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Folder(u32, SystemTime),
    File(u32, SystemTime, Vec<u8>),
    Link(PathBuf),
}

/// Records every entry below `root` with its mode, modification time and content,
/// without following links. Input: the folder. Output: the entries by relative path.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for item in std::fs::read_dir(&folder).unwrap() {
            let path = item.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let mode = metadata.permissions().mode();
            let entry = if metadata.file_type().is_symlink() {
                Entry::Link(std::fs::read_link(&path).unwrap())
            } else if metadata.is_dir() {
                pending.push(path.clone());
                Entry::Folder(mode, metadata.modified().unwrap())
            } else {
                Entry::File(
                    mode,
                    metadata.modified().unwrap(),
                    std::fs::read(&path).unwrap(),
                )
            };
            entries.insert(path.strip_prefix(root).unwrap().to_path_buf(), entry);
        }
    }
    entries
}

/// Runs each named shell attempt as the agent and reads which ones succeeded.
/// Input: the agent, the attempts and the profile home they target.
/// Output: the outcome of every attempt by name, with the agent's stderr.
fn attempt(
    agent: &ManagedAgent,
    attempts: &[(&str, &str)],
    profile: &Path,
) -> (BTreeMap<String, String>, String) {
    let quoted = format!("'{}'", profile.display());
    let script = attempts
        .iter()
        .map(|(name, command)| {
            format!(
                "({}) 2>/dev/null && echo {name}=written || echo {name}=refused\n",
                command.replace("{p}", &quoted)
            )
        })
        .collect::<String>();
    let output = agent.shell(&script);
    let outcomes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, outcome)| (name.to_owned(), outcome.to_owned()))
        .collect();
    (
        outcomes,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Seeds the managed profile home with the owner's files, lets a tools-on agent try to
/// change it every way a shell can, and requires every attempt refused, the home byte
/// for byte unchanged and the agent still able to write its own home and worktree.
/// Input: the launch harness and its real executable.
fn agent_cannot_change_the_profile_home(harness: &'static str, executable: &Path) {
    let agent = ManagedAgent::new(harness, executable);
    let profile = &agent.profile_home;
    for (name, text) in OWNED {
        let path = profile.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let before = snapshot(profile);
    let (blocked, blocked_errors) = attempt(&agent, &ATTEMPTS, profile);
    let (allowed, allowed_errors) = attempt(&agent, &ALLOWED, profile);
    let after = snapshot(profile);
    let mut problems = Vec::new();
    if agent.home == *profile || agent.home.starts_with(profile) {
        problems.push(format!(
            "the agent's home {} is the managed profile home",
            agent.home.display()
        ));
    }
    for (name, _) in ATTEMPTS {
        let outcome = blocked.get(name).map_or("missing", String::as_str);
        if outcome != "refused" {
            problems.push(format!("{name}: {outcome}"));
        }
    }
    for (name, _) in ALLOWED {
        let outcome = allowed.get(name).map_or("missing", String::as_str);
        if outcome != "written" {
            problems.push(format!("{name}: {outcome} ({allowed_errors})"));
        }
    }
    if after != before {
        let changed = before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .map(|path| path.display().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        problems.push(format!("profile home changed: {changed:?}"));
    }
    assert!(
        problems.is_empty(),
        "a tools-on {harness} agent changed its managed profile home {}:\n{}\n{blocked_errors}",
        profile.display(),
        problems.join("\n")
    );
}

#[test]
fn a_tools_on_codex_agent_cannot_change_its_managed_profile_home() {
    agent_cannot_change_the_profile_home("codex", &codex());
}

#[test]
fn a_tools_on_claude_agent_cannot_change_its_managed_profile_home() {
    agent_cannot_change_the_profile_home("claude-code", &claude());
}

/// Fetches `url` with curl and returns the HTTP status it printed, `000` without a reply.
/// Input: the command that runs curl. Output: the status text and curl's stderr.
fn http_status(command: &mut std::process::Command) -> (String, String) {
    let output = crate::spawn_gate::output(command).unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Starts the real harness inside a tools-on managed agent's sandbox with the home the
/// runtime gave it, then requires the provider's HTTPS endpoint to answer from inside
/// that sandbox. A machine with no route to the provider is a BLOCKED prerequisite.
/// Input: the launch harness, its real executable and the provider URL.
fn agent_starts_and_reaches_the_provider(harness: &'static str, executable: &Path, url: &str) {
    let agent = ManagedAgent::new(harness, executable);
    let version =
        crate::spawn_gate::output(&mut agent.command(executable, &["--version"])).unwrap();
    assert!(
        version.status.success() && !version.stdout.is_empty(),
        "the {harness} harness did not start in its sandbox: {}",
        String::from_utf8_lossy(&version.stderr)
    );
    let arguments = [
        "-sS",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "--max-time",
        "20",
        url,
    ];
    let (outside, outside_errors) = http_status(
        std::process::Command::new("/usr/bin/curl")
            .args(arguments)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    );
    assert!(
        outside.len() == 3 && outside != "000",
        "BLOCKED: {url} does not answer from this machine: {outside} {outside_errors}"
    );
    let (inside, inside_errors) =
        http_status(&mut agent.command(Path::new("/usr/bin/curl"), &arguments));
    assert!(
        inside.len() == 3 && inside != "000",
        "a tools-on {harness} agent cannot reach {url} from its sandbox: {inside} {inside_errors}"
    );
}

#[test]
fn a_tools_on_codex_agent_starts_its_harness_and_reaches_the_provider() {
    agent_starts_and_reaches_the_provider(
        "codex",
        &codex(),
        "https://chatgpt.com/backend-api/codex/responses",
    );
}

#[test]
fn a_tools_on_claude_agent_starts_its_harness_and_reaches_the_provider() {
    agent_starts_and_reaches_the_provider(
        "claude-code",
        &claude(),
        "https://api.anthropic.com/v1/messages",
    );
}
