use super::*;
use std::process::Command;

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

/// Setup finds a tool exactly where a launch finds it, in the tool resolver's places: a tmux
/// only in ~/.local/bin shows the entry and version the launch would run, and a tool found
/// nowhere, or a script whose interpreter is missing, gives the resolver's own answer for
/// that reason, whose line the setup row and the launch's failure both say.
#[test]
fn setup_finds_a_tool_where_the_launch_does_and_says_why_it_cannot_run() {
    let root = private_root();
    let home = root.path().join("home");
    let local = home.join(".local/bin");
    std::fs::create_dir_all(&local).unwrap();
    let places = crate::tools::Places::new(
        vec![root.path().join("setup/bin"), local.clone()],
        Some(home),
    );
    let installer = PinnedInstaller::with_places(root.path(), Vec::new(), places.clone());
    let missing = installer.lookup(Dependency::Tmux).unwrap().unwrap_err();
    assert_eq!(missing, crate::tools::resolve("tmux", &places).unwrap_err());
    assert_eq!(missing.line(), crate::tools::TMUX_MISSING);
    let script = |name: &str, text: &str| {
        let file = local.join(name);
        std::fs::write(&file, text).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    script("tmux", "#!/bin/sh\necho 'tmux 3.5a'\n");
    let launch = crate::tools::resolve("tmux", &places).unwrap();
    assert_eq!(
        installer.locate(Dependency::Tmux),
        Some((launch.entry, Version::parse("3.5a").unwrap()))
    );
    assert_eq!(
        installer.lookup(Dependency::Tmux).unwrap(),
        Ok(crate::setup::Located {
            version: Version::parse("3.5a").unwrap(),
            bundled: false,
            note: None,
        })
    );
    script(
        "graphify",
        "#!/usr/bin/env crazyagent-test-missing-python\nprint('graphify 0.9.9')\n",
    );
    let needs = installer.lookup(Dependency::Graphify).unwrap().unwrap_err();
    assert_eq!(needs.reason, crate::tools::Reason::NeedsInterpreter);
    assert_eq!(
        needs,
        crate::tools::resolve("graphify", &places).unwrap_err()
    );
}

/// DEC-157: setup finds the tmux the engine uses, the `crazyagent-tmux` the app ships beside
/// the engine, reads its version through the same words and marks it bundled, so its row says
/// Bundled with CrazyAgent; with the file gone it is never marked bundled.
#[test]
fn setup_marks_the_tmux_the_app_ships_beside_the_engine_as_bundled() {
    let root = private_root();
    let app = root.path().join("CrazyAgent.app/Contents/MacOS");
    std::fs::create_dir_all(&app).unwrap();
    let sidecar = app.join("crazyagent-tmux");
    std::fs::write(&sidecar, "#!/bin/sh\necho 'tmux 3.8'\n").unwrap();
    std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).unwrap();
    let places = crate::tools::Places::engine(
        None,
        Some(&app),
        Some(root.path().join("home")),
        Some(std::ffi::OsStr::new("")),
    );
    let installer = PinnedInstaller::with_places(root.path(), Vec::new(), places);
    assert_eq!(
        installer.lookup(Dependency::Tmux).unwrap(),
        Ok(crate::setup::Located {
            version: Version::parse("3.8").unwrap(),
            bundled: true,
            note: None,
        })
    );
    std::fs::remove_file(&sidecar).unwrap();
    assert!(!matches!(
        installer.lookup(Dependency::Tmux),
        Ok(Ok(crate::setup::Located { bundled: true, .. }))
    ));
}

/// Leader 20:08: a bundled tmux that cannot run, one that fails its version read or reports a
/// tmux older than the supported 3.2, is passed over for the system tmux, and setup says the
/// bundled one can't run; the terminal, and so its sandbox grant, picks that same system tmux.
#[test]
fn setup_and_the_terminal_use_the_system_tmux_when_the_bundled_one_cannot_run() {
    let root = private_root();
    let app = root.path().join("CrazyAgent.app/Contents/MacOS");
    let system = root.path().join("system/bin");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&system).unwrap();
    let tmux = system.join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\necho 'tmux 3.5a'\n").unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    let places = crate::tools::Places::new(vec![system], Some(root.path().join("home")))
        .with_app_folder(app.clone());
    let installer = PinnedInstaller::with_places(root.path(), Vec::new(), places.clone());
    for (number, broken) in ["#!/bin/sh\nexit 1\n", "#!/bin/sh\necho 'tmux 3.1'\n"]
        .into_iter()
        .enumerate()
    {
        let sidecar = app.join(format!("crazyagent-tmux.{number}"));
        std::fs::write(&sidecar, broken).unwrap();
        std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&sidecar, app.join("crazyagent-tmux")).unwrap();
        assert_eq!(
            installer.lookup(Dependency::Tmux).unwrap(),
            Ok(crate::setup::Located {
                version: Version::parse("3.5a").unwrap(),
                bundled: false,
                note: Some(crate::tools::BundleNote::CannotRun),
            }),
            "{broken}"
        );
        assert_eq!(
            crate::terminal::tmux::installed_at(&places),
            Some(tmux.canonicalize().unwrap()),
            "{broken}"
        );
    }
    let fixed = app.join("crazyagent-tmux.fixed");
    std::fs::write(&fixed, "#!/bin/sh\necho 'tmux 3.8'\n").unwrap();
    std::fs::set_permissions(&fixed, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&fixed, app.join("crazyagent-tmux")).unwrap();
    let sidecar = app.join("crazyagent-tmux");
    assert_eq!(
        installer.lookup(Dependency::Tmux).unwrap(),
        Ok(crate::setup::Located {
            version: Version::parse("3.8").unwrap(),
            bundled: true,
            note: None,
        }),
        "a failed read is never kept once the bundled file is replaced"
    );
    assert_eq!(
        crate::terminal::tmux::installed_at(&places),
        Some(sidecar.canonicalize().unwrap())
    );
}

/// Setup reads the version of the copy the tool resolver finds first, the one a launch runs,
/// and never passes over it to a later copy: a first copy that reports no version shows
/// Failed with the plain reason, while the good copy later in the places stays unused.
#[test]
fn a_broken_first_copy_shows_failed_and_a_later_good_copy_is_not_used() {
    let root = private_root();
    let first = root.path().join("first");
    let later = root.path().join("later");
    for (folder, text) in [
        (&first, "#!/bin/sh\nexit 1\n"),
        (&later, "#!/bin/sh\necho 'tmux 3.5a'\n"),
    ] {
        std::fs::create_dir_all(folder).unwrap();
        let tmux = folder.join("tmux");
        std::fs::write(&tmux, text).unwrap();
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let places = crate::tools::Places::new(vec![first, later], None);
    let installer = PinnedInstaller::with_places(root.path(), Vec::new(), places);
    let unreadable = InstallError("setup_version_unreadable");
    assert_eq!(installer.lookup(Dependency::Tmux), Err(unreadable));
    assert_eq!(installer.find(Dependency::Tmux), Err(unreadable));
    assert_eq!(installer.locate(Dependency::Tmux), None);
    let progress = crate::setup::progress::Progress {
        checked: Some(crate::setup::progress::Checked {
            at: std::time::Instant::now(),
            found: vec![(Dependency::Tmux, installer.lookup(Dependency::Tmux))],
            probe: crate::tools::probe::Mark {
                generation: 1,
                first_run_pending: false,
            },
        }),
        ..crate::setup::progress::Progress::default()
    };
    let rows = crate::setup::progress::dependency_rows(&progress);
    assert_eq!(
        rows[0].state,
        crate::setup::DependencyState::Failed {
            reason: crate::setup::plain_reason("setup_version_unreadable").to_owned()
        }
    );
}

/// SETUP-PROBE-GAP: setup's tmux row and the terminal read the same places, so a tmux only
/// in a folder the login-shell probe found, such as MacPorts' `/opt/local/bin`, is the file
/// setup shows and the one a terminal starts, never found by one and missing for the other.
#[test]
fn a_tmux_only_on_the_login_path_is_the_one_setup_shows_and_the_terminal_starts() {
    let root = private_root();
    let login = root.path().join("opt/local/bin");
    std::fs::create_dir_all(&login).unwrap();
    let tmux = login.join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\necho 'tmux 3.5a'\n").unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
    let before = crate::tools::Places::new(Vec::new(), None);
    assert_eq!(crate::terminal::tmux::installed_at(&before), None);
    let installer = PinnedInstaller::with_places(root.path(), Vec::new(), before.clone());
    assert_eq!(installer.locate(Dependency::Tmux), None);
    let places = before.with_probed(vec![login]);
    let installer = PinnedInstaller::with_places(root.path(), Vec::new(), places.clone());
    let (entry, version) = installer.locate(Dependency::Tmux).unwrap();
    assert_eq!(version, Version::parse("3.5a").unwrap());
    let started = crate::terminal::tmux::installed_at(&places).unwrap();
    assert_eq!(started, entry.canonicalize().unwrap());
    assert_eq!(started, tmux.canonicalize().unwrap());
}

fn found(name: &str) -> PathBuf {
    let mut folders: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        folders.push(PathBuf::from(home).join(".local/bin"));
    }
    folders.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    folders
        .into_iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| runnable(candidate))
        .unwrap_or_else(|| panic!("prerequisite missing: {name} on PATH"))
}

fn wheel(dir: &Path, python: &Path) -> (PathBuf, String) {
    let stage = dir.join("stage");
    let info = stage.join("demo_tool-1.0.dist-info");
    std::fs::create_dir_all(stage.join("demo_tool")).unwrap();
    std::fs::create_dir_all(&info).unwrap();
    let files = [
        (
            stage.join("demo_tool/__init__.py"),
            "def main():\n    print(\"demo-tool 1.0\")\n",
        ),
        (
            info.join("METADATA"),
            "Metadata-Version: 2.1\nName: demo-tool\nVersion: 1.0\n",
        ),
        (
            info.join("WHEEL"),
            "Wheel-Version: 1.0\nGenerator: crazyagent-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        ),
        (
            info.join("entry_points.txt"),
            "[console_scripts]\ndemo-tool = demo_tool:main\n",
        ),
        (
            info.join("RECORD"),
            "demo_tool/__init__.py,,\ndemo_tool-1.0.dist-info/METADATA,,\ndemo_tool-1.0.dist-info/WHEEL,,\ndemo_tool-1.0.dist-info/entry_points.txt,,\ndemo_tool-1.0.dist-info/RECORD,,\n",
        ),
    ];
    for (path, text) in files {
        std::fs::write(path, text).unwrap();
    }
    let wheel = dir.join("demo_tool-1.0-py3-none-any.whl");
    let status = crate::spawn_gate::status(
        Command::new(python)
            .args(["-I", "-m", "zipfile", "-c"])
            .arg(&wheel)
            .args(["demo_tool", "demo_tool-1.0.dist-info"])
            .current_dir(&stage),
    )
    .unwrap();
    assert!(status.success());
    let sha256 = file_sha256(&wheel).unwrap();
    (wheel, sha256)
}

fn hashed(wheel: &Path, sha256: &str) -> String {
    format!(
        "demo-tool @ file://{} \\\n    --hash=sha256:{sha256}\n",
        wheel.display()
    )
}

fn environments(home: &Path) -> usize {
    std::fs::read_dir(home).map_or(0, |entries| entries.count())
}

#[test]
fn a_locked_tool_installs_only_files_whose_sha256_its_lock_lists() {
    let uv = found("uv");
    let python = found("python3");
    let root = private_root();
    let work = tempfile::tempdir().unwrap();
    let (wheel, sha256) = wheel(work.path(), &python);
    let mut folders = vec![uv.parent().unwrap().to_path_buf()];
    folders.push(python.parent().unwrap().to_path_buf());
    let installer = PinnedInstaller::with_paths(root.path(), folders);
    let good = hashed(&wheel, &sha256);
    let tampered = hashed(&wheel, &"0".repeat(64));
    assert_ne!(good, tampered);
    let unhashed = format!("demo-tool @ file://{}\n", wheel.display());
    let lock = |requirements| ToolLock {
        package: "demo-tool",
        version: "1.0",
        programs: &["demo-tool"],
        requirements,
    };
    let program = root.path().join("setup/bin/demo-tool");
    let home = root.path().join("setup/uv/tools/demo-tool");

    for refused in [tampered.as_str(), unhashed.as_str()] {
        assert_eq!(
            installer.install_locked(&lock(refused), "3"),
            Err(InstallError("setup_install_failed"))
        );
        assert!(std::fs::symlink_metadata(&program).is_err());
        assert_eq!(environments(&home), 0);
    }

    assert_eq!(installer.install_locked(&lock(&good), "3"), Ok(()));
    assert_eq!(installer.program("demo-tool"), Some(program.clone()));
    let report = |installer: &PinnedInstaller| {
        let (ok, output) = run(
            &program,
            &[],
            &installer.env(),
            root.path(),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(ok);
        String::from_utf8(output).unwrap()
    };
    assert_eq!(report(&installer), "demo-tool 1.0\n");
    let linked = std::fs::read_link(&program).unwrap();
    assert_eq!(environments(&home), 1);

    assert_eq!(installer.install_locked(&lock(&good), "3"), Ok(()));
    assert_eq!(std::fs::read_link(&program).unwrap(), linked);

    assert_eq!(
        installer.install_locked(&lock(&tampered), "3"),
        Err(InstallError("setup_install_failed"))
    );
    assert_eq!(std::fs::read_link(&program).unwrap(), linked);
    assert_eq!(report(&installer), "demo-tool 1.0\n");
    assert_eq!(environments(&home), 1);
}

#[test]
fn a_tool_lock_with_an_unsafe_name_or_python_is_refused_before_uv_runs() {
    let root = private_root();
    let installer = PinnedInstaller::with_paths(root.path(), vec![]);
    let lock = |package, programs| ToolLock {
        package,
        version: "1.0",
        programs,
        requirements: "demo-tool==1.0 \\\n    --hash=sha256:00\n",
    };
    for (package, programs, python) in [
        ("demo-tool", &["demo-tool"][..], "3 --index-url x"),
        ("demo-tool", &["../demo-tool"][..], "3"),
        ("-e", &["demo-tool"][..], "3"),
        ("demo-tool", &[][..], "3"),
    ] {
        assert_eq!(
            installer.install_locked(&lock(package, programs), python),
            Err(InstallError("setup_pin_invalid"))
        );
    }
    assert_eq!(
        installer.install_locked(&lock("demo-tool", &["demo-tool"]), "3"),
        Err(InstallError("setup_uv_missing"))
    );
    assert!(!root.path().join("setup/uv/tools").exists());
}

#[test]
fn a_tool_the_previous_build_installed_with_uv_tool_install_updates_to_its_lock() {
    let uv = found("uv");
    let python = found("python3");
    let root = private_root();
    let work = tempfile::tempdir().unwrap();
    let (wheel, sha256) = wheel(work.path(), &python);
    let mut folders = vec![uv.parent().unwrap().to_path_buf()];
    folders.push(python.parent().unwrap().to_path_buf());
    let installer = PinnedInstaller::with_paths(root.path(), folders);
    let setup = root.path().join("setup");
    let bin = setup.join("bin");
    let tools = setup.join("uv/tools");
    for folder in [
        bin.clone(),
        tools.clone(),
        setup.join("uv/cache"),
        setup.join("uv/python"),
        setup.join("uv/python-bin"),
    ] {
        private_folder(&folder).unwrap();
    }
    let mut env = installer.env();
    env.extend([
        ("UV_TOOL_DIR", tools.clone().into_os_string()),
        ("UV_TOOL_BIN_DIR", bin.clone().into_os_string()),
        ("UV_CACHE_DIR", setup.join("uv/cache").into_os_string()),
        (
            "UV_PYTHON_INSTALL_DIR",
            setup.join("uv/python").into_os_string(),
        ),
        (
            "UV_PYTHON_BIN_DIR",
            setup.join("uv/python-bin").into_os_string(),
        ),
        ("UV_NO_CONFIG", "1".into()),
        ("UV_NO_PROGRESS", "1".into()),
    ]);
    let (ok, _) = run(
        &uv,
        &[
            "tool".into(),
            "install".into(),
            "--python".into(),
            "3".into(),
            wheel.clone().into_os_string(),
        ],
        &env,
        &setup,
        UV_TOOL_TIME,
    )
    .unwrap();
    assert!(ok);
    let home = tools.join("demo-tool");
    let program = bin.join("demo-tool");
    assert_ne!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o077,
        0
    );
    assert!(home.join("uv-receipt.toml").is_file());
    let old = std::fs::read_link(&program).unwrap();
    assert!(old.starts_with(&home));
    let report = |installer: &PinnedInstaller| {
        let (ok, output) = run(
            &program,
            &[],
            &installer.env(),
            root.path(),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(ok);
        String::from_utf8(output).unwrap()
    };
    assert_eq!(report(&installer), "demo-tool 1.0\n");
    let good = hashed(&wheel, &sha256);
    let tampered = hashed(&wheel, &"0".repeat(64));
    let lock = |requirements| ToolLock {
        package: "demo-tool",
        version: "1.0",
        programs: &["demo-tool"],
        requirements,
    };

    assert_eq!(
        installer.install_locked(&lock(&tampered), "3"),
        Err(InstallError("setup_install_failed"))
    );
    assert_eq!(std::fs::read_link(&program).unwrap(), old);
    assert_eq!(report(&installer), "demo-tool 1.0\n");

    assert_eq!(installer.install_locked(&lock(&good), "3"), Ok(()));
    let linked = std::fs::read_link(&program).unwrap();
    assert_ne!(linked, old);
    assert!(linked.starts_with(&home));
    assert_eq!(report(&installer), "demo-tool 1.0\n");
    assert_eq!(environments(&home), 1);
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

/// Setup leaves no group or other write on the Python and the tool environment it
/// installed (DEC-162 R2), as uv writes them 0775 under a umask of 002: a stand-in uv,
/// never a real one, writes both that way, and after the locked install every folder and
/// file of both is writable only by this user, so the resolver runs the tool.
#[test]
fn a_locked_tool_install_leaves_its_python_and_environment_writable_only_by_this_user() {
    let root = private_root();
    let work = private_root();
    let fake = work.path().join("bin");
    std::fs::create_dir(&fake).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let uv = fake.join("uv");
    std::fs::write(
        &uv,
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --version) echo 'uv 0.13.5' ;;\n\
         venv)\n\
           py=\"$UV_PYTHON_INSTALL_DIR/cpython-9.9.9-dummy\"\n\
           /bin/mkdir -p \"$py/bin\" \"$6/bin\"\n\
           printf '#!/bin/sh\\nexit 0\\n' > \"$py/bin/python3\"\n\
           /bin/chmod 775 \"$py\" \"$py/bin\" \"$py/bin/python3\" \"$6\" \"$6/bin\"\n\
           /bin/ln -s \"$py/bin/python3\" \"$6/bin/python\" ;;\n\
         pip)\n\
           bin=\"${5%/python}\"\n\
           printf '#!/bin/sh\\necho demo-tool 1.0\\n' > \"$bin/demo-tool\"\n\
           /bin/chmod 775 \"$bin/demo-tool\" ;;\n\
         *) exit 1 ;;\n\
         esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
    let installer = PinnedInstaller::with_paths(root.path(), vec![fake.clone()]);
    let lock = ToolLock {
        package: "demo-tool",
        version: "1.0",
        programs: &["demo-tool"],
        requirements: "demo-tool==1.0 --hash=sha256:0000\n",
    };

    assert_eq!(installer.install_locked(&lock, "3"), Ok(()));

    let mode = |path: &Path| {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    };
    let python = root.path().join("setup/uv/python/cpython-9.9.9-dummy");
    for path in [
        python.clone(),
        python.join("bin"),
        python.join("bin/python3"),
    ] {
        assert_eq!(mode(&path), 0o755, "{}", path.display());
    }
    let program = root.path().join("setup/bin/demo-tool");
    let entry = std::fs::read_link(&program).unwrap();
    let venv = entry.parent().unwrap().parent().unwrap();
    for path in [venv.to_path_buf(), venv.join("bin"), entry.clone()] {
        assert_eq!(mode(&path), 0o755, "{}", path.display());
    }
    assert_eq!(installer.program("demo-tool"), Some(program));
}

/// DEC-162: the permission fix acts only on what the installer resolves itself, in a
/// fixture HOME with a uv-like graphify whose Python is group writable: the refused lookup
/// names the Python's prefix as the fix target, a fix confirmed for another folder changes
/// nothing (`tool_fix_changed`), the confirmed one removes go-w from that tree only, the
/// tool then resolves, and a second fix is not needed.
#[test]
fn the_permission_fix_acts_only_on_the_target_the_installer_resolved_and_confirmed() {
    let scratch = private_root();
    let home = scratch.path().join("home");
    let prefix = home.join(".local/share/uv/python/cpython-9.9.9-dummy");
    let python = prefix.join("bin/python3");
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::write(&python, "#!/bin/sh\nexit 0\n").unwrap();
    for path in [&python, &prefix.join("bin"), &prefix] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o775)).unwrap();
    }
    let venv = home.join(".local/share/uv/tools/graphifyy/bin");
    std::fs::create_dir_all(&venv).unwrap();
    std::os::unix::fs::symlink(&python, venv.join("python3")).unwrap();
    std::fs::write(
        venv.join("graphify"),
        format!("#!{}\nprint('graphify')\n", venv.join("python3").display()),
    )
    .unwrap();
    std::fs::set_permissions(
        venv.join("graphify"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let local_bin = home.join(".local/bin");
    std::fs::create_dir_all(&local_bin).unwrap();
    std::os::unix::fs::symlink(venv.join("graphify"), local_bin.join("graphify")).unwrap();
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o777)).unwrap();
    let root = scratch.path().join("engine");
    let places = crate::tools::Places::new(vec![local_bin.clone()], Some(home.clone()));
    let installer = PinnedInstaller::with_places(&root, vec![local_bin], places);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;

    let Ok(Err(missing)) = installer.lookup(Dependency::Graphify) else {
        panic!("graphify should be found and refused");
    };
    assert_eq!(missing.reason, crate::tools::Reason::Refused);
    assert_eq!(installer.lookup_program("graphify"), Err(missing.clone()));
    assert_eq!(installer.program("graphify"), None);
    let target = installer.fix_target(&missing).unwrap();
    assert!(target.recursive);
    assert_eq!(target.path, prefix.canonicalize().unwrap());

    assert_eq!(
        installer.fix_permissions(Dependency::Graphify, &elsewhere.display().to_string(), true),
        Err(crate::tools::permission_fix::TOOL_FIX_CHANGED)
    );
    assert_eq!(mode(&python), 0o775);
    assert_eq!(mode(&elsewhere), 0o777);

    let confirmed = target.path.display().to_string();
    assert_eq!(
        installer.fix_permissions(Dependency::Graphify, &confirmed, false),
        Err(crate::tools::permission_fix::TOOL_FIX_CHANGED),
        "a confirmed single folder never runs as the whole tree"
    );
    for path in [&python, &prefix.join("bin"), &prefix] {
        assert_eq!(mode(path), 0o775, "{}", path.display());
    }
    let report = installer
        .fix_permissions(Dependency::Graphify, &confirmed, true)
        .unwrap();
    assert_eq!(report.changed, 3);
    for path in [&python, &prefix.join("bin"), &prefix] {
        assert_eq!(mode(path), 0o755, "{}", path.display());
    }
    assert_eq!(mode(&elsewhere), 0o777);
    assert!(crate::tools::resolve("graphify", &installer.places).is_ok());
    assert_eq!(
        installer.lookup_program("graphify").ok(),
        installer.program("graphify")
    );
    assert!(installer.program("graphify").is_some());
    assert_eq!(
        installer.fix_permissions(Dependency::Graphify, &confirmed, true),
        Err(crate::tools::permission_fix::TOOL_FIX_NOT_NEEDED)
    );
}
