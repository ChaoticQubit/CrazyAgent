use super::*;

/// Makes the two submodule-store folders a sandbox profile denies writes to, the common
/// `.git/modules` and this lease's admin `modules`, through `seatbelt::ensure_store`, so every
/// platform guards a real folder. A store already there is kept when it is a plain folder;
/// a file or link there is refused, never trusted. Inputs: the repository and the lease
/// admin git dir. Output: success, or `submodule_store_invalid` or the filesystem refusal.
#[cfg(unix)]
pub(super) fn ensure_submodule_stores(repository: &Path, admin: &Path) -> Result<(), EngineError> {
    use kernel_sandbox::profile::seatbelt::ensure_store;
    for store in [
        repository.join(".git").join("modules"),
        admin.join("modules"),
    ] {
        ensure_store(&store).map_err(|problem| match problem.kind() {
            std::io::ErrorKind::InvalidData => error("submodule_store_invalid"),
            _ => error(problem),
        })?;
    }
    Ok(())
}

/// Lists the files and symlinks of a commit tree and refuses one the lease cannot copy:
/// past its bounds, holding a submodule or other non-blob entry, an unsafe or `.git`
/// path, or a symlink target longer than the system path limit. Inputs: the repository,
/// the commit, the bounds and the deadline. Output: the entries in Git's tree order.
#[cfg(unix)]
pub(super) fn tree_entries(
    repository: &Path,
    head: &str,
    bounds: &TreeBounds,
    deadline: Instant,
) -> Result<Vec<TreeEntry>, EngineError> {
    let bytes = git_until(
        repository,
        &["ls-tree", "-r", "-z", "-l", head],
        TREE_LISTING_BYTES_MAX,
        deadline,
    )?;
    let mut entries = Vec::new();
    let mut total: u64 = 0;
    for entry in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if entries.len() >= bounds.files {
            return Err(error("tree_file_capacity"));
        }
        let entry = std::str::from_utf8(entry).map_err(error)?;
        let (header, name) = entry
            .split_once('\t')
            .ok_or_else(|| error("invalid_tree_entry"))?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        let path = PathBuf::from(name);
        if fields.len() != 4
            || !matches!(fields[0], "100644" | "100755" | "120000")
            || fields[1] != "blob"
            || path
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
            || path
                .components()
                .any(|p| crate::instruction_files::names_git(&p.as_os_str().to_string_lossy()))
        {
            return Err(error("unsupported_or_unsafe_tree_entry"));
        }
        if path.components().count() > bounds.components {
            return Err(error("tree_path_depth"));
        }
        validate_oid(fields[2])?;
        let size = fields[3]
            .parse::<u64>()
            .map_err(|_| error("invalid_tree_entry"))?;
        let link = fields[0] == "120000";
        if link && size > LINK_TARGET_BYTES_MAX {
            return Err(error("unsupported_or_unsafe_tree_entry"));
        }
        total = total.saturating_add(size);
        if total > bounds.bytes {
            return Err(error("tree_byte_capacity"));
        }
        entries.push(TreeEntry {
            link,
            executable: fields[0] == "100755",
            oid: fields[2].into(),
            size,
            path,
        });
    }
    assert!(entries.len() <= bounds.files);
    assert!(total <= bounds.bytes);
    Ok(entries)
}

/// Streams every entry's blob from one `git cat-file --batch` into the worktree folder,
/// killing Git when the deadline passes. Inputs: the repository, the sealed worktree
/// folder, the listed entries and the deadline. Output: the paths of the symlinks left
/// out because they would point outside the lease, or the refusal code.
#[cfg(unix)]
pub(super) fn write_tree(
    repository: &Path,
    directory: &std::fs::File,
    entries: &[TreeEntry],
    deadline: Instant,
) -> Result<Vec<String>, EngineError> {
    let mut child = kernel_processes::spawn_gate::spawn(
        git_command(repository, &["cat-file", "--batch"]).stdin(Stdio::piped()),
    )
    .map_err(error)?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| error("git_stdin_missing"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| error("git_stdout_missing"))?;
    let copied = std::thread::scope(|scope| {
        let worker = scope.spawn(move || copy_blobs(input, output, directory, entries));
        loop {
            if Instant::now() >= deadline {
                child.kill().map_err(error)?;
                let _ = worker.join();
                break Err(error("materialization_deadline"));
            }
            if worker.is_finished() {
                break worker
                    .join()
                    .map_err(|_| error("materialization_worker_failed"))?;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let status = loop {
        if let Some(status) = child.try_wait().map_err(error)? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().map_err(error)?;
            child.wait().map_err(error)?;
            return Err(error("materialization_deadline"));
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let left_out = copied?;
    if !status.success() {
        return Err(error("git_operation_failed_or_output_capacity"));
    }
    assert!(left_out.len() <= LINKS_LEFT_OUT_MAX);
    Ok(left_out)
}

/// Asks Git for each entry's blob in turn and writes it: a file with its exact bytes and
/// mode, a symlink when its target stays inside the lease. Inputs: Git's batch input and
/// output, the worktree folder and the entries. Output: the left-out symlink paths.
#[cfg(unix)]
fn copy_blobs(
    mut input: std::process::ChildStdin,
    output: std::process::ChildStdout,
    directory: &std::fs::File,
    entries: &[TreeEntry],
) -> Result<Vec<String>, EngineError> {
    use std::io::BufRead;
    let mut output = std::io::BufReader::with_capacity(64 * 1024, output);
    let mut folders = Folders::new(directory);
    let mut left_out = Vec::new();
    for entry in entries {
        input
            .write_all(format!("{}\n", entry.oid).as_bytes())
            .map_err(error)?;
        input.flush().map_err(error)?;
        let mut header = Vec::new();
        (&mut output)
            .take(128)
            .read_until(b'\n', &mut header)
            .map_err(error)?;
        if header != format!("{} blob {}\n", entry.oid, entry.size).as_bytes() {
            return Err(error("tree_object_mismatch"));
        }
        let mut body = (&mut output).take(entry.size);
        if entry.link {
            let mut target = Vec::new();
            body.read_to_end(&mut target).map_err(error)?;
            if target.len() as u64 != entry.size {
                return Err(error("tree_object_truncated"));
            }
            if link_stays_inside(&entry.path, &target) {
                folders.write_link(&entry.path, &target)?;
            } else {
                if left_out.len() >= LINKS_LEFT_OUT_MAX {
                    return Err(error("tree_link_capacity"));
                }
                let path = entry
                    .path
                    .to_str()
                    .ok_or_else(|| error("invalid_tree_entry"))?;
                left_out.push(path.to_owned());
            }
        } else {
            folders.write_file(&entry.path, &mut body, entry.size, entry.executable)?;
        }
        let mut end = [0u8; 1];
        output.read_exact(&mut end).map_err(error)?;
        if end != *b"\n" {
            return Err(error("tree_object_mismatch"));
        }
    }
    assert!(left_out.len() <= LINKS_LEFT_OUT_MAX);
    Ok(left_out)
}

/// Says whether a tracked symlink resolves inside the lease wherever it is followed from:
/// its target is relative, climbs only with leading `..` steps and no higher than the
/// folder holding the link, then names plain components other than `.git` in any spelling
/// a case-insensitive volume opens as it ([`crate::instruction_files::names_git`]). Inputs:
/// the link's path in the tree and its target bytes. Output: true when it stays inside.
#[cfg(unix)]
pub(super) fn link_stays_inside(link: &Path, target: &[u8]) -> bool {
    let Ok(target) = std::str::from_utf8(target) else {
        return false;
    };
    let depth = link.components().count().saturating_sub(1);
    let mut climbing = true;
    let mut up = 0;
    for part in target.split('/') {
        match part {
            ".." if climbing => up += 1,
            "" | "." | ".." => return false,
            name if crate::instruction_files::names_git(name) || name.contains('\0') => {
                return false;
            }
            _ => climbing = false,
        }
    }
    up <= depth
}

/// The worktree folder and the chain of subfolders opened for the previous entry, so
/// entries in Git's tree order reuse open folders. Every folder is created or opened
/// relative to its parent descriptor without following a symlink.
#[cfg(unix)]
pub(super) struct Folders<'a> {
    root: &'a std::fs::File,
    open: Vec<(std::ffi::OsString, std::fs::File)>,
}

#[cfg(unix)]
impl<'a> Folders<'a> {
    pub(super) fn new(root: &'a std::fs::File) -> Self {
        Self {
            root,
            open: Vec::new(),
        }
    }

    /// Opens, creating where missing, the folders above a tree path. Input: the path.
    /// Output: the descriptor of its parent folder and its final component.
    fn parent<'p>(
        &mut self,
        path: &'p Path,
    ) -> Result<(&std::fs::File, &'p std::ffi::OsStr), EngineError> {
        use rustix::fs::{Mode, OFlags, mkdirat, openat};
        let components = path
            .components()
            .map(|c| match c {
                Component::Normal(name) => Ok(name),
                _ => Err(error("invalid_materialization_path")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some((name, folders)) = components.split_last() else {
            return Err(error("invalid_materialization_path"));
        };
        if components.len() > TREE_BOUNDS.components {
            return Err(error("invalid_materialization_path"));
        }
        let shared = self
            .open
            .iter()
            .zip(folders.iter())
            .take_while(|((open, _), folder)| open.as_os_str() == **folder)
            .count();
        self.open.truncate(shared);
        for folder in &folders[shared..] {
            let parent = self.open.last().map_or(self.root, |(_, file)| file);
            match mkdirat(parent, *folder, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(error(e)),
            }
            let next = openat(
                parent,
                *folder,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(std::fs::File::from)
            .map_err(error)?;
            self.open.push((folder.to_os_string(), next));
        }
        assert_eq!(self.open.len(), folders.len());
        Ok((self.open.last().map_or(self.root, |(_, file)| file), name))
    }

    /// Creates a new file at a tree path with the given bytes. Inputs: the path, the
    /// source, its exact size and whether it is executable. Output: success or the error.
    pub(super) fn write_file(
        &mut self,
        path: &Path,
        source: &mut impl Read,
        size: u64,
        executable: bool,
    ) -> Result<(), EngineError> {
        use rustix::fs::{Mode, OFlags, openat};
        let (parent, name) = self.parent(path)?;
        let mode = Mode::from_raw_mode(if executable { 0o700 } else { 0o600 });
        let mut file = openat(
            parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )
        .map(std::fs::File::from)
        .map_err(error)?;
        let written = std::io::copy(&mut source.take(size), &mut file).map_err(error)?;
        if written != size {
            return Err(error("tree_object_truncated"));
        }
        let metadata = file.metadata().map_err(error)?;
        assert_eq!(metadata.len(), size);
        assert_eq!(metadata.nlink(), 1);
        Ok(())
    }

    /// Creates a symlink at a tree path. Inputs: the path and a target that
    /// `link_stays_inside` accepted. Output: success or the error.
    fn write_link(&mut self, path: &Path, target: &[u8]) -> Result<(), EngineError> {
        assert!(link_stays_inside(path, target));
        let target = std::str::from_utf8(target).map_err(error)?;
        let (parent, name) = self.parent(path)?;
        rustix::fs::symlinkat(target, parent, name).map_err(error)?;
        Ok(())
    }
}
