use super::*;

const SECRET_NAME: &str = "root-swap.txt";
const SECRET_TEXT: &str = "root-swap secret 5c1e9a\n";
const LEAK_NAME: &str = "root-swap-leak.txt";
const HELPER_REFUSED: i32 = 3;

/// Wraps an agent that copies `<home>/root-swap.txt` into the project, then, before it
/// starts, moves the agent's home aside and puts an absolute link to a secret folder in its
/// place, as a process that writes the home's parent could between the wrap and the helper.
/// Inputs: the case and the sandbox. Output: the launch's exit code and what the agent copied.
fn swapped_launch(case: &Case, sandbox: &LinuxSandbox) -> (Option<i32>, Option<String>) {
    swapped_launch_to(case, sandbox, &case.outside.join("secret"))
}

/// Runs [`swapped_launch`] with the link's own text as given, absolute or relative to the
/// home's parent. Inputs: the case, the sandbox and the link text. Output: the launch's exit
/// code and what the agent copied.
fn swapped_launch_to(
    case: &Case,
    sandbox: &LinuxSandbox,
    target: &Path,
) -> (Option<i32>, Option<String>) {
    let secret = case.outside.join("secret");
    fs::create_dir_all(&secret).unwrap();
    fs::write(secret.join(SECRET_NAME), SECRET_TEXT).unwrap();
    let leak = case.project.join("worktrees").join(LEAK_NAME);
    let mut harness = Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg(format!(
            "cat {} > {}",
            quoted(&case.home.join(SECRET_NAME)),
            quoted(&leak)
        ))
        .current_dir(case.project.join("worktrees"));
    let mut wrapped = sandbox.wrap(&harness).unwrap();
    let moved = case.home.with_file_name("home-moved");
    fs::rename(&case.home, &moved).unwrap();
    std::os::unix::fs::symlink(target, &case.home).unwrap();
    assert!(fs::symlink_metadata(&case.home).unwrap().is_symlink());
    let output = kernel_processes::spawn_gate::output(
        wrapped
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    (output.status.code(), fs::read_to_string(&leak).ok())
}

/// A Landlock agent whose home is swapped for a link to a secret folder after its wrap is
/// refused before it starts, and never reads the secret through the link.
#[test]
fn a_landlock_root_swapped_for_a_link_after_the_wrap_grants_nothing_at_its_target() {
    let case = case();
    let landlock = pinned(policy(&case, SandboxNetwork::Only(Vec::new())));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    let (code, copied) = swapped_launch(&case, &landlock);
    assert!(
        copied
            .as_deref()
            .is_none_or(|text| !text.contains(SECRET_TEXT.trim_end())),
        "the agent read the secret through the swapped home: {copied:?}"
    );
    assert_eq!(code, Some(HELPER_REFUSED));
}

/// A nested agent whose home is swapped for a link to a secret folder after its wrap is
/// refused before it starts, and never reads the secret through the link. A machine that
/// cannot nest fails this row: a skip is no proof.
#[test]
fn a_nested_root_swapped_for_a_link_after_the_wrap_grants_nothing_at_its_target() {
    let case = case();
    let nested = nested(policy(&case, SandboxNetwork::Only(Vec::new())))
        .expect("the root swap proof needs a machine that nests agents");
    let (code, copied) = swapped_launch(&case, &nested);
    assert!(
        copied
            .as_deref()
            .is_none_or(|text| !text.contains(SECRET_TEXT.trim_end())),
        "the agent read the secret through the swapped home: {copied:?}"
    );
    assert_eq!(code, Some(HELPER_REFUSED));
}

/// The plain Bubblewrap backend for a policy, as on a kernel without Landlock file rules.
/// Input: the policy. Output: the sandbox.
fn bubblewrap(policy: ProjectSandbox) -> LinuxSandbox {
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    LinuxSandbox::new(
        policy,
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap()
}

/// Asserts a swapped Bubblewrap launch was refused before its agent started and copied
/// nothing of the secret. Inputs: the launch's exit code and what the agent copied.
fn assert_refused(code: Option<i32>, copied: Option<&str>) {
    assert!(
        copied.is_none_or(|text| !text.contains(SECRET_TEXT.trim_end())),
        "the agent read the secret through the swapped home: exit {code:?}, copied {copied:?}"
    );
    assert_eq!(
        code,
        Some(HELPER_REFUSED),
        "exit {code:?}, copied {copied:?}"
    );
}

/// A Bubblewrap agent whose home is swapped for an absolute link to a secret folder after
/// its wrap is refused before it starts, and never reads the secret through the link.
#[test]
fn a_bubblewrap_root_swapped_for_an_absolute_link_after_the_wrap_grants_nothing_at_its_target() {
    let case = case();
    let sandbox = bubblewrap(policy(&case, SandboxNetwork::Only(Vec::new())));
    let (code, copied) = swapped_launch(&case, &sandbox);
    assert_refused(code, copied.as_deref());
}

/// A Bubblewrap agent whose home is swapped for a relative link to a secret folder after its
/// wrap is refused before it starts, and never reads the secret through the link.
#[test]
fn a_bubblewrap_root_swapped_for_a_relative_link_after_the_wrap_grants_nothing_at_its_target() {
    let case = case();
    let sandbox = bubblewrap(policy(&case, SandboxNetwork::Only(Vec::new())));
    let (code, copied) = swapped_launch_to(&case, &sandbox, Path::new("../outside/secret"));
    assert_refused(code, copied.as_deref());
}

/// A Bubblewrap read root first reached through a linked parent and canonicalized before the
/// build is the folder the launch holds, so the agent starts and reads it.
#[test]
fn a_bubblewrap_read_root_canonicalized_from_a_linked_parent_still_launches() {
    let case = case();
    let shared = case.outside.join("shared");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("note.txt"), "shared note 41d7\n").unwrap();
    assert!(
        fs::symlink_metadata(case.project.join("link"))
            .unwrap()
            .is_symlink()
    );
    let canonical = fs::canonicalize(case.project.join("link").join("shared")).unwrap();
    assert_eq!(canonical, shared);
    let read = policy(&case, SandboxNetwork::Only(Vec::new()))
        .allow_read(&canonical)
        .unwrap();
    let sandbox = bubblewrap(read);
    let output = run(
        &sandbox,
        &case,
        &format!("cat {}", quoted(&canonical.join("note.txt"))),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("shared note 41d7"),
        "{output:?}"
    );
}
