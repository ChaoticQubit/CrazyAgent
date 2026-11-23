use std::path::Path;
use std::process::Command;

/// What the project access lists read from the engine that runs them, passed in so this
/// crate names no engine module. The engine passes its one value; rows pass a stub.
pub struct AccessHooks {
    /// The owner's git for a project folder, ready to take arguments. Input: the canonical
    /// project folder. Output: the command, `Ok(None)` when the folder holds no
    /// repository, or `Err(())` when the owner's git is refused or cannot start.
    pub owner_git: fn(&Path) -> Result<Option<Command>, ()>,
}
