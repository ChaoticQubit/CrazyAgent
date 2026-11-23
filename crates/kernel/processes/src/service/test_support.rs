use super::*;

/// The engine service of one platform for the owner on a Mac, at fixed paths, for rows that
/// read its definition and commands. Input: the platform. Output: the service.
pub fn service(platform: Platform) -> Service {
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
        home: Some(PathBuf::from("/Users/owner")),
        definition: PathBuf::from("/Users/owner/Library/LaunchAgents/dev.crazyagent.engine.plist"),
        user: None,
    }
}

/// The commands as plain strings, for rows that compare them. Input: the commands.
/// Output: each command as its words.
pub fn texts(commands: Vec<Vec<OsString>>) -> Vec<Vec<String>> {
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

/// One command as owned words. Input: the words. Output: the command.
pub fn words(command: &[&str]) -> Vec<String> {
    command.iter().map(|word| (*word).to_owned()).collect()
}

/// The service name of the default data root. Input: the platform. Output: the name.
pub fn standard(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => LAUNCHD_LABEL,
        Platform::Linux => SYSTEMD_UNIT,
        Platform::Windows => WINDOWS_TASK,
    }
}

/// The 12 lowercase hex digits that tell one data root's service name apart, asserted to be
/// there. Inputs: the name and the platform. Output: the digits.
pub fn short_hash(name: &str, platform: Platform) -> String {
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

/// A scratch folder beside the test binary that no other user can change. Output: the
/// folder, removed when dropped.
#[cfg(unix)]
pub fn durable_folder() -> tempfile::TempDir {
    let beside = std::env::current_exe().unwrap();
    let folder = tempfile::tempdir_in(beside.parent().unwrap()).unwrap();
    assert_eq!(changeable_by_others(folder.path()), None);
    folder
}

/// Writes a file with a mode, making its folders. Inputs: the path, the text and the mode.
#[cfg(unix)]
pub fn write_mode(path: &Path, text: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Makes a folder with a mode. Inputs: the path and the mode. Output: the folder.
#[cfg(unix)]
pub fn shared_folder(path: &Path, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.to_path_buf()
}

/// The permission bits of an entry, without following a link. Input: the path. Output: the
/// bits.
#[cfg(unix)]
pub fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

/// An app engine `<root>/<folder>/CrazyAgent.app/Contents/MacOS/crazyagent` written on disk.
/// Inputs: the row's folder and the folder the app sits in. Output: the engine's path.
#[cfg(unix)]
pub fn app_engine(root: &Path, folder: &str) -> PathBuf {
    let engine = root
        .join(folder)
        .join("CrazyAgent.app")
        .join("Contents")
        .join("MacOS")
        .join("crazyagent");
    std::fs::create_dir_all(engine.parent().unwrap()).unwrap();
    std::fs::write(&engine, b"engine").unwrap();
    engine
}

/// The service with a launchd label or unit name of its own, unique to this run and
/// moment, so a row that reaches the service manager never meets the owner's engine.
/// Inputs: the service and a tag. Output: the relabelled service.
#[cfg(unix)]
pub fn labelled(mut service: Service, tag: &str) -> Service {
    service.name = format!(
        "dev.crazyagent.test.{tag}.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    service
}
