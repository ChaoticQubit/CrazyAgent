use super::{LOOKUP_FAILED, NOT_FOUND, OUTCOME_UNKNOWN, UNKNOWN_INSTALL};
use crate::harnesses::finding;

pub(super) const PATH_COPY: [(&str, &str); 13] = [
    (
        finding::UNKNOWN_HARNESS,
        "You can only choose the file for Codex and Claude Code.",
    ),
    (finding::PATH_INVALID, "That isn't a full path to one file."),
    (finding::PATH_NOT_FOUND, "There's no file there."),
    (
        finding::PATH_NOT_EXECUTABLE,
        "That file isn't a program this Mac can run.",
    ),
    (
        finding::PATH_AGENT_SPACE,
        "That file is in CrazyAgent's own folders, where agents work, so CrazyAgent won't run it.",
    ),
    (
        finding::PATH_SHIM,
        "That file is a Volta, asdf or mise shim, which starts another program. Choose the real file instead.",
    ),
    (
        finding::PATH_CHANGEABLE,
        "Another user on this Mac can change that file or a folder above it, so CrazyAgent won't run it.",
    ),
    (
        finding::PATH_UNREADABLE,
        "CrazyAgent couldn't read or save the chosen file. Try again.",
    ),
    (
        "native_auth_host_executable_unsafe",
        "Another user on this Mac can change that file, or it isn't a plain program file, so CrazyAgent won't run it.",
    ),
    (
        "native_auth_host_executable_changed",
        "That file changed while CrazyAgent checked it. Try again.",
    ),
    (
        "native_auth_host_unavailable",
        "The engine can't add a harness right now. Restart the engine, then try again.",
    ),
    (
        "native_auth_host_registration_frozen",
        "The engine is stopping, so it can't add a harness now. Try again once it has restarted.",
    ),
    ("owner_runtime_stale", "The engine restarted. Try again."),
];
pub(super) const COPY: [(&str, &str); 28] = [
    (
        UNKNOWN_INSTALL,
        "CrazyAgent can't tell how this was installed, so it won't update it.",
    ),
    (
        "harness_update_with_app",
        "Comes with CrazyAgent and updates with it.",
    ),
    (
        "harness_update_tool_missing",
        "The tool that updates it is no longer on this Mac.",
    ),
    (
        "harness_update_tool_unsafe",
        "The tool that updates it can be changed by another user, so CrazyAgent won't run it.",
    ),
    (
        "harness_update_needs_admin",
        "Updating it needs an administrator. Update it yourself.",
    ),
    (
        "harness_update_as_root",
        "CrazyAgent is running as the root user, so it won't run an update.",
    ),
    ("harness_update_off", "Harness updates are off in settings."),
    (
        "harness_update_check_only",
        "Harness updates are set to Check only.",
    ),
    (
        "harness_update_busy",
        "This harness is already updating or installing.",
    ),
    (
        "harness_update_not_installed",
        "It isn't installed on this Mac.",
    ),
    (
        "harness_update_timeout",
        "The update took too long and was stopped.",
    ),
    ("harness_update_failed", "The update didn't finish."),
    (
        "harness_update_unsupported_here",
        "CrazyAgent can't update it on this computer.",
    ),
    (LOOKUP_FAILED, "Couldn't check for a newer version."),
    (
        "harness_update_check_unreadable",
        "Couldn't read the newer version from the vendor's site.",
    ),
    (
        "harness_update_unknown_harness",
        "CrazyAgent doesn't know this harness.",
    ),
    ("harness_update_outcome_unknown", OUTCOME_UNKNOWN),
    (
        "harness_update_check_failed",
        "The check for a newer version didn't finish.",
    ),
    (
        "harness_update_start_failed",
        "CrazyAgent couldn't start the tool that updates or installs it.",
    ),
    (
        "harness_update_settings_unreadable",
        "CrazyAgent couldn't read the harness update settings.",
    ),
    (
        "harness_update_unavailable",
        "The engine couldn't open its update records. Try again.",
    ),
    (
        "harness_install_no_tool",
        "CrazyAgent installs with Homebrew or npm and found no way to install this one.",
    ),
    (
        "harness_install_off",
        "Install from the app is off in settings.",
    ),
    (
        "harness_install_already",
        "It's already installed on this Mac.",
    ),
    (
        NOT_FOUND,
        "The install finished, but CrazyAgent can't find the harness. Choose Check again.",
    ),
    ("harness_install_failed", "The install didn't finish."),
    (
        "harness_install_needs_node",
        "CrazyAgent installs this one with npm, and Node.js isn't on this Mac yet.",
    ),
    (
        "harness_install_list_invalid",
        "CrazyAgent couldn't install that list of harnesses. Pick them again.",
    ),
];
