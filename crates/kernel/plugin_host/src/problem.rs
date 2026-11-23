use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

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

impl Problem {
    /// A loopback plugin port inside the ports this Linux hands out by itself, where an agent
    /// could listen before the plugin's server does (Q-8): the field, the port and the range
    /// named, the same words on every screen. Inputs: the port and the range holding it.
    /// Output: the problem.
    pub fn port_handed_out(port: u16, handed: &RangeInclusive<u16>) -> Self {
        assert!(handed.contains(&port));
        let (low, high) = (handed.start(), handed.end());
        let mut problem = Self::new(PORT_HANDED_OUT, None, Some("http.url"));
        problem.message = format!(
            "http.url: port {port} is one this Linux hands out by itself (ports {low} to \
             {high}), so an agent could listen on it; run the plugin on a port outside that \
             range, or have an administrator move net.ipv4.ip_local_port_range away from port \
             {port}."
        );
        assert!(problem.message.starts_with("http.url: port "));
        problem
    }
}

/// The code of a loopback plugin port inside the ports this Linux hands out by itself.
pub const PORT_HANDED_OUT: &str = "app_plugin_port_handed_out";

fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The plain sentence for an app plugin code, for screens and the Problem message.
/// Input: the code. Output: the sentence, or none for a code this module does not own.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    PLAIN
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, plain)| *plain)
}

pub(crate) const PLAIN: &[(&str, &str)] = &[
    (
        "plugin_manifest_invalid",
        "the app cannot use this plugin.toml: it has a key the app does not read, misses one it needs, holds a value the app cannot use, or is not TOML text of at most 64 KiB.",
    ),
    (
        "plugin_id_invalid",
        "a plugin id is 1 to 64 lowercase letters, digits or hyphens.",
    ),
    (
        "app_plugin_schema_unknown",
        "this app reads version = 1 plugin files only.",
    ),
    (
        "app_plugin_kind_unknown",
        "the only kind this app reads is embedding.",
    ),
    (
        "app_plugin_field_unknown",
        "this field is not part of a plugin file.",
    ),
    ("app_plugin_field_missing", "this field is required."),
    (
        "app_plugin_field_invalid",
        "this value has the wrong type or length.",
    ),
    (
        "app_plugin_name_invalid",
        "a name is 1 to 40 lowercase letters, digits or -, starting with a letter.",
    ),
    (
        "app_plugin_url_invalid",
        "use a full URL with no user name, password or # part.",
    ),
    (
        "app_plugin_port_invalid",
        "the URL's port must be 1 to 65535.",
    ),
    (
        PORT_HANDED_OUT,
        "the URL's port is one this Linux hands out by itself, so an agent could listen on it; run the plugin on a port outside that range.",
    ),
    (
        "app_plugin_url_not_https",
        "the URL must use https; plain http is allowed only to 127.0.0.1, ::1 or localhost.",
    ),
    (
        "app_plugin_secret_needs_https",
        "a plugin that sends a secret must use https.",
    ),
    (
        "app_plugin_host_invalid",
        "a host is an exact lowercase name, with no * or port.",
    ),
    (
        "app_plugin_host_metadata",
        "the URL's host is a link-local or cloud metadata address, which no plugin may call.",
    ),
    (
        "app_plugin_host_undeclared",
        "the URL's host is not in hosts.",
    ),
    (
        "app_plugin_host_unused",
        "this host is declared but the plugin never calls it.",
    ),
    (
        "app_plugin_hosts_limit",
        "a plugin declares at most 4 hosts.",
    ),
    (
        "app_plugin_secret_invalid",
        "a secret name is 1 to 32 lowercase letters, digits or _, starting with a letter.",
    ),
    (
        "app_plugin_secrets_limit",
        "a plugin declares at most 4 secrets.",
    ),
    (
        "app_plugin_secret_duplicate",
        "each secret needs its own name.",
    ),
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
    (
        "app_plugin_inputs_repeated",
        "use \"{{inputs}}\" only once.",
    ),
    (
        "app_plugin_header_invalid",
        "a header name is letters, digits or -, and its value one line of plain text.",
    ),
    (
        "app_plugin_header_reserved",
        "the app sets this header itself.",
    ),
    (
        "app_plugin_headers_limit",
        "a plugin sets at most 16 headers.",
    ),
    (
        "app_plugin_vectors_invalid",
        "vectors is a path like data.*.embedding with exactly one *.",
    ),
    (
        "app_plugin_dimensions_invalid",
        "dimensions is a whole number from 64 to 1024.",
    ),
    (
        "app_plugin_batch_invalid",
        "batch is a whole number from 1 to 128.",
    ),
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
    (
        "app_plugin_folder_unavailable",
        "the plugins folder could not be read.",
    ),
    ("app_plugin_not_found", "no plugin has that name."),
    (
        "app_plugin_name_taken",
        "a plugin with this name already exists; remove it or choose replace.",
    ),
    (
        "app_plugin_bundled",
        "plugins that ship with the app can only be turned off.",
    ),
    (
        "app_plugin_changed",
        "the file changed after its trust screen was shown; review it again.",
    ),
    (
        "app_plugin_needs_review",
        "review this plugin's trust screen before turning it on.",
    ),
    (
        "app_plugin_refused",
        "this plugin's file is refused; fix it first.",
    ),
    (
        "app_plugin_past_cap",
        "only the first 64 plugin files by name are read; remove one to use this plugin.",
    ),
    ("app_plugin_off", "this plugin is turned off."),
    (
        "app_plugin_needs_key",
        "this plugin needs a key: pick an account or enter a key for it.",
    ),
    (
        "app_plugin_secret_not_declared",
        "this plugin does not declare that secret.",
    ),
    (
        "app_plugin_account_not_found",
        "that account no longer exists.",
    ),
    (
        "app_plugin_account_has_no_key",
        "that account has no saved API key to use.",
    ),
    (
        "app_plugin_key_invalid",
        "enter the key on one line with no spaces, up to 1024 characters.",
    ),
    (
        "app_plugin_key_store_unavailable",
        "the key could not be saved; nothing was changed.",
    ),
    (
        "app_plugin_request_invalid",
        "the request could not be read.",
    ),
    ("app_plugin_request_bounds", "the request is too large."),
    (
        "app_plugin_outcome_unknown",
        "the engine did not answer in time; check the list again.",
    ),
    (
        "app_plugins_unavailable",
        "the engine could not be reached.",
    ),
];
