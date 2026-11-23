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

/// SECAUDIT sandbox-seatbelt-unfiltered-sysctl-read-procargs r2: `lsof` lists process ids
/// before it reads one, so listing them stays open and a confined agent can still ask what
/// its own process holds.
#[test]
fn a_confined_lsof_on_its_own_process_succeeds() {
    let case = case();
    let confined = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let output = run(
        &confined,
        &case.profiles.join("lsof.sb"),
        "/usr/sbin/lsof -p $$",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(text.contains("COMMAND") && text.contains("cwd"), "{text}");
}

/// B5 r2 (rv-sec3 F1, rv-code4 F1): a project folder named with long runs of `s` and `f`
/// gets a profile Seatbelt reads, so the agent starts, and the folder's own app deny still
/// holds under the bounded spelling of its name: a framework Python app planted there,
/// which the profile runs anywhere else, does not run.
#[test]
fn an_agent_starts_in_a_project_named_with_long_fold_runs_and_runs_no_app_planted_there() {
    let case = case();
    let project = case.root.join("Glasssssss-ffffffffff");
    let bundle = project.join("Py.framework/Versions/3/Resources/Python.app/Contents/MacOS");
    fs::create_dir_all(&bundle).unwrap();
    fs::copy("/usr/bin/true", bundle.join("Python")).unwrap();
    let sandbox = ProjectSandbox::new(
        &crazyagent::sandbox::PROFILE_HOOKS,
        &project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let profile = case.profiles.join("runs.sb");
    let started = run(&sandbox, &profile, "/usr/bin/true");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let planted = run(&sandbox, &profile, &quoted(&bundle.join("Python")));
    assert!(!planted.status.success(), "the planted app ran");
    let stderr = String::from_utf8_lossy(&planted.stderr);
    assert!(stderr.contains("Operation not permitted"), "{stderr}");
}

#[test]
fn an_agent_still_runs_framework_python_whose_launcher_starts_its_python_app() {
    let case = case();
    let sandbox = sandbox(&case, SandboxNetwork::Only(Vec::new()));
    let profile = case.profiles.join("python.sb");
    let python = Path::new("/opt/homebrew/bin/python3");
    let real = fs::canonicalize(python).unwrap();
    assert!(
        real.to_str().unwrap().contains(".framework/Versions/"),
        "the probe needs a framework build at {}",
        python.display()
    );
    let output = run(
        &sandbox,
        &profile,
        &format!("{} -c 'print(\"ok\")'", quoted(python)),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}
