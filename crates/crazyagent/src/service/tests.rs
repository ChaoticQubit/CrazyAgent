use super::*;
use kernel_processes::service::test_support::*;
use kernel_processes::service::*;
use std::ffi::OsString;

#[test]
fn the_current_user_service_follows_the_data_root() {
    let platform = Platform::CURRENT;
    let environment = crate::paths::RootEnvironment {
        home_override: None,
        ..crate::paths::RootEnvironment::current()
    };
    let executable = PathBuf::from("/opt/crazyagent/crazyagent");
    let default = Layout::new(crate::paths::data_root(platform, &environment).unwrap()).unwrap();
    let service = current_user(platform, executable.clone(), &default).unwrap();
    assert_eq!(service.name, standard(platform));
    let temp = tempfile::tempdir().unwrap();
    let other = current_user(
        platform,
        executable,
        &Layout::new(temp.path().join("engine")).unwrap(),
    )
    .unwrap();
    short_hash(&other.name, platform);
    if platform == Platform::MacOs {
        assert_eq!(
            other.definition.file_name().unwrap(),
            OsStr::new(&format!("{}.plist", other.name))
        );
        assert_eq!(
            texts(vec![other.status_command(501)]),
            vec![words(&[
                "/bin/launchctl",
                "print",
                &format!("gui/501/{}", other.name)
            ])]
        );
    }
}

#[test]
fn a_linux_or_windows_service_that_is_not_installed_reads_stopped_and_stops_as_a_no_op() {
    #[cfg(target_os = "macos")]
    for tool in [SYSTEMCTL, SCHTASKS] {
        assert_eq!(
            crate::paths::find_in_path(tool, std::env::var_os("PATH").as_deref()),
            None,
            "{tool} exists here, so its missing-tool path is not what this host runs"
        );
    }
    let temp = tempfile::tempdir().unwrap();
    for platform in [Platform::Linux, Platform::Windows] {
        let mut absent = service(platform);
        absent.name = format!("crazyagent-test-absent-{}", std::process::id());
        absent.definition = temp.path().join("not-installed");
        assert_eq!(
            absent.status().unwrap(),
            Status {
                installed: false,
                running: false,
                pid: None,
            },
            "{platform:?}"
        );
        absent.stop().unwrap();
        assert!(!absent.definition.exists());
    }
}

#[cfg(unix)]
fn bundle(folder: &Path) -> PathBuf {
    for name in [
        "crazyagent",
        "crazyagent-rcli",
        "crazyagent-runner",
        "crazyagent-tmux",
        "crazyagent-landlock",
    ] {
        write_mode(&folder.join(name), &format!("{name} bytes"), 0o755);
    }
    write_mode(&folder.join("notes.txt"), "not an engine file", 0o644);
    folder.join("crazyagent")
}

#[cfg(unix)]
fn refusal(executable: &Path, entry: &Path) -> String {
    format!(
        "{}: other users can change {}, so neither a login service nor the command line \
         link may run the engine from there; install CrazyAgent in a folder only you and \
         the system can change",
        executable.display(),
        entry.display()
    )
}

#[cfg(unix)]
#[test]
fn install_refuses_an_engine_other_users_can_change_before_writing_anything() {
    let temp = durable_folder();
    let shared = shared_folder(&temp.path().join(".mount_Ab12Cd"), 0o1777);
    let planted = shared.join("usr").join("bin").join("crazyagent");
    write_mode(&planted, "engine", 0o755);
    let mut linux = service(Platform::Linux);
    linux.executable = planted.clone();
    linux.root = temp.path().join("data").join("engine");
    linux.definition = temp.path().join("units").join(SYSTEMD_UNIT);
    let error = linux.install(&HOOKS).unwrap_err();
    assert_eq!(error.0, refusal(&planted, &shared));
    assert!(!linux.definition.exists());
    assert!(!linux.root.exists());
    assert!(!temp.path().join("units").exists());
}

#[cfg(unix)]
#[test]
fn persistable_runs_an_engine_outside_the_mount_in_place_and_copies_one_from_the_appimage_mount() {
    let temp = durable_folder();
    let mount = temp.path().join(".mount_Ab12Cd");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    assert_eq!(
        persistable(&bundled, layout.root(), None, &HOOKS).unwrap(),
        bundled
    );
    let elsewhere = temp.path().join("elsewhere");
    assert_eq!(
        persistable(&bundled, layout.root(), Some(&elsewhere), &HOOKS).unwrap(),
        bundled
    );
    assert!(!layout.root().exists());
    let copies = layout.root().join("setup").join("bin");
    let installed = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    assert_eq!(installed, copies.join("crazyagent"));
    assert_eq!(changeable_by_others(&installed), None);
    assert_eq!(mode_of(&copies), 0o700);
    let listed = || {
        let mut names: Vec<String> = std::fs::read_dir(&copies)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    let names = listed();
    assert_eq!(
        names,
        [
            "crazyagent",
            "crazyagent-landlock",
            "crazyagent-rcli",
            "crazyagent-runner",
            "crazyagent-tmux"
        ],
        "the bundled tmux is copied beside the engine, which finds it there (DEC-157)"
    );
    for name in &names {
        assert_eq!(
            std::fs::read_to_string(copies.join(name)).unwrap(),
            format!("{name} bytes")
        );
        assert_eq!(mode_of(&copies.join(name)), 0o700);
    }
    let mut linux = service(Platform::Linux);
    linux.executable = installed.clone();
    linux.root = layout.root().to_path_buf();
    let unit = linux.definition_text().unwrap();
    assert!(
        unit.contains(&format!("ExecStart=\"{}\" engine", installed.display())),
        "{unit}"
    );
    assert!(!unit.contains(".mount_"), "{unit}");

    write_mode(&copies.join("crazyagent-rcli"), "planted", 0o700);
    write_mode(
        &copies.join("crazyagent-runner"),
        "crazyagent-runner bytes",
        0o755,
    );
    write_mode(
        &mount.join("usr").join("bin").join("crazyagent"),
        "crazyagent newer bytes",
        0o755,
    );
    assert_eq!(
        persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap(),
        installed
    );
    assert_eq!(
        std::fs::read_to_string(copies.join("crazyagent-rcli")).unwrap(),
        "crazyagent-rcli bytes"
    );
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        "crazyagent newer bytes"
    );
    assert_eq!(mode_of(&copies.join("crazyagent-runner")), 0o700);
    assert_eq!(listed(), names);
    let stranger = mount.join("usr").join("bin").join("notes.txt");
    assert!(persistable(&stranger, layout.root(), Some(&mount), &HOOKS).is_err());
}

/// Leader 20:15: the AppImage's persisted copy lives in `<data root>/setup/bin`, which the
/// tool resolver trusts, so the engine run from there finds the `crazyagent-tmux` beside it
/// and never refuses it as a file agents can write, as it did in `<data root>/bin`.
#[cfg(unix)]
#[test]
fn an_appimage_persisted_tmux_is_found_beside_the_engine_not_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mount = root.join(".mount_Ab12Cd");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(root.join("data").join("engine")).unwrap();
    let installed = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    let app = installed.parent().unwrap();
    assert_eq!(app, layout.root().join("setup").join("bin"));
    let places = crate::tools::tools_engine(Some(layout.root()), Some(app), None, None);
    let tmux = kernel_processes::finding::resolve("tmux", &places).unwrap();
    assert_eq!(tmux.entry, app.join("crazyagent-tmux"));
}

/// The copy a 2.4.0 AppImage kept in `<data root>/bin`, a private folder (700) holding the
/// release files and a stranger file. Input: the layout. Output: the old engine executable.
#[cfg(unix)]
fn old_copy(layout: &Layout) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let old = layout.root().join("bin");
    let engine = bundle(&old);
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o700)).unwrap();
    engine
}

/// rv-code F1: retiring removes files only from a `<data root>/bin` that is a real folder,
/// not a link, owned by this user and private (700), as 2.4.0 made it; through a link, or
/// in a folder others can read, a release-named file stays, wherever it sits.
#[cfg(unix)]
#[test]
fn retiring_never_follows_a_linked_or_shared_old_folder() {
    use std::os::unix::fs::PermissionsExt;
    let temp = durable_folder();
    let mount = temp.path().join(".mount_Ab12Cd");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    layout.prepare().unwrap();
    let old = layout.root().join("bin");
    let elsewhere = bundle(&temp.path().join("elsewhere"));
    std::os::unix::fs::symlink(elsewhere.parent().unwrap(), &old).unwrap();
    let installed = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    retire_old_copies_with(layout.root(), &installed, None, &HOOKS);
    assert!(elsewhere.is_file(), "a file outside the data root stays");
    assert!(
        std::fs::symlink_metadata(&old)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::remove_file(&old).unwrap();
    let shared = bundle(&old);
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    retire_old_copies_with(layout.root(), &installed, None, &HOOKS);
    assert!(
        shared.is_file(),
        "a folder that is not private is left alone"
    );
}

/// Leader 20:15: the first start after the move retires the old `<data root>/bin` copy: the
/// command line link that named it names the new copy, every release file there is removed,
/// a file CrazyAgent did not put there stays, and the folder goes once it is empty.
#[cfg(unix)]
#[test]
fn a_first_start_retires_the_old_appimage_copy_and_repoints_the_command_line_link() {
    let temp = durable_folder();
    let mount = temp.path().join(".mount_Ab12Cd");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    layout.prepare().unwrap();
    let old = layout.root().join("bin");
    old_copy(&layout);
    let cli = temp.path().join("home").join(".local").join("bin");
    std::fs::create_dir_all(&cli).unwrap();
    std::os::unix::fs::symlink(old.join("crazyagent"), cli.join("crazyagent")).unwrap();
    let installed = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    assert_eq!(
        installed,
        layout.root().join("setup").join("bin").join("crazyagent")
    );
    retire_old_copies_with(layout.root(), &installed, Some(&cli), &HOOKS);
    assert_eq!(
        std::fs::read_link(cli.join("crazyagent")).unwrap(),
        installed
    );
    for name in kernel_processes::update::BINARIES {
        assert!(
            std::fs::symlink_metadata(old.join(name)).is_err(),
            "{name} is retired"
        );
    }
    assert!(old.join("notes.txt").is_file(), "a stranger file stays");
    std::fs::remove_file(old.join("notes.txt")).unwrap();
    retire_old_copies_with(layout.root(), &installed, Some(&cli), &HOOKS);
    assert!(!old.exists(), "the empty old folder goes");
}

/// `service link-cli` keeps the engine too, but a login service a 2.4.0 AppImage installed
/// names the old `<data root>/bin` copy until a replacing `service start` rewrites it, so
/// keeping the engine never removes that copy; retiring it is the start's step after the
/// service names the new copy, and an engine outside `<data root>/setup/bin` retires nothing.
#[cfg(unix)]
#[test]
fn keeping_the_engine_leaves_the_old_copy_a_login_service_may_still_name() {
    let temp = durable_folder();
    let mount = temp.path().join(".mount_Ab12Cd");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    layout.prepare().unwrap();
    let old = old_copy(&layout);
    let installed = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    assert_ne!(installed, old);
    assert!(
        old.is_file(),
        "a service naming the old copy would not start"
    );
    retire_old_copies_with(layout.root(), &old, None, &HOOKS);
    assert!(old.is_file(), "an engine outside setup/bin retires nothing");
    retire_old_copies_with(layout.root(), &installed, None, &HOOKS);
    assert!(!old.exists());
}

/// A command line link that names anything but the old copy is never touched.
#[cfg(unix)]
#[test]
fn retiring_the_old_appimage_copy_leaves_a_link_to_another_engine_alone() {
    let temp = durable_folder();
    let mount = temp.path().join(".mount_Ab12Cd");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    layout.prepare().unwrap();
    old_copy(&layout);
    let other = bundle(&temp.path().join("other"));
    let cli = temp.path().join("home").join(".local").join("bin");
    std::fs::create_dir_all(&cli).unwrap();
    std::os::unix::fs::symlink(&other, cli.join("crazyagent")).unwrap();
    let installed = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    retire_old_copies_with(layout.root(), &installed, Some(&cli), &HOOKS);
    assert_eq!(std::fs::read_link(cli.join("crazyagent")).unwrap(), other);
}

#[cfg(unix)]
#[test]
fn persistable_never_copies_from_a_folder_other_users_can_change_and_install_refuses_it() {
    let temp = durable_folder();
    let shared = shared_folder(&temp.path().join("shared"), 0o777);
    let bundled = bundle(&shared);
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    assert_eq!(
        persistable(&bundled, layout.root(), None, &HOOKS).unwrap(),
        bundled
    );
    assert!(!layout.root().exists());
    let mut linux = service(Platform::Linux);
    linux.executable = bundled.clone();
    linux.root = layout.root().to_path_buf();
    linux.definition = temp.path().join("units").join(SYSTEMD_UNIT);
    assert_eq!(
        linux.install(&HOOKS).unwrap_err().0,
        refusal(&bundled, &shared)
    );
    assert!(!layout.root().exists());
    assert!(!linux.definition.exists());
}

#[cfg(unix)]
#[test]
fn link_cli_refuses_an_engine_other_users_can_change_and_links_the_private_copy() {
    let temp = durable_folder();
    let shared = shared_folder(&temp.path().join(".mount_Ab12Cd"), 0o1777);
    let planted = shared.join("usr").join("bin").join("crazyagent");
    write_mode(&planted, "engine", 0o755);
    let bin = temp.path().join("home").join(".local").join("bin");
    assert_eq!(
        link_cli(&planted, &bin).unwrap_err().0,
        refusal(&planted, &shared)
    );
    assert!(!bin.exists());
    let mount = temp.path().join("private").join(".mount_Ef34Gh");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    let copy = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    let link = link_cli(&copy, &bin).unwrap();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        layout.root().join("setup").join("bin").join("crazyagent")
    );
}

/// rv-secreview UE-L1: `service start` replaces a definition that runs another program only
/// with an installed app's engine outside any cargo target folder, and never under a newer
/// engine: the last one that ran at the data root or the app bundle the installed plist runs.
#[cfg(unix)]
#[test]
fn only_an_installed_app_engine_that_is_not_older_may_replace_another_program() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = temp
        .path()
        .join("Applications")
        .join("CrazyAgent.app")
        .join("Contents")
        .join("MacOS")
        .join("crazyagent");
    std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
    std::fs::write(&bundled, b"engine").unwrap();
    let mut own = service(Platform::MacOs);
    own.root = temp.path().join("engine");
    std::fs::create_dir_all(&own.root).unwrap();
    own.executable = bundled.clone();
    own.definition = temp.path().join("dev.crazyagent.engine.plist");
    assert!(own.may_replace(&HOOKS));
    let mut plain = own.clone();
    plain.executable = temp.path().join("bin").join("crazyagent");
    assert!(!plain.may_replace(&HOOKS));
    let target = temp.path().join("worktree").join("target");
    let built = target
        .join("release")
        .join("bundle")
        .join("macos")
        .join("CrazyAgent.app")
        .join("Contents")
        .join("MacOS")
        .join("crazyagent");
    std::fs::create_dir_all(built.parent().unwrap()).unwrap();
    std::fs::write(&built, b"engine").unwrap();
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    let mut dev = own.clone();
    dev.executable = built;
    assert!(!dev.may_replace(&HOOKS));
    let old = temp.path().join("Old.app");
    std::fs::create_dir_all(old.join("Contents").join("MacOS")).unwrap();
    let mut installed = own.clone();
    installed.executable = old.join("Contents").join("MacOS").join("crazyagent");
    write_definition(
        &own.definition,
        &installed.definition_text().unwrap(),
        Platform::MacOs,
    )
    .unwrap();
    let info = |version: &str| {
        std::fs::write(
            old.join("Contents").join("Info.plist"),
            format!("<plist version=\"1.0\">\n<dict>\n\t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n</dict>\n</plist>\n"),
        )
        .unwrap();
    };
    info("99.0.0");
    assert!(!own.may_replace(&HOOKS));
    info("0.0.1");
    assert!(own.may_replace(&HOOKS));
    std::fs::write(
        own.root.join("owner-current"),
        br#"{"engine_version":"99.0.0"}"#,
    )
    .unwrap();
    assert!(!own.may_replace(&HOOKS));
    std::fs::write(
        own.root.join("owner-current"),
        br#"{"engine_version":"0.0.1"}"#,
    )
    .unwrap();
    assert!(own.may_replace(&HOOKS));
}

/// A dev build's app engine inside a cargo target folder, which [`Service::may_replace`]
/// refuses. Input: the row's folder. Output: the engine's path.
#[cfg(unix)]
fn dev_engine(root: &Path) -> PathBuf {
    let target = root.join("worktree").join("target");
    let built = app_engine(&target.join("release").join("bundle"), "macos");
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    built
}

/// RV-DS241-118: a login service left naming a program that is gone, as after the app was
/// moved or renamed, is reinstalled for this app's engine when no engine answers and never
/// started as it is, also by a dev build, which only keeps away from a program that still
/// exists. A definition naming this engine starts as it is.
#[cfg(unix)]
#[test]
fn a_definition_naming_a_missing_program_is_reinstalled_not_started() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = app_engine(temp.path(), "Applications");
    let built = dev_engine(temp.path());
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut own = service(platform);
        own.root = temp.path().join("engine");
        own.executable = bundled.clone();
        own.definition = temp.path().join(format!("{platform:?}.definition"));
        let mut gone = own.clone();
        gone.executable = temp
            .path()
            .join("Moved")
            .join("CrazyAgent.app")
            .join("Contents")
            .join("MacOS")
            .join("crazyagent");
        write_definition(&own.definition, &gone.definition_text().unwrap(), platform).unwrap();
        assert!(own.runs_another_program());
        assert_eq!(own.broken_program(), Some(gone.executable.clone()));
        assert_eq!(own.repair(&bundled, true, false, &HOOKS), Repair::Install);
        let mut dev = own.clone();
        dev.executable = built.clone();
        assert!(!dev.may_replace(&HOOKS));
        assert_eq!(dev.repair(&built, true, false, &HOOKS), Repair::Install);
        write_definition(&own.definition, &own.definition_text().unwrap(), platform).unwrap();
        assert_eq!(own.broken_program(), None);
        assert_eq!(own.repair(&bundled, true, false, &HOOKS), Repair::Start);
        assert_eq!(own.repair(&bundled, true, true, &HOOKS), Repair::Install);
        assert_eq!(own.repair(&bundled, false, false, &HOOKS), Repair::Install);
    }
}

/// ENGINE-START-PRE-READY: an installed launchd definition without the `ProcessType`
/// Interactive key, as every install before 2.4.2 wrote, is reinstalled when no engine
/// answers or after an update stops it, since launchd throttles such an engine's CPU and
/// I/O. The same definition with the key starts as it is.
#[cfg(unix)]
#[test]
fn an_installed_launchd_definition_without_process_type_is_reinstalled_not_started() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = app_engine(temp.path(), "Applications");
    let mut own = service(Platform::MacOs);
    own.root = temp.path().join("engine");
    own.executable = bundled.clone();
    own.definition = temp.path().join("dev.crazyagent.engine.plist");
    let key = "\t<key>ProcessType</key>\n\t<string>Interactive</string>\n";
    let text = own.definition_text().unwrap();
    let before = text.replace(key, "");
    assert!(!before.contains("ProcessType"));
    write_definition(&own.definition, &before, Platform::MacOs).unwrap();
    assert_eq!(own.broken_program(), None);
    assert!(!own.runs_another_program());
    assert_eq!(
        own.repair(&bundled, true, false, &HOOKS),
        Repair::Install,
        "an installed plist without ProcessType Interactive must be reinstalled"
    );
    write_definition(&own.definition, &text, Platform::MacOs).unwrap();
    assert_eq!(
        own.repair(&bundled, true, false, &HOOKS),
        Repair::Start,
        "an installed plist with ProcessType Interactive starts as it is"
    );
}

/// RV-DS241-118: a definition naming an engine on a disk image or another disk (`/Volumes`)
/// or in a Gatekeeper AppTranslocation folder is never started as it is, since the path goes
/// away and another disk of the same name could take its place. An engine there is itself
/// never installed as a login service: it starts a usable installed definition and is
/// refused otherwise.
#[cfg(unix)]
#[test]
fn a_definition_naming_a_volumes_or_translocated_program_is_never_started_as_is() {
    let mounted = Path::new("/Volumes/CrazyAgent/CrazyAgent.app/Contents/MacOS/crazyagent");
    let translocated = Path::new(
        "/private/var/folders/ab/xyz/T/AppTranslocation/0A1B2C3D/d/CrazyAgent.app/Contents/MacOS/crazyagent",
    );
    assert!(transient(mounted));
    assert!(transient(translocated));
    assert!(!transient(Path::new(
        "/Applications/CrazyAgent.app/Contents/MacOS/crazyagent"
    )));
    assert!(!transient(Path::new(
        "/Users/owner/Volumes/CrazyAgent.app/Contents/MacOS/crazyagent"
    )));
    assert!(!transient(Path::new(
        "/VolumesX/CrazyAgent.app/Contents/MacOS/crazyagent"
    )));
    let temp = tempfile::tempdir().unwrap();
    let bundled = app_engine(temp.path(), "Applications");
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut own = service(platform);
        own.root = temp.path().join("engine");
        own.executable = bundled.clone();
        own.definition = temp.path().join(format!("{platform:?}.definition"));
        for program in [mounted, translocated] {
            let mut there = own.clone();
            there.executable = program.to_path_buf();
            write_definition(&own.definition, &there.definition_text().unwrap(), platform).unwrap();
            assert_eq!(own.broken_program().as_deref(), Some(program));
            assert_eq!(own.repair(&bundled, true, false, &HOOKS), Repair::Install);
            assert_eq!(there.broken_program().as_deref(), Some(program));
            assert_eq!(there.repair(program, true, false, &HOOKS), Repair::Refuse);
            assert_eq!(there.repair(program, true, true, &HOOKS), Repair::Refuse);
            assert_eq!(there.repair(program, false, false, &HOOKS), Repair::Refuse);
        }
        write_definition(&own.definition, &own.definition_text().unwrap(), platform).unwrap();
        let mut there = own.clone();
        there.executable = mounted.to_path_buf();
        assert_eq!(there.broken_program(), None);
        assert_eq!(there.repair(mounted, true, false, &HOOKS), Repair::Start);
        assert_eq!(there.repair(mounted, true, true, &HOOKS), Repair::Refuse);
    }
}

/// RV-DS241-129: Restart engine, a forced reinstall, never points the login service away
/// from another program that still exists when this engine may not replace it, as with a
/// newer engine known at the data root or a dev build: the definition is kept
/// (`engine_newer_than_app`). A broken definition, this engine's own, or one it may replace
/// is still reinstalled, and a plain start still starts the kept one as it is.
#[cfg(unix)]
#[test]
fn a_forced_reinstall_never_replaces_another_program_this_engine_may_not() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = app_engine(temp.path(), "Applications");
    let other = app_engine(temp.path(), "Newer");
    let built = dev_engine(temp.path());
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut own = service(platform);
        own.root = temp.path().join(format!("{platform:?}-engine"));
        std::fs::create_dir_all(&own.root).unwrap();
        own.executable = bundled.clone();
        own.definition = temp.path().join(format!("{platform:?}.definition"));
        let mut newer = own.clone();
        newer.executable = other.clone();
        write_definition(&own.definition, &newer.definition_text().unwrap(), platform).unwrap();
        assert!(own.may_replace(&HOOKS));
        assert_eq!(own.repair(&bundled, true, true, &HOOKS), Repair::Install);
        let mut dev = own.clone();
        dev.executable = built.clone();
        assert_eq!(dev.repair(&built, true, true, &HOOKS), Repair::Keep);
        assert_eq!(dev.repair(&built, true, false, &HOOKS), Repair::Start);
        std::fs::write(
            own.root.join("owner-current"),
            br#"{"engine_version":"99.0.0"}"#,
        )
        .unwrap();
        assert!(!own.may_replace(&HOOKS));
        assert_eq!(own.repair(&bundled, true, true, &HOOKS), Repair::Keep);
        assert_eq!(own.repair(&bundled, true, false, &HOOKS), Repair::Start);
        write_definition(&own.definition, &own.definition_text().unwrap(), platform).unwrap();
        assert_eq!(own.repair(&bundled, true, true, &HOOKS), Repair::Install);
        let mut gone = own.clone();
        gone.executable = temp.path().join("Gone").join("crazyagent");
        write_definition(&own.definition, &gone.definition_text().unwrap(), platform).unwrap();
        assert_eq!(own.repair(&bundled, true, true, &HOOKS), Repair::Install);
        assert_eq!(dev.repair(&built, true, true, &HOOKS), Repair::Install);
    }
}

/// DS-118 r2 F1 (rv-secreview): an AppImage app's login service runs the private copy in
/// `<root>/setup/bin`, so whether this app may replace another program is judged on the
/// app's own engine in its `.mount_` folder, not on the copy, which is in no app bundle.
/// Restart engine and a plain start then replace an older program, and a newer one known
/// at the data root is kept on Restart engine and started as it is on a plain start. The
/// copy is one path every AppImage runs, so a definition naming it, with this PATH or
/// another, is also kept and started as it is while a newer engine is known there, and is
/// refreshed only once none is (DS-118 r3 F1).
#[cfg(unix)]
#[test]
fn an_appimage_copy_is_judged_by_the_app_engine_it_was_copied_from() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = temp
        .path()
        .join(".mount_CrazyAgentX")
        .join("usr")
        .join("bin")
        .join("crazyagent");
    std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
    std::fs::write(&bundled, b"engine").unwrap();
    let mut copy = service(Platform::Linux);
    copy.root = temp.path().join("engine");
    copy.executable = copy.root.join("setup").join("bin").join("crazyagent");
    std::fs::create_dir_all(copy.executable.parent().unwrap()).unwrap();
    std::fs::write(&copy.executable, b"engine").unwrap();
    copy.definition = temp.path().join("crazyagent.service");
    let old = copy.root.join("bin").join("crazyagent");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"older engine").unwrap();
    let mut older = copy.clone();
    older.executable = old;
    write_definition(
        &copy.definition,
        &older.definition_text().unwrap(),
        Platform::Linux,
    )
    .unwrap();
    assert!(copy.runs_another_program());
    assert_eq!(copy.broken_program(), None);
    assert!(!copy.may_replace(&HOOKS), "the copy is in no app bundle");
    assert_eq!(copy.repair(&bundled, true, true, &HOOKS), Repair::Install);
    assert_eq!(copy.repair(&bundled, true, false, &HOOKS), Repair::Install);
    std::fs::write(
        copy.root.join("owner-current"),
        br#"{"engine_version":"99.0.0"}"#,
    )
    .unwrap();
    assert_eq!(copy.repair(&bundled, true, true, &HOOKS), Repair::Keep);
    assert_eq!(copy.repair(&bundled, true, false, &HOOKS), Repair::Start);
    let mut elsewhere = copy.clone();
    elsewhere.path = OsString::from("/usr/bin:/bin");
    write_definition(
        &copy.definition,
        &elsewhere.definition_text().unwrap(),
        Platform::Linux,
    )
    .unwrap();
    assert!(!copy.runs_another_program());
    assert_eq!(copy.repair(&bundled, true, false, &HOOKS), Repair::Start);
    assert_eq!(copy.repair(&bundled, true, true, &HOOKS), Repair::Keep);
    write_definition(
        &copy.definition,
        &copy.definition_text().unwrap(),
        Platform::Linux,
    )
    .unwrap();
    assert!(copy.current());
    assert_eq!(copy.repair(&bundled, true, false, &HOOKS), Repair::Start);
    assert_eq!(copy.repair(&bundled, true, true, &HOOKS), Repair::Keep);
    std::fs::remove_file(copy.root.join("owner-current")).unwrap();
    assert_eq!(copy.repair(&bundled, true, false, &HOOKS), Repair::Start);
    assert_eq!(copy.repair(&bundled, true, true, &HOOKS), Repair::Install);
    write_definition(
        &copy.definition,
        &elsewhere.definition_text().unwrap(),
        Platform::Linux,
    )
    .unwrap();
    assert_eq!(copy.repair(&bundled, true, false, &HOOKS), Repair::Install);
    assert_eq!(copy.repair(&bundled, true, true, &HOOKS), Repair::Install);
    std::fs::write(copy.root.join("setup").join("bin-version"), "99.0.0\n").unwrap();
    assert_eq!(copy.repair(&bundled, true, false, &HOOKS), Repair::Start);
    assert_eq!(copy.repair(&bundled, true, true, &HOOKS), Repair::Keep);
    assert_eq!(copy.repair(&bundled, false, false, &HOOKS), Repair::Install);
}

/// DS-118 r3 F1 (rv-code): an older AppImage never writes its bytes over the private copy
/// in `<root>/setup/bin` while the data root knows a newer engine, since every AppImage runs
/// that one path: the copy and the files beside it keep the newer bytes. A newer engine is
/// known from its locator while it runs, or from the version the app that last wrote the
/// copy recorded beside it, which outlives a clean stop. A copy that is not this user's
/// private file, or one with no newer engine known, gets the app's bytes and its version.
#[cfg(unix)]
#[test]
fn persistable_keeps_the_copy_of_a_newer_engine_the_data_root_reports() {
    let temp = durable_folder();
    let mount = temp.path().join(".mount_Ab12Cd");
    let bundled = bundle(&mount.join("usr").join("bin"));
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    let installed = persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap();
    let copies = layout.root().join("setup").join("bin");
    let runner = copies.join("crazyagent-runner");
    let read = |path: &Path| std::fs::read_to_string(path).unwrap();
    write_mode(&installed, "crazyagent newer bytes", 0o700);
    write_mode(&runner, "crazyagent-runner newer bytes", 0o700);
    std::fs::write(
        layout.root().join("owner-current"),
        br#"{"engine_version":"99.0.0"}"#,
    )
    .unwrap();
    assert_eq!(
        persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap(),
        installed
    );
    assert_eq!(read(&installed), "crazyagent newer bytes");
    assert_eq!(read(&runner), "crazyagent-runner newer bytes");
    write_mode(&installed, "crazyagent newer bytes", 0o755);
    assert_eq!(
        persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap(),
        installed
    );
    assert_eq!(read(&installed), "crazyagent bytes");
    assert_eq!(mode_of(&installed), 0o700);
    write_mode(&installed, "crazyagent newer bytes", 0o700);
    write_mode(&runner, "crazyagent-runner newer bytes", 0o700);
    std::fs::remove_file(layout.root().join("owner-current")).unwrap();
    assert_eq!(
        persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap(),
        installed
    );
    assert_eq!(read(&installed), "crazyagent bytes");
    assert_eq!(read(&runner), "crazyagent-runner bytes");
    let recorded = layout.root().join("setup").join("bin-version");
    assert_eq!(read(&recorded), kernel_processes::version::VERSION);
    write_mode(&installed, "crazyagent newer bytes", 0o700);
    std::fs::write(&recorded, "99.0.0\n").unwrap();
    assert_eq!(
        persistable(&bundled, layout.root(), Some(&mount), &HOOKS).unwrap(),
        installed
    );
    assert_eq!(
        read(&installed),
        "crazyagent newer bytes",
        "the newer app's copy outlives its engine's locator"
    );
    assert_eq!(read(&recorded), "99.0.0\n");
}

/// DS-118 r1 F1 (rv-code): a refused start removes an installed definition naming a broken
/// program, so launchd no longer runs a gone, /Volumes or translocated path at login or on a
/// KeepAlive retry, and keeps a usable one. The service has a label of its own, never the
/// owner's.
#[cfg(target_os = "macos")]
#[test]
fn a_refusal_removes_a_definition_naming_a_broken_program_and_keeps_a_usable_one() {
    let temp = tempfile::tempdir().unwrap();
    let mounted = Path::new("/Volumes/CrazyAgent/CrazyAgent.app/Contents/MacOS/crazyagent");
    let bundled = app_engine(temp.path(), "Applications");
    let mut there = labelled(service(Platform::MacOs), "refuse");
    there.root = temp.path().join("engine");
    there.executable = mounted.to_path_buf();
    there.definition = temp.path().join(format!("{}.plist", there.name));
    write_definition(
        &there.definition,
        &there.definition_text().unwrap(),
        Platform::MacOs,
    )
    .unwrap();
    assert_eq!(there.repair(mounted, true, false, &HOOKS), Repair::Refuse);
    let refused = there.refuse().0;
    assert!(
        refused.contains("move CrazyAgent to Applications"),
        "{refused}"
    );
    assert!(
        !there.definition.exists(),
        "the broken definition is removed"
    );
    let mut usable = there.clone();
    usable.executable = bundled;
    write_definition(
        &there.definition,
        &usable.definition_text().unwrap(),
        Platform::MacOs,
    )
    .unwrap();
    assert_eq!(there.repair(mounted, true, true, &HOOKS), Repair::Refuse);
    let refused = there.refuse().0;
    assert!(
        refused.contains("move CrazyAgent to Applications"),
        "{refused}"
    );
    assert!(there.definition.is_file(), "a usable definition stays");
}

/// DS-118 r1 F1 (rv-secreview): `install()` refuses an engine on a disk image or in a
/// translocated folder before it prepares, writes or registers anything, so no login
/// service ever names such a path.
#[cfg(unix)]
#[test]
fn install_refuses_a_transient_engine_before_writing_anything() {
    let temp = tempfile::tempdir().unwrap();
    for platform in [Platform::MacOs, Platform::Linux] {
        for program in [
            "/Volumes/CrazyAgent/CrazyAgent.app/Contents/MacOS/crazyagent",
            "/private/var/folders/ab/xyz/T/AppTranslocation/0A1B2C3D/d/CrazyAgent.app/Contents/MacOS/crazyagent",
        ] {
            let mut there = labelled(service(platform), "install");
            there.root = temp.path().join("engine");
            there.executable = PathBuf::from(program);
            there.definition = temp.path().join(format!("{platform:?}.definition"));
            let refused = there.install(&HOOKS).unwrap_err().0;
            assert!(
                refused.contains("move CrazyAgent to Applications"),
                "{refused}"
            );
            assert!(!there.definition.exists());
            assert!(!there.root.exists(), "nothing was prepared");
        }
    }
}

/// RV-DS241-118: a program path written escaped (XML in a plist; quoted, with `$$` and `%%`,
/// for systemd) reads back as itself, so an engine kept in such a folder is never taken for
/// a gone one and starts as it is.
#[cfg(unix)]
#[test]
fn an_escaped_program_path_reads_back_as_itself_and_starts_as_it_is() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("Tools & $Apps %1 \"q\" \\b 'x' <y>");
    let bundled = app_engine(&folder, "Applications");
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut own = service(platform);
        own.root = temp.path().join("engine");
        own.executable = bundled.clone();
        own.definition = temp.path().join(format!("{platform:?}.definition"));
        write_definition(&own.definition, &own.definition_text().unwrap(), platform).unwrap();
        assert_eq!(
            own.installed_program(),
            Some(bundled.clone()),
            "{platform:?}"
        );
        assert_eq!(own.broken_program(), None);
        assert_eq!(own.repair(&bundled, true, false, &HOOKS), Repair::Start);
    }
}

/// RV-DS241-118: before a plain start the installed definition is checked for another
/// program, as `service start` does: one that still exists is replaced only by an engine
/// that may replace it, so a dev build or a PATH-only difference starts it as it is.
#[cfg(unix)]
#[test]
fn a_definition_running_another_existing_program_is_replaced_only_by_an_engine_that_may() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = app_engine(temp.path(), "Applications");
    let other = app_engine(temp.path(), "Older");
    let built = dev_engine(temp.path());
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut own = service(platform);
        own.root = temp.path().join("engine");
        own.executable = bundled.clone();
        own.definition = temp.path().join(format!("{platform:?}.definition"));
        let mut older = own.clone();
        older.executable = other.clone();
        write_definition(&own.definition, &older.definition_text().unwrap(), platform).unwrap();
        assert!(own.runs_another_program());
        assert!(own.may_replace(&HOOKS));
        assert_eq!(own.broken_program(), None);
        assert_eq!(own.repair(&bundled, true, false, &HOOKS), Repair::Install);
        let mut dev = own.clone();
        dev.executable = built.clone();
        assert_eq!(dev.repair(&built, true, false, &HOOKS), Repair::Start);
        let mut elsewhere = own.clone();
        elsewhere.path = OsString::from("/usr/bin:/bin");
        write_definition(
            &own.definition,
            &elsewhere.definition_text().unwrap(),
            platform,
        )
        .unwrap();
        assert!(!own.runs_another_program());
        assert_eq!(own.repair(&bundled, true, false, &HOOKS), Repair::Start);
    }
}

#[test]
fn an_unusable_system_folder_refuses_the_start_and_is_named_before_every_other_cause() {
    use crate::engine::bootstrap::refuse_unsafe_git;
    use crate::research::runner::GIT_SAFE_UNAVAILABLE;
    assert!(refuse_unsafe_git(None).is_ok());
    assert_eq!(
        refuse_unsafe_git(Some(GIT_SAFE_UNAVAILABLE)).map_err(|error| error.0),
        Err(GIT_SAFE_UNAVAILABLE.to_owned())
    );
    for backup_failed in [false, true] {
        assert_eq!(
            failure_code(Some(GIT_SAFE_UNAVAILABLE), backup_failed),
            GIT_SAFE_UNAVAILABLE,
            "the TUI and the desktop name the refusal first, backup failed {backup_failed}"
        );
    }
    assert_eq!(failure_code(None, true), "engine_backup_failed");
    assert_eq!(failure_code(None, false), "engine_start_failed");
}
