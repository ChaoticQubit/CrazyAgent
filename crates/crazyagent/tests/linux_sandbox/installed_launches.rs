use super::*;
use crazyagent::harnesses::launch::listed_command;
use kernel_sandbox::profile::env::Handed;

#[test]
fn an_installed_sandbox_on_linux_launches_through_the_linux_boundary() {
    let case = case();
    let profiles = case.outside.join("profiles");
    fs::create_dir_all(&profiles).unwrap();
    let installed = InstalledSandbox::install(
        policy(&case, SandboxNetwork::Open),
        &profiles.join("agent.sb"),
        LinuxSupervisor::On,
        LinuxStrict::Off,
    )
    .unwrap();
    let launcher = installed.launcher().to_path_buf();
    assert!(
        launcher.ends_with("crazyagent-landlock")
            || BWRAP_PATHS.iter().any(|path| launcher == Path::new(path)),
        "{}",
        launcher.display()
    );
    let output = run(&installed, &case, &boundary_script(&case));
    assert_boundary(&case, &output);
}

/// The name a launch hands its relay token under in these rows.
const HANDED_NAME: &str = "OPENAI_API_KEY";

/// An agent that counts the hand-over lines in its own environment, then waits on its input,
/// so it stays alive while another agent looks for it.
const READS_ITSELF: &str = "tr '\\0' '\\n' < /proc/$$/environ | grep -c '^OPENAI_API_KEY=crazyagent-relay-'; cat > /dev/null";

/// Writes the holder's variable names, never values, to a file in its folder.
const NAMES_DUMP: &str = "tr '\\0' '\\n' < /proc/$$/environ | grep -E '^[A-Za-z_][A-Za-z0-9_]*=' | cut -d= -f1 | sort | tr '\\n' ',' > holder-names.txt; ";

/// An agent that reads the environment of every process its `/proc` shows and counts how
/// many it could read and how many hold a hand-over line.
const READS_OTHERS: &str = "seen=0; held=0
for file in /proc/[0-9]*/environ; do
  if tr '\\0' '\\n' < \"$file\" > /dev/null 2>&1; then seen=$((seen + 1)); fi
  if tr '\\0' '\\n' < \"$file\" 2>/dev/null | grep -q '^OPENAI_API_KEY=crazyagent-relay-'; then held=$((held + 1)); fi
done
echo \"seen=$seen held=$held\"";

/// A relay token in the engine's shape: its prefix and 64 hex digits. Output: the token.
fn relay_token() -> String {
    format!(
        "{}{}",
        kernel_common::relay_names::TOKEN_PREFIX,
        "a".repeat(kernel_common::relay_names::TOKEN_HEX)
    )
}

/// A launch on Linux carries its account hand-over: the relay token reaches the agent only
/// under the name the launch hands it, and the same token under any other name refuses the
/// launch.
#[test]
fn a_linux_launch_with_a_hand_over_holds_the_token_only_under_its_names() {
    let case = case();
    let request = linux_launch(
        &case,
        LaunchBoundary::Installed(Arc::new(team_boundary(&case))),
        &case.home,
    );
    let token = relay_token();
    let names = [HANDED_NAME];
    let handed = Handed::new(&names, &token).expect("Linux admits a launch's own relay token");
    let key = (OsString::from(HANDED_NAME), OsString::from(&token));
    let built = listed_command(&request, &[], std::slice::from_ref(&key), handed).unwrap();
    let holding: Vec<&OsStr> = built
        .get_envs()
        .filter(|(_, value)| *value == Some(OsStr::new(&token)))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(holding, [OsStr::new(HANDED_NAME)]);
    let other = (OsString::from("OTHER_TOKEN"), OsString::from(&token));
    let refused = listed_command(&request, &[], &[key, other], handed).unwrap_err();
    assert_eq!(refused.0, "launch_environment_refused");
}

/// The Linux handing wrap keeps a launch's relay token under the names it hands it and
/// refuses the same token under any other name.
#[test]
fn the_linux_handing_wrap_holds_the_token_only_under_its_names() {
    let case = case();
    let landlock = pinned(policy(&case, SandboxNetwork::Only(Vec::new())));
    let token = relay_token();
    let names = [HANDED_NAME];
    let handed = Handed::new(&names, &token).expect("Linux admits a launch's own relay token");
    let mut harness = Command::new("/usr/bin/true");
    harness
        .env(HANDED_NAME, &token)
        .current_dir(case.project.join("worktrees"));
    let built = landlock.wrap_handing(&harness, handed).unwrap();
    let holding: Vec<&OsStr> = built
        .get_envs()
        .filter(|(_, value)| *value == Some(OsStr::new(&token)))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(holding, [OsStr::new(HANDED_NAME)]);
    harness.env("OTHER_TOKEN", &token);
    assert!(matches!(
        landlock.wrap_handing(&harness, handed),
        Err(SandboxError::Invalid(_))
    ));
}

/// Checks one backend's isolation claim for a hand-over: agent A, holding the token under
/// its handed name, finds it in its own environment, while agent B of the same backend,
/// started beside it, reads no environment that holds it. Inputs: the case and the
/// backend's sandbox. Output: none; the row asserts.
fn assert_environ_kept_apart(case: &Case, sandbox: &LinuxSandbox) {
    let token = relay_token();
    let names = [HANDED_NAME];
    let handed = Handed::new(&names, &token).expect("Linux admits a launch's own relay token");
    let mut holder = Command::new("/bin/sh");
    holder
        .args(["-c", &format!("{NAMES_DUMP}{READS_ITSELF}")])
        .env(HANDED_NAME, &token)
        .current_dir(case.project.join("worktrees"));
    let mut holder = kernel_processes::spawn_gate::spawn(
        sandbox
            .wrap_handing(&holder, handed)
            .unwrap()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(holder.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "1\n", "the holder reads its own hand-over");
    let mut reader = Command::new("/bin/sh");
    reader
        .args(["-c", READS_OTHERS])
        .current_dir(case.project.join("worktrees"));
    let found = agent_stdout(Some(sandbox as &dyn Sandbox), reader);
    drop(holder.stdin.take());
    assert!(holder.wait().unwrap().success());
    if let Some(diag) = std::env::var_os("HOLDER_READ_DIAG") {
        let names =
            std::fs::read_to_string(case.project.join("worktrees").join("holder-names.txt"))
                .unwrap_or_else(|error| format!("unread: {error}"));
        let row = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();
        let text = format!(
            "{row} holder={} reader={} holder_names={names}\n",
            line.trim(),
            found.trim()
        );
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(diag)
            .unwrap();
        std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
    }
    let (seen, held) = found
        .trim()
        .split_once(' ')
        .and_then(|(seen, held)| {
            Some((
                seen.strip_prefix("seen=")?.parse::<u32>().ok()?,
                held.strip_prefix("held=")?.parse::<u32>().ok()?,
            ))
        })
        .unwrap_or_else(|| panic!("the reader reports: {found:?}"));
    assert!(
        seen >= 1,
        "the reader reads at least its own environment: {found:?}"
    );
    assert_eq!(held, 0, "no other agent's hand-over is readable: {found:?}");
}

/// The Landlock backend has no process namespace: another agent's `/proc/<pid>/environ` is
/// kept from it only by Landlock's ptrace scope, since the two are sibling domains.
#[test]
fn a_landlock_agent_reads_no_other_agents_hand_over() {
    let case = case();
    let landlock = pinned(policy(&case, SandboxNetwork::Only(Vec::new())));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    assert_environ_kept_apart(&case, &landlock);
}

/// The nested backend starts each agent in its own process namespace with a fresh `/proc`,
/// so another agent's process is not even listed. A machine that cannot nest fails this row:
/// a skip is no proof.
#[test]
fn a_nested_agent_reads_no_other_agents_hand_over() {
    let case = case();
    let nested = nested(policy(&case, SandboxNetwork::Only(Vec::new())))
        .expect("the hand-over proof needs a machine that nests agents");
    assert_environ_kept_apart(&case, &nested);
}

/// The bubblewrap backend starts each agent in its own process namespace with a fresh
/// `/proc`, so another agent's process is not even listed.
#[test]
fn a_bubblewrap_agent_reads_no_other_agents_hand_over() {
    let case = case();
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_environ_kept_apart(&case, &bubblewrap);
}
