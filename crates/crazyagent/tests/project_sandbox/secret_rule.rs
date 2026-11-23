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
