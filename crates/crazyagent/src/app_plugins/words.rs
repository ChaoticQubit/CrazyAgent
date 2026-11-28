/// The plain words the owner reads for each permission a plugin may hold, keyed by the
/// permission name, one row per entry of the generated permission table, sent with the
/// plugin listing so every screen shows the same text.
pub const WORDS: [(&str, &str); 16] = [
    (
        "editor.spawn",
        "Open one of your projects in the code editor",
    ),
    (
        "net.request",
        "Send requests to the internet addresses it lists",
    ),
    (
        "process.install",
        "Install or update a tool on this computer",
    ),
    ("project_rows.page", "Read the list of your projects"),
    ("projects.list", "See the list of your projects"),
    (
        "sandbox.save_settings",
        "Change the sandbox settings that limit what agents can reach",
    ),
    (
        "sandbox.settings",
        "Read the sandbox settings that limit what agents can reach",
    ),
    ("secrets.find", "Look up a key it saved before"),
    ("secrets.forget", "Delete a key it saved"),
    ("secrets.put_from_owner", "Ask you for a key and keep it"),
    (
        "service.install",
        "Set up the service that starts the engine when you log in",
    ),
    ("service.repair", "Repair the engine's login service"),
    (
        "service.status",
        "Check whether the engine's login service is set up and running",
    ),
    ("service.uninstall", "Remove the engine's login service"),
    (
        "settings.save_quit",
        "Save what the agents were doing when the app quits",
    ),
    (
        "update.set_channel",
        "Change which update channel the app follows",
    ),
];

/// The plain words for one permission. Input: the permission name. Output: the words, or
/// none for a name the table does not hold.
pub fn words(permission: &str) -> Option<&'static str> {
    WORDS
        .iter()
        .find(|(name, _)| *name == permission)
        .map(|(_, words)| *words)
}
