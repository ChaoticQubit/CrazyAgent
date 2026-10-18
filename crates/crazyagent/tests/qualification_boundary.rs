use crazyagent::isolation::{
    BoundaryError, NetworkPolicy, ResourceLimits, SandboxProfile, run_boundary_probe,
};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn fixture() -> (TempDir, SandboxProfile, PathBuf, PathBuf) {
    let scratch =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local-state/boundary-tests");
    fs::create_dir_all(&scratch).unwrap();
    let root = tempfile::tempdir_in(scratch).unwrap();
    let candidate = root.path().join("candidate");
    let trusted = root.path().join("trusted");
    fs::create_dir(&candidate).unwrap();
    fs::create_dir(&trusted).unwrap();
    let candidate = candidate.canonicalize().unwrap();
    let trusted = trusted.canonicalize().unwrap();
    let profile = SandboxProfile {
        id: "boundary-test".into(),
        runtime_path: PathBuf::from("/absent/srt"),
        node_path: PathBuf::from("/absent/node"),
        grants: vec![candidate.clone()],
        network_policy: NetworkPolicy::DenyAll,
        credential_refs: vec![trusted.join("synthetic-credential")],
        resource_limits: ResourceLimits {
            cpu_seconds: 2,
            memory_bytes: 64 * 1024 * 1024,
            processes: 4,
            wall_time_seconds: 10,
        },
    };
    (root, profile, candidate, trusted)
}

#[test]
fn validates_disjoint_real_fixture_roots() {
    let (_root, profile, candidate, trusted) = fixture();
    assert!(profile.validate(&candidate, &trusted).is_ok());
}

#[test]
fn rejects_broad_grants_overlapping_roots_and_traversal() {
    let (_root, mut profile, candidate, trusted) = fixture();
    profile.grants.push(trusted.clone());
    assert!(profile.validate(&candidate, &trusted).is_err());
    profile.grants = vec![candidate.clone()];
    assert!(profile.validate(&candidate, &candidate).is_err());
    assert!(profile.validate(&candidate.join(".."), &trusted).is_err());
    profile.credential_refs = vec![candidate.join("credential")];
    assert!(profile.validate(&candidate, &trusted).is_err());
}

#[test]
fn rejects_unbounded_limits_and_symlinked_roots() {
    let (root, mut profile, candidate, trusted) = fixture();
    profile.resource_limits.wall_time_seconds = 0;
    assert!(profile.validate(&candidate, &trusted).is_err());
    profile.resource_limits.wall_time_seconds = 10;
    let link = root.path().join("candidate-link");
    std::os::unix::fs::symlink(&candidate, &link).unwrap();
    assert!(profile.validate(&link, &trusted).is_err());
}

#[test]
fn missing_runtime_is_unavailable_without_creating_probe_artifacts() {
    let (_root, profile, candidate, trusted) = fixture();
    assert!(matches!(
        run_boundary_probe(&profile, &candidate, &trusted),
        Err(BoundaryError::Unavailable(_))
    ));
    assert_eq!(fs::read_dir(&candidate).unwrap().count(), 0);
}

#[test]
fn boundary_denies_escape_and_trusted_writes() {
    let (root, mut profile, candidate, trusted) = fixture();
    let retained = root.keep();
    eprintln!("retained native artifacts: {}", retained.display());
    profile.runtime_path = std::env::var_os("CRAZYAGENT_SRT")
        .map(PathBuf::from)
        .expect("UNAVAILABLE: explicitly set CRAZYAGENT_SRT to the leader-verified srt path");
    profile.node_path = std::env::var_os("CRAZYAGENT_NODE")
        .map(PathBuf::from)
        .expect("UNAVAILABLE: explicitly set CRAZYAGENT_NODE to the verified Node executable");
    let result = run_boundary_probe(&profile, &candidate, &trusted);
    let evidence = match result {
        Ok(evidence) => evidence,
        Err(BoundaryError::Unenforced(evidence)) => *evidence,
        Err(error) => panic!("native qualification blocked: {error}"),
    };
    for name in [
        "candidate-write",
        "child-candidate-write",
        "loopback-control",
    ] {
        assert_eq!(evidence.allowed.get(name), Some(&true), "{evidence:?}");
    }
    for name in [
        "trusted-write",
        "traversal-escape",
        "symlink-escape",
        "child-trusted-write",
        "credential-read",
        "forbidden-network",
    ] {
        assert_eq!(evidence.denied.get(name), Some(&true), "{evidence:?}");
    }
    assert!(!evidence.admissible());
    assert_eq!(
        evidence.confinement_outcome(),
        crazyagent::qualification::Outcome::Unavailable
    );
    assert!(evidence.unenforced.iter().any(|v| v == "memory-bytes"));
}
