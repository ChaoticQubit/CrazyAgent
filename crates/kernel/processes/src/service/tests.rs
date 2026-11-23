use super::test_support::*;
use super::*;

#[cfg(unix)]
#[test]
fn systemd_unit_quotes_paths_and_escapes_specifiers() {
    let mut linux = service(Platform::Linux);
    linux.executable = PathBuf::from("/home/owner/.local/share/crazy agent/crazyagent");
    linux.root = PathBuf::from("/home/owner/.local/share/crazyagent/engine%1");
    linux.path = OsString::from("/home/owner/.local/bin:/usr/bin:/bin");
    let expected = "[Unit]
Description=CrazyAgent engine

[Service]
ExecStart=\"/home/owner/.local/share/crazy agent/crazyagent\" engine --home \"/home/owner/.local/share/crazyagent/engine%%1\"
Environment=\"PATH=/home/owner/.local/bin:/usr/bin:/bin\"
Restart=on-failure
RestartSec=5
TimeoutStopSec=30
StandardOutput=append:/home/owner/.local/share/crazyagent/engine%%1/logs/engine.log
StandardError=append:/home/owner/.local/share/crazyagent/engine%%1/logs/engine.log

[Install]
WantedBy=default.target
";
    assert_eq!(linux.definition_text().unwrap(), expected);
    linux.executable = PathBuf::from("/opt/$HOME/\"q\"/crazyagent");
    let text = linux.definition_text().unwrap();
    assert!(
        text.contains("ExecStart=\"/opt/$$HOME/\\\"q\\\"/crazyagent\" engine"),
        "{text}"
    );
}

/// systemd.exec: Environment= expands no variables, so a `$` in a PATH folder is written as
/// is, while `%` (a specifier), backslash and double quote are still escaped.
#[cfg(unix)]
#[test]
fn systemd_environment_keeps_a_dollar_in_a_path_folder_as_written() {
    let mut linux = service(Platform::Linux);
    linux.path = OsString::from(
        r#"/home/owner/odd$dir/bin:/home/owner/50%/bin:/home/owner/"q"\/bin:/usr/bin"#,
    );
    let text = linux.definition_text().unwrap();
    assert!(
        text.contains(
            r#"
Environment="PATH=/home/owner/odd$dir/bin:/home/owner/50%%/bin:/home/owner/\"q\"\\/bin:/usr/bin"
"#
        ),
        "{text}"
    );
}

#[test]
fn windows_task_runs_at_logon_for_the_user_and_appends_output_to_the_log() {
    let mut windows = service(Platform::Windows);
    windows.executable = PathBuf::from(r"C:\Users\owner\AppData\Local\CrazyAgent\crazyagent.exe");
    windows.root = PathBuf::from(r"C:\Users\owner\AppData\Local\CrazyAgent\engine");
    windows.path = OsString::new();
    windows.user = Some(r"HOST\owner".into());
    if !windows.executable.is_absolute() {
        windows.executable =
            PathBuf::from("/c/Users/owner/AppData/Local/CrazyAgent/crazyagent.exe");
        windows.root = PathBuf::from("/c/Users/owner/AppData/Local/CrazyAgent/engine");
    }
    let text = windows.definition_text().unwrap();
    let executable = windows.executable.to_string_lossy();
    let root = windows.root.to_string_lossy();
    let log = windows.root.join("logs").join("engine.log");
    let log = log.to_string_lossy();
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n"));
    for part in [
        "<LogonTrigger><Enabled>true</Enabled><UserId>HOST\\owner</UserId></LogonTrigger>"
            .to_owned(),
        "<LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel>".to_owned(),
        "<RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>".to_owned(),
        "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>".to_owned(),
        r"<Command>%SystemRoot%\System32\cmd.exe</Command>".to_owned(),
        format!(
            "<Arguments>/d /s /c &quot;&quot;{executable}&quot; engine --home &quot;{root}&quot; \
             &gt;&gt; &quot;{log}&quot; 2&gt;&amp;1&quot;</Arguments>"
        ),
    ] {
        assert!(text.contains(&part), "{part}\n{text}");
    }
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("task.xml");
    write_definition(&file, &text, Platform::Windows).unwrap();
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(&bytes[..2], &[0xff, 0xfe]);
    let units: Vec<u16> = bytes[2..]
        .chunks(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), text);
}

#[test]
fn definitions_refuse_control_characters_and_relative_paths() {
    let mut bad = service(Platform::MacOs);
    bad.root = PathBuf::from("/Users/owner/engine\n<key>");
    assert!(bad.definition_text().is_err());
    let mut relative = service(Platform::Linux);
    relative.executable = PathBuf::from("crazyagent");
    assert!(relative.definition_text().is_err());
    let mut no_path = service(Platform::Linux);
    no_path.path = OsString::new();
    assert!(no_path.definition_text().is_err());
    let mut relative_home = service(Platform::MacOs);
    relative_home.home = Some(PathBuf::from("owner"));
    assert!(relative_home.definition_text().is_err());
    let mut odd_home = service(Platform::MacOs);
    odd_home.home = Some(PathBuf::from("/Users/owner\n<key>"));
    assert!(odd_home.definition_text().is_err());
    for expanded in ["/c/Users/100%owner/engine", "/c/Users/\"owner\"/engine"] {
        let mut windows = service(Platform::Windows);
        windows.path = OsString::new();
        windows.root = PathBuf::from(expanded);
        let error = windows.definition_text().unwrap_err();
        assert!(error.0.contains("CRAZYAGENT_HOME"), "{error}");
    }
}

#[test]
fn launchd_commands_bootstrap_kickstart_bootout_and_print_in_the_gui_domain() {
    let mac = service(Platform::MacOs);
    let plist = "/Users/owner/Library/LaunchAgents/dev.crazyagent.engine.plist";
    let bootstrap = words(&["/bin/launchctl", "bootstrap", "gui/501", plist]);
    let bootout = words(&["/bin/launchctl", "bootout", "gui/501/dev.crazyagent.engine"]);
    assert_eq!(texts(mac.register_commands(501)), vec![bootstrap.clone()]);
    assert_eq!(texts(mac.unregister_commands(501)), vec![bootout.clone()]);
    assert_eq!(
        texts(vec![mac.start_command(501, true)]),
        vec![words(&[
            "/bin/launchctl",
            "kickstart",
            "gui/501/dev.crazyagent.engine"
        ])]
    );
    assert_eq!(texts(vec![mac.start_command(501, false)]), vec![bootstrap]);
    assert_eq!(texts(vec![mac.stop_command(501)]), vec![bootout]);
    assert_eq!(
        texts(vec![mac.status_command(501)]),
        vec![words(&[
            "/bin/launchctl",
            "print",
            "gui/501/dev.crazyagent.engine"
        ])]
    );
}

#[test]
fn systemd_commands_use_the_user_manager() {
    let linux = service(Platform::Linux);
    let unit = "crazyagent-engine.service";
    assert_eq!(
        texts(linux.register_commands(1000)),
        vec![
            words(&["systemctl", "--user", "daemon-reload"]),
            words(&["systemctl", "--user", "enable", "--now", unit]),
        ]
    );
    assert_eq!(
        texts(linux.unregister_commands(1000)),
        vec![words(&["systemctl", "--user", "disable", "--now", unit])]
    );
    assert_eq!(
        texts(vec![linux.start_command(1000, false)]),
        vec![words(&["systemctl", "--user", "start", unit])]
    );
    assert_eq!(
        texts(vec![linux.stop_command(1000)]),
        vec![words(&["systemctl", "--user", "stop", unit])]
    );
    assert_eq!(
        texts(vec![linux.status_command(1000)]),
        vec![words(&["systemctl", "--user", "is-active", unit])]
    );
}

#[test]
fn windows_commands_create_run_end_delete_and_query_the_task() {
    let mut windows = service(Platform::Windows);
    windows.definition = PathBuf::from("/c/engine/crazyagent-engine-task.xml");
    let task = "CrazyAgent Engine";
    assert_eq!(
        texts(windows.register_commands(0)),
        vec![
            words(&[
                "schtasks",
                "/Create",
                "/TN",
                task,
                "/XML",
                "/c/engine/crazyagent-engine-task.xml",
                "/F",
            ]),
            words(&["schtasks", "/Run", "/TN", task]),
        ]
    );
    assert_eq!(
        texts(windows.unregister_commands(0)),
        vec![
            words(&["schtasks", "/End", "/TN", task]),
            words(&["schtasks", "/Delete", "/TN", task, "/F"]),
        ]
    );
    assert_eq!(
        texts(vec![windows.status_command(0)]),
        vec![words(&["schtasks", "/Query", "/TN", task, "/FO", "LIST"])]
    );
}

#[test]
fn status_output_is_read_per_manager() {
    let launchd = "gui/501/dev.crazyagent.engine = {\n\tactive count = 1\n\tpath = /Users/owner/Library/LaunchAgents/dev.crazyagent.engine.plist\n\ttype = LaunchAgent\n\tstate = running\n\n\tprogram = /x/crazyagent\n\tpid = 4242\n\tendpoints = {\n\t\tstate = active\n\t}\n}\n";
    let mac = service(Platform::MacOs);
    assert_eq!(mac.parse_status(true, launchd), (true, Some(4242)));
    assert_eq!(
        mac.parse_status(
            true,
            &launchd.replace("state = running", "state = not running")
        ),
        (false, Some(4242))
    );
    assert_eq!(mac.parse_status(false, launchd), (false, None));
    let linux = service(Platform::Linux);
    assert_eq!(linux.parse_status(true, "active\n"), (true, None));
    assert_eq!(linux.parse_status(false, "inactive\n"), (false, None));
    let windows = service(Platform::Windows);
    let query = "\r\nFolder: \\\r\nHostName:      HOST\r\nTaskName:      \\CrazyAgent Engine\r\nNext Run Time: N/A\r\nStatus:        Running\r\nLogon Mode:    Interactive only\r\n";
    assert_eq!(windows.parse_status(true, query), (true, None));
    assert_eq!(
        windows.parse_status(true, &query.replace("Running", "Ready")),
        (false, None)
    );
}

/// The service PATH keeps the shell's absolute folders in order, then adds the user's and the
/// system folders once each, and leaves out any folder another user can change on this host
/// (on a CI runner `/usr/local/bin` can be one).
#[cfg(unix)]
#[test]
fn service_path_keeps_the_shell_path_then_adds_user_and_system_folders_once() {
    let kept = |folders: &[&str]| {
        std::env::join_paths(
            folders
                .iter()
                .filter(|folder| changeable_by_others(Path::new(folder)).is_none()),
        )
        .unwrap()
    };
    let path = service_path(
        Some(OsStr::new(
            "/opt/homebrew/bin:relative:/Users/owner/bin:/usr/bin",
        )),
        Some(Path::new("/Users/owner")),
        None,
    );
    assert_eq!(
        path,
        kept(&[
            "/opt/homebrew/bin",
            "/Users/owner/bin",
            "/usr/bin",
            "/Users/owner/.local/bin",
            "/usr/local/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ])
    );
    assert_eq!(
        service_path(None, None, None),
        kept(&[
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ])
    );
}

#[test]
fn windows_path_command_hands_the_folder_over_in_its_environment_only() {
    let folder = Path::new("/c/Program Files/Cra'zy;$(exit 7)");
    let command = windows_path_command(folder);
    assert_eq!(command.get_program(), "powershell.exe");
    let args: Vec<_> = command
        .get_args()
        .map(|part| part.to_str().unwrap())
        .collect();
    assert_eq!(args[..3], ["-NoProfile", "-NonInteractive", "-Command"]);
    assert!(args[3].contains(&format!("$f=$env:{WINDOWS_PATH_FOLDER_VARIABLE};")));
    assert!(args[3].contains("$r=$env:CRAZYAGENT_PATH_KEY;"));
    assert!(
        !args[3].contains("HKCU:"),
        "the key is handed over as data too"
    );
    assert!(!args[3].contains("$args"));
    assert!(!args[3].contains("Cra'zy"));
    assert_eq!(args.len(), 4);
    let mut envs: Vec<_> = command.get_envs().collect();
    envs.sort();
    assert_eq!(
        envs,
        [
            (
                OsStr::new(WINDOWS_PATH_FOLDER_VARIABLE),
                Some(folder.as_os_str())
            ),
            (
                OsStr::new("CRAZYAGENT_PATH_KEY"),
                Some(OsStr::new(r"HKCU:\Environment"))
            )
        ]
    );
}

/// The user's PATH is read as written, with its %VAR% references unexpanded, and stored back
/// with the registry kind it had (REG_EXPAND_SZ when there was none), never as an expanded
/// REG_SZ; a changed PATH is announced to running programs.
#[test]
fn windows_path_command_keeps_the_user_path_kind_and_its_unexpanded_text() {
    let command = windows_path_command(Path::new(r"C:\Users\owner\AppData\Local\CrazyAgent"));
    let script = command.get_args().nth(3).unwrap().to_str().unwrap();
    assert!(script.contains("DoNotExpandEnvironmentNames"), "{script}");
    assert!(script.contains("GetValueKind('Path')"), "{script}");
    assert!(
        script.contains("[Microsoft.Win32.RegistryValueKind]::ExpandString"),
        "{script}"
    );
    assert!(script.contains("-Type $t"), "{script}");
    assert!(
        !script.contains("GetEnvironmentVariable('Path'"),
        "{script}"
    );
    assert!(
        !script.contains("SetEnvironmentVariable('Path'"),
        "{script}"
    );
    assert!(
        script.contains(
            "[Environment]::SetEnvironmentVariable('CRAZYAGENT_PATH_REFRESH', $null, 'User')"
        ),
        "{script}"
    );
}

/// Builds one PowerShell command for a script and its environment, with no input and both
/// outputs collected. Inputs: the script and the variables. Output: the command.
#[cfg(windows)]
fn powershell_command(script: &str, env: &[(&str, &str)]) -> std::process::Command {
    assert!(!script.is_empty());
    let mut command = std::process::Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    for (name, value) in env {
        assert!(!name.is_empty());
        command.env(name, value);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}

/// Runs one PowerShell script with the given environment. Takes the script and the
/// variables. Returns its standard output; panics when PowerShell fails.
#[cfg(windows)]
fn powershell(script: &str, env: &[(&str, &str)]) -> String {
    let output = crate::spawn_gate::output(&mut powershell_command(script, env)).unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[cfg(unix)]
#[test]
fn link_cli_links_into_the_bin_folder_and_refuses_a_real_file() {
    let temp = durable_folder();
    let executable = temp.path().join("app/crazyagent");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    let bin = temp.path().join("home/.local/bin");
    let link = link_cli(&executable, &bin).unwrap();
    assert_eq!(link, bin.join("crazyagent"));
    assert_eq!(std::fs::read_link(&link).unwrap(), executable);
    let newer = temp.path().join("newer/crazyagent");
    std::fs::create_dir_all(newer.parent().unwrap()).unwrap();
    std::fs::write(&newer, "#!/bin/sh\n").unwrap();
    link_cli(&newer, &bin).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), newer);
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, "user file").unwrap();
    let error = link_cli(&newer, &bin).unwrap_err();
    assert!(error.0.contains("not a link"), "{error}");
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "user file");
    assert!(link_cli(&temp.path().join("missing"), &bin).is_err());
}

#[test]
fn each_data_root_names_its_own_service_and_the_default_root_keeps_the_standard_name() {
    let temp = tempfile::tempdir().unwrap();
    let default = temp.path().join("default").join("engine");
    let first = temp.path().join("first").join("engine");
    let second = temp.path().join("second").join("engine");
    for platform in [Platform::MacOs, Platform::Linux, Platform::Windows] {
        assert_eq!(
            service_name(platform, &default, Some(&default)),
            standard(platform)
        );
        let one = service_name(platform, &first, Some(&default));
        let two = service_name(platform, &second, Some(&default));
        assert_ne!(short_hash(&one, platform), short_hash(&two, platform));
        assert_eq!(service_name(platform, &first, Some(&default)), one);
        assert_eq!(service_name(platform, &first, None), one);
        assert_ne!(service_name(platform, &default, None), standard(platform));
    }
    assert_eq!(
        short_hash(
            &service_name(Platform::MacOs, &first, None),
            Platform::MacOs
        ),
        short_hash(
            &service_name(Platform::Linux, &first, None),
            Platform::Linux
        )
    );
}

#[cfg(unix)]
#[test]
fn a_root_names_the_same_service_through_links_and_before_and_after_it_exists() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let default = real.join("default");
    std::fs::create_dir(&default).unwrap();
    assert_eq!(
        service_name(Platform::MacOs, &link.join("default"), Some(&default)),
        LAUNCHD_LABEL
    );
    let before = service_name(Platform::MacOs, &link.join("engine"), Some(&default));
    std::fs::create_dir(real.join("engine")).unwrap();
    let after = service_name(Platform::MacOs, &link.join("engine"), Some(&default));
    assert_eq!(before, after);
    assert_eq!(
        service_name(Platform::MacOs, &real.join("engine"), Some(&default)),
        after
    );
    short_hash(&after, Platform::MacOs);
}

#[cfg(unix)]
#[test]
fn changeable_by_others_names_a_world_writable_folder_on_the_path_or_behind_a_link() {
    let temp = durable_folder();
    let private = temp.path().join("private").join("crazyagent");
    write_mode(&private, "engine", 0o755);
    assert_eq!(changeable_by_others(&private), None);
    for mode in [0o777, 0o1777] {
        let shared = shared_folder(&temp.path().join(format!("shared-{mode:o}")), mode);
        let planted = shared.join("crazyagent");
        write_mode(&planted, "engine", 0o755);
        assert_eq!(changeable_by_others(&planted), Some(shared.clone()));
        let link = temp.path().join("private").join(format!("link-{mode:o}"));
        std::os::unix::fs::symlink(&planted, &link).unwrap();
        assert_eq!(
            changeable_by_others(&link),
            Some(std::fs::canonicalize(&shared).unwrap())
        );
        let inward = shared.join("inward");
        std::os::unix::fs::symlink(&private, &inward).unwrap();
        assert_eq!(changeable_by_others(&inward), Some(shared.clone()));
        assert_eq!(
            changeable_by_others(&shared.join("gone").join("crazyagent")),
            Some(shared.clone())
        );
        let leads = temp.path().join("private").join(format!("leads-{mode:o}"));
        std::os::unix::fs::symlink(&shared, &leads).unwrap();
        assert_eq!(
            changeable_by_others(&leads.join("gone").join("bin")),
            Some(std::fs::canonicalize(&shared).unwrap())
        );
    }
    let gone = temp.path().join("private").join("gone").join("crazyagent");
    assert_eq!(changeable_by_others(&gone), None);
}

/// Changes who may write `path` with icacls. Inputs: the path and the icacls arguments.
#[cfg(windows)]
fn icacls(path: &Path, change: &[&str]) {
    let changed =
        crate::spawn_gate::collected(std::process::Command::new("icacls").arg(path).args(change))
            .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
}

/// V243-WIN-CHANGEABLE-BY-OTHERS: on Windows both checks read owners and access lists, as
/// the owner-only program check does. A file Users may write, the folder of a file where
/// Users may add one, and the first folder above a missing part where Users may add a
/// folder are named. A fresh file, its folders and System32 are not, nor is a folder whose
/// parent only lets Users add a folder, as Authenticated Users may at a drive's root.
#[cfg(windows)]
#[test]
fn changeable_by_others_reads_windows_owners_and_access_lists() {
    let users = "*S-1-5-32-545";
    let directory = tempfile::tempdir_in(std::env::var_os("USERPROFILE").unwrap()).unwrap();
    let above = directory.path().join("above");
    let own = above.join("own");
    std::fs::create_dir_all(&own).unwrap();
    let program = own.join("tool.exe");
    std::fs::write(&program, "").unwrap();
    let system32 = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    for clean in [&program, &own, &above, &system32, &own.join("gone.exe")] {
        assert_eq!(changeable_by_others(clean), None, "{}", clean.display());
        assert_eq!(changeable_past_sticky(clean), None, "{}", clean.display());
    }

    icacls(&above, &["/grant", &format!("{users}:(AD)")]);
    assert_eq!(changeable_by_others(&own), None);
    assert_eq!(changeable_by_others(&program), None);
    assert_eq!(
        changeable_by_others(&above.join("gone").join("tool.exe")),
        Some(above.clone())
    );
    icacls(&above, &["/remove:g", users]);

    icacls(&program, &["/grant", &format!("{users}:(W)")]);
    assert_eq!(changeable_by_others(&program), Some(program.clone()));
    assert_eq!(changeable_past_sticky(&program), Some(program.clone()));
    icacls(&program, &["/remove:g", users]);

    icacls(&own, &["/grant", &format!("{users}:(WD)")]);
    for named in [&program, &own, &own.join("gone.exe")] {
        assert_eq!(
            changeable_by_others(named),
            Some(own.clone()),
            "{}",
            named.display()
        );
        assert_eq!(
            changeable_past_sticky(named),
            Some(own.clone()),
            "{}",
            named.display()
        );
    }
    icacls(&own, &["/remove:g", users]);
    assert_eq!(changeable_by_others(&program), None);
}

/// #331 sec F1: on Windows the service PATH keeps a folder that exists and drops one that
/// does not, under a clean folder or on a drive letter no volume holds, since a drive or
/// folder added there later would put whatever it holds on the engine's harness lookup.
#[cfg(windows)]
#[test]
fn service_path_on_windows_drops_a_folder_that_does_not_exist() {
    let directory = tempfile::tempdir_in(std::env::var_os("USERPROFILE").unwrap()).unwrap();
    let kept = directory.path().join("bin");
    std::fs::create_dir(&kept).unwrap();
    let missing = directory.path().join("gone").join("bin");
    let absent = ('D'..='Z')
        .map(|letter| PathBuf::from(format!("{letter}:\\")))
        .find(|root| !root.exists())
        .map(|root| root.join("crazyagent-gone").join("bin"));
    let current = std::env::join_paths(
        [kept.clone(), missing.clone()]
            .into_iter()
            .chain(absent.clone()),
    )
    .unwrap();
    let folders: Vec<PathBuf> =
        std::env::split_paths(&service_path(Some(&current), None, None)).collect();
    assert_eq!(folders.first(), Some(&kept));
    assert!(!folders.contains(&missing));
    assert!(absent.is_none_or(|absent| !folders.contains(&absent)));
}

#[cfg(unix)]
#[test]
fn service_path_drops_the_appimage_mount_and_folders_other_users_can_change() {
    let temp = durable_folder();
    let mount = temp.path().join(".mount_Ab12Cd");
    std::fs::create_dir_all(mount.join("usr").join("bin")).unwrap();
    let shared = shared_folder(&temp.path().join("shared"), 0o1777);
    let home = temp.path().join("home");
    let tools = temp.path().join("tools");
    let current = std::env::join_paths([
        mount.join("usr").join("bin"),
        shared.join("bin"),
        tools.clone(),
        mount.join("usr").join("sbin"),
        PathBuf::from("/usr/bin"),
    ])
    .unwrap();
    let path = service_path(Some(&current), Some(&home), Some(&mount));
    let kept: Vec<PathBuf> = std::env::split_paths(&path).collect();
    assert_eq!(
        kept[..3],
        [
            tools,
            PathBuf::from("/usr/bin"),
            home.join(".local").join("bin")
        ]
    );
    assert!(
        !kept
            .iter()
            .any(|folder| folder.starts_with(&mount) || folder.starts_with(&shared)),
        "{kept:?}"
    );
    let unmounted = service_path(Some(&current), Some(&home), None);
    let kept: Vec<PathBuf> = std::env::split_paths(&unmounted).collect();
    assert_eq!(kept[0], mount.join("usr").join("bin"));
    assert!(!kept.iter().any(|folder| folder.starts_with(&shared)));
}

#[test]
fn current_holds_only_for_the_definition_install_would_write_now() {
    let temp = tempfile::tempdir().unwrap();
    for platform in [Platform::MacOs, Platform::Linux, Platform::Windows] {
        let mut wanted = service(platform);
        wanted.root = temp.path().join("engine");
        wanted.home = Some(temp.path().to_path_buf());
        wanted.executable = temp.path().join("app").join("crazyagent");
        wanted.definition = temp.path().join(format!("{platform:?}.definition"));
        if platform == Platform::Windows {
            wanted.path = OsString::new();
            wanted.user = Some(r"HOST\owner".into());
        }
        assert!(!wanted.current());
        write_definition(
            &wanted.definition,
            &wanted.definition_text().unwrap(),
            platform,
        )
        .unwrap();
        assert!(wanted.current());
        let mut moved = wanted.clone();
        moved.executable = temp
            .path()
            .join(".mount_Ab12Cd")
            .join("usr")
            .join("bin")
            .join("crazyagent");
        assert!(!moved.current());
        std::fs::write(&wanted.definition, wanted.definition_text().unwrap() + "\n").unwrap();
        assert!(!wanted.current());
    }
}

/// v2.4.0 owner update bug: the update's `service start` started the definition an install
/// script wrote for an older engine. A definition running another program is told apart from
/// one that only has another PATH, which still starts as it is.
#[test]
fn a_definition_that_runs_another_program_is_told_apart_from_one_with_another_path() {
    let temp = tempfile::tempdir().unwrap();
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut own = service(platform);
        own.root = temp.path().join("engine");
        own.home = Some(temp.path().to_path_buf());
        own.executable = temp.path().join("CrazyAgent.app").join("crazyagent");
        own.definition = temp.path().join(format!("{platform:?}.definition"));
        assert!(!own.runs_another_program());
        let mut script = own.clone();
        script.executable = temp.path().join("bin").join("engine-start");
        write_definition(
            &own.definition,
            &script.definition_text().unwrap(),
            platform,
        )
        .unwrap();
        assert!(own.runs_another_program());
        let mut elsewhere = own.clone();
        elsewhere.path = OsString::from("/usr/bin:/bin");
        write_definition(
            &own.definition,
            &elsewhere.definition_text().unwrap(),
            platform,
        )
        .unwrap();
        assert!(!own.current());
        assert!(!own.runs_another_program());
        write_definition(&own.definition, &own.definition_text().unwrap(), platform).unwrap();
        assert!(!own.runs_another_program());
    }
}

/// rv-code F1: the definition writes the executable escaped (XML in a plist, quoted for
/// systemd), so an app kept in a folder named with `&` or `$` is still found in its own
/// definition and still told apart from an install script's.
#[test]
fn an_executable_path_with_escaped_characters_is_still_found_in_its_definition() {
    let temp = tempfile::tempdir().unwrap();
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut own = service(platform);
        own.root = temp.path().join("engine");
        own.home = Some(temp.path().to_path_buf());
        own.executable = temp
            .path()
            .join("Tools & $Apps")
            .join("CrazyAgent.app")
            .join("crazyagent");
        own.definition = temp.path().join(format!("{platform:?}.definition"));
        write_definition(&own.definition, &own.definition_text().unwrap(), platform).unwrap();
        assert!(!own.runs_another_program());
        let mut script = own.clone();
        script.executable = temp.path().join("bin").join("engine-start");
        write_definition(
            &own.definition,
            &script.definition_text().unwrap(),
            platform,
        )
        .unwrap();
        assert!(own.runs_another_program());
    }
}

#[test]
fn stale_replaces_a_definition_only_for_a_private_copy_of_the_bundled_engine() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = temp.path().join("CrazyAgent.app").join("crazyagent");
    let copy = temp.path().join("engine").join("bin").join("crazyagent");
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut installed = service(platform);
        installed.root = temp.path().join("engine");
        installed.home = Some(temp.path().to_path_buf());
        installed.executable = bundled.clone();
        installed.path = OsString::from("/usr/bin:/bin");
        installed.definition = temp.path().join(format!("{platform:?}.definition"));
        write_definition(
            &installed.definition,
            &installed.definition_text().unwrap(),
            platform,
        )
        .unwrap();
        let mut launched = installed.clone();
        launched.path = OsString::from("/opt/homebrew/bin:/usr/bin:/bin");
        assert!(!launched.current());
        assert!(!launched.stale(&bundled));
        assert!(!installed.stale(&bundled));
        let mut copied = launched.clone();
        copied.executable = copy.clone();
        assert!(copied.stale(&bundled));
        write_definition(
            &copied.definition,
            &copied.definition_text().unwrap(),
            platform,
        )
        .unwrap();
        assert!(!copied.stale(&bundled));
    }
}

/// `start()` refuses a definition naming a gone, /Volumes or
/// translocated program before any service command, so no caller starts it as it is. The
/// row uses the systemd form and a name of its own, so even without the guard it never
/// reaches launchd or the owner's unit.
#[cfg(unix)]
#[test]
fn start_refuses_a_definition_naming_a_broken_program_before_any_service_command() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = app_engine(temp.path(), "Applications");
    let mut own = labelled(service(Platform::Linux), "start");
    own.root = temp.path().join("engine");
    own.executable = bundled;
    own.definition = temp.path().join(&own.name);
    for program in [
        temp.path().join("Gone").join("crazyagent"),
        PathBuf::from("/Volumes/CrazyAgent/CrazyAgent.app/Contents/MacOS/crazyagent"),
        PathBuf::from(
            "/private/var/folders/ab/xyz/T/AppTranslocation/0A1B2C3D/d/CrazyAgent.app/Contents/MacOS/crazyagent",
        ),
    ] {
        let mut broken = own.clone();
        broken.executable = program.clone();
        write_definition(
            &own.definition,
            &broken.definition_text().unwrap(),
            Platform::Linux,
        )
        .unwrap();
        let refused = own.start().unwrap_err().0;
        assert!(
            refused.contains(&program.display().to_string()),
            "{refused}"
        );
        assert!(
            refused.contains("reinstall it: crazyagent service install"),
            "{refused}"
        );
        assert!(own.definition.is_file());
    }
}

/// A Nix-style store, a sticky folder its group can write such as /nix/store
/// (root:nixbld 1775), is fine two or more levels above an entry whose folder below it
/// exists, since its group cannot replace an entry another user owns; the entry's own
/// folder, and a missing folder below the store, keep the full checks (L8c, 11-12).
#[cfg(unix)]
#[test]
fn a_sticky_group_writable_store_counts_only_as_an_entry_s_own_folder_or_above_a_missing_one() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let temp = durable_folder();
    let store = temp.path().join("store");
    let package = store.join("hash-tool-1.0");
    let bin = package.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("tool");
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    let loose = store.join("loose");
    std::fs::write(&loose, "").unwrap();
    let mode = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    mode(&tool, 0o555);
    mode(&bin, 0o555);
    mode(&package, 0o555);
    mode(&store, 0o1775);
    if trusted_group(std::fs::metadata(&store).unwrap().gid()) {
        mode(&package, 0o755);
        mode(&bin, 0o755);
        return;
    }
    assert_eq!(changeable_by_others(&tool), None);
    assert_eq!(changeable_by_others(&bin), None);
    assert_eq!(
        changeable_by_others(&loose),
        Some(store.clone()),
        "the entry's own folder keeps the full checks"
    );
    assert_eq!(
        changeable_past_sticky(&loose),
        None,
        "for the tools setup and the resolver run, a present entry the group cannot replace \
         passes in the store itself (rv-sec2 corrections F2)"
    );
    assert_eq!(
        changeable_by_others(&store.join("gone/bin/tool")),
        Some(store.clone()),
        "the group could make a missing folder below the store"
    );
    mode(&store, 0o775);
    assert_eq!(
        changeable_by_others(&tool),
        Some(store.clone()),
        "without the sticky bit the group can replace the package"
    );
    mode(&package, 0o755);
    mode(&bin, 0o755);
}

/// The ownership rule on synthetic parts, the entry first: a /tmp-like
/// folder (root 1777) above this user's 0700 folder passes for the tools the resolver runs
/// and stays changeable for the engine service; a part another user owns below it is
/// refused; a /nix/store-like folder (root:nixbld 1775) above 0555 packages passes for
/// both; a missing part below a store, or /tmp as the entry's own folder, is refused.
#[cfg(unix)]
#[test]
fn a_sticky_store_passes_only_above_parts_this_user_or_root_owns() {
    const ME: u32 = 501;
    const OTHER: u32 = 502;
    const NIXBLD: u32 = 30_000;
    let found = |owner, group, mode, folder| Part::Found {
        owner,
        group,
        mode,
        folder,
        link: false,
    };
    let system = found(0, 0, 0o755, true);
    let tmp = found(0, 0, 0o1777, true);
    let tool = found(ME, 20, 0o755, false);
    let own = found(ME, 20, 0o700, true);
    let under_tmp = [tool, own, tmp, system, system];
    assert_eq!(first_changeable_part(&under_tmp, ME, true), None);
    assert_eq!(
        first_changeable_part(&under_tmp, ME, false),
        Some(2),
        "the service check is unchanged"
    );
    let theirs = [tool, found(OTHER, 20, 0o700, true), tmp, system];
    assert_eq!(first_changeable_part(&theirs, ME, true), Some(1));
    let deeper = [tool, own, found(OTHER, 20, 0o755, true), tmp, system];
    assert_eq!(first_changeable_part(&deeper, ME, true), Some(2));
    assert_eq!(
        first_changeable_part(&[tool, tmp, system], ME, true),
        Some(1)
    );
    let missing = [Part::Missing, Part::Missing, tmp, system];
    assert_eq!(first_changeable_part(&missing, ME, true), Some(2));
    let package = found(0, 0, 0o555, true);
    let nix = [
        found(0, 0, 0o555, false),
        package,
        package,
        found(0, NIXBLD, 0o1775, true),
        system,
        system,
    ];
    assert_eq!(first_changeable_part(&nix, ME, true), None);
    assert_eq!(first_changeable_part(&nix, ME, false), None);
    let plain = [tool, own, found(ME, 20, 0o775, true), system];
    assert_eq!(
        first_changeable_part(&plain, ME, true),
        Some(2),
        "a group-writable folder with no sticky bit"
    );
}

/// For the tools the resolver and setup run, a file directly
/// in a sticky store such as /nix/store, where nix-darwin's /opt/homebrew/bin/brew link
/// resolves, passes: the store's group cannot unlink or rename an entry root owns there. The
/// service check is unchanged, and a group-writable parent with no sticky bit, a sticky one
/// another user owns, one over a missing entry, and a world-writable sticky one such as /tmp
/// as the entry's own folder still count.
#[cfg(unix)]
#[test]
fn a_file_directly_in_a_sticky_group_store_passes_and_an_unsticky_parent_does_not() {
    const ME: u32 = 501;
    const OTHER: u32 = 502;
    const NIXBLD: u32 = 30_000;
    let found = |owner, group, mode, folder| Part::Found {
        owner,
        group,
        mode,
        folder,
        link: false,
    };
    let system = found(0, 0, 0o755, true);
    let brew = found(0, 0, 0o555, false);
    let store = found(0, NIXBLD, 0o1775, true);
    assert_eq!(
        first_changeable_part(&[brew, store, system, system], ME, true),
        None
    );
    assert_eq!(
        first_changeable_part(&[brew, store, system, system], ME, false),
        Some(1),
        "the service check is unchanged"
    );
    let unsticky = found(0, NIXBLD, 0o775, true);
    assert_eq!(
        first_changeable_part(&[brew, unsticky, system, system], ME, true),
        Some(1)
    );
    let theirs = found(OTHER, NIXBLD, 0o1775, true);
    assert_eq!(
        first_changeable_part(&[brew, theirs, system, system], ME, true),
        Some(1)
    );
    assert_eq!(
        first_changeable_part(&[Part::Missing, store, system, system], ME, true),
        Some(1)
    );
    let tmp = found(0, 0, 0o1777, true);
    assert_eq!(
        first_changeable_part(&[brew, tmp, system], ME, true),
        Some(1)
    );
}

/// RV-DS241-140 r4: on macOS the daemon group, whose only member is root, is trusted like
/// wheel and admin, so /private/var/run (root:daemon 775), where nix-darwin's /run link
/// leads, passes as a part of a harness chain; the same folder writable by staff, which every
/// user is in, stays changeable.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_only_root_and_the_daemon_group_can_write_passes_on_macos() {
    const ME: u32 = 501;
    const DAEMON: u32 = 1;
    const STAFF: u32 = 20;
    let found = |owner, group, mode, folder, link| Part::Found {
        owner,
        group,
        mode,
        folder,
        link,
    };
    let system = found(0, 0, 0o40755, true, false);
    let current = found(0, 0, 0o120755, false, true);
    for open_stores in [false, true] {
        let run = [
            current,
            found(0, DAEMON, 0o40775, true, false),
            system,
            system,
            system,
        ];
        assert_eq!(first_changeable_part(&run, ME, open_stores), None);
        let staff = [
            current,
            found(0, STAFF, 0o40775, true, false),
            system,
            system,
            system,
        ];
        assert_eq!(first_changeable_part(&staff, ME, open_stores), Some(1));
    }
}

/// R2-L1: a file a group other than root's can write counts as changeable, like a folder,
/// for the engine service and for the tools the resolver runs: a 0775 staff file in this
/// user's 0755 folder is refused, 0755 or a root-group 0775 passes, and a link's own mode is
/// never read.
#[cfg(unix)]
#[test]
fn a_file_an_untrusted_group_can_write_is_changeable_like_a_folder() {
    const ME: u32 = 501;
    const STAFF: u32 = 20;
    let found = |group, mode, folder, link| Part::Found {
        owner: ME,
        group,
        mode,
        folder,
        link,
    };
    let own = found(STAFF, 0o755, true, false);
    let system = Part::Found {
        owner: 0,
        group: 0,
        mode: 0o755,
        folder: true,
        link: false,
    };
    for open_stores in [false, true] {
        let grouped = [found(STAFF, 0o775, false, false), own, system];
        assert_eq!(first_changeable_part(&grouped, ME, open_stores), Some(0));
        let plain = [found(STAFF, 0o755, false, false), own, system];
        assert_eq!(first_changeable_part(&plain, ME, open_stores), None);
        let root_group = [found(0, 0o775, false, false), own, system];
        assert_eq!(first_changeable_part(&root_group, ME, open_stores), None);
        let link = [found(STAFF, 0o777, false, true), own, system];
        assert_eq!(
            first_changeable_part(&link, ME, open_stores),
            None,
            "a link's own mode is not read"
        );
    }
}

/// 2.4.1 R2: on Linux the egid's group is trusted for a group-writable folder only when it is
/// this user's private group, named as the user and listing no other member; a shared
/// primary group such as openSUSE's `users`, or a group listing another user, is not.
#[test]
fn only_a_user_private_group_counts_as_the_user_own_group() {
    let listed =
        |names: &[&str]| -> Vec<String> { names.iter().map(|name| (*name).to_owned()).collect() };
    let own = listed(&["ana"]);
    assert!(private_group("ana", &listed(&[]), &own, "ana"));
    assert!(private_group("ana", &listed(&["ana"]), &own, "ana"));
    assert!(
        !private_group("users", &listed(&[]), &own, "ana"),
        "a shared primary group"
    );
    assert!(
        !private_group("ana", &listed(&["ana", "bo"]), &own, "ana"),
        "a group listing another user"
    );
    assert!(
        !private_group("", &listed(&[]), &listed(&[""]), ""),
        "no names at all"
    );
}

/// 2.4.1 r1 F3: a group's member list leaves out the accounts whose primary group it is, so
/// a group named as the user that another account has as its primary group is not private,
/// and neither is one whose own account the local account file does not list, such as a
/// directory account, whose other primary users cannot be seen.
#[test]
fn a_group_another_account_has_as_its_primary_group_is_not_private() {
    let listed =
        |names: &[&str]| -> Vec<String> { names.iter().map(|name| (*name).to_owned()).collect() };
    assert!(private_group("ana", &[], &listed(&["ana"]), "ana"));
    assert!(
        !private_group("ana", &[], &listed(&["ana", "svc"]), "ana"),
        "another account's primary group"
    );
    assert!(
        !private_group("ana", &[], &listed(&[]), "ana"),
        "the user's own account is not in the file"
    );
}

/// 2.4.1 r1 F3: the accounts whose primary group is a gid are read from the local account
/// file's fourth field; comments and blank lines are skipped, and a line that cannot be read
/// or that pulls accounts in from elsewhere (`+` or `-`) gives none, the stricter rule.
#[test]
fn the_accounts_with_a_primary_group_are_read_from_the_local_account_file() {
    let passwd = "root:x:0:0:root:/root:/bin/bash\n# kept by hand\n\nana:x:1000:1000:Ana:/home/ana:/bin/bash\nsvc:x:998:1000::/srv:/usr/sbin/nologin\nbo:x:1001:1001::/home/bo:/bin/sh\n";
    assert_eq!(
        primary_users(passwd, 1000),
        Some(vec!["ana".to_owned(), "svc".to_owned()])
    );
    assert_eq!(primary_users(passwd, 1001), Some(vec!["bo".to_owned()]));
    assert_eq!(primary_users(passwd, 4242), Some(Vec::new()));
    assert_eq!(primary_users("+::::::\n", 1000), None, "a NIS include");
    assert_eq!(primary_users("-bo::::::\n", 1000), None, "a NIS exclude");
    assert_eq!(primary_users("ana:x:1000\n", 1000), None, "too few fields");
    assert_eq!(
        primary_users("ana:x:1000:staff:::\n", 1000),
        None,
        "a group that is not a number"
    );
}

/// RV-DS241-18: a part an access list lets someone other than this user or root change
/// counts as changeable at its depth, for the engine service and for the tools the resolver
/// runs alike, even inside a store.
#[cfg(unix)]
#[test]
fn a_part_an_access_list_opens_to_others_is_changeable() {
    const ME: u32 = 501;
    let own = Part::Found {
        owner: ME,
        group: 20,
        mode: 0o755,
        folder: true,
        link: false,
    };
    let system = Part::Found {
        owner: 0,
        group: 0,
        mode: 0o755,
        folder: true,
        link: false,
    };
    let tool = Part::Found {
        owner: ME,
        group: 20,
        mode: 0o755,
        folder: false,
        link: false,
    };
    for open_stores in [false, true] {
        assert_eq!(
            first_changeable_part(&[tool, Part::Shared, own, system], ME, open_stores),
            Some(1)
        );
        assert_eq!(
            first_changeable_part(&[Part::Shared, own, system], ME, open_stores),
            Some(0)
        );
        assert_eq!(
            first_changeable_part(&[tool, own, system], ME, open_stores),
            None
        );
    }
}

/// RV-DS241-18: a macOS `ls -lde` listing names an entry changeable by others when an
/// allow entry gives write, append, add or delete rights, or rights over the entry's own
/// attributes, security or owner, to anyone but this user, root, wheel, daemon or admin; deny
/// entries and read rights never do, and a listing it cannot read fails closed.
#[cfg(unix)]
#[test]
fn an_access_listing_that_lets_others_change_an_entry_is_shared() {
    let listing = |entries: &[&str]| {
        let mut text = String::from("drwxr-xr-x+ 2 me  staff  64 Nov 14 03:33 /Users/me/bin\n");
        for (index, entry) in entries.iter().enumerate() {
            text.push_str(&format!(" {index}: {entry}\n"));
        }
        text
    };
    let me = Some("me");
    assert_eq!(acl::listed_shared(&listing(&[]), me), Some(false));
    for entry in [
        "group:staff allow add_file,delete_child",
        "group:everyone allow write",
        "user:other allow append",
        "user:other allow add_subdirectory",
        "user:other allow delete",
        "user:other allow writeattr",
        "user:other allow writeextattr",
        "user:other allow writesecurity",
        "user:other allow chown",
        "group:staff inherited allow add_file",
        "ABCDEF01-2345-6789-ABCD-EF0123456789 allow write",
    ] {
        assert_eq!(
            acl::listed_shared(&listing(&["group:everyone deny delete", entry]), me),
            Some(true),
            "{entry}"
        );
        assert_eq!(
            acl::listed_shared(&listing(&[entry]), None),
            Some(true),
            "{entry}"
        );
    }
    for entry in [
        "group:everyone deny delete",
        "user:me allow write,append,delete",
        "user:root allow write",
        "group:wheel allow add_file",
        "group:daemon allow add_file",
        "group:admin allow add_file",
        "user:other allow read,list,search,readattr,readextattr,readsecurity,execute",
        "group:staff inherited deny add_file,delete_child",
    ] {
        assert_eq!(
            acl::listed_shared(&listing(&[entry]), me),
            Some(false),
            "{entry}"
        );
    }
    assert_eq!(
        acl::listed_shared(&listing(&["user:me allow write"]), None),
        Some(true),
        "without this user's name every user entry counts as someone else"
    );
    for broken in [
        format!("{}x: group:staff allow write\n", listing(&[])),
        format!("{} 0: group:staff\n", listing(&[])),
        format!("{} 0: group:staff allow\n", listing(&[])),
        String::new(),
    ] {
        assert_eq!(acl::listed_shared(&broken, me), None, "{broken:?}");
    }
}

/// RV-DS241-18 r2: one `ls -lde` listing of several entries is read block by block, each
/// block going to the one entry its first line ends with, the longest when one name ends
/// another. An entry with no block or two gets none, and so does one whose block does not
/// read, alone; a listing with a block for no entry or an access-list line before any entry
/// gives none for every entry.
#[cfg(unix)]
#[test]
fn a_listing_of_several_entries_is_read_block_by_block() {
    let me = Some("me");
    let line = |name: &str| format!("drwxr-xr-x+ 2 me  staff  64 Nov 14 03:33 {name}\n");
    let open = " 0: group:staff allow add_file,delete_child\n";
    let closed = " 0: group:everyone deny delete\n";
    let entries = ["/Volumes/My /c", "/c", "/Users/me/bin", "/Users/me"];
    let text = format!(
        "{}{}{open}{}{closed}{}",
        line("/Users/me"),
        line("/Users/me/bin"),
        line("/Volumes/My /c"),
        line("/c")
    );
    assert_eq!(
        acl::listed_shared_all(&text, &entries, me),
        vec![Some(false), Some(false), Some(true), Some(false)]
    );
    assert_eq!(
        acl::listed_shared_all(&line("/Users/me"), &["/Users/me", "/gone"], me),
        vec![Some(false), None],
        "an entry the listing left out"
    );
    assert_eq!(
        acl::listed_shared_all(&format!("{0}{0}", line("/a")), &["/a"], me),
        vec![None],
        "an entry listed twice"
    );
    assert_eq!(
        acl::listed_shared_all(
            &format!("{} 0: group:staff\n{}{open}", line("/a"), line("/b")),
            &["/a", "/b"],
            me
        ),
        vec![None, Some(true)],
        "a block that does not read fails alone"
    );
    for whole in [
        format!("{open}{}", line("/a")),
        format!("{}{}{open}", line("/a"), line("/zzz")),
        String::new(),
    ] {
        assert_eq!(
            acl::listed_shared_all(&whole, &["/a", "/b"], me),
            vec![None, None],
            "{whole:?}"
        );
    }
}

/// RV-DS241-18: a Linux `system.posix_acl_access` value names an entry changeable by others
/// when a named user other than this user or root, or a named group the trusted-group rule
/// does not trust, has write; the owner, group and other entries stay with the mode bits,
/// and a value it cannot read fails closed.
#[cfg(unix)]
#[test]
fn a_posix_access_list_that_lets_others_write_is_shared() {
    const ME: u32 = 1000;
    const OTHER: u32 = 1001;
    const OWN_GROUP: u32 = 1000;
    const SHARED_GROUP: u32 = 100;
    let trusted = |group: u32| group == 0 || group == OWN_GROUP;
    let value = |entries: &[(u16, u16, u32)]| {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in entries {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(perm.to_le_bytes());
            bytes.extend(id.to_le_bytes());
        }
        bytes
    };
    let base = [
        (0x01, 7, u32::MAX),
        (0x04, 5, u32::MAX),
        (0x10, 7, u32::MAX),
        (0x20, 5, u32::MAX),
    ];
    let with = |extra: (u16, u16, u32)| {
        let mut entries = base.to_vec();
        entries.insert(1, extra);
        value(&entries)
    };
    assert_eq!(acl::posix_shared(&value(&base), ME, trusted), Some(false));
    for open in [(0x02, 7, OTHER), (0x02, 2, OTHER), (0x08, 6, SHARED_GROUP)] {
        assert_eq!(
            acl::posix_shared(&with(open), ME, trusted),
            Some(true),
            "{open:?}"
        );
    }
    for closed in [
        (0x02, 7, ME),
        (0x02, 7, 0),
        (0x02, 5, OTHER),
        (0x08, 7, OWN_GROUP),
        (0x08, 5, SHARED_GROUP),
    ] {
        assert_eq!(
            acl::posix_shared(&with(closed), ME, trusted),
            Some(false),
            "{closed:?}"
        );
    }
    let mut old = value(&base);
    old[0] = 1;
    assert_eq!(acl::posix_shared(&old, ME, trusted), None);
    let mut cut = value(&base);
    cut.pop();
    assert_eq!(acl::posix_shared(&cut, ME, trusted), None);
    assert_eq!(acl::posix_shared(&[], ME, trusted), None);
}

/// RV-DS241-18: on macOS a folder whose mode bits are private but whose access list lets
/// staff add and delete files is changeable by others, for the engine service and for the
/// resolver's tools, while a deny-only list leaves it private.
#[cfg(target_os = "macos")]
#[test]
fn a_tool_folder_staff_may_change_through_its_access_list_is_changeable() {
    let temp = durable_folder();
    let folder = temp.path().join("bin");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    assert_eq!(changeable_by_others(&tool), None);
    let chmod = |rule: &str| {
        let status = crate::spawn_gate::status(
            std::process::Command::new("/bin/chmod")
                .arg("+a")
                .arg(rule)
                .arg(&folder),
        )
        .unwrap();
        assert!(status.success(), "{rule}");
    };
    chmod("user:nobody deny add_file");
    assert_eq!(
        changeable_by_others(&tool),
        None,
        "a deny entry changes nothing"
    );
    assert_eq!(mode_of(&folder), 0o755);
    chmod("group:staff allow add_file,delete_child");
    assert_eq!(mode_of(&folder), 0o755, "the mode bits still read private");
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    assert_eq!(changeable_past_sticky(&tool), Some(folder.clone()));
}

/// The wall clock's whole second, as change times count them. Output: the second.
#[cfg(target_os = "macos")]
fn now_secs() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_secs()).unwrap()
}

/// The whole second an entry last changed. Input: the entry. Output: the second.
#[cfg(target_os = "macos")]
fn changed_secs(entry: &Path) -> i64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(entry).unwrap().ctime()
}

/// Waits, at most 1.5 s, until the clock is in a later second than every entry last
/// changed, so an access answer read now is kept. Input: the entries.
#[cfg(target_os = "macos")]
fn settle(entries: &[&Path]) {
    let changed = entries.iter().copied().map(changed_secs).max().unwrap();
    for _ in 0..30 {
        if now_secs() > changed {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the clock did not pass second {changed}");
}

/// Waits, at most 1.5 s, until the clock is in the first half of a second, so a change and
/// the listing made right after it share one second.
#[cfg(target_os = "macos")]
fn early_in_a_second() {
    for _ in 0..30 {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        if since.subsec_millis() < 500 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the clock never read the first half of a second");
}

/// HFS-CTIME: an access answer is kept only when its listing started in a later second than
/// the entry last changed, so a later change moves the stamp even where change times keep
/// whole seconds (HFS+), and an answer read in the second of the change is never kept.
#[cfg(target_os = "macos")]
#[test]
fn an_access_answer_is_kept_only_once_its_entry_changed_in_an_earlier_second() {
    assert!(acl::settled(&(1, 2, 100, 0), 101));
    assert!(acl::settled(&(1, 2, 100, 999_999_999), 101));
    assert!(!acl::settled(&(1, 2, 100, 0), 100), "the same whole second");
    assert!(
        !acl::settled(&(1, 2, 100, 999_999_999), 100),
        "the same second, to the nanosecond"
    );
    assert!(
        !acl::settled(&(1, 2, 105, 0), 100),
        "a change time ahead of the clock"
    );
}

/// Changes a tool's folder and reads the tool's access answer within one second, at most
/// three tries, since a slow spawn can carry the listing into the next second: the first try
/// gives staff `add_file` on the folder's access list, a later one flips its mode, which
/// keeps that entry. Each read lists the folder alone. Inputs: the tool, its folder, the root.
#[cfg(target_os = "macos")]
fn changed_and_read_in_one_second(tool: &Path, folder: &Path, root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for attempt in 0..3u32 {
        early_in_a_second();
        if attempt == 0 {
            let status = crate::spawn_gate::status(
                std::process::Command::new("/bin/chmod")
                    .arg("+a")
                    .arg("group:staff allow add_file,delete_child")
                    .arg(folder),
            )
            .unwrap();
            assert!(status.success());
        } else {
            let mode = if attempt == 1 { 0o750 } else { 0o755 };
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert_eq!(changeable_by_others(tool), Some(folder.to_path_buf()));
        let changed = acl::take_listings();
        assert_eq!(changed.len(), 1, "{changed:?}");
        let ours: Vec<PathBuf> = changed[0]
            .iter()
            .filter(|listed| listed.starts_with(root))
            .cloned()
            .collect();
        assert_eq!(ours, vec![folder.to_path_buf()], "the changed part alone");
        if now_secs() == changed_secs(folder) {
            return;
        }
    }
    panic!("no change and its listing shared a second in three tries");
}

/// RV-DS241-18 r2: one `ls -lde` lists every part of a path the first time, names with a
/// space or an accent included; a repeat lists none of the parts it read, and an access-list
/// change moves only the changed part's change time, so that part alone is listed again and
/// the change counts. HFS-CTIME: a part read in the second it changed is read again once
/// that second has passed, and only then kept.
#[cfg(target_os = "macos")]
#[test]
fn one_access_listing_reads_a_whole_path_and_a_repeat_lists_only_changed_parts() {
    let temp = durable_folder();
    let kit = temp.path().join("tool kit \u{e9}");
    let folder = kit.join("bin");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&tool, &folder, &kit, temp.path()]);
    let ours = |listings: &[Vec<PathBuf>]| -> Vec<PathBuf> {
        listings
            .iter()
            .flatten()
            .filter(|listed| listed.starts_with(temp.path()))
            .cloned()
            .collect()
    };
    acl::take_listings();
    assert_eq!(changeable_by_others(&tool), None);
    let first = acl::take_listings();
    assert_eq!(first.len(), 1, "one listing for every part: {first:?}");
    for part in [&tool, &folder, &kit] {
        assert!(first[0].contains(part), "{part:?} in {first:?}");
    }

    assert_eq!(changeable_by_others(&tool), None);
    let repeat = acl::take_listings();
    assert!(repeat.len() <= 1, "{repeat:?}");
    assert_eq!(
        ours(&repeat),
        Vec::<PathBuf>::new(),
        "nothing read is listed again"
    );

    changed_and_read_in_one_second(&tool, &folder, temp.path());
    settle(&[&folder]);
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    assert_eq!(
        ours(&acl::take_listings()),
        vec![folder.clone()],
        "a part read in the second it changed is read again"
    );
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    assert_eq!(
        ours(&acl::take_listings()),
        Vec::<PathBuf>::new(),
        "read in a later second, it is kept"
    );
}

/// The launchd property list rows: what the plist runs, the environment it gives the
/// engine and how it escapes paths. Inputs: the macOS service from [`service`]. Output:
/// none; each row asserts on the written text.
mod launchd;

/// The Windows user PATH rows of link_cli: a folder named like PowerShell code is added
/// once, one holding a ';' is refused, and the PATH keeps its kind and unexpanded
/// references. Inputs: the user's stored PATH, restored after each row. Output: none.
mod windows_user_path;

#[test]
fn service_labels_are_unchanged() {
    assert_eq!(LAUNCHD_LABEL, "dev.crazyagent.engine");
    assert_eq!(SYSTEMD_UNIT, "crazyagent-engine.service");
    assert_eq!(WINDOWS_TASK, "CrazyAgent Engine");
}

/// The definition each manager reads is byte for byte the text kept in
/// `tests/fixtures/service/`, written by the earlier `definition_text`. The Windows task is
/// stored with LF, since the repository checks text out with LF; the text itself ends every
/// line in CRLF.
#[cfg(unix)]
#[test]
fn definition_text_is_byte_equal_for_each_platform() {
    let text = |platform| {
        Service {
            user: Some(r"HOST\owner".to_owned()),
            ..service(platform)
        }
        .definition_text()
        .unwrap()
    };
    assert_eq!(
        text(Platform::MacOs),
        include_str!("../../tests/fixtures/service/launchd.plist")
    );
    assert_eq!(
        text(Platform::Linux),
        include_str!("../../tests/fixtures/service/systemd.service")
    );
    let windows = text(Platform::Windows);
    assert!(windows.ends_with("\r\n"), "{windows:?}");
    assert!(
        windows
            .split_inclusive('\n')
            .all(|line| line.ends_with("\r\n")),
        "{windows:?}"
    );
    assert_eq!(
        windows.replace("\r\n", "\n"),
        include_str!("../../tests/fixtures/service/windows-task.xml")
    );
}

/// The engine's reported version reaches [`Service::may_replace`] only through the hook it is
/// given: a newer one refuses the replace, an older one or none allows it, and the hook is
/// asked about this service's data root. No other hook is called.
#[cfg(unix)]
#[test]
fn may_replace_reads_the_reported_version_it_is_given() {
    let hooks = |reported_version: fn(&Path) -> Option<String>| Hooks {
        reported_version,
        prepare_root: |root| panic!("may_replace prepared {}", root.display()),
        private_folder: |path| panic!("may_replace made {}", path.display()),
        is_private_folder: |path| panic!("may_replace checked {}", path.display()),
    };
    let temp = tempfile::tempdir().unwrap();
    let mut own = service(Platform::MacOs);
    own.root = temp.path().join("engine");
    own.executable = app_engine(temp.path(), "Applications");
    own.definition = temp.path().join("dev.crazyagent.engine.plist");
    assert!(own.may_replace(&hooks(|root| {
        assert!(root.ends_with("engine"), "{}", root.display());
        None
    })));
    assert!(own.may_replace(&hooks(|_| Some("0.0.1".to_owned()))));
    assert!(!own.may_replace(&hooks(|_| Some("99.0.0".to_owned()))));
}
