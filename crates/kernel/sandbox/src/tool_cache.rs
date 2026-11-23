/// What the tool cache asks of the engine that hosts it: clearing an agent's pinning flags,
/// removing a folder an agent wrote and building the priming process. The engine passes one
/// value; rows pass stand-ins.
#[derive(Debug)]
pub struct ToolCacheHooks {
    /// Clears the immutable and append-only flags an agent may set on an entry, never
    /// following a link. Inputs: the open parent and the entry's name. Output: none.
    pub unpin: fn(std::os::fd::BorrowedFd<'_>, &str),
    /// Removes a folder an agent wrote with all it holds. Input: the folder. Output: done
    /// once nothing has its name, or the error naming why it stays.
    pub remove_agent_tree: fn(&std::path::Path) -> std::io::Result<()>,
    /// Builds the unspawned priming process. Inputs: the priming folder and the canonical uv
    /// binary. Output: the command.
    pub priming_command: fn(&std::path::Path, &std::path::Path) -> std::process::Command,
}

#[cfg(test)]
mod tests;
