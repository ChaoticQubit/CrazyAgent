use super::*;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: &str = "1.0.169";
const PINNED: &str = "sha512-94JIaFuLjF9SO2BsGTrbGtyT44K95+9OC8BdbaL/UT76xOkanJLfUR5CzmNw+GELXZQqH4nBrKg9wjBnSFkVnQ==";
const OTHER: &str = "sha512-AAAAaFuLjF9SO2BsGTrbGtyT44K95+9OC8BdbaL/UT76xOkanJLfUR5CzmNw+GELXZQqH4nBrKg9wjBnSFkVnQ==";
const FAILED: &str = "context-mode@1.0.169.failed";
const PROFILE: &str = "context-mode@1.0.169.sb";

struct Case {
    _directory: tempfile::TempDir,
    #[cfg(target_os = "macos")]
    base: PathBuf,
    root: PathBuf,
    bin: PathBuf,
}

/// Makes a private engine root with its `setup/npm` folder and a folder holding `npm` and
/// `node` links to real system programs, so npm never reaches a registry. Input: the program
/// `npm` runs. Output: the case, removed when dropped.
fn case(npm: &str) -> Case {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("engine");
    let bin = base.join("bin");
    for folder in [root.join("setup").join("npm"), bin.clone()] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)
            .unwrap();
    }
    std::os::unix::fs::symlink(npm, bin.join("npm")).unwrap();
    std::os::unix::fs::symlink("/usr/bin/true", bin.join("node")).unwrap();
    Case {
        _directory: directory,
        #[cfg(target_os = "macos")]
        base,
        root,
        bin,
    }
}

impl Case {
    fn npm(&self) -> PathBuf {
        self.root.join("setup").join("npm")
    }

    fn folder(&self) -> PathBuf {
        install_folder(&self.root, VERSION)
    }

    fn install(&self) -> Result<PathBuf, InstallError> {
        install(&self.root, &self.bin, VERSION)
    }

    /// Lists the hidden staging and work folders installs left in `setup/npm`.
    fn staged(&self) -> Vec<String> {
        fs::read_dir(self.npm())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".context-mode@"))
            .collect()
    }
}

/// Writes a finished install of the pinned version as npm leaves it: the manifest, the
/// program and npm's hidden lock with the tarball hash it recorded. Inputs: the install
/// folder and the recorded hash. Output: none.
fn finished(folder: &Path, integrity: &str) {
    let package = folder.join("node_modules").join("context-mode");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        format!("{{\"name\":\"context-mode\",\"version\":\"{VERSION}\"}}"),
    )
    .unwrap();
    fs::write(package.join("cli.bundle.mjs"), "").unwrap();
    let lock = serde_json::json!({
        "packages": {"node_modules/context-mode": {"version": VERSION, "integrity": integrity}}
    });
    fs::write(
        folder.join("node_modules").join(".package-lock.json"),
        lock.to_string(),
    )
    .unwrap();
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn npx(package: &str) -> ServerSpec {
    ServerSpec::Stdio {
        command: "/opt/homebrew/bin/npx".into(),
        args: vec!["-y".into(), package.into()],
    }
}

#[test]
fn only_an_exact_version_of_context_mode_is_pinned() {
    for exact in ["context-mode@1.0.169", "context-mode@2.10.0-beta.1"] {
        assert!(pinned(&npx(exact)).is_some(), "{exact}");
    }
    for loose in [
        "context-mode@latest",
        "context-mode@1.0",
        "context-mode@v1.0.169",
        "context-mode@1.0.169+build",
        "context-mode@01.0.169x",
        "context-mode@^1.0.169",
    ] {
        assert!(
            pinned(&npx(loose)).is_none(),
            "a tag or range never installs: {loose}"
        );
    }
}

#[test]
fn an_install_whose_recorded_tarball_hash_differs_from_the_pin_is_not_complete() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().join(format!("context-mode@{VERSION}"));
    finished(&folder, OTHER);
    assert!(
        !complete(&folder, VERSION),
        "a tarball other than the pinned one is never run"
    );
    finished(&folder, PINNED);
    assert!(complete(&folder, VERSION));
    fs::remove_file(folder.join("node_modules").join(".package-lock.json")).unwrap();
    assert!(
        !complete(&folder, VERSION),
        "an install without npm's record of the tarball is not complete"
    );
}

#[test]
fn an_install_already_complete_is_returned_without_running_npm() {
    let case = case("/usr/bin/false");
    finished(&case.folder(), PINNED);
    assert_eq!(case.install().unwrap(), case.folder());
    assert!(case.staged().is_empty());
    assert!(!case.npm().join(PROFILE).exists(), "npm never started");
}

#[test]
fn a_complete_install_is_never_replaced() {
    let case = case("/usr/bin/false");
    finished(&case.folder(), PINNED);
    let program = case
        .folder()
        .join("node_modules/context-mode/cli.bundle.mjs");
    let before = fs::metadata(&program).unwrap().ino();
    fs::write(case.npm().join(FAILED), now().to_string()).unwrap();
    for _ in 0..2 {
        assert_eq!(case.install().unwrap(), case.folder());
    }
    assert_eq!(fs::metadata(&program).unwrap().ino(), before);
    assert!(case.staged().is_empty());
}

#[test]
fn a_second_installer_waits_for_the_lock_then_finds_the_install_complete() {
    let case = case("/usr/bin/false");
    let held = lock(&case.npm().join(format!("context-mode@{VERSION}.lock"))).unwrap();
    let (root, bin) = (case.root.clone(), case.bin.clone());
    let waiting = std::thread::spawn(move || install(&root, &bin, VERSION));
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !waiting.is_finished(),
        "the second installer waits on the lock"
    );
    finished(&case.folder(), PINNED);
    drop(held);
    assert_eq!(waiting.join().unwrap().unwrap(), case.folder());
    assert!(case.staged().is_empty());
    assert!(!case.npm().join(PROFILE).exists(), "npm never started");
}

#[cfg(target_os = "macos")]
#[test]
fn a_stale_staging_folder_is_removed_under_the_lock() {
    let case = case("/usr/bin/false");
    let stale = case.npm().join(format!(".context-mode@{VERSION}-stale"));
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("left"), "from a crashed install").unwrap();
    assert_eq!(case.install().unwrap_err().0, "context_mode_install_failed");
    assert!(!stale.exists());
    assert!(case.staged().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_npm_run_leaves_no_staging_folder_and_records_when_it_failed() {
    let case = case("/usr/bin/false");
    let before = now();
    assert_eq!(case.install().unwrap_err().0, "context_mode_install_failed");
    assert!(case.staged().is_empty(), "{:?}", case.staged());
    assert!(fs::symlink_metadata(case.folder()).is_err());
    let failed: u64 = fs::read_to_string(case.npm().join(FAILED))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!((before..=now()).contains(&failed));
    let profile = fs::read_to_string(case.npm().join(PROFILE)).unwrap();
    assert!(
        profile.contains("(remote ip \"*:443\")"),
        "npm reaches HTTPS: {profile}"
    );
    assert!(
        !profile.contains("(remote ip \"*:*\")"),
        "npm reaches nothing but HTTPS"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_install_is_not_retried_until_its_backoff_ends() {
    let case = case("/usr/bin/false");
    fs::write(case.npm().join(FAILED), now().to_string()).unwrap();
    assert_eq!(
        case.install().unwrap_err().0,
        "context_mode_install_backoff"
    );
    assert!(case.staged().is_empty());
    assert!(!case.npm().join(PROFILE).exists(), "npm never started");
    fs::write(case.npm().join(FAILED), (now() - 3600).to_string()).unwrap();
    assert_eq!(
        case.install().unwrap_err().0,
        "context_mode_install_failed",
        "after the backoff npm runs again"
    );
    assert!(case.npm().join(PROFILE).exists());
}

#[cfg(target_os = "macos")]
#[test]
fn whatever_is_left_incomplete_at_the_install_path_is_removed_under_the_lock() {
    let edited = case("/usr/bin/false");
    finished(&edited.folder(), PINNED);
    fs::write(
        edited
            .folder()
            .join("node_modules/context-mode/package.json"),
        "{\"name\":\"context-mode\",\"version\":\"9.9.9\"}",
    )
    .unwrap();
    assert!(edited.install().is_err());
    assert!(
        fs::symlink_metadata(edited.folder()).is_err(),
        "an incomplete folder never blocks the next install"
    );

    let linked = case("/usr/bin/false");
    let outside = linked.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("kept"), "not the engine's").unwrap();
    std::os::unix::fs::symlink(&outside, linked.folder()).unwrap();
    assert!(linked.install().is_err());
    assert!(fs::symlink_metadata(linked.folder()).is_err());
    assert!(
        outside.join("kept").exists(),
        "only the link is removed, never what it points at"
    );
}

/// Finds the folder holding the real `npm` and `node` on PATH. Output: the folder; a run
/// without npm on PATH is reported unavailable.
#[cfg(target_os = "macos")]
fn system_bin() -> PathBuf {
    let Some(npm) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|folder| folder.join("npm"))
            .find(|candidate| candidate.is_file())
    }) else {
        panic!("UNAVAILABLE: npm and node on PATH");
    };
    npm.parent().unwrap().to_path_buf()
}

#[cfg(target_os = "macos")]
#[test]
fn npm_starts_under_the_install_sandbox_profile() {
    let bin = system_bin();
    let case = case("/usr/bin/false");
    let prefix = format!("context-mode@{VERSION}");
    let staged = staging_folder(&case.npm(), &prefix).unwrap();
    let work = staging_folder(&case.npm(), &format!("{prefix}-work")).unwrap();
    let mut command = npm_command(&case.root, &staged, &work, &bin, VERSION).unwrap();
    command.arg("--version");
    let (ok, output) = run_command(command, Duration::from_secs(60)).unwrap();
    let version = String::from_utf8(output).unwrap();
    assert!(ok, "npm runs under the profile npm install uses: {version}");
    let numbers: Vec<&str> = version.trim().split('.').collect();
    assert_eq!(numbers.len(), 3, "{version}");
    assert!(
        numbers
            .iter()
            .all(|number| number.bytes().all(|b| b.is_ascii_digit()))
    );
    let profile = fs::read_to_string(case.npm().join(PROFILE)).unwrap();
    assert!(profile.contains("(remote ip \"*:443\")"));
}

#[cfg(target_os = "macos")]
const FORGER: &str = r#"const fs = require("node:fs");
const path = require("node:path");
const { spawn } = require("node:child_process");
const staged = path.resolve("../..");
if (process.argv[2] !== "later") {
  fs.writeFileSync(path.join(staged, "script-ran"), "");
  spawn(process.execPath, [__filename, "later"], { detached: true, stdio: "ignore" }).unref();
} else {
  const lock = path.join(staged, "node_modules", ".package-lock.json");
  const end = Date.now() + 20000;
  const forge = () => {
    if (Date.now() > end || !fs.existsSync(staged)) return;
    try {
      const data = JSON.parse(fs.readFileSync(lock, "utf8"));
      const entry = data.packages["node_modules/context-mode"];
      if (entry.integrity !== "PINNED_HASH") {
        entry.integrity = "PINNED_HASH";
        fs.writeFileSync(lock + ".forged", JSON.stringify(data));
        fs.renameSync(lock + ".forged", lock);
      }
    } catch {}
    setTimeout(forge, 1);
  };
  forge();
}
"#;

/// Packs a real npm package with the pinned context-mode name and version but other
/// bytes. Its postinstall marks that it ran, then starts a detached process outside npm's
/// process group that rewrites npm's record of the tarball hash to the pinned hash once
/// npm writes it. Input: the folder to pack in. Output: the tarball path.
#[cfg(target_os = "macos")]
fn forging_tarball(folder: &Path) -> PathBuf {
    let package = folder.join("package");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("package.json"),
        format!(
            "{{\"name\":\"context-mode\",\"version\":\"{VERSION}\",\"scripts\":{{\"postinstall\":\"node forge.cjs\"}}}}"
        ),
    )
    .unwrap();
    fs::write(package.join("cli.bundle.mjs"), "").unwrap();
    fs::write(
        package.join("forge.cjs"),
        FORGER.replace("PINNED_HASH", PINNED),
    )
    .unwrap();
    let tarball = folder.join(format!("context-mode-{VERSION}.tgz"));
    let packed = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(folder)
            .arg("package"),
    )
    .unwrap();
    assert!(packed.success());
    tarball
}

#[cfg(target_os = "macos")]
#[test]
fn no_package_script_runs_before_npm_s_record_of_the_tarball_hash_is_checked_against_the_pin() {
    let bin = system_bin();
    let case = case("/usr/bin/false");
    let prefix = format!("context-mode@{VERSION}");
    let staged = staging_folder(&case.npm(), &prefix).unwrap();
    let work = staging_folder(&case.npm(), &format!("{prefix}-work")).unwrap();
    let packing = work.join("tmp");
    private_folder(&packing).unwrap();
    let tarball = forging_tarball(&packing);
    let result = fill(
        &case.root,
        &staged,
        &work,
        &bin,
        VERSION,
        tarball.as_os_str(),
    )
    .and_then(|()| checked(&staged, VERSION).map_err(InstallError));
    match result {
        Err(InstallError(code)) => assert_eq!(code, "context_mode_integrity_mismatch"),
        Ok(()) => panic!(
            "a tarball other than the pinned one passed the pin check: its script rewrote npm's record of the hash"
        ),
    }
    assert!(
        !staged.join("script-ran").exists(),
        "no package script runs before the pin is checked"
    );
}

fn npx_in(case: &Case, version: &str) -> ServerSpec {
    ServerSpec::Stdio {
        command: case.bin.join("npx").to_str().unwrap().to_owned(),
        args: vec!["-y".into(), format!("context-mode@{version}")],
    }
}

fn registered(folder: &Path) -> bool {
    INSTALLING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&folder.to_path_buf())
}

#[test]
fn a_check_inside_the_backoff_after_a_failed_install_gets_the_time_left_and_never_npx() {
    let case = case("/usr/bin/false");
    fs::write(case.npm().join(FAILED), (now() - 60).to_string()).unwrap();
    match for_check(&case.root, npx_in(&case, VERSION)) {
        ForCheck::Failed(left) => assert!(
            left <= INSTALL_BACKOFF - Duration::from_secs(59)
                && left >= INSTALL_BACKOFF - Duration::from_secs(70),
            "{left:?}"
        ),
        other => panic!("a recent failure holds the install back: {other:?}"),
    }
    assert!(
        !registered(&case.folder()),
        "an install that cannot run is never registered"
    );
    assert!(!case.npm().join(PROFILE).exists(), "npm never started");
    assert_eq!(
        for_launch(&case.root, npx_in(&case, VERSION)),
        npx_in(&case, VERSION),
        "an agent launch still runs the approved spec"
    );
}

#[test]
fn a_check_without_npm_beside_npx_is_refused_and_never_registered() {
    let case = case("/usr/bin/false");
    fs::remove_file(case.bin.join("npm")).unwrap();
    assert_eq!(
        for_check(&case.root, npx_in(&case, VERSION)),
        ForCheck::Refused("context_mode_npm_missing")
    );
    assert!(!registered(&case.folder()));
}

#[test]
fn a_check_of_a_version_the_pack_does_not_pin_is_refused_and_never_registered() {
    let case = case("/usr/bin/false");
    assert_eq!(
        for_check(&case.root, npx_in(&case, "9.9.9")),
        ForCheck::Refused("context_mode_integrity_unpinned")
    );
    assert!(!registered(&install_folder(&case.root, "9.9.9")));
}

#[test]
fn a_check_runs_the_shared_install_once_complete_even_while_an_install_is_registered() {
    let case = case("/usr/bin/false");
    finished(&case.folder(), PINNED);
    INSTALLING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(case.folder());
    let checked = for_check(&case.root, npx_in(&case, VERSION));
    finish(&case.folder());
    let program = case
        .folder()
        .join("node_modules/context-mode/cli.bundle.mjs");
    assert_eq!(
        checked,
        ForCheck::Run(ServerSpec::Stdio {
            command: case.bin.join("node").to_str().unwrap().to_owned(),
            args: vec![program.to_str().unwrap().to_owned()],
        }),
        "a finished install wins over the registration it has not cleared yet"
    );
}

#[test]
fn a_check_while_the_install_runs_says_it_is_being_set_up() {
    let case = case("/usr/bin/false");
    INSTALLING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(case.folder());
    let during = for_check(&case.root, npx_in(&case, VERSION));
    finish(&case.folder());
    assert_eq!(during, ForCheck::SettingUp);
    assert!(!case.npm().join(PROFILE).exists(), "npm never started");
    assert!(!registered(&case.folder()));
}

#[test]
fn a_check_of_any_other_server_runs_it_as_approved() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("engine");
    for spec in [
        npx("context-mode@latest"),
        npx("other-server@1.0.0"),
        ServerSpec::Stdio {
            command: "/opt/homebrew/bin/uvx".into(),
            args: vec!["mcp-github".into()],
        },
    ] {
        assert_eq!(for_check(&root, spec.clone()), ForCheck::Run(spec));
    }
}
