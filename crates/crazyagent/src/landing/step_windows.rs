#![cfg_attr(
    not(test),
    allow(dead_code, reason = "W5b2b starts confined steps through these")
)]

use super::git::{STEP_FOLDER, Writes, failure};
use crate::engine::EngineError;
use crate::sandbox::windows::WindowsSandbox;
use crate::sandbox::{ProjectSandbox, SandboxNetwork};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The folder of the engine's data root that holds every lease's step profiles and homes,
/// one W1-private folder per lease (rv-sec3 RULING 17:17 C-a).
pub(super) const PROFILES_FOLDER: &str = "step-profiles";

/// How many hex digits of the lease key name a lease's folder: 128 bits.
const LEASE_KEY_CHARS: usize = 32;

/// The places one lease's Windows step runs in: the work tree, the common git folder whose
/// object store alone a step reaches, the step folder in the worktree's git folder, the lease's W1-private folder in the data root,
/// and git's plain program with its marked install folder.
#[derive(Debug, Clone)]
pub(super) struct StepPlaces {
    pub(super) work: PathBuf,
    pub(super) common: PathBuf,
    pub(super) step: PathBuf,
    pub(super) lease: PathBuf,
    pub(super) git: PathBuf,
    pub(super) git_root: PathBuf,
}

impl StepPlaces {
    /// Finds and makes the places of the lease whose worktree is `work` with its git folder
    /// `private` (`<repo>\.git\worktrees\<n>`): the step folder and its temp folder, the
    /// data root's step-profiles folder and the lease's folder in it, both W1-private, and each
    /// kind's home inside that. Inputs: the worktree's git folder, the work tree, the engine's
    /// data root and git's program. Output: the places, `changes_workspace_invalid` for a git
    /// folder that is no linked worktree's, or `changes_git_unsupported`.
    pub(super) fn for_lease(
        private: &Path,
        work: &Path,
        data_root: &Path,
        git: &Path,
    ) -> Result<Self, EngineError> {
        let invalid = || failure("changes_workspace_invalid");
        let unsupported = |_| failure("changes_git_unsupported");
        let worktrees = private.parent().ok_or_else(invalid)?;
        if worktrees.file_name().is_none_or(|name| name != "worktrees") {
            return Err(invalid());
        }
        let common = worktrees.parent().ok_or_else(invalid)?.to_path_buf();
        let (git, git_root) =
            crate::sandbox::windows::owner_git::plain_git(git).map_err(unsupported)?;
        let step = private.join(STEP_FOLDER);
        for folder in [step.clone(), step.join("tmp")] {
            match std::fs::create_dir(&folder) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(invalid()),
            }
            if !std::fs::symlink_metadata(&folder).is_ok_and(|meta| meta.is_dir()) {
                return Err(invalid());
            }
        }
        let profiles = data_root.join(PROFILES_FOLDER);
        let lease = profiles.join(lease_key(work)?);
        for folder in [&profiles, &lease] {
            drop(
                crazyagent_win_sandbox::private::dir(folder)
                    .map_err(|_| failure("changes_git_unsupported"))?,
            );
        }
        for writes in KINDS {
            let home = lease.join(format!("{}-home", kind(writes)));
            match std::fs::create_dir(&home) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(failure("changes_git_unsupported")),
            }
        }
        assert!(step.starts_with(private) && lease.starts_with(data_root));
        Ok(Self {
            work: work.to_path_buf(),
            common,
            step,
            lease,
            git,
            git_root,
        })
    }
}

/// The three kinds a lease's step containers are made for.
pub(super) const KINDS: [Writes; 3] = [Writes::Index, Writes::Objects, Writes::WorkTree];

/// The name of one step kind in its home and profile names. Input: the kind. Output: the name.
pub(super) fn kind(writes: Writes) -> &'static str {
    match writes {
        Writes::Index => "index",
        Writes::Objects => "objects",
        Writes::WorkTree => "worktree",
    }
}

/// The key of the lease whose worktree is `work`: the first 128 bits, in hex, of the SHA-256 of
/// its canonical path and the volume serial and file id of the folder itself, opened without
/// following a link. NTFS gives a folder made again at the same path another file id, so a later
/// lease never repeats a key and its containers never take an earlier lease's names (rv-sec3
/// RULING 17:17 C-c). Input: the work tree. Output: the key, or `changes_workspace_invalid` for
/// a work tree that is a link, a junction or no folder.
pub(super) fn lease_key(work: &Path) -> Result<String, EngineError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let invalid = |_| failure("changes_workspace_invalid");
    let folder = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(work)
        .map_err(invalid)?;
    let meta = folder.metadata().map_err(invalid)?;
    if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(failure("changes_workspace_invalid"));
    }
    let (volume, id) = crazyagent_win_sandbox::private::identity(&folder).map_err(invalid)?;
    let canonical =
        crate::sandbox::canonical(work).map_err(|_| failure("changes_workspace_invalid"))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_os_str().as_encoded_bytes());
    hasher.update(b"\n");
    hasher.update(volume.to_le_bytes());
    hasher.update(id.to_le_bytes());
    let key: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let key = key[..LEASE_KEY_CHARS].to_owned();
    assert_eq!(key.len(), LEASE_KEY_CHARS);
    Ok(key)
}

/// The rights [`lease_key`] opens the work tree with: its attributes, nothing that reads or
/// changes what it holds.
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
/// Read, write and delete sharing, so the key's brief open never blocks a git.
const FILE_SHARE_ALL: u32 = 0x7;
/// Opens a link or junction itself, never its target.
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// Opens a folder at all.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// The attribute of a link or junction.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The policy one step kind runs under (rv-sec3 #746 C1, C2, C5 and RULING 17:42 R2-a). Every
/// kind reads the work tree, the object store and git's install folder, but no other part of
/// the common git folder, so a junction an agent toggles in the work tree after the engine's
/// walk reaches no config, hook or ref; W3 already refuses a repository whose object store
/// names alternates. Every kind writes its own home in the lease's folder and the step
/// folder's temp folder, and has no network. An Index step writes only the step folder, an
/// Objects step the object store too, and a WorkTree step the work tree, reading the step
/// folder's index copy and shadow git folder. Inputs: the places and the kind. Output: the
/// policy, or `changes_git_unsupported`.
pub(super) fn policy(places: &StepPlaces, writes: Writes) -> Result<ProjectSandbox, EngineError> {
    let refused = |_| failure("changes_git_unsupported");
    let home = places.lease.join(format!("{}-home", kind(writes)));
    let tmp = places.step.join("tmp");
    let objects = places.common.join("objects");
    let network = SandboxNetwork::Only(Vec::new());
    let sandbox = match writes {
        Writes::Index => ProjectSandbox::new(&places.step, &home, &tmp, network)
            .map(ProjectSandbox::as_job_folder)
            .and_then(|sandbox| sandbox.allow_read(&objects)),
        Writes::Objects => ProjectSandbox::new(&places.step, &home, &tmp, network)
            .map(ProjectSandbox::as_job_folder)
            .and_then(|sandbox| sandbox.allow_worktree(&objects)),
        Writes::WorkTree => ProjectSandbox::new(&places.work, &home, &tmp, network)
            .and_then(|sandbox| sandbox.allow_read(&places.step))
            .and_then(|sandbox| sandbox.allow_read(&objects)),
    }
    .and_then(|sandbox| match writes {
        Writes::WorkTree => Ok(sandbox),
        Writes::Index | Writes::Objects => sandbox.allow_read(&places.work),
    })
    .and_then(|sandbox| sandbox.allow_read(&places.git_root))
    .map_err(refused)?;
    assert!(sandbox.network() == &SandboxNetwork::Only(Vec::new()));
    Ok(sandbox)
}

/// Installs one step kind's policy at its profile in the lease's W1-private folder, bound to
/// `helper` and recording grants in `grants`. A profile already there is read back through W1
/// first, without following a link, so a link or a file another account can change stops the
/// step before any launch would grant what it says (rv-sec3 RULING 17:17 C-a). Inputs: the
/// places, the kind, the helper and the grants folder. Output: the installed sandbox, or
/// `changes_git_unsupported`.
pub(super) fn installed(
    places: &StepPlaces,
    writes: Writes,
    helper: &Path,
    grants: &Path,
) -> Result<WindowsSandbox, EngineError> {
    let refused = |_| failure("changes_git_unsupported");
    let profile = places.lease.join(format!("{}.sb", kind(writes)));
    if std::fs::symlink_metadata(&profile).is_ok() {
        drop(
            crazyagent_win_sandbox::private::read_only(&profile).map_err(|_| {
                eprintln!("landing_step_profile_refused kind={}", kind(writes));
                failure("changes_git_unsupported")
            })?,
        );
    }
    let sandbox = WindowsSandbox::install(policy(places, writes)?, helper, &profile, grants)
        .map_err(refused)?;
    drop(
        crazyagent_win_sandbox::private::read_only(&profile)
            .map_err(|_| failure("changes_git_unsupported"))?,
    );
    assert_eq!(sandbox.profile(), profile);
    Ok(sandbox)
}

/// The deepest folder, in path components below the work tree, that [`reparse_free`] walks.
pub(super) const WALK_DEPTH_MAX: usize = 64;

/// The most entries, ignored or not, that [`reparse_free`] reads in one work tree.
pub(super) const WALK_ENTRIES_MAX: usize = 1_048_576;

/// Walks the whole work tree before a step scans it (rv-sec3 RULING 17:42 C3 condition A),
/// reading each entry's attributes from its folder's listing and never opening or following
/// a child, and refuses at the first reparse point of any kind: a junction, a symlink or a
/// mount point. Only the work tree's own `.git` file is passed over, and the root itself must
/// be a plain folder. Ignored paths are walked too, since a scanning step takes no excludes.
/// Input: the work tree. Output: Ok, `changes_workspace_invalid` for a reparse point or an
/// unreadable folder, or `changes_workspace_capacity` past [`WALK_DEPTH_MAX`] or
/// [`WALK_ENTRIES_MAX`].
pub(super) fn reparse_free(work: &Path) -> Result<(), EngineError> {
    use std::os::windows::fs::MetadataExt;
    let invalid = || failure("changes_workspace_invalid");
    let root = std::fs::symlink_metadata(work).map_err(|_| invalid())?;
    if !root.is_dir() || root.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid());
    }
    let mut folders = vec![(work.to_path_buf(), 0_usize)];
    let mut entries = 0_usize;
    while let Some((folder, depth)) = folders.pop() {
        for entry in std::fs::read_dir(&folder).map_err(|_| invalid())? {
            let entry = entry.map_err(|_| invalid())?;
            entries += 1;
            if entries > WALK_ENTRIES_MAX {
                return Err(failure("changes_workspace_capacity"));
            }
            let meta = entry.metadata().map_err(|_| invalid())?;
            if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                eprintln!("landing_step_reparse_point_refused depth={depth}");
                return Err(invalid());
            }
            if depth == 0 && entry.file_name() == ".git" && meta.is_file() {
                continue;
            }
            if meta.is_dir() {
                if depth + 1 > WALK_DEPTH_MAX {
                    return Err(failure("changes_workspace_capacity"));
                }
                folders.push((entry.path(), depth + 1));
            }
        }
    }
    assert!(entries <= WALK_ENTRIES_MAX);
    Ok(())
}
