use super::*;

fn service(platform: Platform) -> Service {
    Service {
        platform,
        name: match platform {
            Platform::MacOs => LAUNCHD_LABEL,
            Platform::Linux => SYSTEMD_UNIT,
            Platform::Windows => WINDOWS_TASK,
        }
        .to_owned(),
        executable: PathBuf::from("/Applications/CrazyAgent.app/Contents/MacOS/crazyagent"),
        root: PathBuf::from("/Users/owner/Library/Application Support/CrazyAgent/engine"),
        path: OsString::from("/Users/owner/.local/bin:/opt/homebrew/bin:/usr/bin:/bin"),
        definition: PathBuf::from("/Users/owner/Library/LaunchAgents/dev.crazyagent.engine.plist"),
        user: None,
    }
}

fn texts(commands: Vec<Vec<OsString>>) -> Vec<Vec<String>> {
    commands
        .into_iter()
        .map(|command| {
            command
                .into_iter()
                .map(|part| part.into_string().unwrap())
                .collect()
        })
        .collect()
}

fn words(command: &[&str]) -> Vec<String> {
    command.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn launchd_plist_runs_the_recorded_executable_with_logs_and_restart() {
    let expected = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
\t<key>Label</key>
\t<string>dev.crazyagent.engine</string>
\t<key>ProgramArguments</key>
\t<array>
\t\t<string>/Applications/CrazyAgent.app/Contents/MacOS/crazyagent</string>
\t\t<string>engine</string>
\t\t<string>--home</string>
\t\t<string>/Users/owner/Library/Application Support/CrazyAgent/engine</string>
\t</array>
\t<key>EnvironmentVariables</key>
\t<dict>
\t\t<key>PATH</key>
\t\t<string>/Users/owner/.local/bin:/opt/homebrew/bin:/usr/bin:/bin</string>
\t</dict>
\t<key>RunAtLoad</key>
\t<true/>
\t<key>KeepAlive</key>
\t<dict>
\t\t<key>SuccessfulExit</key>
\t\t<false/>
\t</dict>
\t<key>ExitTimeOut</key>
\t<integer>30</integer>
\t<key>StandardOutPath</key>
\t<string>/Users/owner/Library/Application Support/CrazyAgent/engine/logs/engine.log</string>
\t<key>StandardErrorPath</key>
\t<string>/Users/owner/Library/Application Support/CrazyAgent/engine/logs/engine.log</string>
</dict>
</plist>
";
    assert_eq!(
        service(Platform::MacOs).definition_text().unwrap(),
        expected
    );
}

#[test]
fn launchd_plist_escapes_markup_in_paths() {
    let mut odd = service(Platform::MacOs);
    odd.root = PathBuf::from("/Users/o&<w>\"'/engine");
    let text = odd.definition_text().unwrap();
    assert!(text.contains("<string>/Users/o&amp;&lt;w&gt;&quot;&apos;/engine</string>"));
    assert!(!text.contains("o&<"));
}

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

#[test]
fn service_path_keeps_the_shell_path_then_adds_user_and_system_folders_once() {
    let path = service_path(
        Some(OsStr::new(
            "/opt/homebrew/bin:relative:/Users/owner/bin:/usr/bin",
        )),
        Some(Path::new("/Users/owner")),
    );
    assert_eq!(
        path,
        OsString::from(
            "/opt/homebrew/bin:/Users/owner/bin:/usr/bin:/Users/owner/.local/bin:/usr/local/bin:/bin:/usr/sbin:/sbin"
        )
    );
    assert_eq!(
        service_path(None, None),
        OsString::from("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin")
    );
}

#[test]
fn windows_path_command_passes_the_folder_as_an_argument() {
    let command = windows_path_command(Path::new("/c/Program Files/CrazyAgent"));
    let command: Vec<_> = command
        .into_iter()
        .map(|part| part.into_string().unwrap())
        .collect();
    assert_eq!(
        command[..4],
        [
            "powershell.exe",
            "-NoProfile",
            "-NonInteractive",
            "-Command"
        ]
    );
    assert!(command[4].contains("[Environment]::SetEnvironmentVariable('Path'"));
    assert!(command[4].contains("$args[0]"));
    assert!(!command[4].contains("CrazyAgent"));
    assert_eq!(command[5], "/c/Program Files/CrazyAgent");
    assert_eq!(command.len(), 6);
}

#[cfg(unix)]
#[test]
fn link_cli_links_into_the_bin_folder_and_refuses_a_real_file() {
    let temp = tempfile::tempdir().unwrap();
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

fn standard(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => LAUNCHD_LABEL,
        Platform::Linux => SYSTEMD_UNIT,
        Platform::Windows => WINDOWS_TASK,
    }
}

fn short_hash(name: &str, platform: Platform) -> String {
    let hash = match platform {
        Platform::MacOs => name.strip_prefix("dev.crazyagent.engine."),
        Platform::Linux => name
            .strip_prefix("crazyagent-engine-")
            .and_then(|rest| rest.strip_suffix(".service")),
        Platform::Windows => name.strip_prefix("CrazyAgent Engine "),
    }
    .unwrap_or_else(|| panic!("{name} is not a per-root {platform:?} service name"));
    assert_eq!(hash.len(), 12, "{name}");
    assert!(
        hash.bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{name}"
    );
    hash.to_owned()
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

#[test]
fn the_current_user_service_follows_the_data_root() {
    let platform = Platform::CURRENT;
    let environment = crate::paths::RootEnvironment {
        home_override: None,
        ..crate::paths::RootEnvironment::current()
    };
    let executable = PathBuf::from("/opt/crazyagent/crazyagent");
    let default = Layout::new(crate::paths::data_root(platform, &environment).unwrap()).unwrap();
    let service = Service::current_user(platform, executable.clone(), &default).unwrap();
    assert_eq!(service.name, standard(platform));
    let temp = tempfile::tempdir().unwrap();
    let other = Service::current_user(
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
