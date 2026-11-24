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

/// The Rust files under a folder, depth first, never through a link. Inputs: the folder and
/// the list to fill. Output: every `.rs` file found is pushed onto `found`, or the error
/// that stopped the walk.
fn rust_files(folder: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            rust_files(&path, found)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    Ok(())
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
    for (at, top) in text.match_indices("crate::") {
        let rest = text[at + top.len()..].trim_start();
        let named = match rest.strip_prefix('{') {
            Some(group) => top_items(group).into_iter().any(names_hub),
            None => names_hub(rest),
        };
        if named {
            found.push(hub.clone());
        }
    }
    found
}

/// Whether a path, read from its start, names the hub module itself: its name followed by
/// the end, `::` or any character that cannot continue a name. Input: the path text.
/// Output: the answer.
fn names_hub(path: &str) -> bool {
    path.trim_start()
        .strip_prefix("sandbox")
        .is_some_and(|after| !after.starts_with(|next: char| next == '_' || next.is_alphanumeric()))
}

/// The top-level items of a `{..}` import group, read from just after its `{` up to the
/// `}` that closes it, split on the commas outside any inner group. Input: the text after
/// the `{`. Output: each item, untrimmed.
fn top_items(group: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (at, character) in group.char_indices() {
        match character {
            '{' => depth += 1,
            '}' if depth == 0 => {
                items.push(&group[start..at]);
                return items;
            }
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(&group[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    items.push(&group[start..]);
    items
}

/// The files under a folder that name an engine or hub path. Input: the folder. Output:
/// each file with the paths it names, or the error a read gave; the row fails when the
/// folder holds no Rust file.
fn naming_upward(folder: &Path) -> std::io::Result<Vec<(PathBuf, Vec<String>)>> {
    let mut files = Vec::new();
    rust_files(folder, &mut files)?;
    assert!(!files.is_empty(), "{} holds no Rust file", folder.display());
    let mut naming = Vec::new();
    for path in files {
        let named = upward_paths(&std::fs::read_to_string(&path)?);
        if !named.is_empty() {
            naming.push((path, named));
        }
    }
    Ok(naming)
}

/// kernel/sandbox names no item of the engine crate or of the engine's sandbox hub, so it
/// never grows a dependency back on them; `crazyagent_win_sandbox::` paths are allowed,
/// as the scan reads the character before the name.
#[test]
fn kernel_sandbox_names_no_crazyagent_item() -> std::io::Result<()> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_eq!(naming_upward(&source)?, []);
    Ok(())
}

/// The scan behind [`kernel_sandbox_names_no_crazyagent_item`] finds a planted engine path
/// and a planted hub path, and lets the Windows platform crate's path through, so a green
/// layering row means the scan looked.
#[test]
fn the_scan_finds_a_planted_crazyagent_path() -> std::io::Result<()> {
    let folder = tempfile::tempdir()?;
    let engine = format!("use {}::sandbox::canonical;\n", ["crazy", "agent"].concat());
    let hub = format!("use {}::policy;\n", ["crate::", "sandbox"].concat());
    std::fs::write(folder.path().join("planted.rs"), format!("{engine}{hub}"))?;
    std::fs::write(
        folder.path().join("platform.rs"),
        "use crazyagent_win_sandbox::name;\n",
    )?;
    let found = naming_upward(folder.path())?;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, folder.path().join("planted.rs"));
    assert_eq!(found[0].1.len(), 2, "{found:?}");
    Ok(())
}

/// The dependency tables of a manifest, as cargo reads them at the top and under each
/// `target.*` table.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// The dependencies of a manifest that neither pin an exact version (`=`) nor name a path,
/// in every dependency table at the top and under each `target.*` table, however the
/// manifest writes them. Input: the manifest text. Output: each such dependency's name, or
/// the error when the text is not TOML.
fn unpinned(manifest: &str) -> Result<Vec<String>, toml::de::Error> {
    let parsed: toml::Table = manifest.parse()?;
    let platforms = parsed
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|targets| targets.values().filter_map(toml::Value::as_table));
    let mut names = Vec::new();
    for owner in std::iter::once(&parsed).chain(platforms) {
        for table in DEPENDENCY_TABLES
            .iter()
            .filter_map(|kind| owner.get(*kind).and_then(toml::Value::as_table))
        {
            names.extend(
                table
                    .iter()
                    .filter(|(_, value)| !pinned(value))
                    .map(|(name, _)| name.clone()),
            );
        }
    }
    Ok(names)
}

/// Whether one dependency's value pins an exact version or names a path. Input: the value.
/// Output: the answer.
fn pinned(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(version) => version.starts_with('='),
        toml::Value::Table(table) => {
            table.contains_key("path")
                || table
                    .get("version")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|version| version.starts_with('='))
        }
        _ => false,
    }
}

/// The `cargo tree` listing of one package of the workspace at `root`, on the host target,
/// one crate per line with no prefix, read offline from the lockfile as it stands. Inputs:
/// the workspace folder and the package name. Output: the listing, or the error naming why
/// cargo gave none.
fn path_tree(root: &Path, package: &str) -> std::io::Result<String> {
    let mut tree = Command::new(env!("CARGO"));
    tree.args(["tree", "-p", package, "-e", "normal,build"])
        .args(["--prefix", "none", "--offline", "--locked"])
        .current_dir(root);
    let output = kernel_processes::spawn_gate::collected(&mut tree)?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    String::from_utf8(output.stdout).map_err(std::io::Error::other)
}

/// The crates of a [`path_tree`] listing that lie outside the allowlist of path crates:
/// every local crate, which cargo lists with its folder in parentheses, and every crate
/// named as the engine's are, from wherever it comes. Input: the listing. Output: each such
/// crate name, in listing order.
fn outside(listed: &str) -> Vec<&str> {
    listed
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            let local = line
                .split('(')
                .skip(1)
                .filter_map(|group| group.split(')').next())
                .any(|group| Path::new(group).has_root());
            ((local || name.starts_with("crazyagent")) && !ALLOWED_PATH_CRATES.contains(&name))
                .then_some(name)
        })
        .collect()
}

/// `cargo tree` for kernel/sandbox on the host target, which the batch runs on each farm
/// runner, names only the allowlisted path crates (kernel common, processes, security and
/// storage and the platform crates), never the engine, the SDK or the bus gateway; and its
/// own manifest pins every third-party crate exactly.
#[test]
fn cargo_tree_is_the_allowlist() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let listed = path_tree(&root, "crazyagent-kernel-sandbox")?;
    let names: Vec<&str> = listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(names.first(), Some(&"crazyagent-kernel-sandbox"));
    let outside = outside(&listed);
    assert!(outside.is_empty(), "{outside:?}");
    assert_eq!(
        unpinned(include_str!("../Cargo.toml"))?,
        Vec::<String>::new()
    );
    let planted = "[dependencies]\nserde = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nrustix = { version = \"=1.1.5\" }\nkernel = { path = \"../x\" }\n";
    assert_eq!(unpinned(planted)?, ["serde"]);
    Ok(())
}

/// A dependency declared in a table of its own, under a platform or not, or written with no
/// spaces around its `=`, is read like any other, so an unpinned one there fails the row.
#[test]
fn a_dependency_in_its_own_table_or_with_no_spaces_is_read()
-> Result<(), Box<dyn std::error::Error>> {
    for manifest in [
        "[dependencies.serde]\nversion = \"1\"\n",
        "[target.'cfg(unix)'.dependencies.libc]\nversion = \"0.2\"\n",
        "[dev-dependencies]\nserde=\"1\"\n",
    ] {
        assert_eq!(unpinned(manifest)?.len(), 1, "{manifest}");
    }
    let pinned = "[dependencies.serde]\nversion = \"=1.0.229\"\n\n[build-dependencies.kernel]\npath = \"../x\"\n";
    assert_eq!(unpinned(pinned)?, Vec::<String>::new());
    Ok(())
}

/// Every local path crate a listing names is held to the allowlist, whatever its name, while
/// a registry crate and a procedural macro are not path crates.
#[test]
fn a_path_crate_of_any_name_is_held_to_the_allowlist() {
    let listed = "crazyagent-kernel-sandbox v2.5.0 (/w/crates/kernel/sandbox)\nsdk-gateway v0.1.0 (/w/crates/sdk/gateway)\nserde v1.0.229\nserde_derive v1.0.229 (proc-macro)\ncrazyagent-kernel-common v2.5.0 (/w/crates/kernel/common) (*)\n";
    assert_eq!(outside(listed), ["sdk-gateway"]);
}

/// The hub named inside a grouped `crate::{..}` import is found like a plain hub path, and a
/// module whose name only starts with the hub's is not the hub.
#[test]
fn a_grouped_import_of_the_hub_is_found() {
    let (top, module) = ("crate::", ["sand", "box"].concat());
    let hub = format!("{top}{module}");
    for text in [
        format!("use {top}{{error, {module}::policy}};\n"),
        format!("use {top}{{{module}}};\n"),
        format!("use {top}{{\n    error,\n    {module} as hub,\n}};\n"),
    ] {
        assert_eq!(upward_paths(&text), [hub.as_str()], "{text}");
    }
    assert_eq!(
        upward_paths(&format!("use {hub}_settings::x;\n")),
        Vec::<String>::new()
    );
}

/// A build dependency on the SDK is listed and lies outside the allowlist, so the layering
/// row reads build edges too; the workspace is a stand-in of two path crates in a temp
/// folder, read offline.
#[test]
fn a_build_dependency_on_the_sdk_is_outside_the_allowlist() -> Result<(), Box<dyn std::error::Error>>
{
    let folder = tempfile::tempdir()?;
    let root = folder.path();
    for (path, text) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"sandbox\", \"sdk\"]\nresolver = \"2\"\n",
        ),
        (
            "sandbox/Cargo.toml",
            "[package]\nname = \"crazyagent-kernel-sandbox\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[build-dependencies]\ncrazyagent-sdk = { path = \"../sdk\" }\n",
        ),
        ("sandbox/build.rs", "fn main() {}\n"),
        ("sandbox/src/lib.rs", ""),
        (
            "sdk/Cargo.toml",
            "[package]\nname = \"crazyagent-sdk\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("sdk/src/lib.rs", ""),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().ok_or("a fixture file has a folder")?)?;
        std::fs::write(path, text)?;
    }
    let mut lock = Command::new(env!("CARGO"));
    lock.args(["generate-lockfile", "--offline"])
        .current_dir(root);
    let locked = kernel_processes::spawn_gate::collected(&mut lock)?;
    assert!(
        locked.status.success(),
        "{}",
        String::from_utf8_lossy(&locked.stderr)
    );
    let listed = path_tree(root, "crazyagent-kernel-sandbox")?;
    assert_eq!(outside(&listed), ["crazyagent-sdk"], "{listed}");
    Ok(())
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
