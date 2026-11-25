/// What a running check of an account's sign-in says, the same for every harness.
pub(in crate::tui::screens) const CHECK_LINE: &str = "Checking the sign-in this account already has in CrazyAgent. Nothing else on this computer is read.";

/// The sentence for each sign-in failure code the engine can record: the words the app's sign-in
/// panel shows, so both clients say the same thing, except `native_auth_recovery_required`:
/// the app names its own button, and the TUI says that the engine clears it on a later try
/// or after an engine restart. `{harness}` and `{name}` stand for the harness and provider of the
/// sign-in method.
pub(super) const SIGN_IN_SENTENCES: [(&str, &str); 31] = [
    (
        "native_auth_bootstrap_timeout",
        "CrazyAgent's account helper didn't start in time. Try again.",
    ),
    ("native_auth_cancelled", "Sign-in cancelled."),
    (
        "native_auth_claude_credential_expiring",
        "Claude Code couldn't renew this account's sign-in before it expires. Try again.",
    ),
    ("native_auth_claude_deadline", "This code expired."),
    (
        "native_auth_claude_destination_unsupported",
        "Claude Code gave a sign-in page address CrazyAgent doesn't recognize. Update CrazyAgent and Claude Code, then try again.",
    ),
    (
        "native_auth_claude_input_invalid",
        "Paste the complete code from the Claude page.",
    ),
    (
        "native_auth_claude_keychain_unavailable",
        "CrazyAgent couldn't set up this account's own keychain for Claude Code. Try again. If it keeps failing, delete the account, then add it again.",
    ),
    (
        "native_auth_claude_launch_failed",
        "Claude Code couldn't start. Check that Claude Code is installed and opens on {computer}, then try again.",
    ),
    (
        "native_auth_claude_native_exit",
        "Claude Code stopped before it finished. Try again.",
    ),
    (
        "native_auth_claude_output_invalid",
        "Claude Code didn't finish because of a problem CrazyAgent couldn't handle. Try again, and update Claude Code if it keeps happening.",
    ),
    (
        "native_auth_claude_profile_mismatch",
        "Claude Code's settings folder for this account didn't match the one CrazyAgent set up. Try again, and update Claude Code if it keeps happening.",
    ),
    (
        "native_auth_claude_status_invalid",
        "Claude Code's account check didn't give an answer CrazyAgent could use. Update Claude Code, then try again.",
    ),
    (
        "native_auth_claude_subscription_unsupported",
        "This account isn't signed in with a Claude subscription. Sign in with a Claude Pro or Max account, or use an API key instead.",
    ),
    (
        "native_auth_codex_deadline",
        "Codex didn't answer in time. Try again.",
    ),
    (
        "native_auth_codex_exited_before_observation",
        "Codex stopped before it finished. Try again.",
    ),
    (
        "native_auth_codex_io_failed",
        "CrazyAgent lost contact with Codex. Try again.",
    ),
    (
        "native_auth_codex_protocol_invalid",
        "Codex gave an answer CrazyAgent couldn't use. Update Codex, then try again.",
    ),
    (
        "native_auth_harness_too_old",
        "{harness} is too old to sign in from CrazyAgent. Update {harness}, then choose Check again.",
    ),
    (
        "native_auth_host_executable_changed",
        "{harness} changed on {computer} after CrazyAgent checked it, perhaps from an update. Try again.",
    ),
    (
        "native_auth_host_executable_unsafe",
        "{harness} on {computer} isn't safe to start, so CrazyAgent didn't start it. Reinstall {harness}, then try again.",
    ),
    (
        "native_auth_host_unavailable",
        "CrazyAgent didn't find a {harness} it can run on {computer}, so {name} sign-in isn't available. Install {harness}, then choose Check again, or choose its file.",
    ),
    (
        "native_auth_interrupted",
        "Sign-in didn't finish. Try again.",
    ),
    (
        "native_auth_login_failed",
        "Sign-in didn't finish. Try again.",
    ),
    (
        "native_auth_native_request_failed",
        "Sign-in didn't finish. Try again.",
    ),
    (
        "native_auth_recovery_required",
        "This sign-in didn't confirm how it ended, so changes to its account and new sign-ins wait. Wait a few minutes, then try again. If this keeps happening, restart the CrazyAgent engine.",
    ),
    ("native_auth_sign_in_expired", "This code expired."),
    (
        "native_auth_sign_in_page_timed_out",
        "CrazyAgent stopped waiting for the sign-in page. Start the sign-in again.",
    ),
    (
        "native_auth_sign_in_timed_out",
        "CrazyAgent stopped waiting for this code. Get a new code and enter it within 15 minutes.",
    ),
    (
        "native_profile_custody_unsafe",
        "CrazyAgent couldn't safely use this account's sign-in folder on {computer}, so it stopped. Delete the account, then add it again.",
    ),
    (
        "native_profile_identity_changed",
        "This account's sign-in folder no longer matches what CrazyAgent recorded, so CrazyAgent stopped. Delete the account, then add it again.",
    ),
    (
        "native_profile_in_use",
        "An agent is using this account now. Try again after it stops.",
    ),
];
