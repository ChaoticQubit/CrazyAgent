use super::{CUT_MARK, McpError, ToolResult, VendorTool};
use crate::connectors::settings::SizeCap;
use crate::memory::without_hidden;
use serde_json::{Map, Value};

const NAME_BYTES_MAX: usize = 128;
const DESCRIPTION_BYTES_MAX: usize = 4096;
const SCHEMA_DEPTH_MAX: usize = 32;
const SCHEMA_BYTES_MAX: usize = 64 * 1024;
const MIRRORS_MAX: usize = 32;
const MIRROR_NAME_BYTES_MAX: usize = 64;
const MIRRORED_BYTES_MAX: usize = 8 * 1024;
const VENDOR_TEXT_BYTES_MAX: usize = 300;
const INTEGER_SAFE_MAX: u64 = (1 << 53) - 1;
const MARK: &str = "x-mcp-header";
const SENTINEL: &str = "=?base64?";
/// Schema keywords whose value is data, not a schema, so a header mark is not looked for
/// inside them.
const DATA_KEYWORDS: [&str; 4] = ["const", "default", "enum", "examples"];
/// Schema keywords whose value maps names to schemas.
const NAMED_SCHEMAS: [&str; 4] = [
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
];
const LEFT_OUT_KINDS: [&str; 4] = ["audio", "image", "resource", "resource_link"];

/// One tool argument a vendor wants mirrored into a request header: the chain of
/// property names that leads to it and the name part of its `Mcp-Param-` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mirror {
    path: Vec<String>,
    header: String,
}

/// Whether a tool name may be shown and called: 1 to 128 ASCII letters, digits, `_`, `-`
/// and `.`, the set the protocol asks vendors to keep to. Input: the name. Output: true
/// when it is; any other name is left out and never renamed.
pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    assert!(text.len() <= max);
    text
}

/// A vendor's own error text for the owner: hidden characters out, at most 300 bytes.
/// Input: the text. Output: the cleaned text, which may be empty.
pub(super) fn vendor_text(text: &str) -> String {
    cut(without_hidden(text), VENDOR_TEXT_BYTES_MAX)
}

/// Copies a schema with hidden characters taken out of every key and text. Inputs: the
/// value and how deep it sits. Output: the copy, or none when it nests too deep or two
/// keys become one.
fn shown(value: &Value, depth: usize) -> Option<Value> {
    if depth > SCHEMA_DEPTH_MAX {
        return None;
    }
    Some(match value {
        Value::String(text) => Value::String(without_hidden(text)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| shown(item, depth + 1))
                .collect::<Option<_>>()?,
        ),
        Value::Object(fields) => {
            let mut out = Map::new();
            for (key, field) in fields {
                if out
                    .insert(without_hidden(key), shown(field, depth + 1)?)
                    .is_some()
                {
                    return None;
                }
            }
            Value::Object(out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    })
}

fn token(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MIRROR_NAME_BYTES_MAX
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// Walks one schema for header marks. A mark counts only on a property reached through
/// `properties` keys alone, of type string, integer or boolean, with a header-name token
/// no other mark of the tool has; any other mark makes the tool invalid. Inputs: the
/// schema, the property chain to it, whether that chain is `properties` only, how deep it
/// sits and the marks so far. Output: false for an invalid tool.
fn marks(
    schema: &Value,
    path: &mut Vec<String>,
    reachable: bool,
    depth: usize,
    found: &mut Vec<Mirror>,
) -> bool {
    if depth > SCHEMA_DEPTH_MAX {
        return false;
    }
    let Some(fields) = schema.as_object() else {
        return true;
    };
    if let Some(mark) = fields.get(MARK) {
        let kind = fields.get("type").and_then(Value::as_str);
        let name = mark.as_str().filter(|name| token(name));
        let (Some(name), Some("string" | "integer" | "boolean")) = (name, kind) else {
            return false;
        };
        let taken = found
            .iter()
            .any(|known| known.header.eq_ignore_ascii_case(name));
        let plain = path.iter().all(|step| without_hidden(step) == *step);
        if !reachable || path.is_empty() || taken || !plain || found.len() >= MIRRORS_MAX {
            return false;
        }
        found.push(Mirror {
            path: path.clone(),
            header: name.to_owned(),
        });
    }
    for (key, field) in fields {
        let fine = match (key.as_str(), field) {
            (MARK, _) => true,
            (key, _) if DATA_KEYWORDS.contains(&key) => true,
            ("properties", Value::Object(properties)) => properties.iter().all(|(name, inner)| {
                path.push(name.clone());
                let fine = marks(inner, path, reachable, depth + 1, found);
                path.pop();
                fine
            }),
            (key, Value::Object(named)) if NAMED_SCHEMAS.contains(&key) => named
                .values()
                .all(|inner| marks(inner, path, false, depth + 1, found)),
            (_, Value::Array(items)) => items
                .iter()
                .all(|inner| marks(inner, path, false, depth + 1, found)),
            (_, inner) => marks(inner, path, false, depth + 1, found),
        };
        if !fine {
            return false;
        }
    }
    true
}

/// Reads one tool of a vendor's list for the owner and the agents: a callable name, the
/// description and schema with hidden characters out, the vendor's hints when they are
/// booleans, and for a server of the per-request era its header marks. Inputs: the tool
/// as sent and whether header marks apply. Output: the tool with its marks, or none for a
/// tool that is left out.
pub(super) fn tool(raw: &Value, modern: bool) -> Option<(VendorTool, Vec<Mirror>)> {
    let name = raw.get("name")?.as_str()?;
    if !valid_name(name) {
        return None;
    }
    let schema = raw.get("inputSchema")?;
    if !schema.is_object() {
        return None;
    }
    let mut mirrors = Vec::new();
    if modern && !marks(schema, &mut Vec::new(), true, 0, &mut mirrors) {
        return None;
    }
    let input_schema = shown(schema, 0)?;
    if serde_json::to_vec(&input_schema).ok()?.len() > SCHEMA_BYTES_MAX {
        return None;
    }
    let description = raw.get("description").and_then(Value::as_str).unwrap_or("");
    let hint = |key: &str| {
        raw.get("annotations")
            .and_then(|hints| hints.get(key))
            .and_then(Value::as_bool)
    };
    let tool = VendorTool {
        name: name.to_owned(),
        description: cut(without_hidden(description), DESCRIPTION_BYTES_MAX),
        input_schema,
        read_only_hint: hint("readOnlyHint"),
        destructive_hint: hint("destructiveHint"),
    };
    Some((tool, mirrors))
}

/// Writes a value for a mirrored header: as it is when it is plain visible ASCII with no
/// space at either end, else as base64 of its UTF-8 between `=?base64?` and `?=`. Input:
/// the text. Output: the header value, never empty and with no control byte.
pub(super) fn header_value(text: &str) -> String {
    use base64::Engine;
    let plain = !text.is_empty()
        && text.bytes().all(|byte| (0x20..0x7f).contains(&byte))
        && !text.starts_with(' ')
        && !text.ends_with(' ')
        && !text.starts_with(SENTINEL);
    if plain {
        text.to_owned()
    } else {
        format!(
            "{SENTINEL}{}?=",
            base64::engine::general_purpose::STANDARD.encode(text)
        )
    }
}

/// Builds the `Mcp-Param-` headers of one call from the tool's marks. An argument that
/// is absent or null gives no header. Inputs: the marks and the call's arguments. Output:
/// the headers, `connector_arguments_invalid` for a marked argument that is no string,
/// boolean or safe integer, or `connector_request_too_large` past 8 KiB of values.
pub(super) fn mirrored(
    marks: &[Mirror],
    args: &Map<String, Value>,
) -> Result<Vec<(String, String)>, McpError> {
    let invalid = || McpError::Refused("connector_arguments_invalid");
    let mut headers = Vec::with_capacity(marks.len());
    let mut total = 0;
    for mark in marks {
        let (first, rest) = mark.path.split_first().expect("a mark has a property path");
        let found = rest.iter().fold(args.get(first), |value, step| {
            value.and_then(|value| value.get(step))
        });
        let text = match found {
            None | Some(Value::Null) => continue,
            Some(Value::String(text)) => text.clone(),
            Some(Value::Bool(flag)) => flag.to_string(),
            Some(Value::Number(number)) => {
                let safe = number.as_u64().map_or_else(
                    || {
                        number
                            .as_i64()
                            .is_some_and(|n| n.unsigned_abs() <= INTEGER_SAFE_MAX)
                    },
                    |n| n <= INTEGER_SAFE_MAX,
                );
                if !safe {
                    return Err(invalid());
                }
                number.to_string()
            }
            Some(Value::Array(_) | Value::Object(_)) => return Err(invalid()),
        };
        let value = header_value(&text);
        total += value.len();
        if total > MIRRORED_BYTES_MAX {
            return Err(McpError::Refused("connector_request_too_large"));
        }
        headers.push((format!("Mcp-Param-{}", mark.header), value));
    }
    Ok(headers)
}

/// Turns a tool call's result into text only: each text part with hidden characters out,
/// a one-line note for a part that is not text, or the structured content as JSON when
/// there are no parts; then cut to the size setting with `CUT_MARK` as its last line.
/// Inputs: the result and the size setting. Output: the text result.
pub(super) fn result(result: &Value, cap: SizeCap) -> ToolResult {
    let mut lines: Vec<String> = Vec::new();
    for part in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
        let text = part.get("text").and_then(Value::as_str);
        lines.push(match (kind, text) {
            ("text", Some(text)) => without_hidden(text),
            (kind, _) if LEFT_OUT_KINDS.contains(&kind) => format!("[{kind} left out]"),
            _ => "[other content left out]".to_owned(),
        });
    }
    let mut text = lines.join("\n");
    if lines.is_empty()
        && let Some(structured) = result
            .get("structuredContent")
            .filter(|value| !value.is_null())
    {
        text = without_hidden(&structured.to_string());
    }
    let limit = cap.bytes();
    assert!(limit > CUT_MARK.len());
    let cut_here = text.len() > limit;
    if cut_here {
        text = cut(text, limit - CUT_MARK.len());
        text.push_str(CUT_MARK);
    }
    assert!(text.len() <= limit);
    ToolResult {
        text,
        is_error: result.get("isError").and_then(Value::as_bool) == Some(true),
        cut: cut_here,
    }
}
