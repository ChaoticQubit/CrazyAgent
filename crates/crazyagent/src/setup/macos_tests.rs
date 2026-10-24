use super::*;
use crate::setup::plain_reason;
use crate::setup::process::{file_sha256, is_sha256};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn script(path: &Path, line: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\necho \"{line}\"\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn archive(dir: &Path, line: &str) -> (PathBuf, String) {
    let source = dir.join("source");
    script(&source.join("uv-test/uv"), line);
    let archive = dir.join("uv-test.tar.gz");
    let status = Command::new("/usr/bin/tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&source)
        .arg("uv-test")
        .status()
        .unwrap();
    assert!(status.success());
    let sha256 = file_sha256(&archive).unwrap();
    (archive, sha256)
}

fn release(archive: &Path, sha256: &str) -> Pin {
    Pin::Release {
        version: Version::parse("0.13.5").unwrap(),
        url: format!("file://{}", archive.display()),
        sha256: sha256.to_owned(),
        member: "uv-test/uv".to_owned(),
    }
}

fn leftovers(setup: &Path) -> Vec<String> {
    std::fs::read_dir(setup)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect()
}

#[test]
fn a_dependency_in_the_app_data_folder_is_found_with_its_version() {
    let root = root();
    let installer = MacInstaller::with_paths(root.path(), vec![], vec![]);
    assert_eq!(installer.find(Dependency::Uv).unwrap(), None);
    script(
        &root.path().join("setup/bin/uv"),
        "uv 0.13.5 (fixture 2026-10-22)",
    );
    assert_eq!(
        installer.find(Dependency::Uv).unwrap(),
        Some(Version::parse("0.13.5").unwrap())
    );
    assert_eq!(installer.find(Dependency::Tmux).unwrap(), None);
    let system = root.path().join("system");
    script(&system.join("tmux"), "tmux 3.5a");
    let installer = MacInstaller::with_paths(root.path(), vec![system.clone()], vec![]);
    assert_eq!(
        installer.locate(Dependency::Tmux),
        Some((system.join("tmux"), Version::parse("3.5a").unwrap()))
    );
}

#[test]
fn a_pinned_release_installs_only_when_its_checksum_matches() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let (archive, sha256) = archive(work.path(), "uv 0.13.5 (fixture)");
    let installer = MacInstaller::with_paths(root.path(), vec![], vec![]);
    let wrong = "0".repeat(64);
    assert_ne!(wrong, sha256);

    assert_eq!(
        installer
            .install(Dependency::Uv, &release(&archive, &wrong))
            .unwrap_err(),
        InstallError("setup_checksum_mismatch")
    );
    assert!(!root.path().join("setup/bin/uv").exists());
    assert!(leftovers(installer.setup_folder()).is_empty());
    assert!(plain_reason("setup_checksum_mismatch").contains("checksum"));

    assert_eq!(
        installer
            .install(Dependency::Uv, &release(&archive, &sha256))
            .unwrap(),
        Version::parse("0.13.5").unwrap()
    );
    let installed = root.path().join("setup/bin/uv");
    let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_eq!(
        installer.find(Dependency::Uv).unwrap(),
        Some(Version::parse("0.13.5").unwrap())
    );
    assert!(leftovers(installer.setup_folder()).is_empty());
}

#[test]
fn a_release_reporting_another_version_is_refused_and_not_installed() {
    let root = root();
    let work = tempfile::tempdir().unwrap();
    let (archive, sha256) = archive(work.path(), "uv 0.1.0 (other)");
    let installer = MacInstaller::with_paths(root.path(), vec![], vec![]);
    assert_eq!(
        installer
            .install(Dependency::Uv, &release(&archive, &sha256))
            .unwrap_err(),
        InstallError("setup_version_mismatch")
    );
    assert!(!root.path().join("setup/bin/uv").exists());
}

#[test]
fn a_release_pin_outside_https_or_with_a_bad_member_is_refused_before_download() {
    let root = root();
    let installer = MacInstaller::with_paths(root.path(), vec![], vec![]);
    let sha256 = "a".repeat(64);
    for (url, member) in [
        ("http://example.invalid/uv.tar.gz", "uv-test/uv"),
        ("https://example.invalid/uv.tar.gz", "../uv"),
        ("https://example.invalid/uv.tar.gz", "uv-test/tmux"),
    ] {
        let pin = Pin::Release {
            version: Version::parse("0.13.5").unwrap(),
            url: url.to_owned(),
            sha256: sha256.clone(),
            member: member.to_owned(),
        };
        assert_eq!(
            installer.install(Dependency::Uv, &pin).unwrap_err(),
            InstallError("setup_pin_invalid")
        );
    }
}

#[test]
fn tmux_without_homebrew_and_graphify_without_uv_fail_with_plain_reasons() {
    let root = root();
    let installer = MacInstaller::with_paths(root.path(), vec![], vec![]);
    assert_eq!(
        installer
            .install(Dependency::Tmux, &pin(Dependency::Tmux))
            .unwrap_err(),
        InstallError("setup_homebrew_missing")
    );
    assert!(plain_reason("setup_homebrew_missing").contains("Homebrew"));
    assert_eq!(
        installer
            .install(Dependency::Graphify, &pin(Dependency::Graphify))
            .unwrap_err(),
        InstallError("setup_uv_missing")
    );
}

#[test]
fn the_shipped_pins_name_exact_versions_and_checksums() {
    let Pin::Release {
        version,
        url,
        sha256,
        member,
    } = pin(Dependency::Uv)
    else {
        panic!("uv installs from a release archive");
    };
    assert_eq!(version.as_str(), UV_VERSION);
    assert!(url.starts_with("https://github.com/astral-sh/uv/releases/download/0.13.5/"));
    assert!(is_sha256(&sha256));
    assert!(member.ends_with("/uv"));
    assert_eq!(
        pin(Dependency::Graphify),
        Pin::UvTool {
            package: "graphifyy".into(),
            version: Version::parse("0.9.9").unwrap(),
            python: "3.12".into(),
        }
    );
}

#[test]
fn a_program_that_never_exits_is_stopped_at_its_time_bound() {
    let root = root();
    let program = root.path().join("slow");
    std::fs::write(&program, "#!/bin/sh\n/bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        run(&program, &[], &[], root.path(), Duration::from_millis(500)).unwrap_err(),
        InstallError("setup_timeout")
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}
