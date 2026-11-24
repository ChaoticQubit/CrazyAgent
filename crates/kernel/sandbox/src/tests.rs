use std::path::{Path, PathBuf};
use std::process::Command;

const ALLOWED_PATH_CRATES: [&str; 8] = [
    "crazyagent-kernel-sandbox",
    "crazyagent-kernel-common",
    "crazyagent-kernel-processes",
    "crazyagent-kernel-security",
    "crazyagent-kernel-storage",
    "crazyagent-win-sandbox",
    "crazyagent-linux-supervisor",
    "crazyagent-macos-fs",
];

/// The Rust files under a folder, depth first. Inputs: the folder and the list to fill.
/// Output: none; every `.rs` file found is pushed onto `found`.
fn rust_files(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The engine or hub paths a source text names: the engine crate's name followed by a path
/// separator at a word start, and the engine's sandbox hub path. Input: the text. Output:
/// each path found, in text order.
fn upward_paths(text: &str) -> Vec<String> {
    let engine = format!("{}::", ["crazy", "agent"].concat());
    let hub = ["crate::", "sandbox"].concat();
    let mut found = Vec::new();
    for (at, _) in text.match_indices(&engine) {
        let before = text[..at].chars().next_back();
        if before.is_none_or(|before| before != '_' && !before.is_ascii_alphanumeric()) {
            found.push(engine.clone());
        }
    }
    found.extend(text.match_indices(&hub).map(|(_, path)| path.to_owned()));
    found
}

/// The files under a folder that name an engine or hub path. Input: the folder. Output:
/// each file with the paths it names; the row fails when the folder holds no Rust file.
fn naming_upward(folder: &Path) -> Vec<(PathBuf, Vec<String>)> {
    let mut files = Vec::new();
    rust_files(folder, &mut files);
    assert!(!files.is_empty(), "{} holds no Rust file", folder.display());
    files
        .into_iter()
        .filter_map(|path| {
            let named = upward_paths(&std::fs::read_to_string(&path).unwrap());
            (!named.is_empty()).then_some((path, named))
        })
        .collect()
}

/// kernel/sandbox names no item of the engine crate or of the engine's sandbox hub, so it
/// never grows a dependency back on them; `crazyagent_win_sandbox::` paths are allowed,
/// as the scan reads the character before the name.
#[test]
fn kernel_sandbox_names_no_crazyagent_item() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_eq!(naming_upward(&source), []);
}

/// The scan behind [`kernel_sandbox_names_no_crazyagent_item`] finds a planted engine path
/// and a planted hub path, and lets the Windows platform crate's path through, so a green
/// layering row means the scan looked.
#[test]
fn the_scan_finds_a_planted_crazyagent_path() {
    let folder = tempfile::tempdir().unwrap();
    let engine = format!("use {}::sandbox::canonical;\n", ["crazy", "agent"].concat());
    let hub = format!("use {}::policy;\n", ["crate::", "sandbox"].concat());
    std::fs::write(folder.path().join("planted.rs"), format!("{engine}{hub}")).unwrap();
    std::fs::write(
        folder.path().join("platform.rs"),
        "use crazyagent_win_sandbox::name;\n",
    )
    .unwrap();
    let found = naming_upward(folder.path());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, folder.path().join("planted.rs"));
    assert_eq!(found[0].1.len(), 2, "{found:?}");
}

/// The dependency lines of a manifest that neither pin an exact version (`"=`) nor name a
/// path, in the `[dependencies]`, `[dev-dependencies]` and `target.*` dependency tables.
/// Input: the manifest text. Output: each such line.
fn unpinned(manifest: &str) -> Vec<&str> {
    let mut table = "";
    manifest
        .lines()
        .filter(|line| {
            if line.starts_with('[') {
                table = line;
                return false;
            }
            table.contains("dependencies]")
                && line.contains(" = ")
                && !line.contains("\"=")
                && !line.contains("path = ")
        })
        .collect()
}

/// `cargo tree` for kernel/sandbox on the host target, which the batch runs on each farm
/// runner, names only the allowlisted path crates (kernel common, processes, security and
/// storage and the platform crates), never the engine, the SDK or the bus gateway; and its
/// own manifest pins every third-party crate exactly.
#[test]
fn cargo_tree_is_the_allowlist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut tree = Command::new(env!("CARGO"));
    tree.args(["tree", "-p", "crazyagent-kernel-sandbox", "-e", "normal"])
        .args(["--prefix", "none", "--offline", "--locked"])
        .current_dir(&root);
    let output = kernel_processes::spawn_gate::collected(&mut tree).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed = String::from_utf8(output.stdout).unwrap();
    let names: Vec<&str> = listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(names.first(), Some(&"crazyagent-kernel-sandbox"));
    let outside: Vec<&&str> = names
        .iter()
        .filter(|name| name.starts_with("crazyagent") && !ALLOWED_PATH_CRATES.contains(name))
        .collect();
    assert!(outside.is_empty(), "{outside:?}");
    assert_eq!(unpinned(include_str!("../Cargo.toml")), Vec::<&str>::new());
    let planted = "[dependencies]\nserde = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nrustix = { version = \"=1.1.5\" }\nkernel = { path = \"../x\" }\n";
    assert_eq!(unpinned(planted), ["serde = \"1\""]);
}

#[test]
fn the_model_proxy_variables_name_its_port_and_its_tunnels_dial_no_local_or_metadata_address() {
    use crate::model_proxy::{HOSTS_MAX, may_dial, variables};
    let pairs = variables(29_300);
    assert_eq!(
        pairs.clone().map(|(name, _)| name),
        [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "NO_PROXY",
            "no_proxy"
        ]
    );
    for (name, value) in &pairs[..4] {
        assert_eq!(value, "http://127.0.0.1:29300", "{name}");
    }
    for (name, value) in &pairs[4..] {
        assert!(value.is_empty(), "{name}");
    }
    assert_eq!(HOSTS_MAX, 8);
    for refused in [
        "127.0.0.1",
        "::1",
        "0.0.0.0",
        "169.254.169.254",
        "::ffff:127.0.0.1",
    ] {
        assert!(!may_dial(refused.parse().unwrap()), "{refused}");
    }
    assert!(may_dial("192.0.2.1".parse().unwrap()));
}

#[cfg(target_os = "linux")]
#[test]
fn the_linux_helper_entry_lives_in_the_kernel() {
    let main: fn(Vec<std::ffi::OsString>) -> std::process::ExitCode = crate::linux::helper::main;
    let _ = main;
}
