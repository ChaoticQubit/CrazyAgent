use serde::{Deserialize, Serialize};

pub mod yaml;

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

/// The plain sentence for an app plugin code, for screens and the Problem message.
/// Input: the code. Output: the sentence, or none for a code this module does not own.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    PLAIN
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, plain)| *plain)
}

const PLAIN: &[(&str, &str)] = &[
    (
        "app_plugin_yaml_too_large",
        "the file is larger than 16 KiB.",
    ),
    ("app_plugin_yaml_encoding", "the file is not UTF-8 text."),
    ("app_plugin_yaml_empty", "the file has no settings."),
    ("app_plugin_yaml_tab", "use spaces, not tabs, to indent."),
    (
        "app_plugin_yaml_anchor",
        "anchors (&name) are not supported.",
    ),
    (
        "app_plugin_yaml_alias",
        "aliases (*name) are not supported.",
    ),
    ("app_plugin_yaml_tag", "tags (!name) are not supported."),
    (
        "app_plugin_yaml_block_scalar",
        "block text (| or >) is not supported; write the value on one line in quotes.",
    ),
    (
        "app_plugin_yaml_flow_map",
        "{ } maps are not supported; put each key on its own line, and quote a value that starts with {.",
    ),
    (
        "app_plugin_yaml_directive",
        "% directives are not supported.",
    ),
    (
        "app_plugin_yaml_documents",
        "a plugin file holds one document; remove this --- or ... line.",
    ),
    (
        "app_plugin_yaml_duplicate_key",
        "this key is already set above.",
    ),
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
    (
        "app_plugin_yaml_top_level",
        "the file must be a map of key: value lines.",
    ),
    (
        "app_plugin_yaml_flow_list",
        "[ ] lists hold single values on one line, closed with ].",
    ),
    ("app_plugin_yaml_depth", "the settings are nested too deep."),
];
