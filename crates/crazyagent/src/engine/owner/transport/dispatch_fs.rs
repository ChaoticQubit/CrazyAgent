use super::*;

/// Opens a sealed dispatch folder without following links and checks its owner and mode.
/// Input: the canonical folder path. Output: its live (device, inode) pair, which the
/// caller compares with the seal under the whole-volume renumber rule.
pub(super) fn dispatch_directory(path: &Path) -> Result<(u64, u64), EngineError> {
    if !path.is_absolute()
        || path.components().count() > 64
        || std::fs::canonicalize(path).map_err(io_error)? != path
    {
        return Err(failure("dispatch_path_not_canonical"));
    }
    for ancestor in path.ancestors() {
        if std::fs::symlink_metadata(ancestor)
            .map_err(io_error)?
            .file_type()
            .is_symlink()
        {
            return Err(failure("dispatch_symlink_path"));
        }
    }
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?,
    );
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(failure("dispatch_filesystem_identity_changed"));
    }
    Ok((metadata.dev(), metadata.ino()))
}

/// Reads one small file of a leased worktree or its git folder that an agent may have
/// replaced: opened without following a link and without waiting on a FIFO or device, kept
/// only when it is this user's own regular file with one name, not group or world
/// writable, within `limit` bytes and unchanged while it was read. Inputs: the path and
/// the limit. Output: the bytes, or the refusal.
pub(super) fn dispatch_file(path: &Path, limit: usize) -> Result<Vec<u8>, EngineError> {
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?,
    );
    let before = file.metadata().map_err(io_error)?;
    if !before.is_file()
        || before.uid() != rustix::process::getuid().as_raw()
        || before.mode() & 0o022 != 0
        || before.nlink() != 1
        || before.len() > limit as u64
    {
        return Err(failure("dispatch_file_untrusted_or_capacity"));
    }
    let mut bytes = Vec::new();
    (&file)
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let after = file.metadata().map_err(io_error)?;
    let linked = std::fs::symlink_metadata(path).map_err(io_error)?;
    if bytes.len() > limit
        || Identity::of(&after) != Identity::of(&before)
        || Identity::of(&linked) != Identity::of(&before)
        || after.len() != before.len()
        || after.mtime() != before.mtime()
        || after.mtime_nsec() != before.mtime_nsec()
    {
        return Err(failure("dispatch_file_changed"));
    }
    assert!(bytes.len() <= limit);
    Ok(bytes)
}
