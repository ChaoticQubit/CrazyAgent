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
    let desktop = root.path().join("apps/desktop/src-tauri");
    let desktop_source = workspace.join("apps/desktop/src-tauri");
    std::fs::create_dir_all(&desktop).unwrap();
    copy_source(&desktop_source, &desktop, 0);
    assert_eq!(
        std::fs::read(desktop.join("Cargo.toml")).unwrap(),
        std::fs::read(desktop_source.join("Cargo.toml")).unwrap()
    );
    let mut manifest = std::fs::read_to_string(source.join("Cargo.toml")).unwrap();
    manifest.push_str("\n[[bin]]\nname = 'crazyagent-boundary-child'\npath = 'src/bin/crazyagent-boundary-child.rs'\n");
    std::fs::write(package.join("Cargo.toml"), manifest).unwrap();
    copy_source(&source.join("src"), &package.join("src"), 0);
    let probe = package.join("src/bin/crazyagent-boundary-child.rs");
    if !probe.exists() {
        std::fs::write(probe, "fn main() { std::process::exit(7); }\n").unwrap();
    }
    let output = std::process::Command::new(env!("CARGO"))
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
        .output()
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
