use super::*;

/// Confirms on Windows that a launch lock is the lock name directly in the held home
/// (CodeRabbit CR-1252): both real places are read from their handles, so a lock the home's
/// path reached in another folder, as after a junction above the home was pointed elsewhere
/// between the hold and the lock, is refused. Inputs: the lock and the held home. Output:
/// Ok, or `launch_context_unsafe`.
#[cfg(windows)]
pub(super) fn locked_in(lock: &File, held: &File) -> Result<(), LaunchError> {
    const SEPARATOR: u16 = b'\\' as u16;
    let unsafe_lock = |_: std::io::Error| failure("launch_context_unsafe");
    let mut wanted = crazyagent_win_sandbox::final_name(held).map_err(unsafe_lock)?;
    if wanted.last() != Some(&SEPARATOR) {
        wanted.push(SEPARATOR);
    }
    wanted.extend(LOCK.encode_utf16());
    let found = crazyagent_win_sandbox::final_name(lock).map_err(unsafe_lock)?;
    if found != wanted {
        return Err(failure("launch_context_unsafe"));
    }
    Ok(())
}

/// Opens and locks the agent home's launch lock through W1 (V244-WIN-MASTER S9a): made a
/// private file when it is missing, else opened, never through a link, a second name or a
/// list that is not private, then locked without waiting. A name a launched agent left as a
/// link, a folder or an open file stays refused until S9b clears it handle-relative; no
/// Windows launch reuses a home before then (D6, D8). Input: the home H1's check passed.
/// Output: the locked file, `launch_context_unsafe` or `launch_home_busy`.
#[cfg(windows)]
pub(super) fn lock(home: &Path) -> Result<File, LaunchError> {
    use crazyagent_win_sandbox::private;
    const LOCK_VIOLATION: i32 = 33;
    let path = home.join(LOCK);
    let file = match private::file(&path, true) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            private::file(&path, false)
        }
        made => made,
    }
    .map_err(|_| failure("launch_context_unsafe"))?;
    match private::lock(&file) {
        Ok(()) => Ok(file),
        Err(error) if error.raw_os_error() == Some(LOCK_VIOLATION) => {
            Err(failure("launch_home_busy"))
        }
        Err(_) => Err(failure("launch_context_unsafe")),
    }
}

/// Installs a Claude Code launch with tools off in a home `validate_home` passed, every
/// write held: the home is held and judged as itself, the lock must sit in it, the launch
/// folder is cleared through it and a process's home prepared, links as themselves, then the
/// MCP list and instructions written. Tools, Codex or a relay is refused before any change.
/// Inputs: the context, the home, tools and Codex. Output: the guard, or the code.
pub(super) fn install(
    context: &AgentContext,
    home: &Path,
    tools: bool,
    codex: bool,
) -> Result<Installed, LaunchError> {
    use crate::engine::bootstrap::held::{Folder, HeldError, Name, TREE_DEPTH_MAX};
    if tools || codex || context.relay.is_some() {
        return Err(failure("launch_context_unavailable"));
    }
    let root = Folder::root_judged_by(home, held_home_judge).map_err(held_failure)?;
    let lock = lock(home)?;
    locked_in(&lock, root.file())?;
    let state = Name::constant(STATE);
    match root.remove_tree_unjudged(&state, TREE_DEPTH_MAX) {
        Ok(()) | Err(HeldError::Missing) => {}
        Err(error) => return Err(held_failure(error)),
    }
    let folder = root.make_dir_inherited(&state).map_err(held_failure)?;
    let mut text = context.instructions.clone();
    if let Some(process) = &context.process {
        let given = process_text(process)?;
        let harness = crate::agent_home::Harness::Claude;
        crate::agent_home::held_home::prepare_held(
            &root,
            home,
            harness,
            &process.project,
            &given,
            process.caches,
        )
        .and_then(|()| crate::agent_home::record_skills(&process.project.data, home, &[]))
        .map_err(|problem| LaunchError(problem.0, None))?;
        if !given.text.is_empty() {
            text.push('\n');
            text.push_str(&given.text);
        }
    }
    write_inherited(&folder, "mcp.json", claude_config(&[])?.into_bytes())?;
    write_inherited(&folder, "instructions.md", text.into_bytes())?;
    let path = home.join(STATE);
    let installed = Installed {
        arguments: vec![
            OsString::from("--mcp-config"),
            path.join("mcp.json").into_os_string(),
            OsString::from("--append-system-prompt-file"),
            path.join("instructions.md").into_os_string(),
        ],
        files: Some(InstalledFiles {
            home: home.to_path_buf(),
            codex: false,
            rendered: Vec::new(),
            listed: false,
        }),
        transcripts: context.transcripts,
        hooks: false,
        relay: false,
        denied: denied_tools(&[], &context.off_tools),
        native: None,
        native_threads: false,
        model_proxy: context.model_proxy,
        freeze_record: context.freeze_record.clone(),
        terminal_display: context.terminal_display,
        environment: Vec::new(),
        engine_servers: Vec::new(),
        handover: None,
        tool_home: None,
        _lock: lock,
        held: Some(root),
    };
    assert!(installed.denied.is_empty());
    assert!(installed.model_proxy.is_none());
    Ok(installed)
}

/// Removes a held install's launch folder through the home it held, links as themselves,
/// before the drop releases the lock; a launch with no held home, or one `keep` left to a
/// running pane, removes nothing, since Windows never removes by path. Input: the launch.
pub(super) fn remove_held(installed: &mut Installed) {
    use crate::engine::bootstrap::held::{Name, TREE_DEPTH_MAX};
    if installed.files.take().is_some()
        && let Some(root) = &installed.held
    {
        let _ = root.remove_tree_unjudged(&Name::constant(STATE), TREE_DEPTH_MAX);
    }
}

/// Writes one file made in the held launch folder, create-only, with the folder's
/// inheritable list and no other handle writing it meanwhile, syncs it and checks its length
/// from the handle; the bytes are wiped after. Inputs: the folder, the file name and its
/// bytes. Output: Ok, `launch_context_unsafe` or `launch_context_unwritable`.
fn write_inherited(
    folder: &crate::engine::bootstrap::held::Folder,
    name: &'static str,
    mut bytes: Vec<u8>,
) -> Result<(), LaunchError> {
    use crate::engine::bootstrap::held::Name;
    use std::io::Write;
    let mut file = folder
        .make_file_sole(&Name::constant(name))
        .map_err(held_failure)?;
    let written = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| file.metadata());
    let length = bytes.len() as u64;
    kernel_security::secret::wipe(&mut bytes);
    match written {
        Ok(metadata) if metadata.len() == length => Ok(()),
        Ok(_) => Err(failure("launch_context_unsafe")),
        Err(_) => Err(failure("launch_context_unwritable")),
    }
}

/// Maps a held-folder refusal to the launch's code: a system failure is
/// `launch_context_unwritable`, anything else `launch_context_unsafe`. Input: the
/// refusal. Output: the launch error.
fn held_failure(error: crate::engine::bootstrap::held::HeldError) -> LaunchError {
    use crate::engine::bootstrap::held::HeldError;
    match error {
        HeldError::Unavailable => failure("launch_context_unwritable"),
        HeldError::Missing | HeldError::Unsafe | HeldError::InUse | HeldError::Full => {
            failure("launch_context_unsafe")
        }
    }
}

/// The judge the Windows install holds its home with, run on the held handle: a folder
/// whose facts [`judged_home`] passes. A read that fails refuses. Input: the held home.
/// Output: whether the home may be written.
fn held_home_judge(held: &File) -> bool {
    use crazyagent_win_sandbox::private;
    let folder = held.metadata().is_ok_and(|metadata| metadata.is_dir());
    match (private::facts(held), private::user_sid()) {
        (Ok(facts), Ok(user)) => folder && judged_home(&facts, &user),
        _ => false,
    }
}

/// Judges a held home's facts: the home check `validate_home` runs, and a label handed to
/// what is made in it that is at least medium with no-write-up, as the home's own label
/// must be. Inputs: the facts and the user's SID. Output: whether the home passes.
pub(super) fn judged_home(facts: &crazyagent_win_sandbox::private::Facts, user: &[u8]) -> bool {
    use crazyagent_win_sandbox::dacl;
    const MEDIUM_LEVEL: u32 = 0x2000;
    const NO_WRITE_UP: u32 = 0x1;
    let handed = match facts.label.as_deref().map(dacl::inherited_label) {
        None | Some(Ok(None)) => true,
        Some(Ok(Some((level, policy)))) => level >= MEDIUM_LEVEL && policy & NO_WRITE_UP != 0,
        Some(Err(_)) => false,
    };
    crate::harnesses::launch::judge_home(facts, user).is_ok() && handed
}
