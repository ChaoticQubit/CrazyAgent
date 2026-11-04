use super::picker::FolderPurpose;
use super::{BridgeError, failure};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// The variable that names the folder a native-webdriver build takes its next folder pick
/// from. The trial runner sets it on the app it launches.
pub const PICKS_VARIABLE: &str = "CRAZYAGENT_TRIAL_PICKS";

/// The largest answer file read, far above any folder path.
pub const PICK_BYTES_MAX: u64 = 4096;

/// Takes the trial's waiting answer to this purpose's folder chooser from the folder that
/// [`PICKS_VARIABLE`] names, so a WebDriver run, which cannot drive the macOS chooser, picks
/// a folder the owner could pick. Input: the purpose. Output: as [`take_from`].
pub fn take(purpose: FolderPurpose) -> Result<Option<String>, BridgeError> {
    take_from(std::env::var_os(PICKS_VARIABLE).as_deref(), purpose)
}

/// Takes the answer file named after the purpose from the picks folder and removes it, so
/// each answer is used once. The folder must be an absolute private directory and the file a
/// private regular file of at most [`PICK_BYTES_MAX`] bytes, not a link, holding one absolute
/// path to an existing folder. Inputs: the picks folder, if any, and the purpose. Output: the
/// folder; none when no folder is named or no answer waits, so the real chooser opens; or
/// `trial_pick_invalid` when the folder or the answer is unsafe, so no chooser opens.
pub fn take_from(
    folder: Option<&OsStr>,
    purpose: FolderPurpose,
) -> Result<Option<String>, BridgeError> {
    let Some(folder) = folder.map(Path::new) else {
        return Ok(None);
    };
    let invalid = || failure("trial_pick_invalid");
    let place = std::fs::symlink_metadata(folder).map_err(|_| invalid())?;
    if !folder.is_absolute() || !place.is_dir() || place.permissions().mode() & 0o077 != 0 {
        return Err(invalid());
    }
    let path = folder.join(name(purpose));
    let seen = match std::fs::symlink_metadata(&path) {
        Ok(seen) => seen,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(invalid()),
    };
    if !seen.is_file() || seen.len() > PICK_BYTES_MAX || seen.permissions().mode() & 0o077 != 0 {
        return Err(invalid());
    }
    let file = File::open(&path).map_err(|_| invalid())?;
    let opened = file.metadata().map_err(|_| invalid())?;
    if (opened.dev(), opened.ino()) != (seen.dev(), seen.ino()) {
        return Err(invalid());
    }
    let answer = read_answer(&file);
    std::fs::remove_file(&path).map_err(|_| invalid())?;
    let answer = answer?;
    let picked = answer.strip_suffix('\n').unwrap_or(&answer);
    if !Path::new(picked).is_absolute()
        || picked.chars().any(char::is_control)
        || !Path::new(picked).is_dir()
    {
        return Err(invalid());
    }
    Ok(Some(picked.to_owned()))
}

/// Reads an opened answer file, which may have grown since its size was checked. Input: the
/// file. Output: its text, or `trial_pick_invalid` when it is not UTF-8 or holds more than
/// [`PICK_BYTES_MAX`] bytes (ND6-L1).
fn read_answer(file: &File) -> Result<String, BridgeError> {
    let mut answer = String::new();
    file.take(PICK_BYTES_MAX + 1)
        .read_to_string(&mut answer)
        .map_err(|_| failure("trial_pick_invalid"))?;
    if answer.len() as u64 > PICK_BYTES_MAX {
        return Err(failure("trial_pick_invalid"));
    }
    Ok(answer)
}

/// The answer file name of a purpose, as the web view names the purpose. Input: the purpose.
/// Output: the name.
fn name(purpose: FolderPurpose) -> &'static str {
    match purpose {
        FolderPurpose::Skill => "skill",
        FolderPurpose::Project => "project",
        FolderPurpose::ProjectsHome => "projects_home",
        FolderPurpose::Memory => "memory",
        FolderPurpose::Plugin => "plugin",
    }
}

#[cfg(test)]
mod tests {
    use super::{PICK_BYTES_MAX, read_answer};
    use std::io::{Seek, Write};

    /// An answer file that grew past the bound after its size was checked is refused, not
    /// asserted, and one within the bound is read whole (ND6-L1).
    #[test]
    fn an_answer_that_grew_past_the_bound_after_its_check_is_refused() {
        let bound = usize::try_from(PICK_BYTES_MAX).unwrap();
        for (size, read) in [(bound, true), (bound + 1, false), (bound * 4, false)] {
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&vec![b'a'; size]).unwrap();
            file.rewind().unwrap();
            let answer = read_answer(&file);
            assert_eq!(
                answer.as_ref().map(String::len).map_err(|error| error.code),
                if read {
                    Ok(size)
                } else {
                    Err("trial_pick_invalid")
                },
                "{size} bytes"
            );
        }
    }
}
