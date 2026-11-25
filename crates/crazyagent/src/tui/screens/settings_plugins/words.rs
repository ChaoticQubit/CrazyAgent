pub(super) const COMMAND_HINT: &str = if cfg!(target_os = "windows") {
    "Use the full path of the command, such as C:\\Users\\you\\.local\\bin\\uvx.exe."
} else if cfg!(target_os = "linux") {
    "Use the full path of the command, such as /usr/local/bin/uvx."
} else {
    "Use the full path of the command, such as /opt/homebrew/bin/uvx."
};

/// The hint under an MCP server address that is plain http to another computer.
pub(super) const URL_HINT: &str =
    on_this_computer!("Use an https address. Plain http works only for ", ".");
