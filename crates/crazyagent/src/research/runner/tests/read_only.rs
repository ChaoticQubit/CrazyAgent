use super::*;

/// V243-FIRMLINK-LAUNCH-ROOT (rv-sec2 19:24): a read-only run's folder named through the
/// data volume passes the canonical-folder gate under its short name, and a folder reached
/// through a link is still refused there.
#[test]
#[cfg(target_os = "macos")]
fn a_read_only_run_takes_the_long_name_of_its_folder_but_refuses_a_link() {
    let root = engine_root();
    let runner =
        ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
            .unwrap();
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    let linked = root.base.join("linked");
    std::os::unix::fs::symlink(&folder, &linked).unwrap();
    let long = std::path::PathBuf::from(format!("/System/Volumes/Data{}", folder.display()));
    assert!(long.is_dir());
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let project = ProjectId::new("alpha").unwrap();
    let gated = |name: &str, folder: &std::path::Path| {
        let run = ReadOnlyRun {
            name,
            agent: "onboarding",
            kind: Some(kernel_sandbox::hidden_paths::RunKind::Onboarding),
            project: Some(&project),
            folder,
            prompt: "Map this repository.",
            account: &account,
            wall_time: Duration::from_secs(5),
            output_tokens: None,
        };
        matches!(
            runner.run_read_only(&run, &|| false),
            Err(RunError::Invalid(ref text)) if text == "read-only folder must be canonical"
        )
    };
    assert!(!gated("onboard-m-alpha-1", &long));
    assert!(gated("onboard-m-alpha-2", &linked));
}

#[test]
#[cfg(target_os = "macos")]
fn a_read_only_run_counts_under_its_project_even_on_an_endpoint_account() {
    let root = engine_root();
    let runner =
        ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
            .unwrap();
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let project = ProjectId::new("alpha").unwrap();
    let run = ReadOnlyRun {
        name: "onboard-m-alpha-1",
        agent: "onboarding",
        kind: Some(kernel_sandbox::hidden_paths::RunKind::Onboarding),
        project: Some(&project),
        folder: &folder,
        prompt: "Map this repository.",
        account: &account,
        wall_time: Duration::from_secs(5),
        output_tokens: None,
    };
    assert!(matches!(
        runner.run_read_only(&run, &|| false),
        Err(RunError::Invalid(_))
    ));
    let db = crate::usage::open(root.database.parent().unwrap()).unwrap();
    let row: (String, String, String, String, String) = db
        .query_row(
            "SELECT home,project,account,agent,harness FROM usage_runs",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    let home = root.state.join("agents/onboard-m-alpha-1/home");
    assert_eq!(row.0, home.to_string_lossy());
    assert_eq!(
        (
            row.1.as_str(),
            row.2.as_str(),
            row.3.as_str(),
            row.4.as_str()
        ),
        ("alpha", "", "onboarding", "codex")
    );
}

/// DEC-147.9: a read-only run on a project folder runs under the secret deny of that
/// folder, with the secret the owner shared back readable again and nothing else.
#[test]
#[cfg(target_os = "macos")]
fn a_read_only_run_keeps_the_project_secrets_closed_but_the_shared_back_ones() {
    let root = engine_root();
    let codex = root.base.join("bin").join("codex");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = codex.clone();
    let runner =
        ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
            .unwrap()
            .with_harness_lookup(Arc::new(move |harness: &str| {
                (harness == "codex").then(|| pinned.clone())
            }));
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join(".env"), "TOKEN=1\n").unwrap();
    let project = ProjectId::new("alpha").unwrap();
    let connection = rusqlite::Connection::open(&root.database).unwrap();
    let policy = kernel_sandbox::policy::AgentPolicy {
        shared_folders: kernel_sandbox::shared_folders::SharedFolders {
            secrets: vec![".env".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    crate::sandbox::policy_store::save(&connection, project.as_str(), &policy).unwrap();
    drop(connection);
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let run = ReadOnlyRun {
        name: "overview-alpha-1",
        agent: "overview",
        kind: Some(kernel_sandbox::hidden_paths::RunKind::Overview),
        project: Some(&project),
        folder: &folder,
        prompt: "Describe this project.",
        account: &account,
        wall_time: Duration::from_secs(5),
        output_tokens: None,
    };
    let _ = runner.run_read_only(&run, &|| false);
    let profile = fs::read_to_string(root.state.join("profiles").join("overview-alpha-1.sb"))
        .expect("the run installed its profile");
    assert!(
        profile.contains(&format!(
            "(require-not (subpath \"{}/.git\"))",
            folder.display()
        )),
        "{profile}"
    );
    assert!(
        profile.contains(&format!(
            "(allow file-read-data file-read-xattr\n  (subpath \"{}\")",
            folder.join(".env").display()
        )),
        "{profile}"
    );
}

/// CONFIGURED-HOMES-GUARD: a read-only run's profile denies the folder of configured SDK
/// homes in the engine root, as every guarded sandbox does, and its stand-in harness,
/// which reads a keyed account's dummy config there, never gets the text.
#[test]
#[cfg(target_os = "macos")]
fn a_read_only_run_denies_every_configured_sdk_home() {
    let root = engine_root();
    let engine = root.database.parent().unwrap().to_path_buf();
    let homes = engine.join("configured-sdk-homes");
    let config = homes
        .join("6".repeat(64))
        .join(".backboard")
        .join("config.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let dummy = "configured-homes-guard-dummy-read-only-config";
    fs::write(&config, dummy).unwrap();
    let codex = root.base.join("bin").join("codex");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(
        &codex,
        format!("#!/bin/sh\ncat '{}'\nexit 0\n", config.display()),
    )
    .unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = codex.clone();
    let runner =
        ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
            .unwrap()
            .with_harness_lookup(Arc::new(move |harness: &str| {
                (harness == "codex").then(|| pinned.clone())
            }));
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let run = ReadOnlyRun {
        name: "overview-alpha-6",
        agent: "overview",
        kind: Some(kernel_sandbox::hidden_paths::RunKind::Overview),
        project: None,
        folder: &folder,
        prompt: "Describe this project.",
        account: &account,
        wall_time: Duration::from_secs(5),
        output_tokens: None,
    };
    let _ = runner.run_read_only(&run, &|| false);
    let profile = fs::read_to_string(root.state.join("profiles").join("overview-alpha-6.sb"))
        .expect("the guarded run still installs its profile");
    assert!(
        profile.contains(&format!("\n  (subpath \"{}\")", homes.display())),
        "{profile}"
    );
    let events = root
        .state
        .join("agents")
        .join("overview-alpha-6")
        .join("events.jsonl");
    assert!(events.is_file());
    assert!(!fs::read_to_string(&events).unwrap().contains(dummy));
}

/// L4 r1 H1: a read-only run follows the project's shared folders choice too. Under
/// Chosen an untracked local folder not picked stays closed, and a saved Read-write never
/// opens the picked one for writes, since a read-only run only reads.
#[test]
#[cfg(target_os = "macos")]
fn a_read_only_run_keeps_the_folders_the_project_does_not_share_closed() {
    let root = engine_root();
    let codex = root.base.join("bin").join("codex");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = codex.clone();
    let runner =
        ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
            .unwrap()
            .with_harness_lookup(Arc::new(move |harness: &str| {
                (harness == "codex").then(|| pinned.clone())
            }));
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("README.md"), "readme\n").unwrap();
    repository_git(&folder, &["init", "-q"]);
    repository_git(&folder, &["add", "README.md"]);
    repository_git(&folder, &["commit", "-q", "-m", "Start"]);
    for name in ["notes", "drafts"] {
        fs::create_dir(folder.join(name)).unwrap();
        fs::write(folder.join(name).join("plan.txt"), "plan\n").unwrap();
    }
    let project = ProjectId::new("alpha").unwrap();
    let connection = rusqlite::Connection::open(&root.database).unwrap();
    let policy = kernel_sandbox::policy::AgentPolicy {
        shared_folders: kernel_sandbox::shared_folders::SharedFolders {
            scope: kernel_sandbox::shared_folders::SharedScope::Chosen,
            access: kernel_sandbox::shared_folders::SharedAccess::ReadWrite,
            chosen: vec!["notes".into()],
            secrets: Vec::new(),
        },
        ..Default::default()
    };
    crate::sandbox::policy_store::save(&connection, project.as_str(), &policy).unwrap();
    drop(connection);
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let run = ReadOnlyRun {
        name: "overview-alpha-2",
        agent: "overview",
        kind: Some(kernel_sandbox::hidden_paths::RunKind::Overview),
        project: Some(&project),
        folder: &folder,
        prompt: "Describe this project.",
        account: &account,
        wall_time: Duration::from_secs(5),
        output_tokens: None,
    };
    let _ = runner.run_read_only(&run, &|| false);
    let profile = fs::read_to_string(root.state.join("profiles").join("overview-alpha-2.sb"))
        .expect("the run installed its profile");
    assert!(
        profile.contains(&format!(
            "(subpath \"{}\")",
            folder.join("drafts").display()
        )),
        "{profile}"
    );
    let writes = profile
        .split("(allow file-write*")
        .nth(1)
        .and_then(|rest| rest.split("))\n").next())
        .expect("the profile allows its own writes");
    assert!(!writes.contains(&folder.display().to_string()), "{writes}");
    assert!(
        !profile.contains(&format!(
            "(deny file-write-unlink\n  (require-all\n    (subpath \"{}\")",
            folder.join("notes").display()
        )),
        "{profile}"
    );
}

#[cfg(target_os = "macos")]
const PROFILE_CREDENTIAL: &str = "sec-ro-guard-dummy-profile-credential";

/// Writes a dummy sign-in credential where a native profile keeps its own, under
/// `<engine>/native-profiles/<key>/home`. Input: the engine root. Output: the path of
/// the dummy `.credentials.json`, which holds [`PROFILE_CREDENTIAL`].
#[cfg(target_os = "macos")]
fn native_profile_credential(engine: &Path) -> PathBuf {
    let home = engine
        .join(crate::engine::bootstrap::NATIVE_PROFILES_FOLDER)
        .join("5".repeat(64))
        .join("home");
    fs::create_dir_all(&home).unwrap();
    let credential = home.join(".credentials.json");
    fs::write(&credential, PROFILE_CREDENTIAL).unwrap();
    assert!(credential.starts_with(engine));
    credential
}

/// A runner whose Codex is a stand-in at `codex` that prints the file at `target` and
/// exits. Inputs: the engine root fixture, the stand-in path and the file it reads.
/// Output: the runner.
#[cfg(target_os = "macos")]
fn reading_runner(root: &EngineRoot, codex: &Path, target: &Path) -> ResearchRunner {
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(
        codex,
        format!("#!/bin/sh\ncat '{}'\nexit 0\n", target.display()),
    )
    .unwrap();
    fs::set_permissions(codex, fs::Permissions::from_mode(0o700)).unwrap();
    let pinned = codex.to_path_buf();
    ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
        .unwrap()
        .with_harness_lookup(Arc::new(move |harness: &str| {
            (harness == "codex").then(|| pinned.clone())
        }))
}

/// Tells whether any regular file under `folder`, not following links, holds `text`.
/// Input: the folder and the text. Output: true when one does.
#[cfg(target_os = "macos")]
fn holds_text(folder: &Path, text: &str) -> bool {
    fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => holds_text(&entry.path(), text),
            Ok(kind) if kind.is_file() => fs::read(entry.path()).is_ok_and(|bytes| {
                bytes
                    .windows(text.len())
                    .any(|part| part == text.as_bytes())
            }),
            _ => false,
        })
}

/// SEC-RO-GUARD: a read-only run gets the engine-secret wall every other sandbox gets.
/// Its Seatbelt profile names the native sign-in profiles folder, the run still starts
/// with its own home, work and temp folders inside the engine root, and a stand-in agent
/// that tries to read a profile's credentials gets nothing.
#[test]
#[cfg(target_os = "macos")]
fn a_read_only_run_cannot_read_a_native_profile_credential() {
    let root = engine_root();
    let engine = root.database.parent().unwrap().to_path_buf();
    let credential = native_profile_credential(&engine);
    let runner = reading_runner(&root, &root.base.join("bin").join("codex"), &credential);
    let folder = root.base.join("repo");
    fs::create_dir(&folder).unwrap();
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let run = ReadOnlyRun {
        name: "overview-alpha-3",
        agent: "overview",
        kind: Some(kernel_sandbox::hidden_paths::RunKind::Overview),
        project: None,
        folder: &folder,
        prompt: "Describe this project.",
        account: &account,
        wall_time: Duration::from_secs(5),
        output_tokens: None,
    };
    let _ = runner.run_read_only(&run, &|| false);
    let profile = fs::read_to_string(root.state.join("profiles").join("overview-alpha-3.sb"))
        .expect("the guarded run still installs its profile");
    let profiles = engine.join(crate::engine::bootstrap::NATIVE_PROFILES_FOLDER);
    assert!(
        profile.contains(&format!("(subpath \"{}\")", profiles.display())),
        "{profile}"
    );
    let agent = root.state.join("agents").join("overview-alpha-3");
    assert!(agent.join("events.jsonl").is_file());
    assert!(!holds_text(&agent, PROFILE_CREDENTIAL));
}

/// SEC-RO-GUARD: no read-only grant may hold the engine root. A run folder holding it
/// and a harness installed in it are each refused before the agent starts, so the
/// stand-in never reads a native profile's credentials.
#[test]
#[cfg(target_os = "macos")]
fn a_read_only_grant_that_holds_the_engine_root_is_refused() {
    let root = engine_root();
    let engine = root.database.parent().unwrap().to_path_buf();
    let credential = native_profile_credential(&engine);
    let repo = root.base.join("repo");
    fs::create_dir(&repo).unwrap();
    let account = ResearchAccount::Endpoint {
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "local-model".into(),
        key: None,
    };
    let cases = [
        (
            "overview-alpha-4",
            root.base.clone(),
            root.base.join("bin").join("codex"),
        ),
        ("overview-alpha-5", repo, engine.join("codex")),
    ];
    for (name, folder, codex) in cases {
        assert!(engine.starts_with(&folder) || engine.starts_with(codex.parent().unwrap()));
        let runner = reading_runner(&root, &codex, &credential);
        let run = ReadOnlyRun {
            name,
            agent: "overview",
            kind: Some(kernel_sandbox::hidden_paths::RunKind::Overview),
            project: None,
            folder: &folder,
            prompt: "Describe this project.",
            account: &account,
            wall_time: Duration::from_secs(5),
            output_tokens: None,
        };
        let ending = runner.run_read_only(&run, &|| false);
        assert!(
            matches!(
                &ending,
                Err(RunError::Sandbox(SandboxError::Invalid(reason)))
                    if reason == kernel_sandbox::paths::SECRET_ROOT_REFUSED
            ),
            "{name}: {ending:?}"
        );
        assert!(!holds_text(
            &root.state.join("agents").join(name),
            PROFILE_CREDENTIAL
        ));
    }
}
