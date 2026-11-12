#![cfg(target_os = "macos")]

use crazyagent::sandbox::toolchains::{self, ToolHomes, Toolchains};
use crazyagent::sandbox::{ProjectSandbox, SECRET_ROOT_REFUSED, SandboxError, SandboxNetwork};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    profiles: PathBuf,
    tools: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let profiles = root.join("profiles");
    let tools = root.join("tools");
    for directory in [&project, &home, &tmp, &profiles, &tools] {
        fs::create_dir_all(directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    Case {
        _temp: temp,
        project,
        home,
        tmp,
        profiles,
        tools,
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn fake_tool_homes(tools: &Path) -> ToolHomes {
    write(
        &tools.join(".rustup/toolchains/stable-x/bin/rustc"),
        "toolchain fixture\n",
    );
    write(
        &tools.join(".rustup/settings.toml"),
        "default_toolchain = \"stable-x\"\n",
    );
    write(
        &tools.join(".rustup/update-hashes/stable-x"),
        "hash fixture\n",
    );
    write(&tools.join(".cargo/bin/cargo"), "proxy fixture\n");
    write(
        &tools.join(".cargo/registry/index.txt"),
        "registry fixture\n",
    );
    write(
        &tools.join(".cargo/credentials.toml"),
        "token = \"FIXTURE-CARGO-HOME-TOKEN\"\n",
    );
    write(
        &tools.join(".nvm/versions/node/v1/bin/node"),
        "node fixture\n",
    );
    write(
        &tools.join(".pyenv/versions/3.1/bin/python"),
        "python fixture\n",
    );
    write(
        &tools.join(".local/share/uv/python/cpython-3.1/bin/python"),
        "uv fixture\n",
    );
    write(
        &tools.join(".local/share/uv/credentials/credentials.toml"),
        "FIXTURE-UV-TOKEN\n",
    );
    ToolHomes::at(tools)
}

fn run(sandbox: &ProjectSandbox, profile: &Path, script: &str) -> Output {
    run_in(sandbox, profile, Path::new("/bin/sh"), script)
}

fn run_in(sandbox: &ProjectSandbox, profile: &Path, shell: &Path, script: &str) -> Output {
    sandbox.install(profile).unwrap();
    let mut command = sandbox.command(profile, shell);
    command
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crazyagent::spawn_gate::output(&mut command).unwrap()
}

fn path_of(command: &std::process::Command) -> String {
    command
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

fn node_harness(prefix: &Path) -> PathBuf {
    let node = prefix.join("bin").join("node");
    write(&node, "#!/bin/sh\necho \"node beside codex ran $1\"\n");
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let script = prefix.join("lib/node_modules/@openai/codex/bin/codex.js");
    write(&script, "#!/usr/bin/env node\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn quoted(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

fn owner_home() -> PathBuf {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .unwrap()
        .unwrap();
    user.dir.canonicalize().unwrap_or(user.dir)
}

fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn detection_grants_only_the_toolchains_the_project_uses() {
    let case = case();
    let homes = fake_tool_homes(&case.tools);
    let tools = &case.tools;

    let none = Toolchains::detect(&case.project, &homes);
    assert!(none.roots().is_empty() && none.files().is_empty());

    write(&case.project.join("Cargo.toml"), "[package]\n");
    let rust = Toolchains::detect(&case.project, &homes);
    assert_eq!(
        rust.roots(),
        [
            tools.join(".rustup/toolchains"),
            tools.join(".cargo/bin"),
            tools.join(".cargo/registry"),
        ]
    );
    assert_eq!(rust.files(), [tools.join(".rustup/settings.toml")]);
    assert!(
        rust.roots()
            .iter()
            .all(|root| !homes.cargo().starts_with(root) && !homes.rustup().starts_with(root))
    );

    fs::remove_file(case.project.join("Cargo.toml")).unwrap();
    write(&case.project.join("rust-toolchain.toml"), "[toolchain]\n");
    assert_eq!(
        Toolchains::detect(&case.project, &homes).roots(),
        rust.roots()
    );

    fs::remove_file(case.project.join("rust-toolchain.toml")).unwrap();
    write(&case.project.join("package.json"), "{}\n");
    write(&case.project.join("pyproject.toml"), "[project]\n");
    let scripts = Toolchains::detect(&case.project, &homes);
    assert_eq!(
        scripts.roots(),
        [
            tools.join(".nvm"),
            tools.join(".pyenv"),
            tools.join(".local/share/uv/python"),
        ]
    );
    assert!(scripts.files().is_empty());
}

#[test]
fn a_sandboxed_agent_cannot_read_a_credential_planted_inside_a_granted_tool_root() {
    let case = case();
    let homes = fake_tool_homes(&case.tools);
    let registry = case.tools.join(".cargo/registry");
    let planted = [
        registry.join("credentials.toml"),
        registry.join("deep/.npmrc"),
        registry.join("deep/.pypirc"),
        registry.join("deep/.netrc"),
        registry.join("deep/.git-credentials"),
        registry.join("deep/.ssh/id_ed25519"),
        registry.join("deep/.aws/config"),
        registry.join("deep/.config/gcloud/credentials.db"),
        registry.join("deep/.docker/config.json"),
        registry.join("deep/.kube/config"),
        registry.join("deep/pip.conf"),
        registry.join("deep/.yarnrc.yml"),
        case.tools.join(".nvm/versions/node/v1/etc/npmrc"),
    ];
    for (index, path) in planted.iter().enumerate() {
        write(path, &format!("FIXTURE-PLANTED-SECRET-{index}\n"));
    }
    let sources = registry.join("src/cloud-auth-1.0.0/src");
    write(&sources.join("credentials/mod.rs"), "credentials module\n");
    write(&sources.join("gcloud/mod.rs"), "gcloud module\n");
    write(&case.project.join("Cargo.toml"), "[package]\n");
    write(&case.project.join("package.json"), "{}\n");
    let detected = Toolchains::detect(&case.project, &homes);
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_tools(&detected)
    .unwrap();
    let profile = case.profiles.join("tools.sb");

    let allowed = format!(
        "cat {} {} {} {} {} {}",
        quoted(&registry.join("index.txt")),
        quoted(&case.tools.join(".rustup/settings.toml")),
        quoted(&case.tools.join(".rustup/toolchains/stable-x/bin/rustc")),
        quoted(&case.tools.join(".nvm/versions/node/v1/bin/node")),
        quoted(&sources.join("credentials/mod.rs")),
        quoted(&sources.join("gcloud/mod.rs")),
    );
    let output = run(&sandbox, &profile, &allowed);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for fixture in [
        "registry fixture",
        "default_toolchain",
        "toolchain fixture",
        "node fixture",
        "credentials module",
        "gcloud module",
    ] {
        assert!(stdout.contains(fixture), "{stdout}");
    }

    let denied: Vec<PathBuf> = planted
        .iter()
        .cloned()
        .chain([
            case.tools.join(".cargo/credentials.toml"),
            case.tools.join(".rustup/update-hashes/stable-x"),
            case.tools.join(".pyenv/versions/3.1/bin/python"),
        ])
        .collect();
    for path in &denied {
        let output = run(&sandbox, &profile, &format!("cat {}", quoted(path)));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!output.status.success(), "{} was readable", path.display());
        assert!(!stdout.contains("FIXTURE"), "{stdout}");
    }
    let listing = run(
        &sandbox,
        &profile,
        &format!("ls -a {}", quoted(&registry.join("deep"))),
    );
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
}

#[test]
fn owner_credential_locations_are_always_secret_and_never_grantable() {
    let case = case();
    let owner = owner_home();
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let secrets = sandbox.secret_paths();
    for credential in [
        owner.join(".cargo/credentials"),
        owner.join(".cargo/credentials.toml"),
        owner.join(".npmrc"),
        owner.join(".pypirc"),
        owner.join(".netrc"),
        owner.join(".git-credentials"),
        owner.join(".ssh"),
        owner.join(".aws"),
        owner.join(".config/gcloud"),
        PathBuf::from("/opt/homebrew/etc/npmrc"),
        PathBuf::from("/usr/local/etc/npmrc"),
    ] {
        assert!(secrets.contains(&credential), "{}", credential.display());
        assert!(
            sandbox
                .profile()
                .contains(&format!("(subpath \"{}\")", credential.display())),
            "{}",
            credential.display()
        );
    }
    match ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_read(&owner.join(".cargo"))
    {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, SECRET_ROOT_REFUSED),
        other => panic!("the owner's cargo home was granted: {other:?}"),
    }
}

#[test]
fn the_agent_temp_folder_is_short_private_writable_and_removed() {
    let case = case();
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    assert!(tmp.starts_with("/private/tmp"));
    assert!(tmp.as_os_str().len() <= 40, "{}", tmp.display());
    let metadata = fs::symlink_metadata(&tmp).unwrap();
    assert!(metadata.is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    assert_eq!(toolchains::make_tmp(&case.home).unwrap(), tmp);
    assert_eq!(toolchains::existing_tmp(&case.home), Some(tmp.clone()));

    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_scratch(&tmp)
    .unwrap();
    assert!(sandbox.writes(&tmp.join("file")));
    let output = run(
        &sandbox,
        &case.profiles.join("tmp.sb"),
        "echo scratch > \"$TMPDIR/file\" && cat \"$TMPDIR/file\" && echo \"$TMPDIR\"",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, format!("scratch\n{}\n", tmp.display()));

    toolchains::remove_tmp(&case.home).unwrap();
    assert!(!tmp.exists());
    assert_eq!(toolchains::existing_tmp(&case.home), None);
    toolchains::remove_tmp(&case.home).unwrap();

    let squatter = case.home.join("squatter");
    let squatted = toolchains::tmp_path(&squatter);
    std::os::unix::fs::symlink(&case.tmp, &squatted).unwrap();
    assert!(toolchains::make_tmp(&squatter).is_err());
    assert_eq!(toolchains::existing_tmp(&squatter), None);
    assert!(toolchains::remove_tmp(&squatter).is_err());
    assert!(
        fs::symlink_metadata(&squatted)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&squatted).unwrap();

    let widened = case.home.join("widened");
    let opened = toolchains::make_tmp(&widened).unwrap();
    fs::set_permissions(&opened, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(toolchains::existing_tmp(&widened), None);
    assert_eq!(toolchains::make_tmp(&widened).unwrap(), opened);
    assert_eq!(
        fs::symlink_metadata(&opened).unwrap().permissions().mode() & 0o777,
        0o700
    );
    toolchains::remove_tmp(&widened).unwrap();
    assert!(!opened.exists());
}

/// An agent writes inside its temp folder but cannot open, remove or replace the folder
/// itself, so the sockets the engine binds there stay where the engine put them (DEC-111
/// S1R1-1).
#[test]
fn an_agent_cannot_open_remove_or_replace_its_temp_folder() {
    let case = case();
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_scratch(&tmp)
    .unwrap();
    let output = run(
        &sandbox,
        &case.profiles.join("tmp.sb"),
        "echo kept > \"$TMPDIR/file\" && cat \"$TMPDIR/file\"; chmod 755 \"$TMPDIR\"; chmod +a 'everyone allow list' \"$TMPDIR\"; rm -rf \"$TMPDIR\"; ln -s / \"$TMPDIR\"; true",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "kept\n");
    let metadata = fs::symlink_metadata(&tmp);
    assert!(
        metadata.as_ref().is_ok_and(|metadata| metadata.is_dir()),
        "{metadata:?}"
    );
    assert_eq!(metadata.unwrap().permissions().mode() & 0o777, 0o700);
    let listed = crazyagent::spawn_gate::output(
        std::process::Command::new("/bin/ls")
            .arg("-lde")
            .arg(&tmp)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert_eq!(
        listed.lines().count(),
        1,
        "no access list was added: {listed}"
    );
    assert_eq!(toolchains::existing_tmp(&case.home), Some(tmp));
    toolchains::remove_tmp(&case.home).unwrap();
}

#[test]
fn an_agent_builds_and_tests_a_tiny_crate_offline_with_the_owner_toolchain() {
    let case = case();
    let homes = ToolHomes::owner().unwrap();
    let toolchains_before = names(&homes.rustup().join("toolchains"));
    write(
        &case.project.join("Cargo.toml"),
        "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    write(
        &case.project.join("src/lib.rs"),
        "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n}\n",
    );
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    toolchains::prepare_cargo_home(&case.home, &homes).unwrap();
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_tools(&Toolchains::detect(&case.project, &homes))
    .unwrap()
    .allow_scratch(&tmp)
    .unwrap();
    let profile = case.profiles.join("cargo.sb");
    let command = sandbox.command(&profile, Path::new("/bin/sh"));
    let env: Vec<(String, String)> = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_str().unwrap().to_owned(),
                value.unwrap().to_str().unwrap().to_owned(),
            )
        })
        .collect();
    for (name, value) in [
        ("RUSTUP_AUTO_INSTALL", "0".to_owned()),
        ("RUSTUP_HOME", homes.rustup().to_str().unwrap().to_owned()),
        (
            "CARGO_HOME",
            case.home.join(".cargo").to_str().unwrap().to_owned(),
        ),
        ("TMPDIR", tmp.to_str().unwrap().to_owned()),
    ] {
        assert!(
            env.contains(&(name.to_owned(), value.clone())),
            "{name}={value} missing from {env:?}"
        );
    }

    let output = run(
        &sandbox,
        &profile,
        "cargo build --offline && cargo test --offline",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("test tests::adds ... ok"), "{stdout}");
    assert!(case.project.join("target/debug").is_dir());
    assert!(!case.home.join(".rustup").exists());
    assert_eq!(names(&homes.rustup().join("toolchains")), toolchains_before);
    toolchains::remove_tmp(&case.home).unwrap();
}

#[test]
fn an_agent_compiles_links_and_runs_git_without_an_xcrun_cache_error() {
    let case = case();
    let homes = ToolHomes::owner().unwrap();
    write(
        &case.project.join("Cargo.toml"),
        "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    );
    write(
        &case.project.join("src/main.rs"),
        "fn main() {\n    println!(\"tiny ran\");\n}\n",
    );
    write(
        &case.project.join("hello.c"),
        "int main(void) {\n    return 0;\n}\n",
    );
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    toolchains::prepare_cargo_home(&case.home, &homes).unwrap();
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_tools(&Toolchains::detect(&case.project, &homes))
    .unwrap()
    .allow_scratch(&tmp)
    .unwrap();
    let profile = case.profiles.join("compile.sb");

    let output = run(
        &sandbox,
        &profile,
        "cc -o hello hello.c && ./hello && cargo run --offline && git --version",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("tiny ran"), "{stdout}");
    assert!(stdout.contains("git version"), "{stdout}");
    for refused in ["couldn't create cache file", "xcrun: error"] {
        assert!(!stderr.contains(refused), "{refused}: {stderr}");
    }
    toolchains::remove_tmp(&case.home).unwrap();
}

#[test]
fn a_missing_toolchain_is_never_installed_by_an_agent() {
    let case = case();
    let homes = ToolHomes::owner().unwrap();
    let toolchains_before = names(&homes.rustup().join("toolchains"));
    write(
        &case.project.join("Cargo.toml"),
        "[package]\nname = \"pinned\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&case.project.join("src/lib.rs"), "");
    write(
        &case.project.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.70.0\"\n",
    );
    toolchains::prepare_cargo_home(&case.home, &homes).unwrap();
    let sandbox = ProjectSandbox::new(&case.project, &case.home, &case.tmp, SandboxNetwork::Open)
        .unwrap()
        .allow_tools(&Toolchains::detect(&case.project, &homes))
        .unwrap();
    let output = run(
        &sandbox,
        &case.profiles.join("pinned.sb"),
        "cargo build --offline",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("is not installed"), "{stderr}");
    assert!(!case.home.join(".rustup").exists());
    assert_eq!(names(&homes.rustup().join("toolchains")), toolchains_before);
}

#[test]
fn a_heredoc_works_in_every_agent_shell() {
    let case = case();
    let scratch = toolchains::make_tmp(&case.home).unwrap();
    let granted = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_scratch(&scratch)
    .unwrap();
    let plain = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    for (sandbox, profile) in [(&granted, "granted.sb"), (&plain, "plain.sb")] {
        for shell in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            let output = run_in(
                sandbox,
                &case.profiles.join(profile),
                Path::new(shell),
                "cat <<EOF\nheredoc $((40 + 2))\nEOF\n",
            );
            assert!(
                output.status.success(),
                "{shell} {profile}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                "heredoc 42\n",
                "{shell} {profile}"
            );
        }
    }
    toolchains::remove_tmp(&case.home).unwrap();
}

#[test]
fn an_agent_never_writes_the_owner_toolchains_or_reads_their_credentials() {
    let case = case();
    let homes = fake_tool_homes(&case.tools);
    write(&case.project.join("Cargo.toml"), "[package]\n");
    toolchains::prepare_cargo_home(&case.home, &homes).unwrap();
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_tools(&Toolchains::detect(&case.project, &homes))
    .unwrap();
    let profile = case.profiles.join("walls.sb");
    let readable = run(
        &sandbox,
        &profile,
        &format!(
            "cat {} {}",
            quoted(&case.home.join(".cargo/registry/index.txt")),
            quoted(&case.tools.join(".rustup/settings.toml"))
        ),
    );
    assert!(
        readable.status.success(),
        "{}",
        String::from_utf8_lossy(&readable.stderr)
    );
    for target in [
        case.home.join(".cargo/registry/planted"),
        case.tools.join(".cargo/registry/planted"),
        case.tools.join(".cargo/bin/planted"),
        case.tools.join(".cargo/config.toml"),
        case.tools.join(".rustup/toolchains/stable-x/bin/planted"),
        case.tools.join(".rustup/settings.toml"),
    ] {
        let output = run(
            &sandbox,
            &profile,
            &format!("echo FIXTURE-PLANTED >> {}", quoted(&target)),
        );
        assert!(!output.status.success(), "{} was written", target.display());
        assert!(
            !fs::read_to_string(&target)
                .unwrap_or_default()
                .contains("FIXTURE-PLANTED"),
            "{}",
            target.display()
        );
    }
    let credential = run(
        &sandbox,
        &profile,
        &format!(
            "cat {}",
            quoted(&case.tools.join(".cargo/credentials.toml"))
        ),
    );
    assert!(!credential.status.success());
    assert!(!String::from_utf8_lossy(&credential.stdout).contains("FIXTURE"));
}

#[test]
fn a_toolchain_the_owner_switched_off_is_unreadable_and_one_switched_on_is_readable() {
    use crazyagent::sandbox::toolchains::Toolchain;
    let case = case();
    let homes = fake_tool_homes(&case.tools);
    write(&case.project.join("Cargo.toml"), "[package]\n");
    let used: Vec<Toolchain> = Toolchain::ALL
        .into_iter()
        .filter(|toolchain| toolchain.used_by(&case.project))
        .collect();
    assert_eq!(used, [Toolchain::Rust]);
    let probe = |chosen: &[Toolchain], name: &str| {
        let sandbox = ProjectSandbox::new(
            &case.project,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .allow_tools(&Toolchains::grant(&homes, chosen))
        .unwrap();
        let profile = case.profiles.join(name);
        [
            case.tools.join(".cargo/bin/cargo"),
            case.tools.join(".rustup/settings.toml"),
            case.tools.join(".nvm/versions/node/v1/bin/node"),
        ]
        .map(|file| {
            run(
                &sandbox,
                &profile,
                &format!("cat {} > /dev/null", quoted(&file)),
            )
            .status
            .success()
        })
    };
    assert_eq!(probe(&[], "none.sb"), [false, false, false]);
    assert_eq!(probe(&[Toolchain::Rust], "rust.sb"), [true, true, false]);
    assert_eq!(probe(&[Toolchain::Node], "node.sb"), [false, false, true]);
    assert_eq!(
        Toolchains::grant(&homes, &[Toolchain::Rust]).roots(),
        Toolchains::detect(&case.project, &homes).roots(),
        "a project that uses Rust gets the same grant as the Rust switch"
    );
}

struct AgentGit {
    case: Case,
    git: PathBuf,
}

impl AgentGit {
    fn new(lead: bool) -> Self {
        let case = case();
        let tmp = toolchains::make_tmp(&case.home).unwrap();
        if lead {
            toolchains::lead_git(&tmp, true).unwrap();
        }
        let git = tmp.join("bin/git");
        let metadata = fs::symlink_metadata(&git).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        Self { case, git }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = std::process::Command::new(&self.git);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.case.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Agent")
            .env("GIT_AUTHOR_EMAIL", "agent@example.invalid")
            .env("GIT_COMMITTER_NAME", "Agent")
            .env("GIT_COMMITTER_EMAIL", "agent@example.invalid")
            .env("LC_ALL", "C")
            .current_dir(&self.case.project)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crazyagent::spawn_gate::output(&mut command).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn head(&self) -> String {
        self.ok(&["rev-parse", "HEAD"])
    }

    fn refused(&self, args: &[&str], reason: &str) {
        let before = self.head();
        let output = self.run(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "git {args:?}: {stderr}");
        assert!(
            stderr.starts_with("crazyagent: git ")
                && stderr.contains("is refused")
                && stderr.contains("DEC-107")
                && stderr.contains(reason),
            "git {args:?}: {stderr}"
        );
        assert_eq!(self.head(), before, "git {args:?} moved HEAD");
        assert!(!self.case.project.join(".git/MERGE_HEAD").exists());
    }

    fn branch_with_commit(&self, branch: &str, from: &str) -> String {
        self.ok(&["switch", "-q", "-c", branch, from]);
        self.ok(&["commit", "-q", "--allow-empty", "-m", branch]);
        let tip = self.head();
        self.ok(&["switch", "-q", "-"]);
        tip
    }
}

impl Drop for AgentGit {
    fn drop(&mut self) {
        let _ = toolchains::remove_tmp(&self.case.home);
    }
}

#[test]
fn a_workers_git_refuses_to_merge_pull_rebase_or_cherry_pick_another_branch() {
    let agent = AgentGit::new(false);
    agent.ok(&["init", "-q", "--template=", "-b", "own"]);
    agent.ok(&["commit", "-q", "--allow-empty", "-m", "start"]);
    let start = agent.head();
    agent.branch_with_commit("other", &start);
    agent.ok(&["commit", "-q", "--allow-empty", "-m", "own work"]);
    agent.ok(&["config", "alias.mm", "merge"]);
    agent.ok(&["config", "alias.pk", "cherry-pick --no-commit"]);
    let reason = "tell your lead";
    for args in [
        &["merge", "other"][..],
        &["merge", "--no-ff", "--no-edit", "other"],
        &["pull", ".", "other"],
        &["pull"],
        &["rebase", "other"],
        &["cherry-pick", "other"],
        &["--no-pager", "merge", "other"],
        &["mm", "other"],
        &["pk", "other"],
        &["-c", "alias.m=merge", "m", "other"],
        &["-c", "ALIAS.m=merge", "status"],
        &["--config-env=alias.m=HOME", "m", "other"],
    ] {
        agent.refused(args, reason);
    }
    let abort = agent.run(&["merge", "--abort"]);
    let stderr = String::from_utf8_lossy(&abort.stderr);
    assert!(
        !abort.status.success() && stderr.contains("MERGE_HEAD") && !stderr.contains("crazyagent"),
        "{stderr}"
    );
    assert!(agent.ok(&["status", "--short"]).is_empty());
    assert_eq!(agent.ok(&["log", "-1", "--format=%s"]), "own work");
}

#[test]
fn a_leads_git_merges_only_its_own_workers_branches() {
    let agent = AgentGit::new(true);
    agent.ok(&["init", "-q", "--template=", "-b", "task"]);
    agent.ok(&["commit", "-q", "--allow-empty", "-m", "start"]);
    let start = agent.head();
    let ann = agent.branch_with_commit("task--ann", &start);
    agent.branch_with_commit("task--bob", &start);
    agent.branch_with_commit("other", &start);
    agent.branch_with_commit("task--ann--x", &start);
    let reason = "A lead merges only the branches of its own workers";
    let project = agent.case.project.to_str().unwrap().to_owned();
    for args in [
        &["merge", "--no-edit", "other"][..],
        &["merge", "--no-edit", "task--ann", "other"],
        &["merge", "--no-edit", "task--ann--x"],
        &["merge", "--no-edit", "task--"],
        &["-C", project.as_str(), "merge", "--no-edit", "task--ann"],
        &["rebase", "task--ann"],
        &["pull", ".", "task--ann"],
        &["cherry-pick", "task--ann"],
    ] {
        agent.refused(args, reason);
    }
    agent.ok(&["merge", "-q", "--no-ff", "--no-edit", "task--ann"]);
    assert_eq!(agent.ok(&["rev-parse", "HEAD^2"]), ann);
    agent.ok(&[
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "bob",
        "refs/heads/task--bob",
    ]);
    assert_eq!(agent.ok(&["log", "-1", "--format=%s"]), "bob");
}

fn agent_git_script(tmp: &Path) -> String {
    fs::read_to_string(tmp.join("bin/git")).unwrap()
}

fn real_git_of(script: &str) -> PathBuf {
    let line = script
        .lines()
        .find_map(|line| line.strip_prefix("git='"))
        .unwrap();
    PathBuf::from(line.strip_suffix('\'').unwrap())
}

#[test]
fn make_tmp_writes_the_agent_git_once_replaces_a_changed_copy_and_never_follows_a_planted_link() {
    use std::os::unix::fs::MetadataExt;
    let case = case();
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let git = tmp.join("bin/git");
    let script = agent_git_script(&tmp);
    assert!(script.starts_with("#!/bin/sh\ngit='/"), "{script}");
    assert!(script.contains("\nlead=0\n"), "{script}");
    assert!(script.ends_with("exec \"$git\" \"$@\"\n"), "{script}");
    let real = real_git_of(&script);
    assert_ne!(real, Path::new("/usr/bin/git"));
    assert!(fs::metadata(&real).unwrap().is_file());
    let first = fs::symlink_metadata(&git).unwrap();
    assert_eq!(first.permissions().mode() & 0o777, 0o700);
    assert_eq!(
        fs::symlink_metadata(tmp.join("bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    toolchains::make_tmp(&case.home).unwrap();
    assert_eq!(fs::symlink_metadata(&git).unwrap().ino(), first.ino());

    fs::write(&git, "#!/bin/sh\nexit 7\n").unwrap();
    toolchains::make_tmp(&case.home).unwrap();
    assert_eq!(agent_git_script(&tmp), script);
    assert_ne!(fs::symlink_metadata(&git).unwrap().ino(), first.ino());

    toolchains::lead_git(&tmp, true).unwrap();
    assert!(agent_git_script(&tmp).contains("\nlead=1\n"));
    toolchains::make_tmp(&case.home).unwrap();
    assert_eq!(agent_git_script(&tmp), script);

    fs::remove_file(&git).unwrap();
    fs::remove_dir(tmp.join("bin")).unwrap();
    let elsewhere = case.tmp.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, tmp.join("bin")).unwrap();
    assert_eq!(toolchains::make_tmp(&case.home).unwrap(), tmp);
    assert!(names(&elsewhere).is_empty());
    assert!(
        fs::symlink_metadata(tmp.join("bin"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(toolchains::lead_git(&tmp, true).is_err());
    assert!(names(&elsewhere).is_empty());
    assert_eq!(
        toolchains::with_agent_git(&case.home, "/usr/bin:/bin".to_owned()),
        "/usr/bin:/bin"
    );
    toolchains::remove_tmp(&case.home).unwrap();
}

/// A native lead's own temp folder starts empty and private on every start (TPH-2
/// residual): what an earlier start left is gone, a planted file or link at its name is
/// replaced by a fresh folder and the link's target keeps its files, and a short temp folder
/// open to others is refused.
#[test]
fn a_native_lead_s_own_temp_folder_starts_empty_and_private_and_never_follows_a_planted_link() {
    use std::os::unix::fs::MetadataExt;
    let case = case();
    let shared = toolchains::make_tmp(&case.home).unwrap();
    let own = toolchains::fresh_native_lead_tmp(&shared).unwrap();
    assert_eq!(own, toolchains::native_lead_tmp(&shared));
    assert_eq!(own.parent(), Some(shared.as_path()));
    let private = |path: &Path| {
        let metadata = fs::symlink_metadata(path).unwrap();
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.mode() & 0o777 == 0o700
            && names(path).is_empty()
    };
    assert!(private(&own));

    fs::create_dir(own.join("left")).unwrap();
    fs::write(own.join("left").join("staged.txt"), "earlier").unwrap();
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(toolchains::fresh_native_lead_tmp(&shared).unwrap(), own);
    assert!(private(&own));

    let elsewhere = case.tmp.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("kept.txt"), "kept").unwrap();
    fs::remove_dir(&own).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &own).unwrap();
    assert_eq!(toolchains::fresh_native_lead_tmp(&shared).unwrap(), own);
    assert!(private(&own));
    assert_eq!(names(&elsewhere), ["kept.txt"]);

    fs::remove_dir(&own).unwrap();
    fs::write(&own, "planted").unwrap();
    assert_eq!(toolchains::fresh_native_lead_tmp(&shared).unwrap(), own);
    assert!(private(&own));

    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        toolchains::fresh_native_lead_tmp(&shared)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(private(&own));
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o700)).unwrap();
    toolchains::remove_tmp(&case.home).unwrap();
}

#[test]
fn a_node_script_harness_runs_with_the_node_beside_it_inside_the_sandbox() {
    let case = case();
    let prefix = case
        .tools
        .join("nvm")
        .join("versions")
        .join("node")
        .join("v22.11.0");
    let script = node_harness(&prefix);
    let node = prefix.join("bin").join("node");
    let bin = prefix.join("bin");
    let bin = bin.to_str().unwrap();
    let plain = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_node_for(&node)
    .unwrap();
    let profile = case.profiles.join("plain.sb");
    plain.install(&profile).unwrap();
    assert!(!path_of(&plain.command(&profile, Path::new("/bin/sh"))).contains(bin));

    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_read(script.parent().unwrap())
    .unwrap()
    .allow_exec(&script)
    .unwrap()
    .allow_node_for(&script)
    .unwrap();
    let profile = case.profiles.join("node.sb");
    sandbox.install(&profile).unwrap();
    let mut harness = std::process::Command::new(&script);
    harness.current_dir(&case.project);
    let mut wrapped = sandbox.wrap(&profile, &harness).unwrap();
    let path = path_of(&wrapped);
    let folders: Vec<&str> = path.split(':').collect();
    let node_at = folders.iter().position(|folder| *folder == bin).unwrap();
    assert!(
        node_at
            < folders
                .iter()
                .position(|folder| *folder == "/usr/bin")
                .unwrap(),
        "{path}"
    );
    assert_eq!(
        path_of(&sandbox.command(&profile, Path::new("/bin/sh")))
            .split(':')
            .next(),
        Some(bin)
    );
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut wrapped).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("node beside codex ran {}", script.display())
    );
}

#[test]
fn a_node_harness_reads_its_node_and_package_but_no_npm_credential_or_other_package() {
    let case = case();
    let prefix = case.tools.join("nvm/versions/node/v22.11.0");
    let script = node_harness(&prefix);
    let node = prefix.join("bin/node");
    let package = prefix.join("lib/node_modules/@openai/codex");
    let vendor = package.join("vendor/codex.txt");
    write(&vendor, "VENDOR-READABLE\n");
    let planted = [
        prefix.join("etc/npmrc"),
        prefix.join(".npmrc"),
        package.join(".npmrc"),
        package.join("node_modules/dep/.npmrc"),
        prefix.join("lib/node_modules/other/token.txt"),
    ];
    for (index, path) in planted.iter().enumerate() {
        write(path, &format!("FIXTURE-NODE-SECRET-{index}\n"));
    }
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_read(script.parent().unwrap())
    .unwrap()
    .allow_exec(&script)
    .unwrap()
    .allow_node_for(&script)
    .unwrap();
    assert!(!sandbox.read_roots().contains(&prefix));
    assert_eq!(sandbox.tool_roots(), [prefix.join("bin"), package.clone()]);
    let mut probe = format!(
        "cat {} >/dev/null && echo node-read\ncat {}\n",
        quoted(&node),
        quoted(&vendor)
    );
    for path in &planted {
        probe.push_str(&format!(
            "cat {} 2>/dev/null || echo refused\n",
            quoted(path)
        ));
    }
    let profile = case.profiles.join("node-secrets.sb");
    let output = run(&sandbox, &profile, &probe);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("node-read"), "{stdout}");
    assert!(stdout.contains("VENDOR-READABLE"), "{stdout}");
    assert_eq!(stdout.matches("refused").count(), planted.len(), "{stdout}");
    assert!(!stdout.contains("FIXTURE-NODE-SECRET"), "{stdout}");
    let mut harness = std::process::Command::new(&script);
    harness.current_dir(&case.project);
    let mut wrapped = sandbox.wrap(&profile, &harness).unwrap();
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut wrapped).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("node beside codex ran {}", script.display())
    );
}

#[test]
fn a_node_prefix_in_the_project_an_agent_worktree_or_the_agent_home_gives_no_path_entry() {
    let case = case();
    for prefix in [
        case.project
            .join(".crazyagent/worktrees/agent-2/nvm/versions/node/v22.11.0"),
        case.project.join("tools/nvm/versions/node/v22.11.0"),
        case.home.join("nvm/versions/node/v22.11.0"),
    ] {
        let script = node_harness(&prefix);
        let sandbox = ProjectSandbox::new(
            &case.project,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .allow_node_for(&script)
        .unwrap();
        let profile = case.profiles.join("agent-node.sb");
        sandbox.install(&profile).unwrap();
        let bin = prefix.join("bin");
        assert!(
            !path_of(&sandbox.command(&profile, Path::new("/bin/sh")))
                .contains(bin.to_str().unwrap()),
            "{}",
            prefix.display()
        );
        assert!(sandbox.tool_roots().is_empty(), "{}", prefix.display());
    }
}

#[test]
fn a_sandboxed_harness_runs_git_through_the_agent_copy_without_the_xcrun_shim() {
    let case = case();
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let agent_bin = tmp.join("bin");
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_scratch(&tmp)
    .unwrap();
    let profile = case.profiles.join("git.sb");
    sandbox.install(&profile).unwrap();
    let path_of = |command: &std::process::Command| -> String {
        command
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    };
    let engine = sandbox.command(&profile, Path::new("/bin/sh"));
    assert!(!path_of(&engine).contains(agent_bin.to_str().unwrap()));
    let unconfined = sandbox
        .wrap_unconfined(std::process::Command::new("/bin/sh").current_dir(&case.project))
        .unwrap();
    assert!(!path_of(&unconfined).contains(agent_bin.to_str().unwrap()));

    let real = real_git_of(&agent_git_script(&tmp));
    for arguments in [
        &["init", "-q", "--template="][..],
        &[
            "-c",
            "user.name=A",
            "-c",
            "user.email=a@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
        ],
    ] {
        let mut owner = std::process::Command::new(&real);
        owner
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &case.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(arguments)
            .current_dir(&case.project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let made = crazyagent::spawn_gate::output(&mut owner).unwrap();
        assert!(made.status.success(), "{made:?}");
    }
    let mut harness = std::process::Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg("command -v git && git status --short >/dev/null && git log --format=%s")
        .current_dir(&case.project);
    let mut wrapped = sandbox.wrap(&profile, &harness).unwrap();
    assert!(path_of(&wrapped).starts_with(&format!("{}:", agent_bin.display())));
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut wrapped).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("xcrun"), "{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{}\none\n", agent_bin.join("git").display())
    );
    toolchains::remove_tmp(&case.home).unwrap();
}

#[test]
fn a_sandboxed_harness_git_runs_its_helpers_from_its_own_toolchain_not_the_xcrun_shims() {
    let case = case();
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_scratch(&tmp)
    .unwrap();
    let profile = case.profiles.join("git.sb");
    sandbox.install(&profile).unwrap();
    let real = real_git_of(&agent_git_script(&tmp));
    let upstream = case.home.join("upstream");
    fs::create_dir(&upstream).unwrap();
    for (folder, subject) in [(&case.project, "one"), (&upstream, "upstream")] {
        for arguments in [
            &["init", "-q", "--template="][..],
            &[
                "-c",
                "user.name=A",
                "-c",
                "user.email=a@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                subject,
            ],
        ] {
            let mut owner = std::process::Command::new(&real);
            owner
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &case.home)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(arguments)
                .current_dir(folder)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let made = crazyagent::spawn_gate::output(&mut owner).unwrap();
            assert!(made.status.success(), "{made:?}");
        }
    }
    let mut harness = std::process::Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg(format!(
            "GIT_TRACE=1 git fetch -q '{}' HEAD && git log -1 --format=%s FETCH_HEAD",
            upstream.display()
        ))
        .current_dir(&case.project);
    let mut wrapped = sandbox.wrap(&profile, &harness).unwrap();
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut wrapped).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "upstream\n");
    let helpers = real
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("libexec/git-core");
    assert!(
        stderr.contains(&format!(
            "resolved executable path from Darwin stack: {}\n",
            helpers.join("git-upload-pack").display()
        )),
        "{stderr}"
    );
    assert!(
        !stderr.contains("start_command: /usr/bin/") && !stderr.contains("xcrun"),
        "{stderr}"
    );
    toolchains::remove_tmp(&case.home).unwrap();
}

fn tmux_at(tmux: &Path, socket: &Path, args: &[&str]) -> Output {
    let mut command = std::process::Command::new(tmux);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg("-S")
        .arg(socket)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crazyagent::spawn_gate::output(&mut command).unwrap()
}

#[test]
fn a_lead_profile_runs_its_own_tmux_server_and_never_reaches_another() {
    let case = case();
    let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let own = tmp.join("tmux.sock");
    let engine = tmp.parent().unwrap().join(format!(
        "ca-engine-{}.sock",
        tmp.file_name().unwrap().to_str().unwrap()
    ));
    let base = || {
        ProjectSandbox::new(
            &case.project,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
    };
    let invalid = |result: Result<ProjectSandbox, SandboxError>| match result {
        Err(SandboxError::Invalid(reason)) => reason,
        other => panic!("granted: {:?}", other.map(|_| ())),
    };
    assert!(invalid(base().allow_tmux_server(&own, &tmux)).contains("temp folder"));
    let scratch = || base().allow_scratch(&tmp).unwrap();
    assert!(
        invalid(scratch().allow_tmux_server(&case.tmp.join("tmux.sock"), &tmux))
            .contains("agent temp folder")
    );
    assert!(
        invalid(scratch().allow_tmux_server(&own, Path::new("/opt/homebrew/bin/../bin/tmux")))
            .contains("canonical")
    );
    let sandbox = scratch().allow_tmux_server(&own, &tmux).unwrap();
    assert!(invalid(sandbox.clone().allow_tmux_server(&own, &tmux)).contains("already has a tmux"));
    let text = sandbox.profile();
    assert!(text.matches(own.to_str().unwrap()).count() >= 2, "{text}");
    assert!(!text.contains(engine.to_str().unwrap()));

    let outside = tmux_at(
        &tmux,
        &engine,
        &["new-session", "-d", "-s", "engine", "sleep 60"],
    );
    assert!(
        outside.status.success(),
        "{}",
        String::from_utf8_lossy(&outside.stderr)
    );
    let script = format!(
        "{t} -S {o} new-session -d -s lead 'sleep 60' && {t} -S {o} list-sessions -F '#{{session_name}}' && {t} -S {e} list-sessions",
        t = quoted(&tmux),
        o = quoted(&own),
        e = quoted(&engine),
    );
    let output = run(&sandbox, &case.profiles.join("lead.sb"), &script);
    let _ = tmux_at(&tmux, &own, &["kill-server"]);
    let _ = tmux_at(&tmux, &engine, &["kill-server"]);
    let _ = fs::remove_file(&engine);
    toolchains::remove_tmp(&case.home).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stdout, "lead\n", "{stderr}");
    assert!(
        !output.status.success(),
        "the lead listed the engine server: {stdout}"
    );
    assert!(
        stderr.contains("Operation not permitted") || stderr.contains("error connecting"),
        "{stderr}"
    );
}

/// Runs a probe script in one of a native lead's two sandboxes.
type Probe = fn(&LiveTeam, &str) -> Output;

/// Neither a native lead nor its teammates can bind, block or take the engine's caller
/// socket (DEC-111 Q18, rv-secreview RH-1): with the path free, a listener, a folder or a
/// file either places there is refused; once the engine listens, both reach it and get its
/// answer, yet removing, flagging or moving its node is refused, and the engine's socket
/// stays the one it bound, so every hook's answer on it comes from the engine.
#[test]
fn neither_a_native_lead_nor_its_teammates_bind_block_or_take_the_caller_socket() {
    use std::os::macos::fs::MetadataExt;
    let team = LiveTeam::start();
    let c = quoted(&team.callers);
    let roles: [(&str, Probe); 2] = [
        ("lead", LiveTeam::as_lead),
        ("teammate", LiveTeam::teammate),
    ];
    for (role, run) in roles {
        let free = run(
            &team,
            &format!(
                "{}; echo bind=$?\nmkdir {c}; echo mkdir=$?\n/usr/bin/touch {c}; echo file=$?\n",
                team.session(&team.callers, "planted")
            ),
        );
        let text = String::from_utf8_lossy(&free.stdout).into_owned();
        for name in ["bind", "mkdir", "file"] {
            assert_ne!(
                status_of(&text, name),
                "0",
                "the {role}'s {name} at the free caller socket path succeeded: {}",
                String::from_utf8_lossy(&free.stderr)
            );
        }
        assert!(
            fs::symlink_metadata(&team.callers).is_err(),
            "the {role} left a node at the caller socket path"
        );
    }

    let listener = crazyagent::spawn_gate::bind_unix_std(&team.callers).unwrap();
    listener.set_nonblocking(true).unwrap();
    let node = |path: &Path| {
        fs::symlink_metadata(path).ok().map(|meta| {
            (
                meta.st_dev(),
                meta.st_ino(),
                meta.st_mode(),
                meta.st_flags(),
            )
        })
    };
    let bound = node(&team.callers);
    assert!(bound.is_some());
    let engine = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut answered = 0;
        while answered < roles.len() && std::time::Instant::now() < deadline {
            match crazyagent::spawn_gate::accept_unix_std(&listener) {
                Ok(mut stream) => {
                    stream.set_nonblocking(false).unwrap();
                    let mut line = String::new();
                    std::io::BufRead::read_line(
                        &mut std::io::BufReader::new(&mut stream),
                        &mut line,
                    )
                    .unwrap();
                    assert_eq!(line, "who\n");
                    std::io::Write::write_all(&mut stream, b"engine\n").unwrap();
                    answered += 1;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        answered
    });
    for (role, run) in roles {
        let taken = run(
            &team,
            &format!(
                "printf 'who\\n' | /usr/bin/nc -U {c}; echo reach=$?\nrm -f {c}; echo rm=$?\nchflags uchg {c}; echo chflags=$?\nmv -f {c} {c}.away; echo mv=$?\n"
            ),
        );
        let text = String::from_utf8_lossy(&taken.stdout).into_owned();
        let errors = String::from_utf8_lossy(&taken.stderr);
        assert!(
            text.lines().any(|line| line == "engine"),
            "the {role} reached the engine: {text} {errors}"
        );
        assert_eq!(status_of(&text, "reach"), "0", "{errors}");
        for name in ["rm", "chflags", "mv"] {
            assert_ne!(
                status_of(&text, name),
                "0",
                "the {role}'s {name} of the caller socket succeeded: {errors}"
            );
        }
        assert_eq!(
            node(&team.callers),
            bound,
            "the engine's socket survives the {role}"
        );
    }
    assert_eq!(engine.join().unwrap(), roles.len());
}

/// A native lead's teammates neither read nor change the lead's part of the relay mailbox,
/// where the engine keeps the lead's letters, reports, asks and answers: reading a letter,
/// listing the part, taking a letter by rename, planting an entry, and removing or moving
/// the part are all refused, and only the roster the engine keeps there is readable. The
/// same reads, plants and moves spelled in another letter case, and a hard link of a
/// letter out of the part, are refused too (relay RC-1). The lead reads and takes its own
/// letters (DEC-111 relay mailbox forgery, C1).
#[test]
fn native_teammates_neither_read_nor_change_the_leads_part_of_the_relay_mailbox() {
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    let part = mailbox.join(crazyagent::coord::relay::LEAD_PART);
    fs::create_dir(&part).unwrap();
    fs::set_permissions(&part, fs::Permissions::from_mode(0o700)).unwrap();
    let letter = part.join(format!("{:032x}.inbox", 5));
    fs::write(&letter, b"letter for the lead").unwrap();
    fs::write(part.join("relay.json"), b"{}").unwrap();
    let (l, r, p) = (
        quoted(&letter),
        quoted(&part.join("relay.json")),
        quoted(&part),
    );
    let shown = quoted(&part.join(format!("{:032x}.shown", 5)));
    let away = quoted(&mailbox.join("away"));
    let upper = mailbox
        .join(crazyagent::coord::relay::LEAD_PART.to_uppercase())
        .join(letter.file_name().unwrap());
    let mixed = mailbox.join("Lead");
    let linked = mailbox.join("linked");
    let calls = team
        .case
        .tmp
        .join(crazyagent::team::tool::MAILBOX_FOLDER.to_uppercase());
    assert!(
        upper.is_file() && mixed.is_dir() && calls.is_dir(),
        "the volume finds the part and the mailbox in any letter case"
    );
    let (u, x, k, c) = (
        quoted(&upper),
        quoted(&mixed),
        quoted(&linked),
        quoted(&calls),
    );
    let gone = quoted(&team.case.tmp.join("calls-away"));
    let probed = team.teammate(&format!(
        "/bin/cat {l}; echo read=$?\n/bin/cat {r} >/dev/null; echo roster=$?\n/bin/ls {p}; echo list=$?\n/bin/mv {l} {shown}; echo take=$?\n/usr/bin/touch {p}/planted; echo plant=$?\n/bin/rm -r {p}; echo rm=$?\n/bin/mv {p} {away}; echo move=$?\n/bin/cat {u}; echo read_case=$?\n/usr/bin/touch {x}/planted; echo plant_case=$?\n/bin/ln {l} {k}; echo link=$?\n/bin/mv {c} {gone}; echo move_case=$?\n"
    ));
    let text = String::from_utf8_lossy(&probed.stdout).into_owned();
    let errors = String::from_utf8_lossy(&probed.stderr);
    assert!(
        !text.contains("letter for the lead"),
        "a teammate read the lead's letter: {text}"
    );
    for name in [
        "read",
        "list",
        "take",
        "plant",
        "rm",
        "move",
        "read_case",
        "plant_case",
        "link",
        "move_case",
    ] {
        assert_ne!(
            status_of(&text, name),
            "0",
            "the teammate's {name} in the lead's part succeeded: {errors}"
        );
    }
    assert_eq!(status_of(&text, "roster"), "0", "{errors}");
    assert!(
        letter.is_file()
            && !part.join("planted").exists()
            && fs::symlink_metadata(&linked).is_err()
            && mailbox.is_dir(),
        "the lead's part is as the engine left it"
    );

    let lead = team.as_lead(&format!(
        "/bin/cat {l} >/dev/null; echo read=$?\n/bin/mv {l} {shown}; echo take=$?\n"
    ));
    let text = String::from_utf8_lossy(&lead.stdout).into_owned();
    for name in ["read", "take"] {
        assert_eq!(
            status_of(&text, name),
            "0",
            "{}",
            String::from_utf8_lossy(&lead.stderr)
        );
    }
}

#[test]
fn native_teammates_never_move_the_folders_holding_the_leads_part_of_the_relay_mailbox() {
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    let part = mailbox.join(crazyagent::coord::relay::LEAD_PART);
    fs::create_dir(&part).unwrap();
    fs::set_permissions(&part, fs::Permissions::from_mode(0o700)).unwrap();
    let letter = part.join(format!("{:032x}.inbox", 5));
    fs::write(&letter, b"letter for the lead").unwrap();
    let (m, t) = (quoted(&mailbox), quoted(&team.case.tmp));
    let shifted = quoted(&team.case.tmp.join("shifted"));
    let uprooted = quoted(&team.case.project.join("uprooted"));
    let probed = team.teammate(&format!(
        "/bin/mv {m} {shifted}; echo shift=$?\n/bin/mv {t} {uprooted}; echo uproot=$?\n"
    ));
    let text = String::from_utf8_lossy(&probed.stdout).into_owned();
    let errors = String::from_utf8_lossy(&probed.stderr);
    for name in ["shift", "uproot"] {
        assert_ne!(
            status_of(&text, name),
            "0",
            "the teammate's {name} of a folder holding the lead's part succeeded: {errors}"
        );
    }
    assert!(
        letter.is_file(),
        "the lead's letter stays where the engine left it"
    );
}

/// A native lead in a project that hides as many folders as a project may save still gets
/// its teammates' sandbox, which hides the lead's part of the relay mailbox and the lead's
/// own temp folder besides (relay C1 merged with lead-own-tmp).
#[test]
fn a_native_team_starts_in_a_project_hiding_as_many_folders_as_it_may() {
    let case = case();
    let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let mates_home = team_homes(&case.home);
    let mailbox = case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    fs::create_dir(&mailbox).unwrap();
    fs::set_permissions(&mailbox, fs::Permissions::from_mode(0o700)).unwrap();
    let mut sandbox = ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_scratch(&tmp)
    .unwrap();
    for index in 0..crazyagent::sandbox::hidden_paths::HIDDEN_PATHS_MAX {
        sandbox = sandbox
            .hide(&case.project.join(format!("hidden-{index}")))
            .unwrap();
    }
    let team = sandbox.native_team(
        &tmp.join("tmux.sock"),
        &tmp.join("teammates-0123456789abcdef.sock"),
        &tmp.join(crazyagent::team::caller::CALLER_SOCKET),
        &tmux,
        &mates_home,
    );
    assert!(team.is_ok(), "{:?}", team.as_ref().err());
}

/// Neither a native lead nor its teammates can place, remove or move a node at the name the
/// engine binds its caller socket at before one rename moves it over the caller socket's
/// path (rv-secreview RH2R2-1): with the name free, a listener, a folder or a file either
/// places there is refused, and once the engine binds there, removing or moving its node
/// is refused, so no agent swaps its own listener in for the one the engine moves.
#[test]
fn neither_a_native_lead_nor_its_teammates_touch_the_caller_socket_s_binding_name() {
    let team = LiveTeam::start();
    let binding = crazyagent::team::caller::binding_path(&team.callers);
    assert!(binding != team.callers && binding.parent() == team.callers.parent());
    let b = quoted(&binding);
    let roles: [(&str, Probe); 2] = [
        ("lead", LiveTeam::as_lead),
        ("teammate", LiveTeam::teammate),
    ];
    for (role, run) in roles {
        let free = run(
            &team,
            &format!(
                "{}; echo bind=$?\nmkdir {b}; echo mkdir=$?\n/usr/bin/touch {b}; echo file=$?\n",
                team.session(&binding, "planted")
            ),
        );
        let _ = tmux_at(&team.tmux, &binding, &["kill-server"]);
        let text = String::from_utf8_lossy(&free.stdout).into_owned();
        for name in ["bind", "mkdir", "file"] {
            assert_ne!(
                status_of(&text, name),
                "0",
                "the {role}'s {name} at the free binding name succeeded: {}",
                String::from_utf8_lossy(&free.stderr)
            );
        }
        assert!(
            fs::symlink_metadata(&binding).is_err(),
            "the {role} left a node at the binding name"
        );
    }

    let listener = crazyagent::spawn_gate::bind_unix_std(&binding).unwrap();
    let node = || {
        fs::symlink_metadata(&binding)
            .ok()
            .map(|meta| std::os::unix::fs::MetadataExt::ino(&meta))
    };
    let bound = node();
    assert!(bound.is_some());
    for (role, run) in roles {
        let taken = run(
            &team,
            &format!("rm -f {b}; echo rm=$?\nmv -f {b} {b}.away; echo mv=$?\n"),
        );
        let text = String::from_utf8_lossy(&taken.stdout).into_owned();
        let errors = String::from_utf8_lossy(&taken.stderr);
        for name in ["rm", "mv"] {
            assert_ne!(
                status_of(&text, name),
                "0",
                "the {role}'s {name} at the binding name succeeded: {errors}"
            );
        }
        assert_eq!(node(), bound, "the engine's node survives the {role}");
    }
    drop(listener);
}

/// Reads one `name=status` line a probe script printed. Takes the output text and the name.
/// Returns the status, panicking when the line is missing.
fn status_of(text: &str, name: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name}= in {text}"))
        .to_owned()
}

#[test]
fn a_teammates_profile_serves_its_tmux_socket_but_never_reaches_unlinks_or_takes_the_leads() {
    let case = case();
    let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let own = tmp.join("tmux.sock");
    let mates = tmp.join("teammates-0123456789abcdef.sock");
    let scratch = || {
        ProjectSandbox::new(
            &case.project,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .allow_scratch(&tmp)
        .unwrap()
    };
    let invalid = |result: Result<ProjectSandbox, SandboxError>| match result {
        Err(SandboxError::Invalid(reason)) => reason,
        other => panic!("granted: {:?}", other.map(|_| ())),
    };
    let teammates = scratch()
        .serve_tmux_server(&mates, &tmux)
        .unwrap()
        .shut_unix_socket(&own)
        .unwrap();
    assert!(invalid(teammates.clone().reach_unix_socket(&mates)).contains("already has a grant"));
    assert!(
        invalid(teammates.clone().allow_tmux_server(&own, &tmux)).contains("already has a tmux")
    );
    let lead = scratch()
        .allow_tmux_server(&own, &tmux)
        .unwrap()
        .reach_unix_socket(&mates)
        .unwrap();

    let served = run(
        &teammates,
        &case.profiles.join("teammates.sb"),
        &format!(
            "{t} -S {m} new-session -d -s mates 'sleep 60'; echo start=$?\n\
             {t} -S {m} list-sessions; echo own=$?\n\
             rm -f {m}; echo rm=$?\n\
             mv {m} {m}.away; echo mv=$?\n\
             touch {o}; echo lead=$?\n",
            t = quoted(&tmux),
            m = quoted(&mates),
            o = quoted(&own),
        ),
    );
    let reached = run(
        &lead,
        &case.profiles.join("lead.sb"),
        &format!(
            "{t} -S {m} list-sessions -F '#{{session_name}}'; echo reach=$?\nrm -f {m}; echo rm=$?\n",
            t = quoted(&tmux),
            m = quoted(&mates),
        ),
    );
    let still = fs::symlink_metadata(&mates)
        .is_ok_and(|metadata| std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()));
    let _ = tmux_at(&tmux, &mates, &["kill-server"]);
    let _ = fs::remove_file(&mates);
    toolchains::remove_tmp(&case.home).unwrap();

    let served_text = String::from_utf8_lossy(&served.stdout).into_owned();
    let served_errors = String::from_utf8_lossy(&served.stderr);
    assert_eq!(status_of(&served_text, "start"), "0", "{served_errors}");
    for name in ["own", "rm", "mv", "lead"] {
        assert_ne!(
            status_of(&served_text, name),
            "0",
            "a teammate's {name} attempt succeeded: {served_errors}"
        );
    }
    assert!(!own.exists(), "a teammate made the lead's socket path");
    let reached_text = String::from_utf8_lossy(&reached.stdout).into_owned();
    let reached_errors = String::from_utf8_lossy(&reached.stderr);
    assert!(
        reached_text.starts_with("mates\n"),
        "{reached_text} {reached_errors}"
    );
    assert_eq!(status_of(&reached_text, "reach"), "0", "{reached_errors}");
    assert_ne!(status_of(&reached_text, "rm"), "0", "{reached_errors}");
    assert!(still, "the teammates' socket was removed or replaced");
}

/// One native lead's two sandboxes from the production grant set
/// (`ProjectSandbox::native_team`) with the lead's own tmux server and its teammates' server
/// live, each started inside its own installed profile as the engine starts them, and the
/// engine's caller socket path still free. Dropping it ends every tmux server at its paths
/// and removes the temp folder.
struct LiveTeam {
    case: Case,
    tmux: PathBuf,
    own: PathBuf,
    mates: PathBuf,
    callers: PathBuf,
    other: PathBuf,
    lead: ProjectSandbox,
    teammates: ProjectSandbox,
}

impl LiveTeam {
    /// Builds both sandboxes and starts the lead's `lead` session and the teammates' `mates`
    /// session. Takes nothing. Returns the live team, panicking when a server does not start.
    fn start() -> Self {
        let case = case();
        let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
        let tmp = toolchains::make_tmp(&case.home).unwrap();
        let own = tmp.join("tmux.sock");
        let mates = tmp.join("teammates-0123456789abcdef.sock");
        let callers = tmp.join(crazyagent::team::caller::CALLER_SOCKET);
        let mates_home = team_homes(&case.home);
        let mailbox = case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
        fs::create_dir(&mailbox).unwrap();
        fs::set_permissions(&mailbox, fs::Permissions::from_mode(0o700)).unwrap();
        let (lead, teammates) = ProjectSandbox::new(
            &case.project,
            &case.home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .allow_scratch(&tmp)
        .unwrap()
        .native_team(&own, &mates, &callers, &tmux, &mates_home)
        .unwrap();
        let team = Self {
            case,
            tmux,
            own,
            mates,
            callers,
            other: tmp.join("other.sock"),
            lead,
            teammates,
        };
        for (output, server) in [
            (team.as_lead(&team.session(&team.own, "lead")), "lead"),
            (team.teammate(&team.session(&team.mates, "mates")), "mates"),
        ] {
            assert!(
                output.status.success(),
                "the {server} server did not start: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        team
    }

    /// The script line that starts one detached tmux session at a socket. Takes the socket
    /// and the session name. Returns the line.
    fn session(&self, socket: &Path, name: &str) -> String {
        format!(
            "{} -S {} new-session -d -s {name} 'sleep 120'",
            quoted(&self.tmux),
            quoted(socket)
        )
    }

    /// Runs a script the way a native lead runs, in the lead's installed profile. Takes the
    /// script. Returns its output.
    fn as_lead(&self, script: &str) -> Output {
        run(&self.lead, &self.case.profiles.join("lead.sb"), script)
    }

    /// Runs a script the way a teammate pane runs, in the teammates' installed profile.
    /// Takes the script. Returns its output.
    fn teammate(&self, script: &str) -> Output {
        run(
            &self.teammates,
            &self.case.profiles.join("teammates.sb"),
            script,
        )
    }

    /// Starts a tmux server outside any sandbox at a socket, standing for a listener the
    /// engine or another process holds there. Takes the socket and the session name.
    fn listen(&self, socket: &Path, name: &str) {
        let started = tmux_at(
            &self.tmux,
            socket,
            &["new-session", "-d", "-s", name, "sleep 120"],
        );
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
    }

    /// The session names the tmux server at a socket lists, asked from outside any sandbox.
    /// Takes the socket. Returns the listing, or the client's error text.
    fn sessions(&self, socket: &Path) -> String {
        let listed = tmux_at(
            &self.tmux,
            socket,
            &["list-sessions", "-F", "#{session_name}"],
        );
        if listed.status.success() {
            String::from_utf8_lossy(&listed.stdout).into_owned()
        } else {
            String::from_utf8_lossy(&listed.stderr).into_owned()
        }
    }
}

impl Drop for LiveTeam {
    fn drop(&mut self) {
        for socket in [&self.own, &self.mates, &self.callers, &self.other] {
            let _ = tmux_at(&self.tmux, socket, &["kill-server"]);
        }
        let _ = toolchains::remove_tmp(&self.case.home);
    }
}

/// A native lead's teammate never holds a listener where the lead's team servers name their
/// calls (rv-audit TTDL F1 (1)): with the caller socket path free, a tmux server it starts
/// there never binds, and once a listener holds the path it can neither remove nor move the
/// node, so the listener there stays the one that bound it.
#[test]
fn a_teammate_never_binds_or_takes_the_caller_socket_path() {
    let team = LiveTeam::start();
    let free = team.teammate(&format!(
        "{}; echo bind=$?\n",
        team.session(&team.callers, "planted")
    ));
    let free_text = String::from_utf8_lossy(&free.stdout).into_owned();
    let free_errors = String::from_utf8_lossy(&free.stderr).into_owned();
    assert_ne!(
        status_of(&free_text, "bind"),
        "0",
        "a teammate bound a server at the caller socket path: {}",
        team.sessions(&team.callers)
    );
    assert!(
        fs::symlink_metadata(&team.callers).is_err(),
        "a teammate left a node at the caller socket path: {free_errors}"
    );

    team.listen(&team.callers, "engine");
    let taken = team.teammate(&format!(
        "rm -f {c}; echo rm=$?\nmv -f {c} {c}.away; echo mv=$?\n",
        c = quoted(&team.callers)
    ));
    let taken_text = String::from_utf8_lossy(&taken.stdout).into_owned();
    let taken_errors = String::from_utf8_lossy(&taken.stderr);
    for name in ["rm", "mv"] {
        assert_ne!(
            status_of(&taken_text, name),
            "0",
            "a teammate's {name} of the caller socket succeeded: {taken_errors}"
        );
    }
    assert_eq!(team.sessions(&team.callers), "engine\n");
}

/// A teammate cannot swap the live teammates' socket for another listener (rv-audit TTDL F1
/// (2)): renaming a live listener's node from the same folder, or the caller socket's node,
/// onto it is refused, as is a forced link over it, and the lead's tmux still lists the
/// teammates' `mates` session there.
#[test]
fn a_teammate_cannot_rename_another_listener_onto_the_live_teammates_socket() {
    let team = LiveTeam::start();
    team.listen(&team.other, "other");
    team.listen(&team.callers, "engine");
    let swapped = team.teammate(&format!(
        "mv -f {o} {m}; echo other=$?\nmv -f {c} {m}; echo caller=$?\nln -sf {o} {m}; echo link=$?\n",
        o = quoted(&team.other),
        c = quoted(&team.callers),
        m = quoted(&team.mates),
    ));
    let swapped_text = String::from_utf8_lossy(&swapped.stdout).into_owned();
    let swapped_errors = String::from_utf8_lossy(&swapped.stderr);
    for name in ["other", "caller", "link"] {
        assert_ne!(
            status_of(&swapped_text, name),
            "0",
            "a teammate's {name} swap onto the teammates' socket succeeded: {swapped_errors}"
        );
    }
    let reached = team.as_lead(&format!(
        "{} -S {} list-sessions -F '#{{session_name}}'; echo reach=$?\n",
        quoted(&team.tmux),
        quoted(&team.mates)
    ));
    let reached_text = String::from_utf8_lossy(&reached.stdout).into_owned();
    let reached_errors = String::from_utf8_lossy(&reached.stderr);
    assert!(
        reached_text.starts_with("mates\n"),
        "{reached_text} {reached_errors}"
    );
    assert_eq!(status_of(&reached_text, "reach"), "0", "{reached_errors}");
    assert_eq!(team.sessions(&team.other), "other\n");
    assert_eq!(team.sessions(&team.callers), "engine\n");
}

/// A link at the caller socket path naming the lead's live tmux socket reaches nothing
/// (rv-audit TTDL F1 (3)): a teammate places neither a symbolic nor a hard link there, and
/// through either link placed from outside the sandbox it still cannot connect, since
/// Seatbelt checks the path the listener bound.
#[test]
fn a_link_at_the_caller_socket_path_never_reaches_the_lead_s_server() {
    let team = LiveTeam::start();
    let placed = team.teammate(&format!(
        "ln -s {o} {c}; echo symbolic=$?\nrm -f {c}\nln {o} {c}; echo hard=$?\nrm -f {c}\n",
        o = quoted(&team.own),
        c = quoted(&team.callers),
    ));
    let placed_text = String::from_utf8_lossy(&placed.stdout).into_owned();
    let placed_errors = String::from_utf8_lossy(&placed.stderr);
    for name in ["symbolic", "hard"] {
        assert_ne!(
            status_of(&placed_text, name),
            "0",
            "a teammate placed a {name} link at the caller socket path: {placed_errors}"
        );
    }

    let connect = format!(
        "{} -S {} list-sessions -F '#{{session_name}}'; echo connect=$?\n",
        quoted(&team.tmux),
        quoted(&team.callers)
    );
    for kind in ["symbolic", "hard"] {
        match kind {
            "symbolic" => std::os::unix::fs::symlink(&team.own, &team.callers).unwrap(),
            _ => fs::hard_link(&team.own, &team.callers).unwrap(),
        }
        let through = team.teammate(&connect);
        fs::remove_file(&team.callers).unwrap();
        let through_text = String::from_utf8_lossy(&through.stdout).into_owned();
        let through_errors = String::from_utf8_lossy(&through.stderr);
        assert!(
            !through_text.contains("lead"),
            "a teammate reached the lead's server through a {kind} link: {through_text}"
        );
        assert_ne!(
            status_of(&through_text, "connect"),
            "0",
            "{kind}: {through_errors}"
        );
    }
    assert_eq!(team.sessions(&team.own), "lead\n");
}

/// Makes the folders a native lead's two sandboxes need beside its home, as the engine makes
/// them: the team folders Claude Code shares in the lead's home and the teammates' private
/// home beside it. Takes the lead's home. Returns the teammates' home.
fn team_homes(home: &Path) -> PathBuf {
    for folder in crazyagent::sandbox::CLAUDE_TEAM_SHARED {
        fs::create_dir_all(home.join(folder)).unwrap();
    }
    let mates = home.with_file_name("mates-home");
    fs::create_dir_all(&mates).unwrap();
    fs::set_permissions(&mates, fs::Permissions::from_mode(0o700)).unwrap();
    mates
}

/// A native lead's teammates' home sits beside the lead's, and no root the teammates write
/// holds the lead's home (TPH-2 C): `native_team` refuses a teammates' home inside the
/// lead's, and a lead's home inside the project folder its teammates write, and builds the
/// engine's shape.
#[test]
fn native_team_refuses_a_teammates_home_in_the_lead_s_or_a_lead_home_the_project_holds() {
    let case = case();
    let tmux = crazyagent::terminal::tmux::installed(&case.project).expect("tmux is installed");
    let tmp = toolchains::make_tmp(&case.home).unwrap();
    let mailbox = case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    fs::create_dir(&mailbox).unwrap();
    fs::set_permissions(&mailbox, fs::Permissions::from_mode(0o700)).unwrap();
    let team = |home: &Path, mates_home: &Path| {
        ProjectSandbox::new(
            &case.project,
            home,
            &case.tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .allow_scratch(&tmp)
        .unwrap()
        .native_team(
            &tmp.join("tmux.sock"),
            &tmp.join("teammates-0123456789abcdef.sock"),
            &tmp.join(crazyagent::team::caller::CALLER_SOCKET),
            &tmux,
            mates_home,
        )
    };
    let beside = team_homes(&case.home);
    let inside = case.home.join("mates-home");
    fs::create_dir_all(&inside).unwrap();
    fs::set_permissions(&inside, fs::Permissions::from_mode(0o700)).unwrap();
    let held = case.project.join("agent").join("home");
    fs::create_dir_all(&held).unwrap();
    fs::set_permissions(&held, fs::Permissions::from_mode(0o700)).unwrap();
    let held_mates = team_homes(&held);

    let refused = |made: Result<(ProjectSandbox, ProjectSandbox), SandboxError>| match made {
        Err(SandboxError::Invalid(reason)) => reason,
        other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
    };
    assert_eq!(
        refused(team(&case.home, &inside)),
        "the teammates' home must sit beside the lead's"
    );
    assert_eq!(
        refused(team(&held, &held_mates)),
        "a teammates' writable root holds the lead's home"
    );
    assert!(team(&case.home, &beside).is_ok());
    toolchains::remove_tmp(&case.home).unwrap();
}

/// SECAUDIT team-mailbox-reply-forgery-shared-native-lead: no agent of a native team makes or
/// changes an answer in the tool mailbox the team shares, by any route or spelling or by
/// moving the folder itself, so the answer another agent reads is the engine's. Reading and
/// listing stay open. Each door has its own name and every door left open is listed at once.
#[test]
fn no_agent_of_a_native_team_can_forge_or_change_an_answer_in_the_tool_mailbox() {
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    let folded = team
        .case
        .tmp
        .join(crazyagent::team::tool::MAILBOX_FOLDER.to_uppercase());
    let volume_folds = folded.exists();
    let attempts = |id: u32| {
        let at = |suffix: &str| quoted(&mailbox.join(format!("{id:032x}{suffix}")));
        let (forge, upper, moved, linked) = (
            at("-forge.reply"),
            at("-upper.REPLY"),
            at("-moved.reply"),
            at("-linked.reply"),
        );
        let (symlinked, cloned, part) = (
            at("-symlinked.reply"),
            at("-cloned.reply"),
            at(".reply-part"),
        );
        let (side, other, control) = (at(".side"), at(".other"), at(".control"));
        let engine = quoted(&mailbox.join(format!("{:032x}.reply", id + 1)));
        let in_case = quoted(&folded.join(format!("{id:032x}-case.reply")));
        let spelled = quoted(&folded);
        format!(
            "swap() {{ /usr/bin/perl -e '$r = syscall(488, -2, $ARGV[0], -2, $ARGV[1], 2); exit($r == 0 ? 0 : 1)' \"$1\" \"$2\"; }}\n\
             printf forged > {forge}; echo forge=$?\n\
             printf forged > {upper}; echo upper=$?\n\
             printf forged > {side}; /bin/mv {side} {moved}; echo moved=$?\n\
             printf forged > {side}; /bin/ln {side} {linked}; echo linked=$?\n\
             /bin/ln -s {side} {symlinked}; echo symlinked=$?\n\
             printf forged >> {engine}; echo changed=$?\n\
             /usr/bin/touch {part}; echo plant=$?\n\
             /bin/cat {engine} > /dev/null; echo read=$?\n\
             printf forged > {side}; /bin/cp -c {side} {control}; echo cloneok=$?\n\
             /bin/cp -c {side} {cloned}; echo cloned=$?\n\
             /bin/mv -f {side} {engine}; echo overwrote=$?\n\
             printf forged > {side}; printf other > {other}; swap {side} {other}; echo swapok=$?\n\
             swap {side} {engine}; echo swapped=$?\n\
             printf forged > {in_case}; echo foldercase=$?\n\
             /bin/ls {spelled} > /dev/null; echo resolves=$?\n"
        )
    };
    let moves = |id: u32, who: &str| {
        let answer = format!("{id:032x}-folder.reply");
        let place = |name: &str| team.case.tmp.join(format!("{name}-{who}"));
        let (aside, made) = (place("aside"), place("built"));
        let (aside_answer, made_answer) =
            (quoted(&aside.join(&answer)), quoted(&made.join(&answer)));
        let (aside, made, gone, again) = (
            quoted(&aside),
            quoted(&made),
            quoted(&place("gone")),
            quoted(&place("again")),
        );
        let (folder, spelled) = (quoted(&mailbox), quoted(&folded));
        format!(
            "/bin/mv {folder} {aside}; echo aside=$?\n\
             printf forged > {aside_answer}; /bin/mv {aside} {folder}; echo back=$?\n\
             /bin/mkdir {made}; echo built=$?; printf forged > {made_answer}\n\
             /bin/mv {folder} {gone}; echo away=$?\n\
             /usr/bin/perl -e 'exit(rename($ARGV[0], $ARGV[1]) ? 0 : 1)' {made} {folder}; echo replaced=$?\n\
             /bin/mv {spelled} {again}; echo asidecase=$?\n"
        )
    };
    let mut open = Vec::new();
    for (who, id) in [("teammate", 20_u32), ("lead", 30_u32)] {
        let engine = mailbox.join(format!("{:032x}.reply", id + 1));
        fs::write(&engine, b"engine").unwrap();
        let run = |script: &str| {
            if who == "lead" {
                team.as_lead(script)
            } else {
                team.teammate(script)
            }
        };
        let output = run(&attempts(id));
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let errors = String::from_utf8_lossy(&output.stderr).into_owned();
        for name in [
            "forge",
            "upper",
            "moved",
            "linked",
            "symlinked",
            "changed",
            "plant",
            "cloned",
            "overwrote",
            "swapped",
            "foldercase",
        ] {
            if status_of(&text, name) == "0" {
                open.push(format!("{who} {name}"));
            }
        }
        for name in ["read", "cloneok", "swapok"] {
            assert_eq!(status_of(&text, name), "0", "{who} {name}: {errors}");
        }
        assert_eq!(
            status_of(&text, "resolves") == "0",
            volume_folds,
            "{who}: {errors}"
        );
        for name in [
            "-forge.reply",
            "-upper.REPLY",
            "-moved.reply",
            "-linked.reply",
            "-symlinked.reply",
            "-cloned.reply",
            ".reply-part",
            "-case.reply",
        ] {
            if fs::symlink_metadata(mailbox.join(format!("{id:032x}{name}"))).is_ok() {
                open.push(format!("{who} made {name}"));
            }
        }
        if fs::read(&engine).unwrap() != b"engine" {
            open.push(format!("{who} engine bytes"));
        }
        let output = run(&moves(id, who));
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let errors = String::from_utf8_lossy(&output.stderr).into_owned();
        for name in ["aside", "back", "away", "replaced", "asidecase"] {
            if status_of(&text, name) == "0" {
                open.push(format!("{who} {name}"));
            }
        }
        if who == "lead" {
            assert_eq!(status_of(&text, "built"), "0", "{who}: {errors}");
        }
        if mailbox.join(format!("{id:032x}-folder.reply")).exists() {
            open.push(format!("{who} folder forged"));
        }
    }
    assert_eq!(open, Vec::<String>::new(), "doors left open");
}

/// SECAUDIT-8-R2-L1: an empty tool mailbox is as fixed as one holding answers. `rename` can
/// replace an empty folder, so neither agent of a native team replaces it or swaps it with a
/// folder of its own. Each first moves that folder, so the refusal is the mailbox's own.
#[test]
fn no_agent_of_a_native_team_replaces_or_swaps_an_empty_tool_mailbox() {
    use std::os::unix::fs::MetadataExt;
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    let pinned = fs::metadata(&mailbox).unwrap().ino();
    assert_eq!(fs::read_dir(&mailbox).unwrap().count(), 0);
    for who in ["teammate", "lead"] {
        let place = |name: &str| team.case.tmp.join(format!("{name}-{who}"));
        let (made, moved) = (place("made"), place("moved"));
        fs::create_dir(&made).unwrap();
        fs::set_permissions(&made, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(made.join("forged.reply"), b"forged").unwrap();
        let (from, to, folder) = (quoted(&made), quoted(&moved), quoted(&mailbox));
        let script = format!(
            "swap() {{ /usr/bin/perl -e '$r = syscall(488, -2, $ARGV[0], -2, $ARGV[1], 2); exit($r == 0 ? 0 : 1)' \"$1\" \"$2\"; }}\n\
             rename() {{ /usr/bin/perl -e 'exit(rename($ARGV[0], $ARGV[1]) ? 0 : 1)' \"$1\" \"$2\"; }}\n\
             rename {from} {to}; echo control=$?\n\
             rename {to} {folder}; echo replaced=$?\n\
             swap {to} {folder}; echo swappedfolder=$?\n"
        );
        let output = if who == "lead" {
            team.as_lead(&script)
        } else {
            team.teammate(&script)
        };
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let errors = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(status_of(&text, "control"), "0", "{who}: {errors}");
        for name in ["replaced", "swappedfolder"] {
            assert_ne!(status_of(&text, name), "0", "{who} {name}: {errors}");
        }
        assert_eq!(fs::metadata(&mailbox).unwrap().ino(), pinned, "{who}");
        assert_eq!(fs::read_dir(&mailbox).unwrap().count(), 0, "{who}");
    }
}

/// What the tool server of each agent of a native team does stays open beside those rules:
/// it sends a call and removes the answer it has read.
#[test]
fn native_team_agents_still_send_a_call_and_remove_the_answer_they_read() {
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    for (who, number) in [("teammate", 40_u32), ("lead", 50_u32)] {
        let id = format!("{number:032x}");
        let answer = mailbox.join(format!("{id}.reply"));
        fs::write(&answer, b"{}").unwrap();
        let call = mailbox.join(format!("{id}.call"));
        let (part, sent, reply) = (
            quoted(&mailbox.join(format!("{id}.part"))),
            quoted(&call),
            quoted(&answer),
        );
        let script = format!(
            "/usr/bin/touch {part}; echo part=$?\n\
             /bin/mv {part} {sent}; echo call=$?\n\
             /bin/cat {reply} > /dev/null; echo read=$?\n\
             /bin/rm {reply}; echo removed=$?\n"
        );
        let output = if who == "lead" {
            team.as_lead(&script)
        } else {
            team.teammate(&script)
        };
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let errors = String::from_utf8_lossy(&output.stderr);
        for name in ["part", "call", "read", "removed"] {
            assert_eq!(status_of(&text, name), "0", "{who} {name}: {errors}");
        }
        assert!(call.exists(), "{who}");
        assert!(!answer.exists(), "{who}");
    }
}
