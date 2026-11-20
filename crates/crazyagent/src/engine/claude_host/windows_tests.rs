use super::{INSTRUCTIONS_BYTES_MAX, MasterTools, launch_command, tools_launch_command};
use crazyagent_win_sandbox::private;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const CLAUDE: &str = r"C:\registered\claude.exe";

/// A turn's project folder, private home and private instruction file, made in a fresh
/// temporary folder. Output: the temporary folder, kept alive, and the three paths.
fn turn() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let home = root.path().join("home");
    drop(private::dir(&home).unwrap());
    let instructions = home.join("instructions.md");
    let mut file = private::file(&instructions, true).unwrap();
    file.write_all(b"the owner's instructions").unwrap();
    drop(file);
    (root, project, home, instructions)
}

/// Grants the Users group read on `path` with icacls, so the path is no longer private.
/// Input: the path. Output: none.
fn grant_users_read(path: &Path) {
    let icacls = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("icacls.exe");
    let mut command = std::process::Command::new(icacls);
    command.arg(path).args(["/grant", "*S-1-5-32-545:(R)"]);
    let granted = crate::spawn_gate::collected(&mut command).unwrap();
    assert!(granted.status.success(), "icacls refused the grant");
}

/// The code a tools-off Claude Code master command on Windows is refused with, or none.
/// Inputs: the home and the instruction file. Output: the code.
fn refusal(project: &Path, home: &Path, instructions: Option<&Path>) -> Option<String> {
    launch_command(
        Path::new(CLAUDE),
        project,
        home,
        "admitted-model",
        instructions,
    )
    .err()
    .map(|error| error.0)
}

/// V244-WIN-MASTER S3 (D4): the tools-off Claude Code master command is built on Windows
/// for a private home and a private instruction file, with the Unix arguments and the Unix
/// environment; it carries no shell and no background variable, which only a tools-on
/// master gets. It used to be refused as `claude_host_unavailable`.
#[test]
fn a_windows_claude_master_command_is_built_for_a_private_home_and_instructions() {
    let (_root, project, home, instructions) = turn();
    let command = launch_command(
        Path::new(CLAUDE),
        &project,
        &home,
        "admitted-model",
        Some(&instructions),
    )
    .unwrap();
    assert_eq!(command.get_program(), CLAUDE);
    assert_eq!(command.get_current_dir(), Some(project.as_path()));
    let args = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect::<Vec<_>>();
    for pair in [
        ["--model", "admitted-model"],
        ["--tools", ""],
        ["--max-turns", "1"],
        ["--setting-sources", ""],
    ] {
        assert!(args.windows(2).any(|found| found == pair), "{pair:?}");
    }
    let file = [
        "--append-system-prompt-file",
        instructions.to_str().unwrap(),
    ];
    assert!(args.windows(2).any(|found| found == file));
    assert!(args.contains(&"--no-session-persistence"));
    let home_text = home.to_str().unwrap();
    let environment = command
        .get_envs()
        .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        environment,
        BTreeMap::from([
            ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
            ("CLAUDE_CONFIG_DIR", home_text),
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR", home_text),
            ("HOME", home_text),
            ("TERM", "dumb"),
        ])
    );
    assert_eq!(refusal(&project, &home, None), None);
}

/// A Windows home that is missing, a link, or open to the Users group refuses the master
/// command, and a missing one is not made: `claude_host_profile_unavailable` for the
/// missing one, `claude_host_unsafe_directory` for the others.
#[test]
fn a_windows_claude_master_home_that_is_missing_linked_or_open_is_refused() {
    let (root, project, home, _) = turn();
    let missing = root.path().join("missing");
    assert_eq!(
        refusal(&project, &missing, None).as_deref(),
        Some("claude_host_profile_unavailable")
    );
    assert!(!missing.exists());
    let linked = root.path().join("linked");
    std::os::windows::fs::symlink_dir(&home, &linked).unwrap();
    assert_eq!(
        refusal(&project, &linked, None).as_deref(),
        Some("claude_host_unsafe_directory")
    );
    assert_eq!(refusal(&project, &home, None), None);
    grant_users_read(&home);
    assert_eq!(
        refusal(&project, &home, None).as_deref(),
        Some("claude_host_unsafe_directory")
    );
}

/// A Windows instruction file that is missing, relative, one the Users group can read, a
/// second name of another file or past the length bound refuses the master command:
/// `claude_host_instructions_unavailable` for the missing one,
/// `claude_host_unsafe_instructions` for the others; each starts from a private file that
/// passes. A runner's temporary folder is private to its user, so a file the row writes
/// there is opened to the Users group by name.
#[test]
fn a_windows_claude_master_instruction_file_that_is_open_linked_or_too_long_is_refused() {
    let (root, project, home, instructions) = turn();
    let unsafe_instructions = Some("claude_host_unsafe_instructions");
    assert_eq!(
        refusal(&project, &home, Some(&home.join("missing.md"))).as_deref(),
        Some("claude_host_instructions_unavailable")
    );
    assert_eq!(
        refusal(&project, &home, Some(Path::new("instructions.md"))).as_deref(),
        unsafe_instructions
    );
    let open = root.path().join("open.md");
    fs::write(&open, "instructions").unwrap();
    grant_users_read(&open);
    assert_eq!(
        refusal(&project, &home, Some(&open)).as_deref(),
        unsafe_instructions
    );
    assert_eq!(refusal(&project, &home, Some(&instructions)), None);
    let second = home.join("second.md");
    fs::hard_link(&instructions, &second).unwrap();
    assert_eq!(
        refusal(&project, &home, Some(&instructions)).as_deref(),
        unsafe_instructions
    );
    fs::remove_file(&second).unwrap();
    assert_eq!(refusal(&project, &home, Some(&instructions)), None);
    let long = home.join("long.md");
    let file = private::file(&long, true).unwrap();
    file.set_len(INSTRUCTIONS_BYTES_MAX + 1).unwrap();
    drop(file);
    drop(private::read_only(&long).unwrap());
    assert_eq!(
        refusal(&project, &home, Some(&long)).as_deref(),
        unsafe_instructions
    );
}

/// A Windows Claude Code master with its tools on stays refused as `claude_host_unavailable`
/// until its shell, background setting and team server mailbox work on Windows (S8).
#[test]
fn a_windows_claude_master_command_with_its_tools_on_stays_refused() {
    let (_root, project, home, instructions) = turn();
    let tools = MasterTools {
        engine: Path::new(r"C:\engine\crazyagent.exe"),
        drop: Path::new(r"C:\turn\tmp\drop"),
        mailbox: Path::new(r"C:\turn\tmp\mailbox"),
        off: &[],
    };
    assert_eq!(
        tools_launch_command(
            Path::new(CLAUDE),
            &project,
            &home,
            "admitted-model",
            Some(&instructions),
            tools,
        )
        .map(drop)
        .map_err(|error| error.0),
        Err("claude_host_unavailable".to_owned())
    );
}
