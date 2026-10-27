use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The file in a project's IDE folder that holds its running server's connection token.
pub const FILE: &str = "token";

/// How many random bytes one server launch's token holds: 256 bits.
pub const TOKEN_BYTES: usize = 32;

/// Makes `path` a private folder of this user: creates it 0700, or accepts an existing real
/// folder this user owns and narrows it to 0700. Input: the folder, whose parent exists.
/// Output: done, or `ide_folder_unsafe` for a link, a file or another user's folder.
pub fn private_folder(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(io::Error::other("ide_folder_unsafe"));
    }
    if metadata.mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes a new connection token for one server launch into a project's private IDE folder:
/// [`TOKEN_BYTES`] random bytes as hex digits, in a file created new with mode 0600, never
/// through a link. The previous launch's token file is removed first, so no two launches
/// share a token. Input: the project's IDE folder. Output: the token file's path and the
/// token, which the engine keeps in memory for its proxy and log redaction.
pub fn issue(folder: &Path) -> io::Result<(PathBuf, String)> {
    remove(folder)?;
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| io::Error::other("ide_token_entropy_unavailable"))?;
    let text: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(text.len(), 2 * TOKEN_BYTES);
    let path = folder.join(FILE);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    let metadata = file.metadata()?;
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.len(), text.len() as u64);
    Ok((path, text))
}

/// Removes a project's token file, once its server has stopped or before a new launch.
/// Input: the project's IDE folder. Output: done, also when there was none.
pub fn remove(folder: &Path) -> io::Result<()> {
    match fs::remove_file(folder.join(FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
