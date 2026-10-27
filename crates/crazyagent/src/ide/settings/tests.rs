use super::*;
use serde_json::json;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    folder: PathBuf,
    server_data: PathBuf,
}

impl Case {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("ca-ide-lock-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let ide = root.join("data").join("ide");
        std::fs::create_dir_all(&ide).unwrap();
        for folder in [root.join("data"), ide.clone()] {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let folder = ide.join("alpha");
        let server_data = folder.join("server");
        Self {
            _temp: temp,
            root,
            folder,
            server_data,
        }
    }

    fn machine(&self) -> PathBuf {
        self.server_data
            .join("data")
            .join("Machine")
            .join("settings.json")
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn expected_locks() -> Value {
    json!({
        "chat.disableAIFeatures": true,
        "chat.agent.enabled": false,
        "chat.commandCenter.enabled": false,
        "chat.mcp.gallery.enabled": false,
        "chat.mcp.access": "none",
        "chat.extensionTools.enabled": false,
        "window.commandCenter": false,
        "terminal.integrated.initialHint": false,
        "workbench.tips.enabled": false,
        "workbench.startupEditor": "none",
        "workbench.activity.showAccounts": false,
        "workbench.welcomePage.extraAnnouncements": false,
        "workbench.welcomePage.walkthroughs.openOnInstall": false,
        "workbench.remoteIndicator.showExtensionRecommendations": false,
        "workbench.settings.showAISearchToggle": false,
        "search.searchView.semanticSearchBehavior": "manual",
        "extensions.ignoreRecommendations": true,
        "telemetry.telemetryLevel": "off",
        "update.mode": "none",
        "extensions.autoUpdate": false,
        "extensions.autoCheckUpdates": false,
        "task.allowAutomaticTasks": "off",
    })
}

#[test]
fn written_remote_settings_hold_every_locked_key() {
    let case = Case::new();
    let written = write(&case.folder, &case.server_data).unwrap();
    assert_eq!(written.machine, case.machine());
    assert_eq!(written.locked, case.folder.join(LOCKED_FILE));

    let machine = read(&written.machine);
    assert_eq!(
        machine,
        expected_locks(),
        "a fresh Remote settings file holds the locks and nothing else"
    );
    assert_eq!(machine["chat.disableAIFeatures"], json!(true));
    assert_eq!(read(&written.locked), expected_locks());
    assert_eq!(Value::Object(locked()), expected_locks());
    for key in THEME_KEYS {
        assert!(machine.get(key).is_none(), "{key} is never locked");
        assert!(!locked().contains_key(key), "{key} is never locked");
    }
    for path in [&written.machine, &written.locked] {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o7777, 0o600, "{}", path.display());
    }
    for folder in [
        case.folder.clone(),
        case.server_data.clone(),
        case.server_data.join("data"),
        case.server_data.join("data").join("Machine"),
    ] {
        let metadata = std::fs::symlink_metadata(&folder).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.mode() & 0o7777, 0o700, "{}", folder.display());
    }
    assert!(
        std::fs::read_dir(&case.folder)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")),
        "no temporary file is left behind"
    );

    let before = std::fs::symlink_metadata(&written.machine).unwrap().ino();
    let again = write(&case.folder, &case.server_data).unwrap();
    assert_eq!(read(&again.machine), expected_locks());
    assert_ne!(
        std::fs::symlink_metadata(&again.machine).unwrap().ino(),
        before,
        "each start writes a new file, so a link to the old one changes nothing"
    );
}

#[test]
fn owner_settings_outside_the_locked_keys_are_kept() {
    let case = Case::new();
    write(&case.folder, &case.server_data).unwrap();
    std::fs::write(
        case.machine(),
        r#"{"editor.fontSize": 15, "workbench.preferredDarkColorTheme": "Solarized Dark", "chat.disableAIFeatures": false, "update.mode": "start"}"#,
    )
    .unwrap();

    let written = write(&case.folder, &case.server_data).unwrap();
    let machine = read(&written.machine);
    assert_eq!(machine["editor.fontSize"], json!(15));
    assert_eq!(
        machine["workbench.preferredDarkColorTheme"],
        json!("Solarized Dark"),
        "a theme the owner chose stays"
    );
    assert_eq!(machine["chat.disableAIFeatures"], json!(true));
    assert_eq!(machine["update.mode"], json!("none"));
    let mut expected = expected_locks();
    expected["editor.fontSize"] = json!(15);
    expected["workbench.preferredDarkColorTheme"] = json!("Solarized Dark");
    assert_eq!(machine, expected);
    assert_eq!(
        read(&written.locked),
        expected_locks(),
        "owner keys never join the lock"
    );

    let unreadable = "// my notes\n{\"editor.fontSize\": 16,}\n";
    std::fs::write(case.machine(), unreadable).unwrap();
    let written = write(&case.folder, &case.server_data).unwrap();
    assert_eq!(read(&written.machine), expected_locks());
    assert_eq!(
        std::fs::read_to_string(case.machine().with_file_name(UNREADABLE)).unwrap(),
        unreadable,
        "a file the engine cannot read is kept aside, never dropped"
    );

    let elsewhere = case.root.join("elsewhere.json");
    std::fs::write(&elsewhere, r#"{"editor.fontSize": 18}"#).unwrap();
    std::fs::remove_file(case.machine()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, case.machine()).unwrap();
    let written = write(&case.folder, &case.server_data).unwrap();
    assert!(
        std::fs::symlink_metadata(&written.machine)
            .unwrap()
            .is_file()
    );
    assert_eq!(
        read(&written.machine),
        expected_locks(),
        "a link is replaced, never followed"
    );
    assert_eq!(
        std::fs::read_to_string(&elsewhere).unwrap(),
        r#"{"editor.fontSize": 18}"#
    );
}

#[test]
fn the_lock_layers_agree_and_leave_the_theme_free() {
    let all = locked();
    for (key, value) in machine()
        .into_iter()
        .chain(workspace())
        .chain(application())
    {
        assert_eq!(all.get(&key), Some(&value), "{key} has one locked value");
    }
    assert_eq!(
        Value::Object(workspace()),
        json!({
            "chat.disableAIFeatures": true,
            "chat.mcp.access": "none",
            "chat.extensionTools.enabled": false,
            "workbench.settings.showAISearchToggle": false,
            "extensions.ignoreRecommendations": true,
            "workbench.startupEditor": "none",
        })
    );
    let defaults = defaults();
    for (key, value) in &all {
        assert_eq!(
            defaults.get(key),
            Some(value),
            "{key} holds from first render"
        );
    }
    assert_eq!(defaults["window.autoDetectColorScheme"], json!(true));
    assert_eq!(
        defaults["workbench.preferredDarkColorTheme"],
        json!("CrazyAgent Dark")
    );
    assert_eq!(
        defaults["workbench.preferredLightColorTheme"],
        json!("CrazyAgent Light")
    );
    assert_eq!(defaults.len(), all.len() + THEME_KEYS.len());
}

#[test]
fn settings_outside_the_ide_folder_are_refused() {
    let case = Case::new();
    for server_data in [
        case.folder.clone(),
        case.root.join("server"),
        PathBuf::from("server"),
        case.folder.join("..").join("server"),
    ] {
        assert_eq!(
            write(&case.folder, &server_data).unwrap_err().0,
            "ide_settings_invalid",
            "{}",
            server_data.display()
        );
    }
    assert!(!case.root.join("server").exists());
}

/// Runs a shell command under the IDE server's real profile, built and installed by
/// [`super::super::sandbox`] with the server data folder as its home and the Remote settings
/// folder ([`machine_folder`]) locked, as the server starts. Returns whether it succeeded.
#[cfg(target_os = "macos")]
fn sandboxed(case: &Case, script: &str) -> bool {
    use super::super::sandbox::{Grants, command, install};
    let data = case.root.join("data");
    let project = case.root.join("project");
    let build = data.join("ide").join("build");
    for folder in [&project, &build] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let tmp = crate::sandbox::toolchains::make_tmp(&case.server_data).unwrap();
    let locked = case.folder.join(LOCKED_FILE);
    let machine = machine_folder(&case.server_data);
    let grants = Grants {
        data: &data,
        root: &project,
        build: &build,
        home: &case.server_data,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[locked.as_path()],
        locked: &[machine.as_path()],
    };
    let profile = install(&case.folder, &grants, 45_733).unwrap();
    let mut shell = command(&profile, Path::new("/bin/sh"));
    shell
        .args(["-c", script])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let ran = crate::spawn_gate::status(&mut shell).unwrap().success();
    crate::sandbox::toolchains::remove_tmp(&case.server_data).unwrap();
    ran
}

#[cfg(target_os = "macos")]
#[test]
fn the_server_can_never_change_replace_or_remove_its_remote_settings() {
    let case = Case::new();
    let written = write(&case.folder, &case.server_data).unwrap();
    let file = written.machine.display().to_string();
    let machine = case.server_data.join("data").join("Machine");
    let data = case.server_data.join("data");
    let (machine, data) = (machine.display(), data.display());
    let home = case.server_data.display();
    let project = case.root.join("project");
    let project = project.display();
    let off = r#"{"chat.disableAIFeatures": false}"#;
    for attempt in [
        format!("printf '%s' '{off}' > '{file}'"),
        format!("chmod 644 '{file}'"),
        format!("touch '{file}'"),
        format!("rm '{file}'"),
        format!("mv '{file}' '{machine}/old.json'"),
        format!("mv '{machine}' '{data}/Machine.old'"),
        format!("mv '{data}' '{home}/data.old'"),
        format!("rmdir '{machine}'"),
        format!("ln '{file}' '{home}/linked.json'"),
        format!("printf '%s' '{off}' > '{home}/new.json' && mv '{home}/new.json' '{file}'"),
        format!(
            "ln -s '{home}/new.json' '{machine}/link.json' && mv '{machine}/link.json' '{file}'"
        ),
        format!("printf '{{}}' > '{machine}/mcp.json'"),
        format!("mkdir '{machine}/User'"),
        format!("printf '%s' '{off}' > '{data}/MACHINE/Settings.json'"),
        format!("mv '{data}' '{project}/data-aside'"),
        format!("mv '{home}' '{project}/server-aside'"),
    ] {
        assert!(
            !sandboxed(&case, &attempt),
            "the server could run: {attempt}"
        );
        assert_eq!(read(&written.machine), expected_locks(), "after: {attempt}");
    }
    assert!(!case.server_data.join("linked.json").exists());
    assert!(
        sandboxed(
            &case,
            &format!(
                "mkdir -p '{data}/User' && printf '{{}}' > '{data}/User/settings.json' && printf '{{}}' > '{data}/logs.txt'"
            )
        ),
        "the server still writes the rest of its data folder"
    );
    assert_eq!(read(&written.machine), expected_locks());
    assert!(case.server_data.join("data").join("Machine").is_dir());
    assert_eq!(
        std::fs::read_dir(case.server_data.join("data").join("Machine"))
            .unwrap()
            .count(),
        1,
        "nothing was added next to the engine's settings"
    );
}
