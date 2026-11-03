use crate::args::Topic;
use crate::locate::{DB_ENV, FILE, FOLDER};
use crate::{ACTOR_ENV, EXIT_REFUSED, EXIT_USAGE};
use serde_json::{Map, Value};
use std::fmt::Write;
use trackwright_core::{FieldSpec, FieldType, OpSpec};

/// How deep nested reply objects are drawn before they are summarized.
const DEPTH_MAX: usize = 3;
/// The most next actions drawn, the contract's own bound.
const NEXT_MAX: usize = 5;
/// The keys a row shows first, in this order, before its other plain values.
const ROW_KEYS: [&str; 7] = [
    "id", "position", "status", "state", "priority", "title", "text",
];

/// Renders a reply envelope as short text for a person: the data, a refusal's code and
/// message, then one runnable `next: trackwright ...` line per next action. Every agent-written
/// string is escaped so it cannot drive the terminal. Input: the envelope. Output: the text.
pub fn render(envelope: &Value) -> String {
    let ok = envelope["ok"].as_bool().expect("every envelope says ok");
    let mut out = String::new();
    if !ok {
        let code = envelope["code"].as_str().expect("a refusal has a code");
        let message = envelope["message"]
            .as_str()
            .expect("a refusal has a message");
        writeln!(out, "refused: {}: {}", line(code), line(message)).unwrap();
    }
    data(&mut out, &envelope["data"]);
    let actions = envelope["next"]
        .as_array()
        .expect("every envelope lists next actions");
    assert!(
        actions.len() <= NEXT_MAX,
        "the core sends at most five next actions"
    );
    for action in actions {
        next(&mut out, action);
    }
    if let Some(cursor) = envelope["cursor"].as_str() {
        writeln!(out, "more: run it again with --cursor {}", line(cursor)).unwrap();
    }
    assert!(ok || out.starts_with("refused: "));
    out
}

/// The help text for the whole tool, `init`, `mcp` or one operation, built from the catalog.
/// Inputs: the topic, the catalog. Output: the text.
pub fn help(topic: Topic, catalog: &[OpSpec]) -> String {
    let mut out = String::new();
    match topic {
        Topic::Overview => overview(&mut out, catalog),
        Topic::Init => out.push_str(
            "trackwright init --key KEY [--name NAME] [--db PATH] [--actor ACTOR] [--json]\n\
             Creates the database when it is missing and adds the project KEY (2 to 8 letters A-Z).\n",
        ),
        Topic::Mcp => out.push_str(
            "trackwright mcp --actor ACTOR [--db PATH]\n\
             Serves every operation as an MCP tool over stdin and stdout, as ACTOR.\n",
        ),
        Topic::Op(spec) => operation(&mut out, spec),
    }
    assert!(out.starts_with("trackwright"));
    out
}

fn overview(out: &mut String, catalog: &[OpSpec]) {
    out.push_str(
        "trackwright <operation> [--field value ...] [--json] [--db PATH] [--actor ACTOR]\n\
         trackwright <operation> --help\n\
         trackwright init --key KEY [--name NAME]\n\
         trackwright mcp --actor ACTOR\n\
         Operations:\n",
    );
    for spec in catalog {
        writeln!(out, "  {:<16} {}", spec.kind.as_str(), spec.summary).unwrap();
    }
    writeln!(
        out,
        "Database: --db, then {DB_ENV}, then {FOLDER}/{FILE} here or in the nearest folder above.\n\
         Actor: --actor, then {ACTOR_ENV}. trackwright schema needs neither.\n\
         --json prints the reply envelope as one line. Exit status: 0 done, {EXIT_REFUSED} refused, \
         {EXIT_USAGE} usage error."
    )
    .unwrap();
}

fn operation(out: &mut String, spec: &OpSpec) {
    writeln!(
        out,
        "trackwright {} [--field value ...] [--json] [--db PATH] [--actor ACTOR]\n{}\nFields:",
        spec.kind.as_str(),
        spec.summary
    )
    .unwrap();
    for field in spec.fields {
        let required = if field.required { " required" } else { "" };
        let shape = shape(field);
        writeln!(
            out,
            "  --{} <{shape}>{required}  {}",
            field.name, field.summary
        )
        .unwrap();
    }
}

fn shape(field: &FieldSpec) -> String {
    match field.ty {
        FieldType::Text => "text".to_owned(),
        FieldType::Integer => "integer".to_owned(),
        FieldType::Boolean => "true|false".to_owned(),
        FieldType::Choice(choices) => choices.join("|"),
        FieldType::ShortId => "KEY-N".to_owned(),
        FieldType::TextList => "text, repeat the flag".to_owned(),
        FieldType::Json => "json, or plain text".to_owned(),
    }
}

fn next(out: &mut String, action: &Value) {
    let op = action["op"].as_str().expect("a next action names its op");
    write!(out, "next: trackwright {}", line(op)).unwrap();
    if let Some(id) = action["id"].as_str() {
        write!(out, " --id {}", line(id)).unwrap();
    }
    if let Some(why) = action["why"].as_str() {
        write!(out, "  ({})", line(why)).unwrap();
    }
    out.push('\n');
}

fn data(out: &mut String, value: &Value) {
    match value {
        Value::Null => {}
        Value::String(text) => {
            for part in text.split('\n') {
                writeln!(out, "{}", line(part)).unwrap();
            }
        }
        Value::Array(rows) => {
            for item in rows {
                writeln!(out, "{}", row(item)).unwrap();
            }
        }
        Value::Object(map) => block(out, map, 0),
        Value::Bool(_) | Value::Number(_) => writeln!(out, "{value}").unwrap(),
    }
}

fn block(out: &mut String, map: &Map<String, Value>, depth: usize) {
    assert!(depth <= DEPTH_MAX);
    let indent = "  ".repeat(depth);
    for (key, value) in map {
        let key = line(key);
        match value {
            Value::Array(items) => {
                writeln!(out, "{indent}{key}:").unwrap();
                for item in items {
                    writeln!(out, "{indent}  {}", row(item)).unwrap();
                }
            }
            Value::Object(inner) if depth < DEPTH_MAX => {
                writeln!(out, "{indent}{key}:").unwrap();
                block(out, inner, depth + 1);
            }
            Value::String(text) if text.contains('\n') => {
                writeln!(out, "{indent}{key}:").unwrap();
                for part in text.split('\n') {
                    writeln!(out, "{indent}  {}", line(part)).unwrap();
                }
            }
            _ => writeln!(out, "{indent}{key}: {}", scalar(value)).unwrap(),
        }
    }
}

fn row(value: &Value) -> String {
    let Value::Object(map) = value else {
        return scalar(value);
    };
    let mut parts: Vec<String> = ROW_KEYS
        .iter()
        .filter_map(|key| map.get(*key))
        .map(scalar)
        .collect();
    for (key, value) in map {
        if !ROW_KEYS.contains(&key.as_str()) {
            parts.push(format!("{}={}", line(key), scalar(value)));
        }
    }
    parts.join("  ")
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => line(text),
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::Object(map) => format!("{{{} fields}}", map.len()),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

/// Makes agent-written text safe on one terminal line: every control character (line
/// breaks and escape sequences included) and every hidden character (bidi controls,
/// zero-width, tag characters) shows as its `\u{...}` escape. Input: text. Output: the line.
pub fn line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if hidden(c) {
            write!(out, "{}", c.escape_unicode()).unwrap();
        } else {
            out.push(c);
        }
    }
    assert!(!out.contains('\n'));
    assert!(!out.contains('\u{1b}'));
    out
}

fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}
