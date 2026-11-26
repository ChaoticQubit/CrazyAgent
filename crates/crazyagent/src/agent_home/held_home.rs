use super::{
    AgentHomeError, AgentProject, CLAUDE_MEMORY, Caches, GIVEN_FOLDER, Given, HOME_PASSES_MAX,
    Harness, Kept, LEVELS_FOLDER, PROCESS_BYTES_MAX, clears_caches, failure, kept_in, record_files,
    write_record,
};
use crate::engine::bootstrap::held::{Folder, HeldError, Kind, Name, TREE_DEPTH_MAX};
use kernel_sandbox::policy::AgentAccess;
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// The most entries one folder of an agent home may list before its preparation refuses.
const HOME_ENTRIES_MAX: usize = 4096;

/// Prepares an agent home on Windows as [`super::prepare`] does, with every home change made
/// through the held root: entries the harness does not keep go as themselves, never through
/// a link, and the instruction file is made create-only, only readable by another handle
/// while it is written. Inputs: the held home, its path, the harness, the project, what
/// `text` built and the caches choice and level. Output: unit, or the same plain codes.
pub(crate) fn prepare_held(
    root: &Folder,
    home: &Path,
    harness: Harness,
    project: &AgentProject,
    given: &Given,
    caches: Caches,
) -> Result<(), AgentHomeError> {
    assert!(project.data.is_absolute());
    assert!(project.folder.as_deref().is_none_or(Path::is_absolute));
    assert!(given.text.len() <= PROCESS_BYTES_MAX);
    if !home.is_absolute() {
        return Err(failure("agent_home_invalid"));
    }
    let clear = clears_caches(&project.data, home, caches)?;
    record_files(&project.data, home, &given.files)?;
    write_record(&project.data, GIVEN_FOLDER, home, &given.text)?;
    keep_only_held(root, &kept_in(harness, clear))?;
    let level = caches.level.unwrap_or(AgentAccess::Sandboxed);
    write_record(&project.data, LEVELS_FOLDER, home, level.id())?;
    let name = Name::constant(harness.instruction_file());
    remove_held(root, &name)?;
    if given.text.is_empty() {
        return Ok(());
    }
    write_sole(root, &name, given.text.as_bytes())
}

/// Removes from one held folder of an agent home every entry `kept` does not name with its
/// kind, keeps each kept folder with its own list to that list and clears each project's
/// auto-memory in a kept `projects`; a lock is kept as a plain file, as on the path walk.
/// Inputs: the folder and what stays in it. Output: unit, or `agent_home_unwritable` when it
/// cannot be listed, one entry stays or it still changes after [`HOME_PASSES_MAX`] passes.
fn keep_only_held(folder: &Folder, kept: &[(&str, Kept)]) -> Result<(), AgentHomeError> {
    for _ in 0..HOME_PASSES_MAX {
        let mut removed = false;
        for child in folder.list(HOME_ENTRIES_MAX).map_err(unwritable)? {
            let name = Name::checked(&child.name).map_err(unwritable)?;
            let wanted = kept
                .iter()
                .find(|(known, _)| child.name == *known)
                .map(|(_, kind)| *kind);
            match (wanted, child.kind) {
                (Some(Kept::File), Kind::File) if kept_file_held(folder, &name)? => {}
                (Some(Kept::Lock), Kind::File) => {}
                (Some(Kept::Folder | Kept::Cache), Kind::Folder) => {}
                (Some(Kept::Link), Kind::Link) => {}
                (Some(Kept::Transcripts), Kind::Folder) => {
                    clear_memory_held(&open_folder(folder, &name)?)?;
                }
                (Some(Kept::Only(inner)), Kind::Folder) => {
                    keep_only_held(&open_folder(folder, &name)?, inner)?;
                }
                _ => {
                    remove_held(folder, &name)?;
                    removed = true;
                }
            }
        }
        if !removed {
            return Ok(());
        }
    }
    Err(failure("agent_home_unwritable"))
}

/// Removes the auto-memory entry [`CLAUDE_MEMORY`] of each project folder in a held
/// `projects` folder, and each link directly in it as itself. Input: the folder. Output:
/// unit, or `agent_home_unwritable` when it cannot be listed or an entry stays.
fn clear_memory_held(folder: &Folder) -> Result<(), AgentHomeError> {
    let memory = Name::constant(CLAUDE_MEMORY);
    for child in folder.list(HOME_ENTRIES_MAX).map_err(unwritable)? {
        let name = Name::checked(&child.name).map_err(unwritable)?;
        match child.kind {
            Kind::Link => remove_held(folder, &name)?,
            Kind::Folder => remove_held(&open_folder(folder, &name)?, &memory)?,
            Kind::File => {}
        }
    }
    Ok(())
}

/// Tells whether a home entry kept as a file has one link and a trusted owner, opened as
/// itself through its held folder. Inputs: the folder and the name. Output: the answer,
/// false for an entry gone meanwhile, or `agent_home_unwritable` when it cannot be opened
/// or read, so a failed open never removes a kept file.
fn kept_file_held(folder: &Folder, name: &Name) -> Result<bool, AgentHomeError> {
    match folder.open_file_unjudged(name) {
        Ok(file) => sole_trusted_link(&file).map_err(|_| failure("agent_home_unwritable")),
        Err(HeldError::Missing) => Ok(false),
        Err(_) => Err(failure("agent_home_unwritable")),
    }
}

/// Opens one child folder of a held folder as itself, bound to it. Inputs: the folder and
/// the name. Output: the held child, or `agent_home_unwritable`.
fn open_folder(folder: &Folder, name: &Name) -> Result<Folder, AgentHomeError> {
    folder.open_dir_unjudged(name).map_err(unwritable)
}

/// Removes one entry of a held folder with all it holds, links as themselves; an entry
/// already gone counts as removed. Inputs: the folder and the name. Output: unit, or
/// `agent_home_unwritable`.
fn remove_held(folder: &Folder, name: &Name) -> Result<(), AgentHomeError> {
    match folder.remove_tree_unjudged(name, TREE_DEPTH_MAX) {
        Ok(()) | Err(HeldError::Missing) => Ok(()),
        Err(_) => Err(failure("agent_home_unwritable")),
    }
}

/// Writes the instruction file in the held home create-only, no other handle writing it
/// meanwhile, syncs it and checks its length from the handle. Inputs: the home, the name
/// and the bytes. Output: unit, or `process_text_unwritable`.
fn write_sole(root: &Folder, name: &Name, bytes: &[u8]) -> Result<(), AgentHomeError> {
    let mut file = root
        .make_file_sole(name)
        .map_err(|_| failure("process_text_unwritable"))?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| file.metadata());
    match written {
        Ok(metadata) if metadata.len() == bytes.len() as u64 => Ok(()),
        _ => Err(failure("process_text_unwritable")),
    }
}

/// Maps any held-folder refusal in an agent home's preparation to its code. Input: the
/// refusal. Output: `agent_home_unwritable`.
fn unwritable(_error: HeldError) -> AgentHomeError {
    failure("agent_home_unwritable")
}

/// Reads from an open file's handle on Windows whether it is no link, has one name and an
/// owner W1's judge trusts: the engine's user, SYSTEM, the administrators or TrustedInstaller,
/// as an elevated user's new files are owned by the administrators (DESIGN-0256 C6). Input:
/// the open file. Output: the answer, or the Windows error.
pub(super) fn sole_trusted_link(file: &File) -> std::io::Result<bool> {
    use crazyagent_win_sandbox::{dacl, private};
    let facts = private::facts(file)?;
    Ok(!facts.reparse
        && facts.links == 1
        && dacl::trusted_owner(&facts.owner, &private::user_sid()?))
}
