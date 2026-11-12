use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_NT,
};

const NAME_UNITS_START: usize = 512;
const NAME_UNITS_MAX: usize = 32768;
const _: () = assert!(NAME_UNITS_START >= 2 && NAME_UNITS_START <= NAME_UNITS_MAX);

/// The real place of an open file or folder: its normalized name on its volume's device,
/// such as `\Device\HarddiskVolume3\work\notes.md`, with every junction and link on the way
/// already resolved. It is read from the handle, so it names what was opened whatever the
/// path does afterwards. The name is for comparing two handles' places, not for opening.
/// Input: the open file or folder. Output: the name in UTF-16 units with no ending zero, at
/// most `NAME_UNITS_MAX` units, or the I/O error.
pub fn final_name(file: &File) -> io::Result<Vec<u16>> {
    name_from(file, NAME_UNITS_START)
}

/// Reads the handle's name into a buffer of `units_start` units and, when Windows says the
/// name needs more, once more into a buffer of the size it asked for. Relies on `file`
/// staying open for the call and on the length passed being the buffer's own length, so
/// Windows writes inside the buffer. Inputs: the open file and the first buffer size, 2 to
/// `NAME_UNITS_MAX` units. Output: the name, or the I/O error.
fn name_from(file: &File, units_start: usize) -> io::Result<Vec<u16>> {
    assert!((2..=NAME_UNITS_MAX).contains(&units_start));
    let mut name = vec![0u16; units_start];
    for _ in 0..2 {
        assert!(name.len() <= NAME_UNITS_MAX + 1);
        let wrote = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                name.as_mut_ptr(),
                name.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_NT,
            )
        } as usize;
        if wrote == 0 {
            return Err(io::Error::last_os_error());
        }
        if wrote < name.len() {
            name.truncate(wrote);
            if name.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "name holds a zero",
                ));
            }
            return Ok(name);
        }
        if wrote > NAME_UNITS_MAX {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "name too long"));
        }
        name = vec![0u16; wrote + 1];
    }
    Err(io::Error::other("name changed while it was read"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;
    use std::process::Command;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    fn junction(link: &Path, target: &Path) {
        let made = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
    }

    fn folder(path: &Path) -> File {
        OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    }

    #[test]
    fn a_file_reached_through_a_junction_is_named_by_its_real_folder() {
        let directory = tempfile::tempdir().unwrap();
        let (real, link) = (directory.path().join("real"), directory.path().join("link"));
        fs::create_dir(&real).unwrap();
        fs::write(real.join("note.md"), "note").unwrap();
        junction(&link, &real);
        assert_eq!(fs::read_to_string(link.join("note.md")).unwrap(), "note");

        let direct = final_name(&File::open(real.join("note.md")).unwrap()).unwrap();
        let through = final_name(&File::open(link.join("note.md")).unwrap()).unwrap();
        assert_eq!(through, direct, "the junction is resolved in the name");
        let held = final_name(&folder(&real)).unwrap();
        assert!(direct.len() > held.len() && direct.starts_with(&held));
        assert_eq!(final_name(&folder(&link)).unwrap(), held);
        let text = String::from_utf16(&direct).unwrap();
        assert!(
            text.starts_with(r"\Device\") && text.ends_with(r"\real\note.md"),
            "{text}"
        );
    }

    #[test]
    fn a_name_longer_than_the_first_buffer_is_read_whole() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "note").unwrap();
        let file = File::open(&path).unwrap();
        let whole = final_name(&file).unwrap();
        assert!(whole.len() > 2 && !whole.contains(&0));
        assert_eq!(name_from(&file, 2).unwrap(), whole);
        assert_eq!(name_from(&file, whole.len()).unwrap(), whole);
        assert_eq!(name_from(&file, whole.len() + 1).unwrap(), whole);
    }
}
