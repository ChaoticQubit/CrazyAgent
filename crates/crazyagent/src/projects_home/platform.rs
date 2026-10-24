use std::fs;
use std::path::{Path, PathBuf};

/// The environment variable naming the signed-in user's home folder.
#[cfg(unix)]
pub(super) const OWNER_HOME_VARIABLE: &str = "HOME";

/// The environment variable naming the signed-in user's home folder.
/// Windows: written, not tested.
#[cfg(target_os = "windows")]
pub(super) const OWNER_HOME_VARIABLE: &str = "USERPROFILE";

/// Permission bits compared between a source and its copy.
/// Unix: the mode's permission, setuid, setgid and sticky bits.
#[cfg(unix)]
pub(super) fn permission_bits(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

/// Permission bits compared between a source and its copy.
/// Windows: only the read-only flag exists. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn permission_bits(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
}

/// Makes a folder being filled by a copy private to the owner and writable.
/// The folder's real permissions are applied after its contents are copied.
#[cfg(unix)]
pub(super) fn private_folder(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Makes a folder being filled by a copy writable.
/// Windows: folders inherit their parent's access list. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn private_folder(path: &Path) -> std::io::Result<()> {
    let mut permissions = fs::symlink_metadata(path)?.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

/// Lets the owner delete inside a folder: adds owner read, write and search.
#[cfg(unix)]
pub(super) fn writable_folder(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)?.permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700))
}

/// Lets the owner delete inside a folder by clearing its read-only flag.
/// Windows: written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn writable_folder(path: &Path) -> std::io::Result<()> {
    private_folder(path)
}

/// Recreates the symlink at `original` as the same link at `copy`, never
/// following it. Input: the original link, its target text and the new path.
#[cfg(unix)]
pub(super) fn copy_link(_original: &Path, target: PathBuf, copy: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, copy)
}

/// Recreates the symlink at `original` as the same link at `copy`, never
/// following it; Windows keeps file and folder links apart. Written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn copy_link(original: &Path, target: PathBuf, copy: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if fs::symlink_metadata(original)?.file_type().is_symlink_dir() {
        std::os::windows::fs::symlink_dir(target, copy)
    } else {
        std::os::windows::fs::symlink_file(target, copy)
    }
}

/// Opens a file for appending, creating it, and refuses to follow a link
/// placed at its path. Input: the file path. Output: the open file.
#[cfg(unix)]
pub(super) fn open_append(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Opens a file for appending, creating it, and refuses a link at its path by
/// opening the reparse point itself. Windows: written, not tested.
#[cfg(target_os = "windows")]
pub(super) fn open_append(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_type().is_symlink() {
        return Err(std::io::Error::other("exclude is a link"));
    }
    Ok(file)
}

/// Moves a file or folder to the macOS Trash through NSFileManager, which
/// needs no Finder automation permission. Input: the path. Output: Ok once
/// it is in the Trash.
#[cfg(target_os = "macos")]
pub(super) fn move_to_trash(path: &Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path)
}

/// Moves a file or folder to the desktop trash (freedesktop trash on Linux,
/// the Recycle Bin on Windows). Written, not tested.
#[cfg(not(target_os = "macos"))]
pub(super) fn move_to_trash(path: &Path) -> Result<(), trash::Error> {
    trash::delete(path)
}
