use super::*;

/// v2.4.0 gate, security: every `SECRET_NAMES` entry stays denied once the secret rule is
/// split into strings Seatbelt reads. Each entry has one sample path only it matches (grep
/// -E outside the sandbox proves the pairing), and under the real profile no sample, at the
/// top of the project or deeper, can be read, while a plain file next to them can.
#[test]
fn every_secret_name_stays_denied_once_the_secret_rule_is_split() {
    use crazyagent::sandbox::shared_folders::apply;
    use kernel_sandbox::shared_folders::SECRET_NAMES;
    use kernel_sandbox::shared_folders::SharedFolders;
    const SAMPLES: [&str; 25] = [
        ".env",
        ".git-credentials",
        ".netrc",
        ".npmrc",
        ".pypirc",
        ".pgpass",
        ".htpasswd",
        ".dockercfg",
        ".s3cfg",
        ".yarnrc.yml",
        "pip.conf",
        "kubeconfig",
        ".ssh/id",
        ".aws/config",
        ".gnupg/pubring.kbx",
        ".kube/config",
        ".docker/config.json",
        ".config/gcloud/adc",
        "id_ed25519",
        "credentials.json",
        "secrets.yaml",
        "my-service-account.json",
        "server.pem",
        "main.tfstate.backup",
        "prod.tfvars",
    ];
    assert_eq!(SAMPLES.len(), SECRET_NAMES.len());
    let case = case();
    let list = case.outside.join("samples.txt");
    fs::write(&list, format!("{}\n", SAMPLES.join("\n"))).unwrap();
    for (at, name) in SECRET_NAMES.iter().enumerate() {
        let found = kernel_processes::spawn_gate::output(
            std::process::Command::new("/usr/bin/grep")
                .arg("-Eix")
                .arg(format!("(.*/)?({name})(/.*)?"))
                .arg(&list)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let matched: Vec<String> = String::from_utf8_lossy(&found.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(matched, [SAMPLES[at]], "{name}");
    }
    let deep = case.project.join("deep").join("er");
    let mut paths = Vec::new();
    for sample in SAMPLES {
        for base in [&case.project, &deep] {
            let path = base.join(sample);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "secret").unwrap();
            paths.push(path);
        }
    }
    let plain = deep.join("plain.txt");
    fs::write(&plain, "plain").unwrap();
    let sandbox = apply(
        sandbox(&case, SandboxNetwork::Only(Vec::new())),
        &case.project,
        &SharedFolders::default(),
    )
    .unwrap();
    let text = sandbox.profile();
    let denies = text
        .matches(&format!(
            "(require-not (subpath \"{}/.git\"))",
            case.project.display()
        ))
        .count();
    assert!(denies >= 2, "{text}");
    let script = paths
        .iter()
        .map(|path| {
            format!(
                "cat {p} >/dev/null 2>&1 && echo open {p};",
                p = quoted(path)
            )
        })
        .chain([format!(
            "cat {} >/dev/null && echo plain-read",
            quoted(&plain)
        )])
        .collect::<Vec<_>>()
        .join(" ");
    let out = run(&sandbox, &case.profiles.join("secret-names.sb"), &script);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "plain-read\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn sandboxed_child_cannot_write_outside_the_project_or_into_the_owner_home() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("denied.sb");
    let outside = case.outside.join("escape.txt");
    let marker = format!("crazyagent-sandbox-probe-{}", std::process::id());
    let in_home = owner_home().join(&marker);
    let in_context = case.project.join("context").join("tampered.md");
    let sibling = case.root.join("sibling.txt");
    for target in [&outside, &in_home, &in_context, &sibling] {
        let script = format!("echo escaped > {}", quoted(target));
        let output = run(&sandbox, &profile, &script);
        assert!(!output.status.success(), "{}", target.display());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
            "{}: {}",
            target.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!target.exists(), "{}", target.display());
    }
    let brief = case.project.join("context").join("brief.md");
    let output = run(
        &sandbox,
        &profile,
        &format!("echo tampered >> {}", quoted(&brief)),
    );
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&brief).unwrap(), "brief\n");
    let link = case.project.join("worktrees").join("context-link");
    let output = run(
        &sandbox,
        &profile,
        &format!(
            "ln -s {} {} && echo tampered > {}/via-link.md",
            quoted(&case.project.join("context")),
            quoted(&link),
            quoted(&link)
        ),
    );
    assert!(!output.status.success());
    assert!(!case.project.join("context").join("via-link.md").exists());
}

#[test]
fn sandboxed_child_cannot_read_outside_files_or_the_owner_codex_and_claude_dirs() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Open);
    let profile = case.profiles.join("reads.sb");
    let output = run(
        &sandbox,
        &profile,
        &format!(
            "cat {} > /dev/null",
            quoted(&case.outside.join("fixture.txt"))
        ),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("outside fixture"));
    let home = owner_home();
    for private in [home.join(".codex"), home.join(".claude")] {
        assert!(
            sandbox
                .profile()
                .contains(&format!("(subpath \"{}\")", private.display())),
            "{}",
            private.display()
        );
        if !private.exists() {
            continue;
        }
        let script = format!(
            "ls -d {dir} > /dev/null 2>&1 && exit 10; cat {dir}/config.toml > /dev/null 2>&1 && exit 11; ls {dir} > /dev/null 2>&1 && exit 12; ls -d {dir} 2>&1 >/dev/null; exit 0",
            dir = quoted(&private)
        );
        let output = run(&sandbox, &profile, &script);
        assert_eq!(output.status.code(), Some(0), "{}", private.display());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Operation not permitted"),
            "{}",
            private.display()
        );
    }
}
