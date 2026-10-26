use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The longest socket path a Unix socket address holds: `sun_path` has 104 bytes on macOS
/// and 108 on Linux, one of them the terminating zero.
pub const ADDRESS_BYTES_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };
const LINK_HEX: usize = 16;

/// The address a socket at `path` is bound and reached at: the path itself when it fits,
/// otherwise `<uid folder>/<16 hex of its folder>/<name>` in `/tmp/crazyagent-<uid>`,
/// where that entry is a link to the socket's own folder, made again whenever it is
/// missing or names another folder. The socket file stays in its own folder. Input: the
/// socket path. Output: the address, or an error when even the short one does not fit
/// (`socket_path_too_long`) or the uid folder is not this user's private folder.
pub fn address(path: &Path) -> io::Result<PathBuf> {
    address_in(path, &folder())
}

/// Tells, without touching the file system, whether a socket at `path` has an address,
/// directly or through the short folder. Input: the socket path. Output: true when it fits.
pub fn fits(path: &Path) -> bool {
    fits_in(path, &folder())
}

fn folder() -> PathBuf {
    PathBuf::from(format!(
        "/tmp/crazyagent-{}",
        rustix::process::getuid().as_raw()
    ))
}

fn direct(path: &Path) -> bool {
    path.as_os_str().len() <= ADDRESS_BYTES_MAX
}

/// The link and short address for an absolute socket path in `folder`, or none when the
/// path has no folder or name or the short address is still too long.
fn short(path: &Path, folder: &Path) -> Option<(PathBuf, PathBuf)> {
    if !path.is_absolute() {
        return None;
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    let hash = crate::memory::hash(parent.as_os_str().as_bytes());
    let link = folder.join(&hash[..LINK_HEX]);
    let address = link.join(name);
    direct(&address).then_some((link, address))
}

fn fits_in(path: &Path, folder: &Path) -> bool {
    direct(path) || short(path, folder).is_some()
}

fn address_in(path: &Path, folder: &Path) -> io::Result<PathBuf> {
    if direct(path) {
        return Ok(path.to_path_buf());
    }
    let (link, address) = short(path, folder)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket_path_too_long"))?;
    let target = path
        .parent()
        .expect("a short address names the socket's folder");
    private_folder(folder)?;
    linked(&link, target)?;
    assert!(direct(&address) && address.starts_with(folder));
    Ok(address)
}

/// Makes the uid folder private to this user, or refuses one that is a link, not a
/// folder, owned by another user, or open to anyone else; it is never changed.
fn private_folder(folder: &Path) -> io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(folder) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(folder)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket_alias_folder_unsafe",
        ));
    }
    Ok(())
}

/// Points `link` at `target`, replacing a link to anything else in one rename, so a
/// process reaching the address meanwhile finds the old link or the new one. An entry
/// that is not a link is refused, not removed.
fn linked(link: &Path, target: &Path) -> io::Result<()> {
    match std::fs::read_link(link) {
        Ok(current) if current == target => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket_alias_unsafe",
            ));
        }
    }
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("socket_alias_unavailable"))?;
    let staging = link.with_extension(
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    );
    std::os::unix::fs::symlink(target, &staging)?;
    std::fs::rename(&staging, link).inspect_err(|_| {
        let _ = std::fs::remove_file(&staging);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    fn temp() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ca-addr-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap()
    }

    fn long_folder(temp: &Path) -> PathBuf {
        let long = temp.join("l".repeat(120));
        std::fs::create_dir(&long).unwrap();
        long
    }

    #[test]
    fn a_path_that_fits_is_its_own_address_and_touches_nothing() {
        let temp = temp();
        let folder = temp.path().join("alias");
        let path = temp.path().join("engine.sock");
        assert_eq!(address_in(&path, &folder).unwrap(), path);
        assert!(fits_in(&path, &folder));
        let edge = temp
            .path()
            .join("e".repeat(ADDRESS_BYTES_MAX - temp.path().as_os_str().len() - 1));
        assert_eq!(edge.as_os_str().len(), ADDRESS_BYTES_MAX);
        assert_eq!(address_in(&edge, &folder).unwrap(), edge);
        assert!(!folder.exists());
    }

    #[test]
    fn a_long_path_is_reached_through_a_private_short_link_to_its_folder() {
        let temp = temp();
        let folder = temp.path().join("alias");
        let long = long_folder(temp.path());
        let path = long.join("engine.sock");
        let address = address_in(&path, &folder).unwrap();
        assert!(
            address.as_os_str().len() <= ADDRESS_BYTES_MAX,
            "{}",
            address.display()
        );
        assert!(address.starts_with(&folder));
        assert_eq!(address.file_name(), path.file_name());
        let metadata = std::fs::symlink_metadata(&folder).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.mode() & 0o7777, 0o700);
        assert_eq!(metadata.uid(), rustix::process::getuid().as_raw());
        assert_eq!(std::fs::read_link(address.parent().unwrap()).unwrap(), long);
        let _listener = crate::spawn_gate::bind_unix_std(&address).unwrap();
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_socket(),
            "the socket is made in its real folder"
        );
        crate::spawn_gate::connect_unix_std(&address).unwrap();
        assert_eq!(
            address_in(&path, &folder).unwrap(),
            address,
            "the same folder always has the same address"
        );
    }

    #[test]
    fn a_missing_or_wrong_link_is_put_back() {
        let temp = temp();
        let folder = temp.path().join("alias");
        let long = long_folder(temp.path());
        let path = long.join("engine.sock");
        let address = address_in(&path, &folder).unwrap();
        let link = address.parent().unwrap().to_path_buf();
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir(&folder).unwrap();
        assert_eq!(
            address_in(&path, &folder).unwrap(),
            address,
            "a temp cleaner removed the link and its folder"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), long);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(temp.path(), &link).unwrap();
        assert_eq!(address_in(&path, &folder).unwrap(), address);
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            long,
            "a link to another folder is replaced"
        );
        assert_eq!(
            std::fs::read_dir(&folder).unwrap().count(),
            1,
            "no staging entry is left"
        );
    }

    #[test]
    fn an_alias_folder_another_user_could_reach_is_refused() {
        let temp = temp();
        let long = long_folder(temp.path());
        let path = long.join("engine.sock");
        let open = temp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = temp.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let file = temp.path().join("file");
        std::fs::write(&file, "").unwrap();
        for folder in [&open, &linked, &file] {
            let error = address_in(&path, folder).unwrap_err();
            assert_eq!(
                error.kind(),
                ErrorKind::PermissionDenied,
                "{}",
                folder.display()
            );
            assert_eq!(error.to_string(), "socket_alias_folder_unsafe");
        }
        assert_eq!(
            std::fs::read_dir(&real).unwrap().count(),
            0,
            "nothing is made through a link"
        );
        assert_eq!(
            std::fs::metadata(&open).unwrap().mode() & 0o7777,
            0o755,
            "a folder that is not this engine's to fix is left as it is"
        );
        assert_eq!(std::fs::read_dir(&open).unwrap().count(), 0);
        let folder = temp.path().join("alias");
        let address = address_in(&path, &folder).unwrap();
        let link = address.parent().unwrap();
        std::fs::remove_file(link).unwrap();
        std::fs::create_dir(link).unwrap();
        assert_eq!(
            address_in(&path, &folder).unwrap_err().to_string(),
            "socket_alias_unsafe",
            "an entry that is not a link is refused, not removed"
        );
        assert!(link.is_dir());
    }

    #[test]
    fn a_name_too_long_even_for_the_short_address_does_not_fit() {
        let temp = temp();
        let folder = temp.path().join("alias");
        let long = long_folder(temp.path());
        assert!(fits_in(&long.join("engine.sock"), &folder));
        let name = long.join("n".repeat(ADDRESS_BYTES_MAX));
        assert!(!fits_in(&name, &folder));
        let error = address_in(&name, &folder).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(error.to_string(), "socket_path_too_long");
        let relative = PathBuf::from("l".repeat(120)).join("engine.sock");
        assert_eq!(
            address_in(&relative, &folder).unwrap_err().to_string(),
            "socket_path_too_long"
        );
        assert!(!folder.exists());
    }
}
