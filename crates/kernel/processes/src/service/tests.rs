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

#[cfg(unix)]
#[test]
fn a_link_chain_through_a_shared_folder_is_changeable_though_both_ends_are_private() {
    let temp = durable_folder();
    let private = temp.path().join("private");
    write_mode(&private.join("real"), "engine", 0o755);
    let shared = shared_folder(&temp.path().join("shared"), 0o777);
    let middle = shared.join("l2");
    std::os::unix::fs::symlink(private.join("real"), &middle).unwrap();
    let first = private.join("l1");
    std::os::unix::fs::symlink(&middle, &first).unwrap();
    assert_eq!(changeable_by_others(&private.join("real")), None);
    assert_eq!(changeable_by_others(&first), Some(shared.clone()));
    assert_eq!(changeable_past_sticky(&first), Some(shared));
}

#[cfg(unix)]
#[test]
fn a_missing_part_a_link_leads_to_is_checked_where_it_would_sit() {
    let temp = durable_folder();
    let private = shared_folder(&temp.path().join("private"), 0o755);
    let shared = shared_folder(&temp.path().join("shared"), 0o777);
    let leads = private.join("leads");
    std::os::unix::fs::symlink(&shared, &leads).unwrap();
    let dangling = private.join("dangling");
    std::os::unix::fs::symlink(shared.join("gone"), &dangling).unwrap();
    let shared = std::fs::canonicalize(&shared).unwrap();
    for path in [leads.join("missing").join(".."), dangling] {
        assert_eq!(
            changeable_by_others(&path),
            Some(shared.clone()),
            "{path:?}"
        );
        assert_eq!(
            changeable_past_sticky(&path),
            Some(shared.clone()),
            "{path:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_folder_link_mid_path_through_a_shared_folder_is_changeable() {
    let temp = durable_folder();
    let private = temp.path().join("private");
    write_mode(&private.join("real-dir").join("engine"), "engine", 0o755);
    let shared = shared_folder(&temp.path().join("shared"), 0o777);
    let middle = shared.join("d2");
    std::os::unix::fs::symlink(private.join("real-dir"), &middle).unwrap();
    let folder = private.join("dir");
    std::os::unix::fs::symlink(&middle, &folder).unwrap();
    let engine = folder.join("engine");
    assert_eq!(
        changeable_by_others(&private.join("real-dir").join("engine")),
        None
    );
    assert_eq!(changeable_by_others(&engine), Some(shared.clone()));
    assert_eq!(changeable_past_sticky(&engine), Some(shared));
}

#[cfg(unix)]
#[test]
fn a_relative_path_is_changeable_rather_than_a_crash() {
    let relative = Path::new("crazyagent").join("engine");
    assert_eq!(changeable_by_others(&relative), Some(relative.clone()));
    assert_eq!(changeable_past_sticky(&relative), Some(relative));
}

#[cfg(unix)]
#[test]
fn a_link_chain_through_private_folders_only_still_passes() {
    let temp = durable_folder();
    let private = temp.path().join("private");
    write_mode(&private.join("real"), "engine", 0o755);
    let other = shared_folder(&temp.path().join("other"), 0o755);
    let middle = other.join("l2");
    std::os::unix::fs::symlink(private.join("real"), &middle).unwrap();
    let first = private.join("l1");
    std::os::unix::fs::symlink(&middle, &first).unwrap();
    assert_eq!(changeable_by_others(&first), None);
    assert_eq!(changeable_past_sticky(&first), None);
}

#[cfg(unix)]
#[test]
fn a_link_loop_or_a_chain_past_the_hop_cap_is_changeable() {
    let temp = durable_folder();
    let private = temp.path().join("private");
    write_mode(&private.join("real"), "engine", 0o755);
    let (one, two) = (private.join("loop-1"), private.join("loop-2"));
    std::os::unix::fs::symlink(&two, &one).unwrap();
    std::os::unix::fs::symlink(&one, &two).unwrap();
    assert!(changeable_by_others(&one).is_some());
    let mut next = private.join("real");
    for hop in 0..=changeable::LINK_HOPS_MAX {
        let link = private.join(format!("hop-{hop}"));
        std::os::unix::fs::symlink(&next, &link).unwrap();
        next = link;
    }
    assert!(changeable_by_others(&next).is_some());
    assert!(changeable_past_sticky(&next).is_some());
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

/// On Windows both checks read owners and access lists, as
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

/// On Windows the service PATH keeps a folder that exists and drops one that
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

/// The definition writes the executable escaped (XML in a plist, quoted for
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

/// On macOS the daemon group, whose only member is root, is trusted like
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

/// A file a group other than root's can write counts as changeable, like a folder,
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

/// A group's member list leaves out the accounts whose primary group it is, so
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

/// Writes a user lookup configuration into a scratch `etc` folder. Inputs: the folder and
/// the file's text.
#[cfg(unix)]
fn lookup_config(etc: &Path, text: &str) {
    std::fs::write(etc.join("nsswitch.conf"), text).unwrap();
}

/// A default Ubuntu desktop lists SSSD without configuring it, so its local account file
/// still holds every account that can have a primary group; comments and action items on
/// the line are not sources.
#[cfg(unix)]
#[test]
fn an_unconfigured_sssd_leaves_the_account_file_complete() {
    let etc = tempfile::tempdir().unwrap();
    for line in [
        "passwd: files sss systemd",
        "passwd:\tfiles sss",
        "passwd: compat",
        "passwd: files [NOTFOUND=return] systemd # sss is set up later",
        "passwd: files [ NOTFOUND=return ] sss\ngroup: files ldap",
    ] {
        lookup_config(etc.path(), line);
        assert!(
            accounts_complete(etc.path()),
            "{line:?} with no sssd folder"
        );
    }
    let drop_ins = etc.path().join("sssd").join("conf.d");
    std::fs::create_dir_all(&drop_ins).unwrap();
    std::fs::write(drop_ins.join("README"), "").unwrap();
    lookup_config(etc.path(), "passwd: files sss systemd");
    assert!(
        accounts_complete(etc.path()),
        "a drop-in folder with no .conf"
    );
}

/// SSSD with a configuration file or a `.conf` drop-in can serve directory accounts the
/// local account file never lists.
#[cfg(unix)]
#[test]
fn a_configured_sssd_leaves_the_account_file_incomplete() {
    let etc = tempfile::tempdir().unwrap();
    lookup_config(etc.path(), "passwd: files sss systemd");
    let sssd = etc.path().join("sssd");
    std::fs::create_dir_all(sssd.join("conf.d")).unwrap();
    std::fs::write(sssd.join("sssd.conf"), "").unwrap();
    assert!(!accounts_complete(etc.path()), "sssd.conf");
    std::fs::remove_file(sssd.join("sssd.conf")).unwrap();
    std::fs::write(sssd.join("conf.d").join("ad.conf"), "").unwrap();
    assert!(!accounts_complete(etc.path()), "a .conf drop-in");
}

/// An SSSD folder this user cannot look into reads as configured, since a configuration
/// there cannot be ruled out.
#[cfg(unix)]
#[test]
fn an_unreadable_sssd_folder_leaves_the_account_file_incomplete() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return;
    }
    let etc = tempfile::tempdir().unwrap();
    lookup_config(etc.path(), "passwd: files sss systemd");
    let sssd = shared_folder(&etc.path().join("sssd"), 0o000);
    let complete = accounts_complete(etc.path());
    std::fs::set_permissions(&sssd, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!complete);
}

/// Any other account source, a missing `passwd` line or a missing configuration leaves the
/// local account file incomplete.
#[cfg(unix)]
#[test]
fn a_directory_source_or_no_lookup_line_leaves_the_account_file_incomplete() {
    let etc = tempfile::tempdir().unwrap();
    assert!(!accounts_complete(etc.path()), "no nsswitch.conf");
    for line in [
        "passwd: files ldap",
        "passwd: files systemd sss nis",
        "passwd: winbind files",
        "passwd: files altfiles systemd",
        "group: files",
        "",
    ] {
        lookup_config(etc.path(), line);
        assert!(!accounts_complete(etc.path()), "{line:?}");
    }
}

/// The accounts whose primary group is a gid are read from the local account
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

/// The access-list rows: a part an access list opens to others is changeable, a listing is
/// read block by block, a POSIX access list that lets others write counts as shared, and an
/// answer is kept only until its entry changes. Inputs: access-list texts and real folders
/// whose macOS access lists chmod sets. Output: none.
mod access_list;

/// The launchd property list rows: what the plist runs, the environment it gives the
/// engine and how it escapes paths. Inputs: the macOS service from [`service`]. Output:
/// none; each row asserts on the written text.
mod launchd;

/// The Windows link rows: a junction chain through a folder others can change is
/// changeable though both its ends are private, and a link's target walks from a drive root
/// or counts as changeable. Inputs: real junctions made with mklink and literal targets.
/// Output: none.
mod windows_links;

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

/// A link planted at the name a definition used to be staged under is never followed: the
/// file it points at keeps its bytes, and the definition lands.
#[cfg(unix)]
#[test]
fn a_link_at_the_old_staged_name_is_not_followed() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("unit.service");
    let target = temp.path().join("elsewhere");
    std::fs::write(&target, "kept").unwrap();
    std::os::unix::fs::symlink(&target, temp.path().join(".unit.service.staged")).unwrap();

    write_definition(&file, "[Unit]\n", Platform::Linux).unwrap();

    assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[Unit]\n");
}

/// Two definition writes in a row both land, the second replacing the first.
#[test]
fn two_definition_writes_in_a_row_both_land() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("unit.service");

    write_definition(&file, "first\n", Platform::Linux).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\n");
    write_definition(&file, "second\n", Platform::Linux).unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "second\n");
}

/// A definition write that fails, here a rename onto a folder that holds a file, leaves no
/// staged file behind.
#[test]
fn a_failed_definition_write_leaves_no_staged_file() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("unit.service");
    std::fs::create_dir(&file).unwrap();
    std::fs::write(file.join("held"), "held").unwrap();

    assert!(write_definition(&file, "[Unit]\n", Platform::Linux).is_err());

    let left: Vec<String> = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["unit.service"]);
}
