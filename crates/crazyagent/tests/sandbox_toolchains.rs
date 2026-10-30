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
