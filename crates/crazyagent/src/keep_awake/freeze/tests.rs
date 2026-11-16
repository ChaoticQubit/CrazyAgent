use super::serves;
use std::path::PathBuf;

/// Leader 01:57 (until T17b): a tmux server counts as this engine's only when its `-S`
/// socket sits in one of this engine's socket folders: its terminal folder or an agent's
/// short temp folder. The owner's own tmux, another engine's and a look-alike folder do
/// not count.
#[test]
fn only_a_tmux_server_on_a_socket_of_this_engine_counts() {
    let folders = [
        PathBuf::from("/Users/o/Library/Application Support/CrazyAgent/terminal"),
        PathBuf::from("/private/tmp/ca-0123456789abcdef"),
    ];
    let ours = [
        "/opt/homebrew/bin/tmux -S /Users/o/Library/Application Support/CrazyAgent/terminal/tmux.sock -C new-session -d",
        "/opt/homebrew/Cellar/tmux/3.8/bin/tmux -D -u -f /dev/null -S /private/tmp/ca-0123456789abcdef/tmux.sock",
    ];
    for args in ours {
        assert!(serves(args, &folders), "{args}");
    }
    let others = [
        "/opt/homebrew/bin/tmux",
        "/opt/homebrew/bin/tmux new-session -d",
        "/opt/homebrew/bin/tmux -S /private/tmp/ca-hook-pane-4dS799/t.sock new-session",
        "/opt/homebrew/bin/tmux -S /Users/o/Library/Application Support/CrazyAgent/terminalx/tmux.sock",
        "/opt/homebrew/bin/tmux -S /private/tmp/ca-0123456789abcdef0/tmux.sock",
        "/opt/homebrew/bin/tmux attach -t /private/tmp/ca-0123456789abcdef/tmux.sock",
    ];
    for args in others {
        assert!(!serves(args, &folders), "{args}");
    }
}
