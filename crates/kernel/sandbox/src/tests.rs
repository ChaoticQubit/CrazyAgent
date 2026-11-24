use std::path::{Path, PathBuf};
use std::process::Command;

const ALLOWED_PATH_CRATES: [&str; 7] = [
    "crazyagent-kernel-sandbox",
    "crazyagent-kernel-common",
    "crazyagent-kernel-processes",
    "crazyagent-kernel-security",
    "crazyagent-kernel-storage",
    "crazyagent-win-sandbox",
    "crazyagent-linux-supervisor",
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

#[cfg(target_os = "linux")]
#[test]
fn the_linux_launch_and_its_host_read_live_in_the_kernel() {
    let read: fn(&std::path::Path, Option<&std::path::Path>) -> crate::host::LinuxHost =
        crate::linux::launch::host::read_host;
    let _ = read;
    let _: Option<crate::linux::launch::LinuxSandbox> = None;
}

/// The confinement the kernel builds for the tmux host holds the very values the engine built
/// it from: the sandbox runner, the keeper and pane shim words and variables, and a variable
/// gate and a keeper undo that answer as the sandbox's own do, for a kept and a refused case.
#[cfg(unix)]
#[test]
fn confinement_equals_the_values_crazyagent_built() {
    use crate::profile::command::SANDBOX_EXEC;
    use crate::profile::consts::AGENT_GIT_ENV;
    use crate::profile::env::agent_variable_allowed;
    use std::ffi::OsStr;
    let confinement = crate::confinement();
    assert_eq!(confinement.sandbox_exec, SANDBOX_EXEC);
    let (git, fixed) = AGENT_GIT_ENV[0];
    let relay = format!("{}stand-in", kernel_common::relay_names::TOKEN_PREFIX);
    for (name, value, allowed) in [
        (git, fixed, true),
        ("STAND_IN_TOKEN", relay.as_str(), false),
    ] {
        let (name, value) = (OsStr::new(name), OsStr::new(value));
        assert_eq!(agent_variable_allowed(name, value), allowed);
        assert_eq!((confinement.variable_allowed)(name, value), allowed);
    }
    #[cfg(target_os = "macos")]
    {
        use crate::profile::command::{
            KEEPER, KEEPER_DEADLINE, KEEPER_STATE, PANE_SHIM, kept_pane, unkept_pane,
        };
        assert_eq!(confinement.keeper, KEEPER);
        assert_eq!(confinement.keeper_deadline_variable, KEEPER_DEADLINE);
        assert_eq!(confinement.keeper_state_variable, KEEPER_STATE);
        assert_eq!(confinement.pane_shim, PANE_SHIM);
        let plain: Vec<String> = [SANDBOX_EXEC, "-f", "/private/tmp/stand-in.sb", "/bin/sh"]
            .map(str::to_owned)
            .to_vec();
        let kept = kept_pane(plain.clone(), None).unwrap();
        assert_eq!(unkept_pane(&kept).as_ref(), Some(&plain));
        assert_eq!((confinement.unkept_pane)(&kept), Some(plain.clone()));
        assert_eq!((confinement.unkept_pane)(&plain), None);
    }
}

/// The engine's host reads and the launch refusal codes live in the kernel on every system:
/// off Linux no host is read and a project may hold another git, and the strict setting's
/// refusal keeps its own code while any other line has none.
#[test]
fn the_host_reads_and_refusal_codes_live_in_the_kernel() {
    use crate::error::SandboxError;
    use crate::lines::{LINUX_NOT_FULL_LINE, refusal_code};
    use crate::profile::consts::LINUX_NOT_FULL;
    let kept: fn() -> Option<crate::host::LinuxHost> = crate::host::linux_host_kept;
    let other: fn(&std::path::Path) -> bool = crate::host::linux_other_git;
    let _ = crate::host::linux_unbridged_notice;
    let strict = SandboxError::Unavailable(LINUX_NOT_FULL_LINE.to_owned());
    assert_eq!(refusal_code(&strict), Some(LINUX_NOT_FULL));
    let plain = SandboxError::Unavailable("no such line".to_owned());
    assert_eq!(refusal_code(&plain), None);
    if cfg!(not(target_os = "linux")) {
        assert_eq!(crate::host::linux_host(), None);
        assert_eq!(kept(), None);
        assert!(other(std::path::Path::new("/")));
    }
}

/// A Sandboxed IDE terminal names its own folder under the engine data root after the
/// project, so the kernel refuses, before it writes anything, every project that is not a
/// project id: empty, `.`, `..`, a nested path, an absolute path, a trailing dot, a space,
/// a quote, a newline or `..` inside a name.
#[cfg(target_os = "macos")]
#[test]
fn an_ide_terminal_refuses_a_project_that_is_not_one_folder_name() {
    use crate::error::SandboxError;
    use crate::ide_shell::{Launch, command};
    use crate::policy::{AgentAccess, AgentFilesRule, AgentNetwork};
    let scratch = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(scratch.path()).unwrap();
    let (root, folder) = (base.join("data"), base.join("project"));
    for path in [&root, &folder] {
        std::fs::create_dir_all(path).unwrap();
    }
    for project in [
        "", ".", "..", "a/b", "../p", "/p", "p.", "p q", "p\"q", "p\nq", "a..b",
    ] {
        let launch = Launch {
            root: &root,
            project,
            folder: &folder,
            cwd: &folder,
            shell: Path::new("/bin/zsh"),
            arguments: &[],
            incoming: &[],
            extra_reads: &[],
            level: AgentAccess::Sandboxed,
            network: AgentNetwork::default(),
            kept_ports: &[],
            agent_files: AgentFilesRule::default(),
        };
        match command(&crate::test_hooks::STAND_IN, &launch) {
            Err(SandboxError::Invalid(reason)) => {
                assert_eq!(reason, "the project id is not a folder name", "{project:?}")
            }
            other => panic!("{project:?} gave {other:?}"),
        }
        assert!(!root.join("ide-shell").exists(), "{project:?}");
    }
}

/// The installed boundary every launch takes lives in the kernel beside the profile it was
/// installed from, and the kernel profile hooks name the Windows grants folder it installs
/// with.
#[test]
fn the_installed_boundary_lives_in_the_kernel() {
    let _: Option<crate::profile::installed::InstalledSandbox> = None;
    let unconfined: fn(&crate::profile::installed::InstalledSandbox) -> bool =
        crate::profile::installed::InstalledSandbox::is_unconfined;
    let _ = unconfined;
    #[cfg(target_os = "windows")]
    {
        fn grants(
            hooks: &crate::profile::ProfileHooks,
        ) -> Result<std::path::PathBuf, crate::error::SandboxError> {
            (hooks.grants_folder)()
        }
        let _ = grants;
    }
}
