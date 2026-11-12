use super::keys::{ShiftArrows, ShiftEnter};
use super::theme::Colors;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const PREFS_BYTES_MAX: u64 = 64;

/// Where the TUI keeps its Colors choice on this computer.
/// Input: the home folder. Output: `~/Library/Application Support/CrazyAgent/tui-colors`
/// on macOS, `$XDG_CONFIG_HOME/crazyagent/tui-colors` or `~/.config/crazyagent/tui-colors`
/// on Linux, `%APPDATA%\CrazyAgent\tui-colors` on Windows; None without a usable folder.
pub fn default_path(home: Option<&str>) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let _ = home;
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from)?;
        appdata
            .is_absolute()
            .then(|| appdata.join("CrazyAgent").join("tui-colors"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = home.filter(|home| home.starts_with('/'))?;
        Some(unix_path(
            Path::new(home),
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        ))
    }
}

#[cfg(not(target_os = "windows"))]
fn unix_path(home: &Path, xdg_config: Option<&std::ffi::OsStr>) -> PathBuf {
    if cfg!(target_os = "macos") {
        return home
            .join("Library")
            .join("Application Support")
            .join("CrazyAgent")
            .join("tui-colors");
    }
    let config = xdg_config
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".config"));
    config.join("crazyagent").join("tui-colors")
}

fn word(colors: Colors) -> &'static str {
    match colors {
        Colors::FollowMacos => "follow-macos",
        Colors::Dark => "dark",
        Colors::Light => "light",
        Colors::Terminal => "terminal",
    }
}

/// Reads the saved Colors choice.
/// Input: the file path. Output: the choice; Follow macOS when the file is
/// missing, too large or unreadable.
pub fn load_colors(path: &Path) -> Colors {
    let text = read_word(path).unwrap_or_default();
    Colors::ALL
        .iter()
        .map(|(choice, _)| *choice)
        .find(|choice| word(*choice) == text)
        .unwrap_or_default()
}

fn read_word(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > PREFS_BYTES_MAX {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}

fn write_word(path: &Path, word: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("prefs_path_invalid")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("prefs_path_invalid")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".{name}.new"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(word.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn shift_word(choice: ShiftArrows) -> &'static str {
    match choice {
        ShiftArrows::SwitchTabs => "switch-tabs",
        ShiftArrows::SendToAgent => "send-to-agent",
    }
}

/// Where the TUI keeps its Shift-←→ choice for the master terminal: beside the Colors file.
/// Input: the Colors file path. Output: `tui-shift-arrows` in the same folder.
pub fn shift_arrows_path(colors: &Path) -> PathBuf {
    colors.with_file_name("tui-shift-arrows")
}

/// Reads the saved Shift-←→ choice.
/// Input: the file path. Output: the choice; Switch tabs when the file is missing, too
/// large or unreadable.
pub fn load_shift_arrows(path: &Path) -> ShiftArrows {
    let text = read_word(path).unwrap_or_default();
    ShiftArrows::ALL
        .iter()
        .map(|(choice, _)| *choice)
        .find(|choice| shift_word(*choice) == text)
        .unwrap_or_default()
}

/// Saves the Shift-←→ choice, replacing the file atomically.
/// Inputs: the file path and the choice. Output: Ok, or the I/O error text.
pub fn save_shift_arrows(path: &Path, choice: ShiftArrows) -> Result<(), String> {
    write_word(path, shift_word(choice))
}

/// Where the TUI keeps its Shift-Enter choice: beside the Colors file.
/// Input: the Colors file path. Output: `tui-shift-enter` in the same folder.
pub fn shift_enter_path(colors: &Path) -> PathBuf {
    colors.with_file_name("tui-shift-enter")
}

/// Reads the saved Shift-Enter choice.
/// Input: the file path. Output: the choice; Auto when the file is missing, too large or
/// unreadable.
pub fn load_shift_enter(path: &Path) -> ShiftEnter {
    let text = read_word(path).unwrap_or_default();
    ShiftEnter::ALL
        .iter()
        .map(|(choice, _)| *choice)
        .find(|choice| enter_word(*choice) == text)
        .unwrap_or_default()
}

fn enter_word(choice: ShiftEnter) -> &'static str {
    match choice {
        ShiftEnter::Auto => "auto",
        ShiftEnter::Off => "off",
    }
}

/// Saves the Shift-Enter choice, replacing the file atomically.
/// Inputs: the file path and the choice. Output: Ok, or the I/O error text.
pub fn save_shift_enter(path: &Path, choice: ShiftEnter) -> Result<(), String> {
    write_word(path, enter_word(choice))
}

/// Saves the Colors choice, replacing the file atomically.
/// Inputs: the file path and the choice. Output: Ok, or the I/O error text.
pub fn save_colors(path: &Path, colors: Colors) -> Result<(), String> {
    write_word(path, word(colors))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_arrows_round_trip_beside_the_colors_file_and_bad_files_fall_back() {
        let folder = tempfile::tempdir().unwrap();
        let colors = folder.path().join("nested").join("tui-colors");
        let path = shift_arrows_path(&colors);
        assert_eq!(path, folder.path().join("nested").join("tui-shift-arrows"));
        assert_eq!(load_shift_arrows(&path), ShiftArrows::SwitchTabs);
        for (choice, _) in ShiftArrows::ALL {
            save_shift_arrows(&path, choice).unwrap();
            assert_eq!(load_shift_arrows(&path), choice);
        }
        save_colors(&colors, Colors::Light).unwrap();
        assert_eq!(load_colors(&colors), Colors::Light);
        assert_eq!(
            load_shift_arrows(&path),
            ShiftArrows::SendToAgent,
            "each choice keeps its own file"
        );
        std::fs::write(&path, "sideways").unwrap();
        assert_eq!(load_shift_arrows(&path), ShiftArrows::SwitchTabs);
        std::fs::write(&path, "x".repeat(100)).unwrap();
        assert_eq!(load_shift_arrows(&path), ShiftArrows::SwitchTabs);
    }

    #[test]
    fn shift_enter_round_trips_beside_the_colors_file_and_bad_files_fall_back_to_auto() {
        let folder = tempfile::tempdir().unwrap();
        let colors = folder.path().join("nested").join("tui-colors");
        let path = shift_enter_path(&colors);
        assert_eq!(path, folder.path().join("nested").join("tui-shift-enter"));
        assert_eq!(load_shift_enter(&path), ShiftEnter::Auto);
        for (choice, _) in ShiftEnter::ALL {
            save_shift_enter(&path, choice).unwrap();
            assert_eq!(load_shift_enter(&path), choice);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "off");
        save_shift_arrows(&shift_arrows_path(&colors), ShiftArrows::SendToAgent).unwrap();
        assert_eq!(
            load_shift_enter(&path),
            ShiftEnter::Off,
            "each choice keeps its own file"
        );
        std::fs::write(&path, "sometimes").unwrap();
        assert_eq!(load_shift_enter(&path), ShiftEnter::Auto);
        std::fs::write(&path, "x".repeat(100)).unwrap();
        assert_eq!(load_shift_enter(&path), ShiftEnter::Auto);
    }

    #[test]
    fn colors_round_trip_and_bad_files_fall_back() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("nested").join("tui-colors");
        assert_eq!(load_colors(&path), Colors::FollowMacos);
        for (choice, _) in Colors::ALL {
            save_colors(&path, choice).unwrap();
            assert_eq!(load_colors(&path), choice);
        }
        std::fs::write(&path, "purple").unwrap();
        assert_eq!(load_colors(&path), Colors::FollowMacos);
        std::fs::write(&path, "x".repeat(100)).unwrap();
        assert_eq!(load_colors(&path), Colors::FollowMacos);
        #[cfg(target_os = "macos")]
        assert_eq!(
            default_path(Some("/Users/mira")).unwrap(),
            Path::new("/Users/mira/Library/Application Support/CrazyAgent/tui-colors")
        );
        #[cfg(target_os = "linux")]
        {
            let home = Path::new("/home/mira");
            assert_eq!(
                unix_path(home, None),
                Path::new("/home/mira/.config/crazyagent/tui-colors")
            );
            assert_eq!(
                unix_path(home, Some(std::ffi::OsStr::new("/srv/config"))),
                Path::new("/srv/config/crazyagent/tui-colors")
            );
            assert_eq!(
                unix_path(home, Some(std::ffi::OsStr::new("relative"))),
                Path::new("/home/mira/.config/crazyagent/tui-colors")
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(default_path(Some("relative")), None);
            assert_eq!(default_path(None), None);
        }
        #[cfg(target_os = "windows")]
        if let Some(path) = default_path(None) {
            assert!(path.ends_with(r"CrazyAgent\tui-colors"));
        }
    }
}
