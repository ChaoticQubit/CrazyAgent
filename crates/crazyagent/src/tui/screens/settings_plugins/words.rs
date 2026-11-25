pub(super) const COMMAND_HINT: &str = if cfg!(target_os = "windows") {
    "Use the full path of the command, such as C:\\Users\\you\\.local\\bin\\uvx.exe."
} else if cfg!(target_os = "linux") {
    "Use the full path of the command, such as /usr/local/bin/uvx."
} else {
    "Use the full path of the command, such as /opt/homebrew/bin/uvx."
};
