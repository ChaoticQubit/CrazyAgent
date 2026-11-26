use std::path::PathBuf;

/// Tells whether a running program's file lies in one of the folders or is one of the files,
/// read from a snapshot of every process on the computer. Input: the folders and files.
/// Output: whether one runs, or the Windows error code when the snapshot cannot be read.
pub fn image_runs_under(folders: &[PathBuf]) -> Result<bool, u32> {
    let _ = folders;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::image_runs_under;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// The real spelling of a folder as the engine passes it: long names, without the
    /// verbatim prefix. Input: the folder. Output: the spelling.
    fn plain(folder: &std::path::Path) -> PathBuf {
        let real = std::fs::canonicalize(folder).unwrap();
        let text = real.to_str().unwrap();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
    }

    /// A program started from a folder outside any runtime, held suspended so it never needs
    /// its language files, runs under that folder and as that file, not under another
    /// folder, and under none once it has ended.
    #[test]
    fn a_program_started_from_a_folder_runs_under_it_until_it_ends() {
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let made = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let folder = plain(made.path());
        let copy = folder.join("PING.EXE");
        std::fs::copy(system.join("PING.EXE"), &copy).unwrap();
        let mut child = Command::new(&copy)
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let here = [folder.clone()];
        assert_eq!(image_runs_under(&here), Ok(true));
        assert_eq!(image_runs_under(std::slice::from_ref(&copy)), Ok(true));
        assert_eq!(image_runs_under(&[plain(other.path())]), Ok(false));
        child.kill().unwrap();
        child.wait().unwrap();
        drop(child);
        assert_eq!(image_runs_under(&here), Ok(false));
    }
}
