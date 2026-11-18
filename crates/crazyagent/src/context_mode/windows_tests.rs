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
/// and npm's own `node_modules\npm\bin\npm-cli.js` and `npx-cli.js`. Output: the case,
/// removed when dropped.
fn case() -> Case {
    let directory = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(directory.path()).unwrap();
    let root = base.join("engine");
    let bin = root.join("setup").join("bin");
    fs::create_dir_all(root.join("setup").join("npm")).unwrap();
    fs::create_dir_all(npm_cli(&bin).parent().unwrap()).unwrap();
    let npx_cli = npm_cli(&bin).with_file_name("npx-cli.js");
    for file in [
        bin.join("npx.cmd"),
        bin.join("node.exe"),
        npm_cli(&bin),
        npx_cli,
    ] {
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

/// What the boundary was asked to grant, for a failed assertion: npm's folder with its access
/// list as `icacls` prints it, and the boundary profile npm's step wrote beside the staging
/// folders. Inputs: npm's folder and the staging folders' parent. Output: the text.
fn granted(bin: &Path, npm: &Path) -> String {
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let listed = crate::spawn_gate::collected(
        std::process::Command::new(system.join("icacls.exe")).arg(bin),
    )
    .unwrap();
    let profile =
        fs::read_to_string(npm.join(format!("{PACKAGE}@{VERSION}.sb"))).unwrap_or_default();
    format!(
        "bin {}\nicacls: {}\nprofile: {profile}",
        bin.display(),
        String::from_utf8_lossy(&listed.stdout)
    )
}

/// WIN-1 (DEC-193): npm's staging folder is a job folder, so the boundary profile npm's
/// command writes holds no project guard and still hides the engine's secrets; the row fails
/// if `npm_command` stops marking the folder.
#[test]
fn the_npm_staging_folder_holds_no_project_guards() {
    use crazyagent_win_sandbox::profile::{BoundaryProfile, Guard};
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let work = staging_folder(&npm, &format!("{prefix}-work")).unwrap();
    let staged = staging_folder(&npm, &prefix).unwrap();
    assert_eq!(staged.parent(), Some(npm.as_path()));
    npm_command(&case.root, &staged, &work, &case.bin, VERSION).unwrap();
    let loaded = BoundaryProfile::load(&npm.join(format!("{prefix}.sb"))).unwrap();
    assert!(
        loaded
            .guarded
            .iter()
            .all(|item| item.guard != Guard::ReadOnly),
        "{:?}",
        loaded.guarded
    );
    assert!(
        loaded
            .guarded
            .iter()
            .any(|item| item.guard == Guard::Hidden)
    );
}

/// WIN-1 (DEC-193): inside its AppContainer boundary the runner's own npm installs a local
/// package into a fresh staging folder, a job folder with no project guards, so npm adds
/// `node_modules` at its top and only the pin refuses the unpinned tarball, by hand and
/// through `fill`. A failed hand install shows npm's output and the Node folder's grants.
#[test]
#[ignore = "V243-WIN-NPM-BOUNDARY-ACL"]
fn npm_installs_into_a_fresh_staging_folder() {
    let bin = system_bin();
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let work = staging_folder(&npm, &format!("{prefix}-work")).unwrap();
    let packing = work.join("tmp");
    private_folder(&packing).unwrap();
    let tarball = local_tarball(&packing);
    let bare = staging_folder(&npm, &prefix).unwrap();
    assert!(!bare.join(MODULES).exists());
    let installed = install_output(&case, &bare, &work, &bin, &tarball);
    assert!(
        installed.status.success(),
        "{}\n{}",
        shown(&installed),
        granted(&bin, &npm)
    );
    assert!(bare.join(MODULES).join(PACKAGE).join(PROGRAM).is_file());
    assert_eq!(
        checked(&bare, VERSION),
        Err("context_mode_integrity_mismatch")
    );
    let staged = staging_folder(&npm, &prefix).unwrap();
    assert!(!staged.join(MODULES).exists());
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
        "npm installs; only the pin refuses the tarball"
    );
    assert!(staged.join(MODULES).join(PACKAGE).join(PROGRAM).is_file());
}

/// V244-WIN-NPM: where Node.js's folder is one no app container reads and this standard user
/// cannot change, as on the test farm's runners, the node probe and the npm install both
/// refuse with Node.js's own code before the helper runs, never as a Node.js too old.
#[test]
#[ignore = "needs the farm runner's Node.js folder and a standard user (test farm)"]
fn a_node_folder_only_an_administrator_can_open_to_containers_is_refused_by_its_own_code() {
    let bin = system_bin();
    assert!(
        crazyagent_win_sandbox::unreadable_root(&bin, "crazyagent.test.node"),
        "{}",
        bin.display()
    );
    let case = case();
    let unreadable = InstallError("context_mode_node_folder_unreadable");
    assert_eq!(node_supported(&case.root, &bin), Err(unreadable));
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let work = staging_folder(&npm, &format!("{prefix}-work")).unwrap();
    let staged = staging_folder(&npm, &prefix).unwrap();
    let source = OsStr::new("context-mode.tgz");
    assert_eq!(
        fill(&case.root, &staged, &work, &bin, VERSION, source),
        Err(unreadable)
    );
    assert!(!staged.join(MODULES).exists());
}

/// The access list of a folder a job read or wrote, as `icacls` prints it, without the
/// inherited mark a rewritten list may change. Only lists this case owns are compared, since
/// other rows' containers come and go in the machine-wide Packages and grants folders while
/// the farm runs rows side by side. Input: the folder. Output: the list.
fn entries(folder: &Path) -> String {
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let listed = crate::spawn_gate::collected(
        std::process::Command::new(system.join("icacls.exe")).arg(folder),
    )
    .unwrap();
    assert!(listed.status.success(), "{}", shown(&listed));
    let list = String::from_utf8_lossy(&listed.stdout).replace("(I)", "");
    assert!(list.contains("(OI)(CI)"), "{list}");
    list
}

/// The grants record and the AppContainer folder Windows keeps for one container, by its
/// name, so a row checks only its own container. Input: the name. Output: both paths.
fn container_files(name: &str) -> (PathBuf, PathBuf) {
    let grants = crate::paths::Layout::current()
        .unwrap()
        .root()
        .join(crate::sandbox::windows::GRANTS_FOLDER);
    let packages = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap()).join("Packages");
    assert!(name.starts_with("crazyagent."));
    (
        grants.join(format!("container-{name}.grants")),
        packages.join(name),
    )
}

/// V243-WIN-CONTAINER-RELEASE: the node probe releases the container it ran `node.exe` in once
/// the probe ends, so npx's folder keeps no entry for it and no scratch folder is left; here
/// `node.exe` is empty, so the probe names no version.
#[test]
fn the_node_probe_leaves_no_container_behind() {
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let before = entries(&case.bin);
    assert_eq!(node_supported(&case.root, &case.bin), Ok(false));
    assert_eq!(entries(&case.bin), before);
    let left: Vec<_> = fs::read_dir(&npm)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != NODE_CHECK_LOCK)
        .collect();
    assert!(left.is_empty(), "no scratch folder is left: {left:?}");
}

/// V243-WIN-CONTAINER-RELEASE: an npm install releases its container before its staging
/// folder is published or removed, so a failed install, here with an empty `node.exe`, leaves
/// no entry on npx's folder, no boundary profile and no install folder.
#[test]
fn a_failed_npm_install_leaves_no_container_behind() {
    let case = case();
    let folder = install_folder(&case.root, VERSION);
    let before = entries(&case.bin);
    assert_eq!(
        replace(&case.root, &folder, &case.bin, VERSION),
        Err(InstallError("context_mode_install_failed"))
    );
    assert_eq!(entries(&case.bin), before);
    let parent = folder.parent().unwrap();
    assert!(!npm_profile(parent, VERSION).exists());
    assert!(!folder.exists());
}

/// V243-WIN-CONTAINER-RELEASE: npm's job gives its container an entry on the staging folder,
/// a grants record and an AppContainer folder even when `node.exe` cannot start, and the
/// release takes all three back with the profile, so a staging folder renamed into place
/// carries no grant.
#[test]
fn the_release_takes_back_what_the_npm_job_was_granted() {
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let work = staging_folder(&npm, &format!("{prefix}-work")).unwrap();
    let staged = staging_folder(&npm, &prefix).unwrap();
    let before = entries(&staged);
    let job = crate::sandbox::canonical(&staged).unwrap();
    let home = crate::sandbox::canonical(&work).unwrap().join("home");
    let (record, package) = container_files(&crate::sandbox::windows::container_for(&job, &home));
    let mut command = npm_command(&case.root, &staged, &work, &case.bin, VERSION).unwrap();
    let ran = crate::spawn_gate::collected(&mut command).unwrap();
    assert!(!ran.status.success(), "{}", shown(&ran));
    assert_ne!(entries(&staged), before, "{}", shown(&ran));
    assert!(record.is_file(), "{}", shown(&ran));
    assert!(package.is_dir(), "{}", shown(&ran));
    assert!(os::release(&npm_profile(&npm, VERSION), &job, &home));
    assert_eq!(entries(&staged), before);
    assert!(!record.exists());
    assert!(!package.exists());
    assert!(!npm_profile(&npm, VERSION).exists());
}

/// V243-WIN-CONTAINER-RELEASE: a release that cannot go ahead logs and reports false without
/// stopping the engine: a profile path Windows cannot read, and a profile naming another
/// container than the job's folders give, which is kept with nothing of it released.
#[test]
fn a_refused_release_reports_false_and_keeps_the_profile() {
    let case = case();
    let base = crate::sandbox::canonical(&case.root).unwrap();
    let folders = ["project", "home", "tmp"].map(|name| base.join("job").join(name));
    for folder in &folders {
        fs::create_dir_all(folder).unwrap();
    }
    let grants = base.join("grants");
    fs::create_dir_all(&grants).unwrap();
    assert!(!os::release(
        Path::new("C:\\no\0such.sb"),
        &folders[0],
        &folders[1]
    ));
    let sandbox = crate::sandbox::ProjectSandbox::new(
        &folders[0],
        &folders[1],
        &folders[2],
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let mut forged = crate::sandbox::windows::boundary_profile(&sandbox, &grants).unwrap();
    let derived = crate::sandbox::windows::container_for(&folders[0], &folders[1]);
    assert_eq!(forged.container, derived);
    forged.container = format!("crazyagent.{}", "0".repeat(32));
    let profile = base.join("forged.sb");
    fs::write(&profile, forged.encode().unwrap()).unwrap();
    assert!(!os::release(&profile, &folders[0], &folders[1]));
    assert!(profile.is_file());
    assert!(!grants.join(format!("container-{derived}.lock")).exists());
}

/// Runs a node probe's job in a fresh scratch folder and stops before its release, as a probe
/// that crashed there would. Input: the case. Output: the scratch folder, the grants record
/// and the AppContainer folder its job left.
fn crashed_probe(case: &Case) -> (PathBuf, PathBuf, PathBuf) {
    let npm = case.root.join("setup").join("npm");
    let scratch = staging_folder(&npm, NODE_CHECK).unwrap();
    let base = crate::sandbox::canonical(&scratch).unwrap();
    let name = crate::sandbox::windows::container_for(&base.join("project"), &base.join("home"));
    let (record, package) = container_files(&name);
    let mut command = node_command(&case.root, &scratch, &case.bin).unwrap();
    let ran = crate::spawn_gate::collected(&mut command).unwrap();
    assert!(record.is_file(), "{}", shown(&ran));
    assert!(package.is_dir(), "{}", shown(&ran));
    (scratch, record, package)
}

/// V243-WIN-CONTAINER-CRASH-LEFTOVER: a node probe that crashed before its release left its
/// container; the next probe, with no other probe running, releases it, so npx's folder keeps
/// no entry for it and its record, AppContainer folder and scratch folder are gone.
#[test]
fn a_node_probe_releases_the_container_a_crashed_probe_left() {
    let case = case();
    let before = entries(&case.bin);
    let (scratch, record, package) = crashed_probe(&case);
    assert_ne!(entries(&case.bin), before);
    assert_eq!(node_supported(&case.root, &case.bin), Ok(false));
    assert_eq!(entries(&case.bin), before);
    assert!(!record.exists());
    assert!(!package.exists());
    assert!(!scratch.exists());
}

/// V243-WIN-CONTAINER-CRASH-LEFTOVER: while another probe holds the check lock, a probe
/// releases no container of a scratch folder it finds, since it may be that probe's live one;
/// once the lock is free the next probe releases it.
#[test]
fn a_node_probe_leaves_a_live_probe_container_alone() {
    let case = case();
    let (scratch, record, package) = crashed_probe(&case);
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(case.root.join("setup").join("npm").join("node-check.lock"))
        .unwrap();
    held.lock_shared().unwrap();
    assert_eq!(node_supported(&case.root, &case.bin), Ok(false));
    assert!(record.is_file());
    assert!(package.is_dir());
    assert!(scratch.is_dir());
    held.unlock().unwrap();
    drop(held);
    assert_eq!(node_supported(&case.root, &case.bin), Ok(false));
    assert!(!record.exists());
    assert!(!package.exists());
    assert!(!scratch.exists());
}

/// V243-WIN-CONTAINER-CRASH-LEFTOVER: an npm install that crashed before its release left its
/// staging and work folders, paired by one random suffix, and its container; the next install
/// of that version, under the version's lock, releases that container before it removes the
/// folders, so npx's folder keeps no entry for it.
#[test]
fn an_npm_install_releases_the_container_a_crashed_install_left() {
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let before = entries(&case.bin);
    let staged = staging_folder(&npm, &prefix).unwrap();
    let random = staged.file_name().unwrap().to_str().unwrap()[prefix.len() + 2..].to_owned();
    let work = npm.join(format!(".{prefix}-work-{random}"));
    fs::create_dir(&work).unwrap();
    let job = crate::sandbox::canonical(&staged).unwrap();
    let home = crate::sandbox::canonical(&work).unwrap().join("home");
    let (record, package) = container_files(&crate::sandbox::windows::container_for(&job, &home));
    let mut command = npm_command(&case.root, &staged, &work, &case.bin, VERSION).unwrap();
    let ran = crate::spawn_gate::collected(&mut command).unwrap();
    assert!(record.is_file(), "{}", shown(&ran));
    assert!(package.is_dir(), "{}", shown(&ran));
    assert_ne!(entries(&case.bin), before);
    let folder = install_folder(&case.root, VERSION);
    assert_eq!(
        replace(&case.root, &folder, &case.bin, VERSION),
        Err(InstallError("context_mode_install_failed"))
    );
    assert_eq!(entries(&case.bin), before);
    assert!(!record.exists());
    assert!(!package.exists());
    assert!(!staged.exists());
    assert!(!work.exists());
}

/// V243-WIN-CONTAINER-CRASH-LEFTOVER: an install's work folder carries its staging folder's
/// random suffix, so a later install can pair the two, and is never one that exists.
#[test]
fn an_install_work_folder_is_paired_with_its_staging_folder_by_name() {
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let staged = staging_folder(&npm, &prefix).unwrap();
    let random = staged.file_name().unwrap().to_str().unwrap()[prefix.len() + 2..].to_owned();
    let work = work_folder(&staged, &prefix).unwrap();
    assert_eq!(work, npm.join(format!(".{prefix}-work-{random}")));
    assert!(work.is_dir());
    assert_eq!(
        work_folder(&staged, &prefix),
        Err(InstallError("setup_store_unavailable"))
    );
}

/// V243-WIN-CONTAINER-CRASH-LEFTOVER: a left pair whose version profile names another
/// container is not released by the profile's word: the release refuses and keeps the
/// profile, nothing of that container is touched, and both folders are still removed.
#[test]
fn a_left_pair_the_profile_does_not_name_is_refused_and_still_removed() {
    let case = case();
    let npm = case.root.join("setup").join("npm");
    let prefix = format!("{PACKAGE}@{VERSION}");
    let staged = staging_folder(&npm, &prefix).unwrap();
    let work = work_folder(&staged, &prefix).unwrap();
    let (job, home) = npm_job(&staged, &work).unwrap();
    let tmp = crate::sandbox::canonical(&work).unwrap().join("tmp");
    for folder in [&home, &tmp] {
        fs::create_dir_all(folder).unwrap();
    }
    let grants = case.root.join("grants");
    fs::create_dir_all(&grants).unwrap();
    let sandbox = crate::sandbox::ProjectSandbox::new(
        &job,
        &home,
        &tmp,
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let mut forged = crate::sandbox::windows::boundary_profile(&sandbox, &grants).unwrap();
    assert_eq!(
        forged.container,
        crate::sandbox::windows::container_for(&job, &home)
    );
    forged.container = format!("crazyagent.{}", "0".repeat(32));
    let profile = npm_profile(&npm, VERSION);
    fs::write(&profile, forged.encode().unwrap()).unwrap();
    remove_staged(&npm, &prefix, VERSION);
    assert!(profile.is_file(), "a refused release keeps the profile");
    assert!(
        !grants
            .join(format!("container-{}.lock", forged.container))
            .exists()
    );
    assert!(!staged.exists());
    assert!(!work.exists());
}
