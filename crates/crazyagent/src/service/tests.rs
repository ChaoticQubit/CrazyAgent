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

/// The service PATH keeps the shell's absolute folders in order, then adds the user's and the
/// system folders once each, and leaves out any folder another user can change on this host
/// (on a CI runner `/usr/local/bin` can be one).
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
    assert!(args[3].contains("[Environment]::SetEnvironmentVariable('Path'"));
    assert!(args[3].contains(&format!("$f=$env:{WINDOWS_PATH_FOLDER_VARIABLE};")));
    assert!(!args[3].contains("$args"));
    assert!(!args[3].contains("Cra'zy"));
    assert_eq!(args.len(), 4);
    let envs: Vec<_> = command.get_envs().collect();
    assert_eq!(
        envs,
        [(
            OsStr::new(WINDOWS_PATH_FOLDER_VARIABLE),
            Some(folder.as_os_str())
        )]
    );
}

/// Reads the user's stored PATH through PowerShell. Takes nothing. Returns the value, empty
/// when it is unset.
#[cfg(windows)]
fn user_path() -> String {
    let output = crate::spawn_gate::output(
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.Write([Environment]::GetEnvironmentVariable('Path','User'))",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Stores the user's PATH back as it was, handing the value over in the environment.
/// Takes the value. Returns nothing; panics when PowerShell fails.
#[cfg(windows)]
fn restore_user_path(value: &str) {
    let output = crate::spawn_gate::output(
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Environment]::SetEnvironmentVariable('Path', $env:CRAZYAGENT_TEST_PATH, 'User')",
            ])
            .env("CRAZYAGENT_TEST_PATH", value)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[cfg(windows)]
#[test]
fn link_cli_adds_a_folder_named_like_powershell_code_to_the_user_path_once() {
    let before = user_path();
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("cra'zy;$(exit 7)");
    std::fs::create_dir(&folder).unwrap();
    let executable = folder.join("crazyagent.exe");
    let first = link_cli(&executable, root.path());
    let second = link_cli(&executable, root.path());
    let after = user_path();
    restore_user_path(&before);
    assert_eq!(first.unwrap(), folder);
    assert_eq!(second.unwrap(), folder);
    let shown = folder.to_str().unwrap();
    assert_eq!(after.split(';').filter(|entry| *entry == shown).count(), 1);
    assert_eq!(user_path(), before);
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
fn durable_folder() -> tempfile::TempDir {
    let beside = std::env::current_exe().unwrap();
    let folder = tempfile::tempdir_in(beside.parent().unwrap()).unwrap();
    assert_eq!(changeable_by_others(folder.path()), None);
    folder
}

#[cfg(unix)]
fn write_mode(path: &Path, text: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn shared_folder(path: &Path, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.to_path_buf()
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
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
fn install_refuses_an_engine_other_users_can_change_before_writing_anything() {
    let temp = durable_folder();
    let shared = shared_folder(&temp.path().join(".mount_Ab12Cd"), 0o1777);
    let planted = shared.join("usr").join("bin").join("crazyagent");
    write_mode(&planted, "engine", 0o755);
    let mut linux = service(Platform::Linux);
    linux.executable = planted.clone();
    linux.root = temp.path().join("data").join("engine");
    linux.definition = temp.path().join("units").join(SYSTEMD_UNIT);
    let error = linux.install().unwrap_err();
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
    assert_eq!(persistable(&bundled, &layout, None).unwrap(), bundled);
    let elsewhere = temp.path().join("elsewhere");
    assert_eq!(
        persistable(&bundled, &layout, Some(&elsewhere)).unwrap(),
        bundled
    );
    assert!(!layout.root().exists());
    let copies = layout.root().join("setup").join("bin");
    let installed = persistable(&bundled, &layout, Some(&mount)).unwrap();
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
        persistable(&bundled, &layout, Some(&mount)).unwrap(),
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
    assert!(persistable(&stranger, &layout, Some(&mount)).is_err());
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
    let installed = persistable(&bundled, &layout, Some(&mount)).unwrap();
    let app = installed.parent().unwrap();
    assert_eq!(app, layout.root().join("setup").join("bin"));
    let places = crate::tools::Places::engine(Some(layout.root()), Some(app), None, None);
    let tmux = crate::tools::resolve("tmux", &places).unwrap();
    assert_eq!(tmux.entry, app.join("crazyagent-tmux"));
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
    bundle(&old);
    let cli = temp.path().join("home").join(".local").join("bin");
    std::fs::create_dir_all(&cli).unwrap();
    std::os::unix::fs::symlink(old.join("crazyagent"), cli.join("crazyagent")).unwrap();
    let installed = persistable_with(&bundled, &layout, Some(&mount), Some(&cli)).unwrap();
    assert_eq!(
        installed,
        layout.root().join("setup").join("bin").join("crazyagent")
    );
    assert_eq!(
        std::fs::read_link(cli.join("crazyagent")).unwrap(),
        installed
    );
    for name in crate::update::BINARIES {
        assert!(
            std::fs::symlink_metadata(old.join(name)).is_err(),
            "{name} is retired"
        );
    }
    assert!(old.join("notes.txt").is_file(), "a stranger file stays");
    std::fs::remove_file(old.join("notes.txt")).unwrap();
    persistable_with(&bundled, &layout, Some(&mount), Some(&cli)).unwrap();
    assert!(!old.exists(), "the empty old folder goes");
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
    bundle(&layout.root().join("bin"));
    let other = bundle(&temp.path().join("other"));
    let cli = temp.path().join("home").join(".local").join("bin");
    std::fs::create_dir_all(&cli).unwrap();
    std::os::unix::fs::symlink(&other, cli.join("crazyagent")).unwrap();
    persistable_with(&bundled, &layout, Some(&mount), Some(&cli)).unwrap();
    assert_eq!(std::fs::read_link(cli.join("crazyagent")).unwrap(), other);
}

#[cfg(unix)]
#[test]
fn persistable_never_copies_from_a_folder_other_users_can_change_and_install_refuses_it() {
    let temp = durable_folder();
    let shared = shared_folder(&temp.path().join("shared"), 0o777);
    let bundled = bundle(&shared);
    let layout = Layout::new(temp.path().join("data").join("engine")).unwrap();
    assert_eq!(persistable(&bundled, &layout, None).unwrap(), bundled);
    assert!(!layout.root().exists());
    let mut linux = service(Platform::Linux);
    linux.executable = bundled.clone();
    linux.root = layout.root().to_path_buf();
    linux.definition = temp.path().join("units").join(SYSTEMD_UNIT);
    assert_eq!(linux.install().unwrap_err().0, refusal(&bundled, &shared));
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
    let copy = persistable(&bundled, &layout, Some(&mount)).unwrap();
    let link = link_cli(&copy, &bin).unwrap();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        layout.root().join("bin").join("crazyagent")
    );
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
    assert!(own.may_replace());
    let mut plain = own.clone();
    plain.executable = temp.path().join("bin").join("crazyagent");
    assert!(!plain.may_replace());
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
    assert!(!dev.may_replace());
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
    assert!(!own.may_replace());
    info("0.0.1");
    assert!(own.may_replace());
    std::fs::write(
        own.root.join("owner-current"),
        br#"{"engine_version":"99.0.0"}"#,
    )
    .unwrap();
    assert!(!own.may_replace());
    std::fs::write(
        own.root.join("owner-current"),
        br#"{"engine_version":"0.0.1"}"#,
    )
    .unwrap();
    assert!(own.may_replace());
}

#[test]
fn stale_replaces_a_definition_only_for_a_private_copy_of_the_bundled_engine() {
    let temp = tempfile::tempdir().unwrap();
    let bundled = temp.path().join("CrazyAgent.app").join("crazyagent");
    let copy = temp.path().join("engine").join("bin").join("crazyagent");
    for platform in [Platform::MacOs, Platform::Linux] {
        let mut installed = service(platform);
        installed.root = temp.path().join("engine");
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

/// The ownership rule on synthetic parts, the entry first (leader 17:12, L8c): a /tmp-like
/// folder (root 1777) above this user's 0700 folder passes for the tools the resolver runs
/// and stays changeable for the engine service; a part another user owns below it is
/// refused; a /nix/store-like folder (root:nixbld 1775) above 0555 packages passes for
/// both; a missing part below a store, or a store as the entry's own folder, is refused.
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
