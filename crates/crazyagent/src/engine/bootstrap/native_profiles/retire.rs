use super::agent_copy::{CLAUDE_RECORD, CODEX_RECORD};
use super::generated_configured_rcli::{CONFIG, HOMES};
use super::staging::remove_tree;
use super::*;

const KEYCHAINS: [&str; 2] = ["login.keychain-db", "profile.keychain-db"];
const SDK: &str = ".backboard";
const SDK_KEYS: &str = "keys.json";
const HOME_DEPTH_MAX: usize = 1;

/// Holds the engine root through the held layer, without following a link and judged
/// private. Input: the root. Output: its held folder, or `native_profile_custody_unsafe`.
fn engine_root(root: &Path) -> Result<Folder, EngineError> {
    if !root.is_absolute() {
        return Err(failure("native_profile_custody_unsafe"));
    }
    let folder = Folder::root(root).map_err(|_| failure("native_profile_custody_unsafe"))?;
    assert_ne!(folder.pin().inode, 0);
    Ok(folder)
}

/// Opens one engine-made folder below `parent` without following a link. Inputs: the parent
/// and the name. Output: the folder, none when it is missing, or a refusal for a link or
/// an unsafe folder.
fn present(parent: &Folder, name: &OsStr) -> Result<Option<Directory>, EngineError> {
    match Directory::open(parent, name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.0 == "native_profile_incomplete" => Ok(None),
        Err(error) => Err(error),
    }
}

/// Opens one harness-made folder of a profile home without following a link; it need not
/// be private, the home above it is. Inputs: the parent and the name. Output: the folder,
/// none when it is missing, or a refusal for a link.
fn harness_folder(parent: &File, name: &str) -> Result<Option<File>, EngineError> {
    match rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(folder) => Ok(Some(File::from(folder))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(unsafe_custody(error)),
    }
}

/// Deletes one name from an open folder; a link goes and what it points at stays. Inputs:
/// the folder and the name. Output: Ok, also when the name is already gone.
fn unlink(folder: &File, name: &str) -> Result<(), EngineError> {
    match rustix::fs::unlinkat(folder, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(unsafe_custody(error)),
    }
}

/// Runs `action` while no agent, sign-in, check or renewal holds one profile slot, and
/// refuses when one does, so the note stays for the next sweep. Both locks are unlocked,
/// not just closed, before it returns, so a child another thread is starting cannot keep
/// the slot busy. Inputs: the engine root's folder, the slot's digest name and the action.
/// Output: the action's result, or `native_profile_in_use` or `native_profile_renewal_busy`.
fn while_free(
    anchor: &Folder,
    key: &str,
    action: impl FnOnce(Option<&Directory>) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let Some(profiles) = present(anchor, OsStr::new(ROOT))? else {
        return action(None);
    };
    let Some(profile) = present(&profiles.folder, OsStr::new(key))? else {
        return action(None);
    };
    let _renewal = renewal::hold(profile.file(), std::time::Duration::ZERO)?;
    let _manifest = match rustix::fs::openat(
        profile.file(),
        MANIFEST,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(manifest) => {
            let manifest = File::from(manifest);
            take_lock(&manifest, FlockOperation::NonBlockingLockExclusive)?;
            Some(HeldLock(manifest))
        }
        Err(rustix::io::Errno::NOENT) => None,
        Err(error) => return Err(unsafe_custody(error)),
    };
    action(Some(&profile))
}

/// Deletes the sign-in files of a removed native account and keeps the rest of its slot.
/// Inputs: the engine root and the profile reference. Output: Ok, also when nothing is
/// there; a refusal for a link in the walk, or while the profile is in use, keeps the note.
pub(crate) fn remove_sign_in(root: &Path, profile_ref: &CredentialRef) -> Result<(), EngineError> {
    let anchor = engine_root(root)?;
    let key = digest(profile_ref.as_str().as_bytes());
    while_free(&anchor, &key, |profile| {
        let Some(profile) = profile else {
            return Ok(());
        };
        let Some(home) = present(&profile.folder, OsStr::new("home"))? else {
            return Ok(());
        };
        for name in [CODEX_RECORD, CLAUDE_RECORD] {
            unlink(home.file(), name)?;
        }
        let Some(library) = harness_folder(home.file(), "Library")? else {
            return Ok(());
        };
        let Some(keychains) = harness_folder(&library, "Keychains")? else {
            return Ok(());
        };
        for name in KEYCHAINS {
            unlink(&keychains, name)?;
        }
        Ok(())
    })
}

/// Tells whether the generated SDK home of an API key account holds its config but no key
/// file, as a keyed account saved before DEC-150 addendum 9a left it (KY-M1). Inputs: the
/// engine root and the credential reference. Output: true only then; a missing home is
/// false, and a link or an unsafe folder on the way is a refusal.
pub(crate) fn generated_home_needs_key(
    root: &Path,
    profile_ref: &CredentialRef,
) -> Result<bool, EngineError> {
    let anchor = engine_root(root)?;
    let key = digest(profile_ref.as_str().as_bytes());
    let Some(homes) = present(&anchor, OsStr::new(HOMES))? else {
        return Ok(false);
    };
    let Some(home) = present(&homes.folder, OsStr::new(&key))? else {
        return Ok(false);
    };
    let Some(sdk) = present(&home.folder, OsStr::new(SDK))? else {
        return Ok(false);
    };
    let held = |name: &str| match rustix::fs::statat(sdk.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(unsafe_custody(error)),
    };
    Ok(held(CONFIG)? && !held(SDK_KEYS)?)
}

/// Deletes the generated SDK home of a removed API key account, its key first so a home
/// past the bound still loses it. Inputs: the engine root and the credential reference.
/// Output: Ok, also when nothing is there; a refusal keeps the note.
pub(crate) fn remove_generated_home(
    root: &Path,
    profile_ref: &CredentialRef,
) -> Result<(), EngineError> {
    let anchor = engine_root(root)?;
    let key = digest(profile_ref.as_str().as_bytes());
    while_free(&anchor, &key, |_| {
        let Some(homes) = present(&anchor, OsStr::new(HOMES))? else {
            return Ok(());
        };
        let Some(home) = present(&homes.folder, OsStr::new(&key))? else {
            return Ok(());
        };
        if let Some(sdk) = present(&home.folder, OsStr::new(SDK))? {
            unlink(sdk.file(), SDK_KEYS)?;
        }
        remove_tree(&home.folder, HOME_DEPTH_MAX)?;
        home.verify(&homes.folder, OsStr::new(&key))?;
        rustix::fs::unlinkat(homes.file(), key.as_str(), AtFlags::REMOVEDIR)
            .map_err(unsafe_custody)?;
        homes.file().sync_all().map_err(unsafe_custody)
    })
}
