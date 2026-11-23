/// The plain words the owner reads for each permission a plugin may hold, keyed by the
/// permission name, one row per entry of the generated permission table, sent with the
/// plugin listing so every screen shows the same text.
pub const WORDS: [(&str, &str); 5] = [
    (
        "net.request",
        "Send requests to the internet addresses it lists",
    ),
    ("secrets.find", "Look up a key it saved before"),
    ("secrets.forget", "Delete a key it saved"),
    ("secrets.put_from_owner", "Ask you for a key and keep it"),
    (
        "settings.save_quit",
        "Save what the agents were doing when the app quits",
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
