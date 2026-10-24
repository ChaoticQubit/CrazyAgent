use crazyagent::qualification::{Artifact, Capability, Outcome, QualificationReport};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn cli(args: &[&str]) -> Output {
    crazyagent::spawn_gate::output(
        Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap()
}

fn admit(path: &Path, root: &TempDir, source: &str, profile: &str) -> Output {
    crazyagent::spawn_gate::output(
        Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(["qualification", "admit", "--report"])
            .arg(path)
            .args([
                "--source-revision",
                source,
                "--profile-id",
                profile,
                "--evidence-dir",
            ])
            .arg(root.path())
            .args(["--require", "live_recovery"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap()
}

fn complete(root: &TempDir) -> std::path::PathBuf {
    fs::write(root.path().join("probe.txt"), b"abc").unwrap();
    let mut report = QualificationReport::new("revision-a", "profile-a");
    report.versions.insert("synthetic-input".into(), "1".into());
    report
        .results
        .insert(Capability::LiveRecovery, Outcome::Proven);
    report.artifacts.push(Artifact {
        path: "probe.txt".into(),
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
    });
    let path = root.path().join("report.json");
    fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    path
}

#[test]
fn report_serializes_unavailable_capabilities_and_admit_names_them() {
    let output = cli(&[
        "qualification",
        "report",
        "--source-revision",
        "revision-a",
        "--profile-id",
        "profile-a",
    ]);
    assert!(output.status.success(), "{:?}", output);
    let report: QualificationReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report.source_revision, "revision-a");
    assert_eq!(report.profile_id, "profile-a");
    assert!(report.artifacts.is_empty());
    for capability in Capability::ALL {
        assert_eq!(report.results.get(&capability), Some(&Outcome::Unavailable));
    }
    let root = TempDir::new().unwrap();
    let path = root.path().join("unavailable.json");
    fs::write(&path, output.stdout).unwrap();
    let denied = admit(&path, &root, "revision-a", "profile-a");
    assert_eq!(denied.status.code(), Some(1));
    assert!(
        String::from_utf8(denied.stderr)
            .unwrap()
            .contains("live_recovery")
    );
}

#[test]
fn binary_accepts_intact_evidence_and_rejects_changed_bytes() {
    let root = TempDir::new().unwrap();
    let path = complete(&root);
    let accepted = admit(&path, &root, "revision-a", "profile-a");
    assert!(accepted.status.success(), "{:?}", accepted);
    assert!(
        String::from_utf8(accepted.stdout)
            .unwrap()
            .contains("admitted: live_recovery")
    );
    fs::write(root.path().join("probe.txt"), b"changed").unwrap();
    let denied = admit(&path, &root, "revision-a", "profile-a");
    assert_eq!(denied.status.code(), Some(1));
    assert!(
        String::from_utf8(denied.stderr)
            .unwrap()
            .contains("SHA-256 mismatch")
    );
}

#[test]
fn binary_requires_trusted_source_profile_and_nonempty_requirements() {
    let root = TempDir::new().unwrap();
    let path = complete(&root);
    for (source, profile, message) in [
        ("revision-other", "profile-a", "source revision"),
        ("revision-a", "profile-other", "profile"),
    ] {
        let denied = admit(&path, &root, source, profile);
        assert_eq!(denied.status.code(), Some(1));
        assert!(String::from_utf8(denied.stderr).unwrap().contains(message));
    }
    for args in [
        vec!["qualification", "admit"],
        vec!["qualification", "report", "--source-revision", "revision-a"],
        vec![
            "qualification",
            "report",
            "--source-revision",
            "a",
            "--source-revision",
            "b",
            "--profile-id",
            "p",
        ],
        vec![
            "qualification",
            "report",
            "--source-revision",
            "r",
            "--profile-id",
            "p",
            "--unknown",
            "value",
        ],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(2), "{:?}", output);
        assert!(!output.stderr.is_empty());
    }
    let output = crazyagent::spawn_gate::output(
        Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .args(["qualification", "admit", "--report"])
            .arg(&path)
            .args([
                "--source-revision",
                "revision-a",
                "--profile-id",
                "profile-a",
                "--evidence-dir",
            ])
            .arg(root.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
