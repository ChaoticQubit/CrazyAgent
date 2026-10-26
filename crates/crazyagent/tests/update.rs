#![cfg(unix)]

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use crazyagent::update::{self, UpdateError};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn executable(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn script(path: &Path, body: &str) {
    executable(path, format!("#!/bin/sh\n{body}\n").as_bytes());
}

fn real_crazyagent() -> Vec<u8> {
    std::fs::read(env!("CARGO_BIN_EXE_crazyagent")).unwrap()
}

fn code<T: std::fmt::Debug>(result: Result<T, UpdateError>) -> &'static str {
    result.unwrap_err().code
}

fn leftovers(install: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(install)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".old"))
        .collect();
    names.sort();
    names
}

#[test]
fn a_signed_release_archive_is_verified_unpacked_and_swapped_in_with_the_real_binary() {
    let install = tempfile::tempdir().unwrap();
    script(&install.path().join("crazyagent"), "echo crazyagent 0.0.1");
    script(&install.path().join("crazyagent-runner"), "exit 0");
    let release = tempfile::tempdir().unwrap();
    let new_binary = real_crazyagent();
    executable(&release.path().join("crazyagent"), &new_binary);
    let runner = std::fs::read(env!("CARGO_BIN_EXE_crazyagent-runner")).unwrap();
    executable(&release.path().join("crazyagent-runner"), &runner);
    executable(&release.path().join("README-not-installed"), b"ignored");
    let archive = release.path().join("crazyagent.tar.gz");
    let packed = crazyagent::spawn_gate::status(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(release.path())
            .args(["crazyagent", "crazyagent-runner", "README-not-installed"])
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(packed.success());

    let bytes = std::fs::read(&archive).unwrap();
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let key = STANDARD.encode(pair.pk.to_box().unwrap().into_string());
    let trusted = format!("timestamp:1761400000\tfile:crazyagent.tar.gz\tversion:{VERSION}");
    let signature = minisign::sign(
        Some(&pair.pk),
        &pair.sk,
        std::io::Cursor::new(&bytes),
        Some(&trusted),
        None,
    )
    .unwrap();
    let signature = STANDARD.encode(signature.into_string());
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    update::verify_file(&archive, &sha, &signature, &key, VERSION).unwrap();

    let staging = install.path().join(".crazyagent-update-test");
    std::fs::create_dir(&staging).unwrap();
    let unpacked = update::unpack(&archive, &staging.join("unpacked")).unwrap();
    update::swap(install.path(), &unpacked, VERSION).unwrap();

    assert_eq!(
        std::fs::read(install.path().join("crazyagent")).unwrap(),
        new_binary
    );
    assert_eq!(
        std::fs::read(install.path().join("crazyagent-runner")).unwrap(),
        runner
    );
    assert!(!install.path().join("README-not-installed").exists());
    assert!(leftovers(install.path()).is_empty());
    let answered = crazyagent::spawn_gate::output(
        Command::new(install.path().join("crazyagent"))
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(
        String::from_utf8(answered.stdout)
            .unwrap()
            .starts_with(&format!("crazyagent {VERSION}"))
    );
}

#[test]
fn a_new_binary_that_does_not_answer_its_version_never_replaces_the_old_one() {
    let install = tempfile::tempdir().unwrap();
    script(&install.path().join("crazyagent"), "echo crazyagent 0.0.1");
    let staged = install.path().join(".crazyagent-update-broken");
    std::fs::create_dir(&staged).unwrap();
    script(&staged.join("crazyagent"), "exit 3");
    assert_eq!(
        code(update::swap(install.path(), &staged, VERSION)),
        "update_new_version_failed"
    );
    assert_eq!(
        std::fs::read_to_string(install.path().join("crazyagent")).unwrap(),
        "#!/bin/sh\necho crazyagent 0.0.1\n"
    );
    assert!(leftovers(install.path()).is_empty());

    script(&staged.join("crazyagent"), "echo crazyagent 0.0.2");
    assert_eq!(
        code(update::swap(install.path(), &staged, VERSION)),
        "update_new_version_failed"
    );
    assert_eq!(
        std::fs::read_to_string(install.path().join("crazyagent")).unwrap(),
        "#!/bin/sh\necho crazyagent 0.0.1\n"
    );
}

#[test]
fn a_binary_that_fails_after_the_rename_is_rolled_back_to_the_old_files() {
    let install = tempfile::tempdir().unwrap();
    script(&install.path().join("crazyagent"), "echo crazyagent 0.0.1");
    script(&install.path().join("crazyagent-rcli"), "echo old rcli");
    let staged = install.path().join(".crazyagent-update-late");
    std::fs::create_dir(&staged).unwrap();
    script(
        &staged.join("crazyagent"),
        &format!(
            "case \"$0\" in *crazyagent-update-late*) echo crazyagent {VERSION};; *) exit 1;; esac"
        ),
    );
    script(&staged.join("crazyagent-rcli"), "echo new rcli");
    assert_eq!(
        code(update::swap(install.path(), &staged, VERSION)),
        "update_new_version_failed"
    );
    assert_eq!(
        std::fs::read_to_string(install.path().join("crazyagent")).unwrap(),
        "#!/bin/sh\necho crazyagent 0.0.1\n"
    );
    assert_eq!(
        std::fs::read_to_string(install.path().join("crazyagent-rcli")).unwrap(),
        "#!/bin/sh\necho old rcli\n"
    );
    assert!(leftovers(install.path()).is_empty());
}

#[test]
fn an_archive_without_crazyagent_is_refused_before_anything_changes() {
    let release = tempfile::tempdir().unwrap();
    executable(&release.path().join("something-else"), b"x");
    let archive = release.path().join("bad.tar.gz");
    assert!(
        crazyagent::spawn_gate::status(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(release.path())
                .arg("something-else")
                .stdin(Stdio::null()),
        )
        .unwrap()
        .success()
    );
    let into = release.path().join("unpacked");
    assert_eq!(
        code(update::unpack(&archive, &into)),
        "update_archive_invalid"
    );
    std::fs::write(release.path().join("garbage.tar.gz"), b"not an archive").unwrap();
    assert_eq!(
        code(update::unpack(
            &release.path().join("garbage.tar.gz"),
            &release.path().join("unpacked-2")
        )),
        "update_archive_invalid"
    );
}

#[test]
fn every_helper_the_release_ships_is_replaced_including_the_windows_boundary() {
    let install = tempfile::tempdir().unwrap();
    script(&install.path().join("crazyagent"), "echo crazyagent 0.0.1");
    let helpers = [
        "crazyagent-rcli",
        "crazyagent-runner",
        "crazyagent-landlock",
        "crazyagent-win-boundary",
    ];
    for helper in helpers {
        script(&install.path().join(helper), "echo old");
    }
    let staged = install.path().join(".crazyagent-update-helpers");
    std::fs::create_dir(&staged).unwrap();
    executable(&staged.join("crazyagent"), &real_crazyagent());
    for helper in helpers {
        script(&staged.join(helper), &format!("echo new {helper}"));
    }
    update::swap(install.path(), &staged, VERSION).unwrap();
    for helper in helpers {
        assert_eq!(
            std::fs::read_to_string(install.path().join(helper)).unwrap(),
            format!("#!/bin/sh\necho new {helper}\n"),
            "{helper}"
        );
    }
    assert!(leftovers(install.path()).is_empty());
}
