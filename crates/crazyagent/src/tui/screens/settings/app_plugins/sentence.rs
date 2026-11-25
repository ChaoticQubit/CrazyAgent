use crate::tui::text::coded;

/// One plain sentence for a refused App plugins request. Input: the code. Output: the
/// sentence, or the plain fallback naming the code.
pub(in crate::tui) fn sentence(code: &str) -> String {
    let known = match code {
        "app_plugin_outcome_unknown"
        | "owner_outcome_unknown_reconcile_same_identity"
        | "owner_actor_stopped_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. Check the list before you try again."
        }
        "app_plugin_request_bounds" => "This plugin request was too large to send.",
        "app_plugin_request_invalid" => {
            "The engine didn't accept this plugin request. Read the page again and try again."
        }
        "app_plugins_unavailable" => "App plugins aren't available in this engine yet.",
        "app_plugins_readonly" => "App plugins can't be changed while the engine is read-only.",
        "app_plugin_not_found" => {
            "That plugin is gone. Open App plugins again to see the current ones."
        }
        "app_plugin_name_taken" => {
            "A built-in plugin already has that name. Give your plugin another name."
        }
        "app_plugin_name_used" => {
            "A plugin with that name was added after you reviewed this one. Review it again to replace it."
        }
        "app_plugin_bundled" => "Built-in plugins can be turned off but not removed or replaced.",
        "app_plugin_essential" => "The app relies on this plugin, so it stays on.",
        "app_plugin_changed" => {
            "The plugin.toml changed after you checked it. Check it again before you approve it."
        }
        "app_plugin_needs_review" => {
            "This plugin's file changed or is new. Review it before it runs."
        }
        "app_plugin_refused" => "CrazyAgent refused this plugin. Its reason is shown with it.",
        "app_plugin_past_cap" => {
            "The app reads only the first 64 plugins by name, old plugin files included. Remove one of them to use this plugin."
        }
        "app_plugin_port_handed_out" => {
            "This plugin calls a port the app gave to something else on this computer, so it can't be added."
        }
        "app_plugin_folder_unsafe" => {
            "Your plugins folder isn't private to you, so CrazyAgent won't read it. Make it yours alone and try again."
        }
        "app_plugin_folder_unavailable" => {
            "CrazyAgent couldn't open your plugins folder. Try again."
        }
        "app_plugin_secret_not_declared" => "This plugin doesn't declare that secret.",
        "app_plugin_account_not_found" => "That account is gone. Pick another one or enter a key.",
        "app_plugin_account_has_no_key" => {
            "That account has no saved API key to share. Pick another one or enter a key."
        }
        "app_plugin_key_invalid" => {
            "That key can't be used: it is empty, too long or has spaces or control characters."
        }
        "app_plugin_key_store_unavailable" => {
            "CrazyAgent couldn't reach its saved keys. Nothing was changed; try again."
        }
        "app_plugin_setting_unavailable" => {
            "This setting can't be saved while the app has no settings file or is applying a change. Try again."
        }
        "app_plugin_setting_unreadable" => {
            "Your settings file couldn't be read, so CrazyAgent didn't change it. Fix the file and try again."
        }
        "app_plugin_setting_unsafe" => {
            "Your settings file is a link or shared with another name, so CrazyAgent won't write it."
        }
        "owner_entropy_unavailable" => {
            "This computer couldn't make a random value for the request. Nothing changed; try again."
        }
        _ => return coded("CrazyAgent couldn't do that.", code),
    };
    known.to_owned()
}
