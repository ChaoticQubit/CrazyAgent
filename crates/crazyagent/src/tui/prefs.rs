use super::theme::Colors;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const PREFS_BYTES_MAX: u64 = 64;

/// Where the TUI keeps its Colors choice on this Mac.
/// Input: the home folder. Output: `~/Library/Application Support/CrazyAgent/tui-colors`,
/// or None without a home folder.
pub fn default_path(home: Option<&str>) -> Option<PathBuf> {
    let home = home.filter(|home| home.starts_with('/'))?;
    Some(
        Path::new(home)
            .join("Library")
            .join("Application Support")
            .join("CrazyAgent")
            .join("tui-colors"),
    )
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
    let Ok(metadata) = std::fs::metadata(path) else {
        return Colors::default();
    };
    if !metadata.is_file() || metadata.len() > PREFS_BYTES_MAX {
        return Colors::default();
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    Colors::ALL
        .iter()
        .map(|(choice, _)| *choice)
        .find(|choice| word(*choice) == text.trim())
        .unwrap_or_default()
}

/// Saves the Colors choice, replacing the file atomically.
/// Inputs: the file path and the choice. Output: Ok, or the I/O error text.
pub fn save_colors(path: &Path, colors: Colors) -> Result<(), String> {
    let parent = path.parent().ok_or("prefs_path_invalid")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(".tui-colors.new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(word(colors).as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            default_path(Some("/Users/mira")).unwrap(),
            Path::new("/Users/mira/Library/Application Support/CrazyAgent/tui-colors")
        );
        assert_eq!(default_path(Some("relative")), None);
        assert_eq!(default_path(None), None);
    }
}
