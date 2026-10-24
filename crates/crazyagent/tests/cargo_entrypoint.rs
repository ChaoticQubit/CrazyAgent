use std::path::Path;

#[test]
fn documented_cargo_run_selects_main_binary_with_probe_present() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("crates/crazyagent");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(package.join("src/bin")).unwrap();
    let workspace = source.parent().unwrap().parent().unwrap();
    for name in ["Cargo.toml", "Cargo.lock"] {
        std::fs::copy(workspace.join(name), root.path().join(name)).unwrap();
    }
    let members = workspace_members(workspace);
    assert!(members.iter().any(|member| member == "crates/crazyagent"));
    for member in members
        .iter()
        .filter(|member| *member != "crates/crazyagent")
    {
        let copy = root.path().join(member);
        let member_source = workspace.join(member);
        std::fs::create_dir_all(&copy).unwrap();
        copy_source(&member_source, &copy, 0);
        assert_eq!(
            std::fs::read(copy.join("Cargo.toml")).unwrap(),
            std::fs::read(member_source.join("Cargo.toml")).unwrap()
        );
    }
    let mut manifest = std::fs::read_to_string(source.join("Cargo.toml")).unwrap();
    manifest.push_str("\n[[bin]]\nname = 'crazyagent-boundary-child'\npath = 'src/bin/crazyagent-boundary-child.rs'\n");
    std::fs::write(package.join("Cargo.toml"), manifest).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let name = entry.unwrap().file_name();
        if name == "Cargo.toml" || name == "tests" {
            continue;
        }
        let copy = package.join(&name);
        if source.join(&name).is_dir() {
            std::fs::create_dir_all(&copy).unwrap();
            copy_source(&source.join(&name), &copy, 0);
        } else {
            std::fs::copy(source.join(&name), copy).unwrap();
        }
    }
    assert!(package.join("src/lib.rs").is_file() && package.join("assets").is_dir());
    let probe = package.join("src/bin/crazyagent-boundary-child.rs");
    if !probe.exists() {
        std::fs::write(probe, "fn main() { std::process::exit(7); }\n").unwrap();
    }
    let output = crazyagent::spawn_gate::output(
        std::process::Command::new(env!("CARGO"))
            .current_dir(root.path())
            .args([
                "run",
                "--offline",
                "--",
                "qualification",
                "report",
                "--source-revision",
                "entrypoint-check",
                "--profile-id",
                "local-profile",
            ])
            .env(
                "CARGO_TARGET_DIR",
                workspace.join(".local-state/cargo-entrypoint-target"),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source_revision"], "entrypoint-check");
    assert_eq!(report["profile_id"], "local-profile");
}

/// Reads the workspace member folders from the root manifest, so the temporary
/// workspace holds every crate cargo must load. Input: the workspace root.
/// Output: the member paths as the manifest lists them.
fn workspace_members(workspace: &Path) -> Vec<String> {
    let manifest: toml::Table =
        toml::from_str(&std::fs::read_to_string(workspace.join("Cargo.toml")).unwrap()).unwrap();
    let members: Vec<String> = manifest["workspace"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member.as_str().unwrap().to_owned())
        .collect();
    assert!(!members.is_empty() && members.len() <= 16);
    members
}

fn copy_source(source: &Path, target: &Path, depth: usize) {
    assert!(depth < 8);
    let entries: Vec<_> = std::fs::read_dir(source).unwrap().collect();
    assert!(entries.len() <= 128);
    for entry in entries {
        let entry = entry.unwrap();
        let path = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&path).unwrap();
            copy_source(&entry.path(), &path, depth + 1);
        } else {
            assert!(entry.file_type().unwrap().is_file());
            std::fs::copy(entry.path(), path).unwrap();
        }
    }
}
