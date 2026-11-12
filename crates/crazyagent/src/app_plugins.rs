use serde::{Deserialize, Serialize};

pub mod manifest;
pub mod store;
pub mod yaml;

/// The folder beside the engine database that holds the owner's own plugin files, one
/// `<name>.yaml` per plugin.
pub const FOLDER: &str = "app-plugins";
/// The most user plugin files the engine reads from the folder, in name order.
pub const USER_PLUGINS_MAX: usize = 64;
/// The bundled plugin that runs the built-in local embedding runtime.
pub const LOCAL_PLUGIN: &str = "local";

/// Why a manifest or a plugin request was refused: a stable code, the manifest line or
/// field it is about when there is one, and one plain sentence naming them. Never holds a
/// secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    pub code: String,
    pub line: Option<u32>,
    pub field: Option<String>,
    pub message: String,
}

impl Problem {
    /// Builds a problem with its plain sentence. Inputs: a known code, the line (none for
    /// the whole file) and the field path (none when the line says enough). Output: the
    /// problem; an unknown code is a programming error.
    pub fn new(code: &'static str, line: Option<u32>, field: Option<&str>) -> Self {
        let plain = plain_problem(code).expect("every app plugin code has a plain sentence");
        let message = match (line, field) {
            (Some(line), _) if line > 0 => format!("Line {line}: {plain}"),
            (_, Some(field)) => format!("{field}: {plain}"),
            _ => upper_first(plain),
        };
        Self {
            code: code.to_owned(),
            line,
            field: field.map(str::to_owned),
            message,
        }
    }
}

fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// What a plugin does for the app. Only embedding plugins exist now (DEC-156).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Embedding,
}

/// Where a plugin comes from: shipped inside the app, or a file in the owner's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Bundled,
    User,
}

/// How a plugin runs, as its trust screen shows it: the app's own local runtime with the
/// size of its one-time download, or the HTTP API it calls, with the exact hosts it reaches,
/// whether that is plain http to this computer ("local, not encrypted") and the vector size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunsView {
    Local {
        runtime: String,
        download_bytes: u64,
    },
    Http {
        url: String,
        hosts: Vec<String>,
        local_unencrypted: bool,
        dimensions: u16,
    },
}

/// Where one declared secret's value comes from: nowhere yet, a saved API-key account's
/// key (by its credential reference, with the account's name), or a key the owner entered
/// for this plugin. The value itself is never shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Binding {
    None,
    Account { account: String, name: String },
    Key,
}

/// One secret a plugin declares, by name, and where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretView {
    pub name: String,
    pub binding: Binding,
}

/// Whether a plugin can be used: ready, a user file the owner has not reviewed since it
/// changed, a plugin whose declared secrets are not all bound, or a file the engine refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginStatus {
    Ready,
    NeedsReview,
    NeedsKey,
    Refused { problem: Problem },
}

/// One plugin as the App plugins page lists it. A refused file has its file name as name
/// and title and no `runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginView {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub origin: Origin,
    pub status: PluginStatus,
    pub enabled: bool,
    pub runs: Option<RunsView>,
    pub secrets: Vec<SecretView>,
    pub sha256: String,
}

/// Every plugin the engine knows, bundled first then the owner's files in name order, and
/// whether the folder held more than `USER_PLUGINS_MAX` files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    pub plugins: Vec<PluginView>,
    pub more: bool,
}

/// The trust screen of a manifest before it is added or trusted: what it is, how it runs,
/// the hosts it reaches, the secrets it asks for by name, the SHA-256 of its exact bytes
/// (the add or trust request must name it) and whether it replaces a user plugin of the
/// same name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustView {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub kind: PluginKind,
    pub runs: RunsView,
    pub secrets: Vec<String>,
    pub sha256: String,
    pub replaces: bool,
}

/// A manifest read for its trust screen: ready to add, or refused with the line or field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Preview {
    Ready { trust: TrustView },
    Refused { problem: Problem },
}

/// The plain sentence for an app plugin code, for screens and the Problem message.
/// Input: the code. Output: the sentence, or none for a code this module does not own.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    PLAIN
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, plain)| *plain)
}

const PLAIN: &[(&str, &str)] = &[
    ("app_plugin_yaml_too_large", "the file is larger than 16 KiB."),
    ("app_plugin_yaml_encoding", "the file is not UTF-8 text."),
    ("app_plugin_yaml_empty", "the file has no settings."),
    ("app_plugin_yaml_tab", "use spaces, not tabs, to indent."),
    ("app_plugin_yaml_anchor", "anchors (&name) are not supported."),
    ("app_plugin_yaml_alias", "aliases (*name) are not supported."),
    ("app_plugin_yaml_tag", "tags (!name) are not supported."),
    (
        "app_plugin_yaml_block_scalar",
        "block text (| or >) is not supported; write the value on one line in quotes.",
    ),
    (
        "app_plugin_yaml_flow_map",
        "{ } maps are not supported; put each key on its own line, and quote a value that starts with {.",
    ),
    ("app_plugin_yaml_directive", "% directives are not supported."),
    (
        "app_plugin_yaml_documents",
        "a plugin file holds one document; remove this --- or ... line.",
    ),
    ("app_plugin_yaml_duplicate_key", "this key is already set above."),
    (
        "app_plugin_yaml_indent",
        "the indent does not match the lines around it.",
    ),
    (
        "app_plugin_yaml_list_map",
        "list items are single values; lists of maps are not supported.",
    ),
    (
        "app_plugin_yaml_list_item",
        "write each list item as - value on one line.",
    ),
    (
        "app_plugin_yaml_quote",
        "close the quote on the same line, with nothing after it but a # comment.",
    ),
    (
        "app_plugin_yaml_escape",
        "only \\\" \\\\ \\/ \\n \\t and \\uXXXX escapes are supported.",
    ),
    (
        "app_plugin_yaml_key",
        "keys are plain words of letters, digits, _ . or -, without quotes.",
    ),
    ("app_plugin_yaml_complex_key", "? keys are not supported."),
    (
        "app_plugin_yaml_reserved",
        "values cannot start with @ or `; put the value in quotes.",
    ),
    (
        "app_plugin_yaml_syntax",
        "expected key: value; put a value holding \": \" in quotes.",
    ),
    ("app_plugin_yaml_top_level", "the file must be a map of key: value lines."),
    (
        "app_plugin_yaml_flow_list",
        "[ ] lists hold single values on one line, closed with ].",
    ),
    ("app_plugin_yaml_depth", "the settings are nested too deep."),
    ("app_plugin_schema_unknown", "this app reads schema: 1 plugins only."),
    ("app_plugin_kind_unknown", "the only kind is embedding."),
    ("app_plugin_field_unknown", "this field is not part of a plugin file."),
    ("app_plugin_field_missing", "this field is required."),
    ("app_plugin_field_invalid", "this value has the wrong type or length."),
    (
        "app_plugin_name_invalid",
        "a name is 1 to 40 lowercase letters, digits or -, starting with a letter.",
    ),
    (
        "app_plugin_runs_invalid",
        "a plugin has exactly one of local or http.",
    ),
    (
        "app_plugin_runtime_unknown",
        "this app has no local runtime with that id.",
    ),
    (
        "app_plugin_url_invalid",
        "use a full URL with no user name, password or # part.",
    ),
    (
        "app_plugin_url_not_https",
        "the URL must use https; plain http is allowed only to 127.0.0.1, ::1 or localhost.",
    ),
    (
        "app_plugin_secret_needs_https",
        "a plugin that sends a secret must use https.",
    ),
    ("app_plugin_host_invalid", "a host is an exact lowercase name, with no * or port."),
    ("app_plugin_host_undeclared", "the URL's host is not in hosts."),
    ("app_plugin_host_unused", "this host is declared but the plugin never calls it."),
    ("app_plugin_hosts_limit", "a plugin declares at most 4 hosts."),
    (
        "app_plugin_secret_invalid",
        "a secret name is 1 to 32 lowercase letters, digits or _, starting with a letter.",
    ),
    ("app_plugin_secrets_limit", "a plugin declares at most 4 secrets."),
    ("app_plugin_secret_duplicate", "each secret needs its own name."),
    (
        "app_plugin_secret_undeclared",
        "this secret is not in the plugin's secrets list.",
    ),
    (
        "app_plugin_secret_unused",
        "this secret is declared but no header uses it.",
    ),
    (
        "app_plugin_secret_outside_headers",
        "a secret may only be used inside a header value.",
    ),
    (
        "app_plugin_placeholder_unknown",
        "the only placeholders are {{inputs}}, {{purpose}} and {{secret.<name>}} in a header.",
    ),
    (
        "app_plugin_inputs_missing",
        "the body needs one value of exactly \"{{inputs}}\".",
    ),
    ("app_plugin_inputs_repeated", "use \"{{inputs}}\" only once."),
    (
        "app_plugin_header_invalid",
        "a header name is letters, digits or -, and its value one line of plain text.",
    ),
    (
        "app_plugin_header_reserved",
        "the app sets this header itself.",
    ),
    ("app_plugin_headers_limit", "a plugin sets at most 16 headers."),
    (
        "app_plugin_vectors_invalid",
        "vectors is a path like data.*.embedding with exactly one *.",
    ),
    (
        "app_plugin_dimensions_invalid",
        "dimensions is a whole number from 64 to 1024.",
    ),
    ("app_plugin_batch_invalid", "batch is a whole number from 1 to 128."),
    (
        "app_plugin_file_name_mismatch",
        "the file name must be the plugin's name followed by .yaml.",
    ),
    (
        "app_plugin_file_unsafe",
        "the file is not a plain file owned by you; it was not read.",
    ),
    (
        "app_plugin_folder_unsafe",
        "the plugins folder is not a private folder owned by you.",
    ),
    ("app_plugin_folder_unavailable", "the plugins folder could not be read."),
    ("app_plugin_not_found", "no plugin has that name."),
    (
        "app_plugin_name_taken",
        "a plugin with this name already exists; remove it or choose replace.",
    ),
    ("app_plugin_bundled", "plugins that ship with the app can only be turned off."),
    (
        "app_plugin_changed",
        "the file changed after its trust screen was shown; review it again.",
    ),
    (
        "app_plugin_needs_review",
        "review this plugin's trust screen before turning it on.",
    ),
    ("app_plugin_refused", "this plugin's file is refused; fix it first."),
    ("app_plugin_secret_not_declared", "this plugin does not declare that secret."),
    ("app_plugin_account_not_found", "that account no longer exists."),
    (
        "app_plugin_account_has_no_key",
        "that account has no saved API key to use.",
    ),
    ("app_plugin_key_invalid", "enter the key on one line, up to 8192 characters."),
    (
        "app_plugin_key_store_unavailable",
        "the key could not be saved; nothing was changed.",
    ),
    ("app_plugin_request_invalid", "the request could not be read."),
    ("app_plugin_request_bounds", "the request is too large."),
    ("app_plugin_outcome_unknown", "the engine did not answer in time; check the list again."),
    ("app_plugins_unavailable", "the engine could not be reached."),
    (
        "search_confirm_required",
        "choosing a plugin that sends data off this Mac needs a confirm naming its host.",
    ),
    (
        "search_choice_unavailable",
        "that plugin is off, not ready or not an embedding plugin.",
    ),
    ("search_model_missing", "the local model is not downloaded yet."),
    ("search_model_downloading", "the local model is already downloading."),
    (
        "search_model_download_failed",
        "the local model could not be downloaded; check the connection and try again.",
    ),
    (
        "search_model_mismatch",
        "the downloaded file is not the expected model, so it was deleted.",
    ),
    (
        "search_model_cancelled",
        "the download was cancelled.",
    ),
    (
        "search_model_store_unavailable",
        "the model folder could not be read or written.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_code_has_one_plain_sentence() {
        for (index, (code, plain)) in PLAIN.iter().enumerate() {
            assert!(
                code.starts_with("app_plugin") || code.starts_with("search_"),
                "{code}"
            );
            assert!(!plain.is_empty() && !plain.contains('\u{2014}'), "{code}");
            assert!(
                PLAIN[index + 1..].iter().all(|(other, _)| other != code),
                "{code} twice"
            );
        }
    }

    #[test]
    fn a_problem_names_its_line_or_field_or_stands_alone() {
        let line = Problem::new("app_plugin_yaml_tab", Some(3), None);
        assert_eq!(line.message, "Line 3: use spaces, not tabs, to indent.");
        let field = Problem::new("app_plugin_host_undeclared", None, Some("http.url"));
        assert_eq!(field.message, "http.url: the URL's host is not in hosts.");
        let whole = Problem::new("app_plugin_yaml_empty", None, None);
        assert_eq!(whole.message, "The file has no settings.");
    }
}
