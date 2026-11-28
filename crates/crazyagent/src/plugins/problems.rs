pub(super) const PROBLEMS: &[(&str, &str)] = &[
    ("skill_folder_not_found", "That folder was not found."),
    ("skill_not_a_folder", "Pick a folder, not a file."),
    (
        "skill_missing_skill_md",
        "This folder has no SKILL.md file.",
    ),
    (
        "skill_frontmatter_missing",
        "SKILL.md needs a name and description at the top.",
    ),
    (
        "skill_name_invalid",
        "The skill name in SKILL.md must be 1 to 64 lowercase letters, digits or hyphens.",
    ),
    (
        "skill_description_invalid",
        "The description in SKILL.md must be 1 to 1024 characters.",
    ),
    (
        "skill_description_hidden",
        "The description in SKILL.md has characters you cannot see, so agents could read something you did not approve.",
    ),
    (
        "skill_too_many_files",
        "A skill folder can hold up to 200 files.",
    ),
    ("skill_too_large", "A skill folder can be up to 8 MB."),
    (
        "skill_too_deep",
        "A skill folder can be up to 8 folders deep.",
    ),
    (
        "skill_link_outside",
        "This folder links to files outside it.",
    ),
    (
        "skill_file_unsupported",
        "This folder holds something that is not a file or folder.",
    ),
    (
        "skill_unreadable",
        "Some files in this folder can't be read.",
    ),
    (
        "skill_name_taken",
        "A skill with this name already exists here.",
    ),
    ("skill_limit", "You can add up to 100 skills here."),
    ("skill_not_found", "This skill was removed."),
    (
        "skill_text_too_large",
        "This SKILL.md is too large to show here.",
    ),
    (
        "skill_page_invalid",
        "This skill changed while it was read. Open it again.",
    ),
    (
        "skill_import_too_large",
        "A skill brought in from your skills folders can be up to 2 MB.",
    ),
    (
        "skill_import_name_changed",
        "This folder now holds a different skill. Look for your skills again.",
    ),
    (
        "skill_import_home_unknown",
        "Your home folder is unknown, so your skills can't be found.",
    ),
    (
        "server_name_invalid",
        "Use 1 to 64 lowercase letters, digits, - or _ for the name.",
    ),
    (
        "server_name_taken",
        "An MCP server with this name already exists here.",
    ),
    ("server_limit", "You can add up to 50 MCP servers here."),
    ("server_not_found", "This MCP server was removed."),
    #[cfg(unix)]
    (
        "server_command_invalid",
        "Enter the full path to the command, starting with /.",
    ),
    #[cfg(not(unix))]
    (
        "server_command_invalid",
        "Enter the full path to the command, such as C:\\Program Files\\nodejs\\npx.cmd.",
    ),
    (
        "server_args_invalid",
        "Use up to 32 arguments, each on one line, with at most 4 KB for the command and its arguments together.",
    ),
    (
        "server_url_invalid",
        on_this_computer!(
            "Use an https address. Plain http works only for ",
            ", and the address can't hold a password."
        ),
    ),
    (
        "server_secret_name_invalid",
        "Use letters, digits, _ or - for the secret name, starting with a letter.",
    ),
    ("server_secret_limit", "A server can have up to 16 secrets."),
    ("server_secret_duplicate", "Each secret needs its own name."),
    ("server_secret_not_found", "This secret was removed."),
    (
        "server_secret_invalid",
        "Enter the secret on one line, up to 8192 characters.",
    ),
    (
        "server_secret_too_short",
        "Use a secret of at least 8 characters. A shorter one is part of an agent's launch command, so CrazyAgent cannot keep it hidden.",
    ),
    (
        "server_changed",
        "This server changed. Review it again before approving.",
    ),
    (
        "server_check_command_only",
        "Only command servers can be checked from here.",
    ),
    (
        "server_check_busy",
        "Other checks are running. Try again in a moment.",
    ),
    (
        "server_command_not_found",
        "Can't start: command not found.",
    ),
    (
        "server_command_not_runnable",
        "Can't start: this file can't be run.",
    ),
    ("server_cant_start", "The server can't start."),
    (
        "server_batch_file",
        "This command is not a Windows program (.exe or .com), which Windows may start through cmd.exe, so CrazyAgent does not start it. Set the command to the tool's .exe, or to node.exe with the tool's script as the first argument.",
    ),
    (
        "server_stored_spec_invalid",
        "This server's command or address holds hidden or control characters, so it can't start. Remove it, then add it again.",
    ),
    (
        crate::sandbox::OWNER_PORTS_UNREADABLE,
        "CrazyAgent can't read its settings file, so it did not run the check. Fix the file and check again.",
    ),
    (
        "server_cleanup_uncertain",
        "The server's processes could not be confirmed stopped.",
    ),
    (
        crate::harnesses::leftovers::SANDBOX_LEFT,
        "The server left processes running after its check. CrazyAgent ends them.",
    ),
    (
        crate::harnesses::leftovers::SANDBOX_UNCHECKED,
        "The server's processes could not be listed after its check. CrazyAgent keeps looking for them and ends them.",
    ),
    ("server_exited", "The server stopped before answering."),
    (
        "server_check_timeout_powershell",
        "The server did not answer within 10 seconds. It runs on PowerShell, so the likely cause is PowerShell reading every module installed on this computer before its first command, which takes longer when many modules are installed.",
    ),
    (
        "server_check_timeout",
        "The server did not answer within 10 seconds.",
    ),
    (
        "server_setting_up",
        "context-mode is still being set up. Check again in a minute.",
    ),
    (
        "server_install_failed",
        "Setting up context-mode failed. Check again in 10 minutes.",
    ),
    (
        "server_npm_missing",
        "context-mode needs npm and node in the same folder as npx. Install Node.js, then check again.",
    ),
    (
        "server_version_unpinned",
        "This context-mode version is not the one the default pack pins, so CrazyAgent does not install it. Use the default pack's version.",
    ),
    (
        "server_installs_busy",
        "Too many installs are running. Check again in a minute.",
    ),
    (
        "server_node_too_old",
        "context-mode needs Node.js 22.5 or newer. Update Node.js, then check again.",
    ),
    (
        "server_npx_not_found",
        "CrazyAgent did not find the npx this context-mode server runs on this computer, so it did not start it.",
    ),
    (
        "server_node_folder_unreadable",
        "Windows does not let sandboxed programs read the Node.js folder context-mode runs from, and this Windows user cannot change that, so CrazyAgent did not start it.",
    ),
    (
        "server_node_drive_root_unreadable",
        "Windows does not let sandboxed programs read the root of a drive context-mode uses, so CrazyAgent did not start it. Run the CrazyAgent installer again and approve the administrator step.",
    ),
    (
        "server_output_too_large",
        "The server sent too much output.",
    ),
    ("server_refused", "The server refused the request."),
    (
        "server_reply_invalid",
        "The server's answer was not valid MCP.",
    ),
    ("plugin_project_unknown", "This project was not found."),
    ("plugin_not_found", "This item was removed."),
    (
        "plugin_switch_global_only",
        "Only items from global settings can be switched here.",
    ),
    (
        "stale_revision",
        "This changed in another window, so yours was not saved. The list shows it now.",
    ),
    (
        "plugin_store_unavailable",
        "CrazyAgent can't write its plugin folder.",
    ),
    (
        "plugin_store_unsafe",
        "CrazyAgent can't write its plugin folder.",
    ),
    (
        "plugin_secret_store_unavailable",
        "CrazyAgent can't write its plugin folder.",
    ),
    ("hooks_none_found", "This folder has no Claude Code hooks."),
    (
        "hooks_invalid",
        "The hooks file in this folder is not valid hooks JSON.",
    ),
    ("hooks_too_many", "This plugin has more than 64 hooks."),
    (
        "hooks_too_large",
        "The files these hooks run are larger than 16 MB or 2048 files.",
    ),
    (
        "hook_plugin_name_invalid",
        "The plugin name must be 1 to 32 letters, digits, ., - or _.",
    ),
    (
        "hooks_plugin_manifest_missing",
        "This folder has hooks but no .claude-plugin/plugin.json naming the plugin.",
    ),
    (
        "hooks_already_added",
        "These hooks were already added here.",
    ),
    ("hook_limit", "There are already 100 hooks here."),
    (
        "tool_unknown",
        "That tool is not on this server. Check the server again to list its tools.",
    ),
    (
        "tool_delete_builtin_only",
        "Only a CrazyAgent tool can be deleted. Turn an MCP server's tool off instead.",
    ),
    (
        "coord_tool_invalid",
        "This tool's name has characters a switch can't hold. Only letters, digits, _, - and . work.",
    ),
];
