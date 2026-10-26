use std::path::{Path, PathBuf};

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
        for file in kept_files(workspace, member) {
            copy_file(&workspace.join(&file), &root.path().join(&file));
        }
        assert_eq!(
            std::fs::read(copy.join("Cargo.toml")).unwrap(),
            std::fs::read(member_source.join("Cargo.toml")).unwrap()
        );
    }
    let mut manifest = std::fs::read_to_string(source.join("Cargo.toml")).unwrap();
    manifest.push_str("\n[[bin]]\nname = 'crazyagent-boundary-child'\npath = 'src/bin/crazyagent-boundary-child.rs'\n");
    std::fs::write(package.join("Cargo.toml"), manifest).unwrap();
    for file in kept_files(workspace, "crates/crazyagent") {
        let inside = file.strip_prefix("crates/crazyagent").unwrap();
        if inside == Path::new("Cargo.toml") || inside.starts_with("tests") {
            continue;
        }
        copy_file(&workspace.join(&file), &package.join(inside));
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

/// Lists the files git keeps in one workspace folder, tracked or new but never ignored,
/// so the copy holds what a checkout of this working tree holds and no build output.
/// Inputs: the workspace root and a folder relative to it. Output: the files that exist,
/// relative to the workspace root.
fn kept_files(workspace: &Path, folder: &str) -> Vec<PathBuf> {
    let output = crazyagent::spawn_gate::output(
        std::process::Command::new("git")
            .current_dir(workspace)
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                folder,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "UNAVAILABLE: the entrypoint check copies the files git keeps and needs a git checkout: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files: Vec<PathBuf> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| PathBuf::from(String::from_utf8(name.to_vec()).unwrap()))
        .filter(|file| workspace.join(file).is_file())
        .collect();
    assert!(!files.is_empty() && files.len() <= 4096, "{}", files.len());
    files
}

fn copy_file(source: &Path, target: &Path) {
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(source, target).unwrap();
}
