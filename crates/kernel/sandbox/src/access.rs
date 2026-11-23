use std::path::{Path, PathBuf};
use std::process::Command;

/// What the project access lists read from the engine that runs them, passed in so this
/// crate names no engine module. The engine passes its one value; rows pass a stub.
pub struct AccessHooks {
    /// The owner's git for a project folder, ready to take arguments. Input: the canonical
    /// project folder. Output: the command, `Ok(None)` when the folder holds no
    /// repository, or `Err(())` when the owner's git is refused or cannot start.
    pub owner_git: fn(&Path) -> Result<Option<Command>, ()>,
    /// The folders and files in the owner's home that belong to a harness, which agents
    /// never read and no granted folder may sit inside. No input. Output: the names,
    /// relative to the home.
    pub harness_folders: fn() -> Vec<&'static str>,
    /// The folder that holds a project's engine files. Input: the project folder. Output:
    /// its `.crazyagent/` folder when that is a real folder, else the project folder
    /// itself, as for a project made before the projects home.
    pub state_folder: fn(&Path) -> PathBuf,
    /// Every path below a project folder git does not track, a wholly untracked folder once
    /// with a closing `/`. Input: the project folder. Output: the paths, or `None` when git
    /// cannot list them or the system has no such list.
    pub untracked: fn(&Path) -> Option<Vec<String>>,
}
