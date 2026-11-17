use super::*;
use crate::plugins::ContextModeOff;
use std::fs;

const VERSION: &str = "1.0.169";
const PINNED: &str = "sha512-94JIaFuLjF9SO2BsGTrbGtyT44K95+9OC8BdbaL/UT76xOkanJLfUR5CzmNw+GELXZQqH4nBrKg9wjBnSFkVnQ==";

struct Case {
    _directory: tempfile::TempDir,
    root: PathBuf,
    bin: PathBuf,
}

/// Makes an engine root with its `setup\npm` folder and its own `setup\bin`, a folder the
/// engine's tool resolver looks in, holding the Windows Node layout: `npx.cmd`, `node.exe`
/// and npm's own `node_modules\npm\bin\npm-cli.js`. Output: the case, removed when dropped.
fn case() -> Case {
    let directory = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(directory.path()).unwrap();
    let root = base.join("engine");
    let bin = root.join("setup").join("bin");
    fs::create_dir_all(root.join("setup").join("npm")).unwrap();
    fs::create_dir_all(npm_cli(&bin).parent().unwrap()).unwrap();
    for file in [bin.join("npx.cmd"), bin.join("node.exe"), npm_cli(&bin)] {
        fs::write(file, "").unwrap();
    }
    Case {
        _directory: directory,
        root,
        bin,
    }
}

/// npm's own program in a Windows Node folder. Input: the folder. Output: its path.
fn npm_cli(bin: &Path) -> PathBuf {
    bin.join("node_modules")
        .join("npm")
        .join("bin")
        .join("npm-cli.js")
}

/// Writes a finished install of the pinned version as npm leaves it, with the given program
/// bytes and npm's hidden lock holding the recorded tarball hash. Inputs: the install folder,
/// the recorded hash and the program text. Output: none.
fn finished(folder: &Path, integrity: &str, program: &str) {
    let package = folder.join("node_modules").join("context-mode");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"context-mode\",\"version\":\"{VERSION}\"}}"),
    )
    .unwrap();
    fs::write(package.join("cli.bundle.mjs"), program).unwrap();
    let lock = serde_json::json!({
        "packages": {"node_modules/context-mode": {"version": VERSION, "integrity": integrity}}
    });
    fs::write(
        folder.join("node_modules").join(".package-lock.json"),
        lock.to_string(),
    )
    .unwrap();
}

/// The default pack's pinned launch with the case's `npx.cmd`. Input: the case. Output: the
/// spec.
fn npx_in(case: &Case) -> ServerSpec {
    ServerSpec::Stdio {
        command: case.bin.join("npx.cmd").to_str().unwrap().to_owned(),
        args: vec!["-y".into(), format!("context-mode@{VERSION}")],
    }
}

/// WIN-CONTEXT-MODE: `node.exe` beside npx runs with no execute bits, which Windows does
/// not have; a folder or a missing path does not.
#[test]
fn node_exe_beside_npx_is_runnable_without_execute_bits() {
    let case = case();
    assert_eq!(NODE, "node.exe");
    assert!(runnable(&case.bin.join(NODE)));
    assert!(!runnable(&case.bin));
    assert!(!runnable(&case.bin.join("missing.exe")));
}

/// WIN-CONTEXT-MODE: npm runs as `node.exe` with npm's own `npm-cli.js`, never `npm.cmd`,
/// which only cmd.exe starts and which re-parses its arguments; with only `npm.cmd` beside
/// npx, npm counts as missing.
#[test]
fn npm_runs_through_node_exe_and_npm_cli_never_npm_cmd() {
    let case = case();
    assert!(npm_ready(&case.bin));
    let cwd = case.root.join("setup").join("npm");
    let npm = os::npm_invocation(&case.bin, &cwd);
    assert_eq!(npm.get_program(), case.bin.join(NODE).as_os_str());
    assert_eq!(
        npm.get_args().collect::<Vec<_>>(),
        [npm_cli(&case.bin).as_os_str()]
    );
    assert_eq!(npm.get_current_dir(), Some(cwd.as_path()));
    assert!(npm.get_envs().any(|(name, value)| {
        name == "npm_config_update_notifier" && value == Some("false".as_ref())
    }));
    fs::remove_file(npm_cli(&case.bin)).unwrap();
    fs::write(case.bin.join("npm.cmd"), "").unwrap();
    assert!(!npm_ready(&case.bin), "npm.cmd alone never runs");
}

/// WIN-CONTEXT-MODE: the AppContainer refuses every endpoint limit, so the install step's
/// npm gets the open network on Windows, for that step only, while `node --version` keeps
/// no network (leader 04:22 and 04:36, answer A).
#[test]
fn the_install_step_gets_the_open_network_on_windows() {
    assert_eq!(os::npm_network(), crate::sandbox::SandboxNetwork::Open);
}

/// WIN-CONTEXT-MODE: the program hash never reads through a link at the program path, and a
/// folder has none.
#[test]
fn the_program_hash_never_reads_through_a_link() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("cli.bundle.mjs");
    let link = directory.path().join("link.mjs");
    fs::write(&file, "").unwrap();
    std::os::windows::fs::symlink_file(&file, &link).unwrap();
    assert!(program_hash(&file).is_some());
    assert_eq!(program_hash(&link), None, "a link is never read");
    assert_eq!(program_hash(directory.path()), None, "a folder has no hash");
}

/// WIN-CONTEXT-MODE: the install lock is held until it is dropped, so a second handle
/// cannot take it meanwhile, and a link at the lock path is refused.
#[test]
fn the_install_lock_is_exclusive_until_dropped_and_refuses_a_link() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("context-mode@1.0.169.lock");
    let held = lock(&path).unwrap();
    let other = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert!(matches!(
        other.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(held);
    assert!(other.try_lock().is_ok());
    let link = directory.path().join("linked.lock");
    std::os::windows::fs::symlink_file(&path, &link).unwrap();
    assert!(lock(&link).is_err(), "a link at the lock path is refused");
}

/// WIN-CONTEXT-MODE: each staging folder is new and hidden under its parent, and a file
/// where a private folder should be is refused.
#[test]
fn each_staging_folder_is_new_and_a_file_is_no_private_folder() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("npm");
    let first = staging_folder(&parent, "context-mode@1.0.169").unwrap();
    let second = staging_folder(&parent, "context-mode@1.0.169").unwrap();
    assert_ne!(first, second);
    for made in [&first, &second] {
        assert!(made.is_dir());
        let name = made.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(".context-mode@1.0.169-"), "{name}");
    }
    let file = directory.path().join("file");
    fs::write(&file, "").unwrap();
    assert!(private_folder(&file).is_err());
}

/// WIN-CONTEXT-MODE: a bounded step hands back whether it passed and its output, so
/// `node --version` can be read.
#[test]
fn a_bounded_step_reports_its_exit_and_output() {
    let mut passing = std::process::Command::new("cmd.exe");
    passing.args(["/c", "echo v24.0.0"]);
    let (ok, output) = run_command(passing, Duration::from_secs(10)).unwrap();
    assert!(ok);
    assert!(supported_node(&output), "{output:?}");
    let mut failing = std::process::Command::new("cmd.exe");
    failing.args(["/c", "exit 3"]);
    assert!(!run_command(failing, Duration::from_secs(10)).unwrap().0);
}

/// WIN-CONTEXT-MODE: npm's install step runs with its output thrown away, so a long report
/// never fills a pipe nobody reads before the step ends; it still reports pass or fail.
#[test]
fn a_quiet_step_reports_its_exit_without_reading_its_output() {
    let mut loud = std::process::Command::new("cmd.exe");
    loud.args([
        "/c",
        "for /l %i in (1,1,2000) do @echo context-mode-install-line",
    ]);
    assert_eq!(run_quiet(loud, Duration::from_secs(60)), Ok(true));
    let mut failing = std::process::Command::new("cmd.exe");
    failing.args(["/c", "exit 3"]);
    assert_eq!(run_quiet(failing, Duration::from_secs(10)), Ok(false));
}

/// WIN-CONTEXT-MODE: an install checked against both pins runs as `node.exe` beside npx
/// with the absolute cli.bundle.mjs, from an agent launch too; a changed program never runs
/// and the launch says the files did not match, never npx. npm is taken away first, so the
/// new install this asks for is refused before any `node.exe` starts.
#[test]
fn a_checked_install_runs_with_node_exe_and_a_changed_program_never_runs() {
    let case = case();
    let folder = install_folder(&case.root, VERSION);
    finished(&folder, PINNED, "");
    let node = case.bin.join(NODE);
    let program = folder
        .join("node_modules")
        .join("context-mode")
        .join("cli.bundle.mjs");
    let wanted = ServerSpec::Stdio {
        command: node.to_str().unwrap().to_owned(),
        args: vec![program.to_str().unwrap().to_owned()],
    };
    assert_eq!(shared(&folder, &node, VERSION), Some(wanted.clone()));
    assert_eq!(shared_install(&wanted), Some(folder.as_path()));
    assert_eq!(for_launch(&case.root, npx_in(&case)), Ok(wanted));
    fs::write(&program, "changed").unwrap();
    assert_eq!(
        checked(&folder, VERSION),
        Err("context_mode_program_mismatch")
    );
    assert_eq!(shared(&folder, &node, VERSION), None);
    fs::remove_file(npm_cli(&case.bin)).unwrap();
    assert_eq!(
        for_launch(&case.root, npx_in(&case)),
        Err(ContextModeOff::Mismatch)
    );
}

/// WIN-CONTEXT-MODE: every spec other than the pinned launch through an npx the resolver
/// finds runs as approved on Windows: another version or server, a bare `npx` and a `node`.
#[test]
fn any_other_server_launches_as_approved_on_windows() {
    let case = case();
    let npx = case.bin.join("npx.cmd").to_str().unwrap().to_owned();
    let stdio = |command: &str, package: &str| ServerSpec::Stdio {
        command: command.into(),
        args: vec!["-y".into(), package.into()],
    };
    for other in [
        stdio(&npx, "context-mode@latest"),
        stdio(&npx, "other-server@1.0.0"),
        stdio("npx", "context-mode@1.0.169"),
        stdio(r"C:\Program Files\nodejs\node.exe", "context-mode@1.0.169"),
    ] {
        assert_eq!(for_launch(&case.root, other.clone()), Ok(other));
    }
}

/// DEC-185, WIN-CONTEXT-MODE r1 F1: the Windows node check and npm step take strict off and
/// the call supervisor's default whatever the owner stored, since only Linux reads them.
#[test]
fn the_windows_install_steps_take_strict_off_whatever_is_stored() {
    use crate::sandbox::{LinuxStrict, LinuxSupervisor};
    use crate::sandbox_settings::SandboxSettings;
    let case = case();
    let connection = rusqlite::Connection::open(case.root.join("engine.db")).unwrap();
    crate::sandbox_settings::initialize(&connection).unwrap();
    let stored = SandboxSettings {
        linux_supervisor: LinuxSupervisor::Off,
        linux_strict: LinuxStrict::On,
        ..SandboxSettings::default()
    };
    crate::sandbox_settings::save(&connection, &stored).unwrap();
    drop(connection);
    let taken = os::boundary_settings(&case.root).unwrap();
    assert_eq!(
        (taken.linux_supervisor, taken.linux_strict),
        (LinuxSupervisor::On, LinuxStrict::Off)
    );
}

/// The runner's own Node folder: the first folder on PATH holding `node.exe` and npm's own
/// `npm-cli.js`, canonical as the resolver hands it. Output: the folder; the row fails with
/// UNAVAILABLE when PATH has none.
fn system_bin() -> PathBuf {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .find(|folder| folder.join(NODE).is_file() && npm_cli(folder).is_file())
    });
    let Some(bin) = found else {
        panic!("UNAVAILABLE: node.exe and npm's npm-cli.js on PATH");
    };
    let bin = crate::sandbox::canonical(&bin).unwrap();
    assert!(bin.join(NODE).is_file());
    bin
}

/// Packs a package named context-mode at the pinned version, its `package.json` and an empty
/// program, with Windows' own `tar.exe`. Input: the folder to pack in. Output: the tarball.
fn local_tarball(folder: &Path) -> PathBuf {
    let package = folder.join("package");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"{PACKAGE}\",\"version\":\"{VERSION}\"}}"),
    )
    .unwrap();
    fs::write(package.join(PROGRAM), "").unwrap();
    let tarball = folder.join(format!("{PACKAGE}-{VERSION}.tgz"));
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let packed = crate::spawn_gate::status(
        std::process::Command::new(system.join("tar.exe"))
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(folder)
            .arg("package"),
    )
    .unwrap();
    assert!(packed.success());
    assert!(tarball.is_file());
    tarball
}

/// The install step `fill` runs, with its arguments, for a staging folder the caller
/// prepared, with npm's output kept so a refusal shows npm's own reason. Inputs: the case,
/// the staging and work folders, npm's folder and the tarball. Output: npm's exit and output.
fn install_output(
    case: &Case,
    staged: &Path,
    work: &Path,
    bin: &Path,
    tarball: &Path,
) -> std::process::Output {
    let mut command = npm_command(&case.root, staged, work, bin, VERSION).unwrap();
    command
        .arg("install")
        .arg("--prefix")
        .arg(staged)
        .args([
            "--no-audit",
            "--no-fund",
            "--no-update-notifier",
            "--loglevel=error",
            "--ignore-scripts",
            "--no-save",
        ])
        .arg(tarball);
    crate::spawn_gate::collected(&mut command).unwrap()
}

/// npm's exit and the first 4 KiB of each of its streams, for a failed assertion. Input:
/// the output. Output: the text.
fn shown(output: &std::process::Output) -> String {
    let text = |bytes: &[u8]| String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).into_owned();
    format!(
        "{:?}\nstdout: {}\nstderr: {}",
        output.status,
        text(&output.stdout),
        text(&output.stderr)
    )
}

/// WIN-1 (DEC-193): inside its AppContainer boundary the runner's own npm installs a local
/// package into a fresh staging folder once its `node_modules` is there, so only the pin
/// refuses the unpinned tarball, by hand and through `fill`. The same install into a staging
/// folder without `node_modules` is refused, since its root lets npm add entries only inside
/// folders already there, so the row fails if `fill` stops making it.
#[test]
fn npm_installs_into_a_fresh_staging_folder_through_the_node_modules_made_first() {
    let bin = system_bin();
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let work = staging_folder(&npm, &format!("{prefix}-work")).unwrap();
    let packing = work.join("tmp");
    private_folder(&packing).unwrap();
    let tarball = local_tarball(&packing);
    let made = staging_folder(&npm, &prefix).unwrap();
    fs::create_dir(made.join(MODULES)).unwrap();
    let installed = install_output(&case, &made, &work, &bin, &tarball);
    assert!(installed.status.success(), "{}", shown(&installed));
    assert_eq!(
        checked(&made, VERSION),
        Err("context_mode_integrity_mismatch")
    );
    let staged = staging_folder(&npm, &prefix).unwrap();
    assert_eq!(
        fill(
            &case.root,
            &staged,
            &work,
            &bin,
            VERSION,
            tarball.as_os_str()
        ),
        Err(InstallError("context_mode_integrity_mismatch")),
        "fill makes node_modules and npm installs; only the pin refuses the tarball"
    );
    assert!(staged.join(MODULES).join(PACKAGE).join(PROGRAM).is_file());
    let bare = staging_folder(&npm, &prefix).unwrap();
    let refused = install_output(&case, &bare, &work, &bin, &tarball);
    assert!(!refused.status.success(), "{}", shown(&refused));
    assert!(!bare.join(MODULES).exists());
}
