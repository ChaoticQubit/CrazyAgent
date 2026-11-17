use super::Problem;

/// The most bytes of a plugin file.
pub const BYTES_MAX: usize = 16 * 1024;
/// The most nested blocks below the top-level map.
pub const DEPTH_MAX: usize = 8;
const KEY_BYTES_MAX: usize = 64;

/// One value of the strict YAML subset plugin files are written in: block maps in file
/// order, lists of single values, and typed scalars (`true`, `false`, `null`, whole and
/// decimal numbers, anything else a string; quoted values are always strings).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl Value {
    /// Looks a key up in a map. Input: the key. Output: its value, none for a missing key
    /// or a value that is not a map.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// Converts the value to JSON with its types: maps to objects, lists to arrays.
    /// Output: the JSON value.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(value) => serde_json::Value::Bool(*value),
            Self::Int(value) => serde_json::Value::from(*value),
            Self::Float(value) => serde_json::Number::from_f64(*value)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Self::Str(value) => serde_json::Value::String(value.clone()),
            Self::List(items) => {
                serde_json::Value::Array(items.iter().map(Self::to_json).collect())
            }
            Self::Map(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect(),
            ),
        }
    }
}

/// A refused plugin file: the code of the construct and its 1-based line, 0 for the whole
/// file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlError {
    pub code: &'static str,
    pub line: u32,
}

impl YamlError {
    /// The refusal as a Problem with its plain sentence naming the line.
    pub fn problem(&self) -> Problem {
        Problem::new(self.code, (self.line > 0).then_some(self.line), None)
    }

    /// The plain sentence naming the line, e.g. "Line 2: use spaces, not tabs, to indent."
    pub fn message(&self) -> String {
        self.problem().message
    }
}

fn refuse<T>(code: &'static str, line: u32) -> Result<T, YamlError> {
    Err(YamlError { code, line })
}

struct Line<'a> {
    number: u32,
    indent: usize,
    content: &'a str,
}

/// Parses plugin file text. Input: the text. Output: the top-level map, or the refused
/// construct and its line.
pub fn parse(text: &str) -> Result<Value, YamlError> {
    parse_bytes(text.as_bytes())
}

/// Parses plugin file bytes: at most `BYTES_MAX` bytes of UTF-8, an optional byte order
/// mark, `\n` or `\r\n` lines, spaces for indent. Input: the bytes. Output: the top-level
/// map, or the refused construct and its line.
pub fn parse_bytes(bytes: &[u8]) -> Result<Value, YamlError> {
    if bytes.len() > BYTES_MAX {
        return refuse("app_plugin_yaml_too_large", 0);
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let Ok(text) = std::str::from_utf8(bytes) else {
        return refuse("app_plugin_yaml_encoding", 0);
    };
    let lines = logical_lines(text)?;
    let Some(first) = lines.first() else {
        return refuse("app_plugin_yaml_empty", 0);
    };
    if list_item(first.content) {
        return refuse("app_plugin_yaml_top_level", first.number);
    }
    if first.indent != 0 {
        return refuse("app_plugin_yaml_indent", first.number);
    }
    let mut at = 0;
    let value = map(&lines, &mut at, 0, 0)?;
    if let Some(left) = lines.get(at) {
        return refuse("app_plugin_yaml_indent", left.number);
    }
    Ok(value)
}

fn logical_lines(text: &str) -> Result<Vec<Line<'_>>, YamlError> {
    let mut lines = Vec::new();
    let mut marker_allowed = true;
    for (index, raw) in text.split('\n').enumerate() {
        let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.contains('\t') {
            return refuse("app_plugin_yaml_tab", number);
        }
        if raw.chars().any(char::is_control) {
            return refuse("app_plugin_yaml_syntax", number);
        }
        let trimmed = raw.trim_end_matches(' ');
        let content = trimmed.trim_start_matches(' ');
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        let indent = trimmed.len() - content.len();
        if content.starts_with('%') {
            return refuse("app_plugin_yaml_directive", number);
        }
        let marker = |mark: &str| content == mark || content.starts_with(&format!("{mark} "));
        if indent == 0 && marker("---") {
            if marker_allowed {
                marker_allowed = false;
                continue;
            }
            return refuse("app_plugin_yaml_documents", number);
        }
        if indent == 0 && marker("...") {
            return refuse("app_plugin_yaml_documents", number);
        }
        marker_allowed = false;
        lines.push(Line {
            number,
            indent,
            content,
        });
    }
    Ok(lines)
}

fn list_item(content: &str) -> bool {
    content == "-" || content.starts_with("- ")
}

fn block(
    lines: &[Line<'_>],
    at: &mut usize,
    indent: usize,
    depth: usize,
) -> Result<Value, YamlError> {
    let line = &lines[*at];
    if depth > DEPTH_MAX {
        return refuse("app_plugin_yaml_depth", line.number);
    }
    if list_item(line.content) {
        list(lines, at, indent)
    } else {
        map(lines, at, indent, depth)
    }
}

fn map(
    lines: &[Line<'_>],
    at: &mut usize,
    indent: usize,
    depth: usize,
) -> Result<Value, YamlError> {
    let mut entries: Vec<(String, Value)> = Vec::new();
    while let Some(line) = lines.get(*at) {
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return refuse("app_plugin_yaml_indent", line.number);
        }
        if list_item(line.content) {
            return refuse("app_plugin_yaml_syntax", line.number);
        }
        let (key, rest) = split_key(line.content, line.number)?;
        if entries.iter().any(|(known, _)| known == key) {
            return refuse("app_plugin_yaml_duplicate_key", line.number);
        }
        *at += 1;
        let value = if rest.is_empty() || rest.starts_with('#') {
            match lines.get(*at) {
                Some(next) if next.indent > indent => block(lines, at, next.indent, depth + 1)?,
                Some(next) if next.indent == indent && list_item(next.content) => {
                    if depth + 1 > DEPTH_MAX {
                        return refuse("app_plugin_yaml_depth", next.number);
                    }
                    list(lines, at, indent)?
                }
                _ => Value::Null,
            }
        } else {
            scalar(rest, line.number)?
        };
        entries.push((key.to_owned(), value));
    }
    assert!(!entries.is_empty(), "a map starts at a key line");
    Ok(Value::Map(entries))
}

fn list(lines: &[Line<'_>], at: &mut usize, indent: usize) -> Result<Value, YamlError> {
    let mut items = Vec::new();
    while let Some(line) = lines.get(*at) {
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return refuse("app_plugin_yaml_indent", line.number);
        }
        if !list_item(line.content) {
            break;
        }
        let rest = line.content[1..].trim_start_matches(' ');
        if rest.is_empty() || rest.starts_with('#') || list_item(rest) {
            return refuse("app_plugin_yaml_list_item", line.number);
        }
        if map_entry(rest) {
            return refuse("app_plugin_yaml_list_map", line.number);
        }
        items.push(scalar(rest, line.number)?);
        *at += 1;
    }
    assert!(!items.is_empty(), "a list starts at an item line");
    Ok(Value::List(items))
}

fn colon(content: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    bytes
        .iter()
        .enumerate()
        .find(|(index, byte)| {
            **byte == b':' && bytes.get(index + 1).is_none_or(|next| *next == b' ')
        })
        .map(|(index, _)| index)
}

fn plain_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= KEY_BYTES_MAX
        && key.as_bytes()[0].is_ascii_alphanumeric()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn map_entry(content: &str) -> bool {
    colon(content).is_some_and(|at| plain_key(&content[..at]))
}

fn indicator(first: char, line: u32) -> Result<(), YamlError> {
    match first {
        '&' => refuse("app_plugin_yaml_anchor", line),
        '*' => refuse("app_plugin_yaml_alias", line),
        '!' => refuse("app_plugin_yaml_tag", line),
        '|' | '>' => refuse("app_plugin_yaml_block_scalar", line),
        '{' => refuse("app_plugin_yaml_flow_map", line),
        '%' => refuse("app_plugin_yaml_directive", line),
        '@' | '`' => refuse("app_plugin_yaml_reserved", line),
        _ => Ok(()),
    }
}

fn split_key(content: &str, line: u32) -> Result<(&str, &str), YamlError> {
    let first = content.chars().next().expect("a logical line has content");
    match first {
        '"' | '\'' => return refuse("app_plugin_yaml_key", line),
        '?' => return refuse("app_plugin_yaml_complex_key", line),
        '[' => return refuse("app_plugin_yaml_syntax", line),
        _ => indicator(first, line)?,
    }
    let Some(at) = colon(content) else {
        return refuse("app_plugin_yaml_syntax", line);
    };
    let key = &content[..at];
    if !plain_key(key) {
        return refuse("app_plugin_yaml_key", line);
    }
    Ok((key, content[at + 1..].trim_start_matches(' ')))
}

fn scalar(text: &str, line: u32) -> Result<Value, YamlError> {
    let first = text.chars().next().expect("a scalar has text");
    match first {
        '"' | '\'' => {
            let (value, used) = quoted(text, line)?;
            comment_only(&text[used..], "app_plugin_yaml_quote", line)?;
            Ok(Value::Str(value))
        }
        '[' => flow_list(text, line),
        '?' if text.len() == 1 || text.as_bytes()[1] == b' ' => {
            refuse("app_plugin_yaml_complex_key", line)
        }
        '-' if text.len() == 1 || text.as_bytes()[1] == b' ' => {
            refuse("app_plugin_yaml_syntax", line)
        }
        _ => {
            indicator(first, line)?;
            let plain = text.split(" #").next().unwrap_or("").trim_end_matches(' ');
            if plain.contains(": ") || plain.ends_with(':') {
                return refuse("app_plugin_yaml_syntax", line);
            }
            Ok(typed(plain))
        }
    }
}

fn comment_only(rest: &str, code: &'static str, line: u32) -> Result<(), YamlError> {
    let rest = rest.trim_start_matches(' ');
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        refuse(code, line)
    }
}

fn typed(plain: &str) -> Value {
    match plain {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" | "~" => return Value::Null,
        _ => {}
    }
    let digits = plain.strip_prefix('-').unwrap_or(plain);
    let whole = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if whole(digits) {
        if let Ok(value) = plain.parse::<i64>() {
            return Value::Int(value);
        }
    } else if let Some((left, right)) = digits.split_once('.')
        && whole(left)
        && whole(right)
        && let Ok(value) = plain.parse::<f64>()
        && value.is_finite()
    {
        return Value::Float(value);
    }
    Value::Str(plain.to_owned())
}

fn quoted(text: &str, line: u32) -> Result<(String, usize), YamlError> {
    let mut chars = text.char_indices();
    let (_, quote) = chars.next().expect("a quoted scalar starts with its quote");
    let mut value = String::new();
    while let Some((index, current)) = chars.next() {
        if current == quote {
            if quote == '\'' && text[index + 1..].starts_with('\'') {
                chars.next();
                value.push('\'');
                continue;
            }
            return Ok((value, index + 1));
        }
        if quote == '"' && current == '\\' {
            let Some((_, escape)) = chars.next() else {
                return refuse("app_plugin_yaml_quote", line);
            };
            value.push(match escape {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'n' => '\n',
                't' => '\t',
                'u' => {
                    let mut code = 0u32;
                    for _ in 0..4 {
                        let digit = chars
                            .next()
                            .and_then(|(_, digit)| digit.to_digit(16))
                            .ok_or(YamlError {
                                code: "app_plugin_yaml_escape",
                                line,
                            })?;
                        code = code * 16 + digit;
                    }
                    char::from_u32(code).ok_or(YamlError {
                        code: "app_plugin_yaml_escape",
                        line,
                    })?
                }
                _ => return refuse("app_plugin_yaml_escape", line),
            });
            continue;
        }
        value.push(current);
    }
    refuse("app_plugin_yaml_quote", line)
}

fn flow_list(text: &str, line: u32) -> Result<Value, YamlError> {
    let mut items = Vec::new();
    let mut at = 1;
    loop {
        at += text[at..].len() - text[at..].trim_start_matches(' ').len();
        let rest = &text[at..];
        let Some(first) = rest.chars().next() else {
            return refuse("app_plugin_yaml_flow_list", line);
        };
        if first == ']' && items.is_empty() {
            at += 1;
            break;
        }
        match first {
            '"' | '\'' => {
                let (value, used) = quoted(rest, line)?;
                items.push(Value::Str(value));
                at += used;
            }
            '[' | ']' | ',' => return refuse("app_plugin_yaml_flow_list", line),
            _ => {
                indicator(first, line)?;
                let end = rest.find([',', ']']).unwrap_or(rest.len());
                let plain = rest[..end].trim_end_matches(' ');
                if plain.contains(": ") || plain.contains(" #") || plain.contains('[') {
                    return refuse("app_plugin_yaml_flow_list", line);
                }
                items.push(typed(plain));
                at += end;
            }
        }
        at += text[at..].len() - text[at..].trim_start_matches(' ').len();
        match text[at..].chars().next() {
            Some(',') => at += 1,
            Some(']') => {
                at += 1;
                break;
            }
            _ => return refuse("app_plugin_yaml_flow_list", line),
        }
    }
    comment_only(&text[at..], "app_plugin_yaml_flow_list", line)?;
    Ok(Value::List(items))
}

#[cfg(test)]
#[path = "yaml_tests.rs"]
mod tests;
