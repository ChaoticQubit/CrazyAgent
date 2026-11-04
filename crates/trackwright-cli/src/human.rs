use crate::args::Topic;
use crate::locate::{DB_ENV, FILE, FOLDER};
use crate::{ACTOR_ENV, EXIT_REFUSED, EXIT_USAGE};
use serde_json::{Map, Value};
use std::fmt::Write;
use trackwright_core::catalog::{FieldSpec, FieldType, OpSpec};

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
             Adds the project KEY (2 to 8 letters A-Z), creating the database if missing.\n\
             Without --db: ./.trackwright/tracker.db, refused while a folder above holds one.\n\
             --db ./.trackwright/tracker.db makes a separate tracker that hides the one above.\n",
        ),
        Topic::Mcp => out.push_str(
            "trackwright mcp --actor ACTOR [--db PATH]\n\
             Serves every operation as an MCP tool over stdin and stdout, as ACTOR.\n",
        ),
        Topic::Import => out.push_str(
            "trackwright run import --format junit|allure|libtest --file PATH\n\
             \x20   (--project KEY | --run KEY-Rn) [--db PATH] [--actor ACTOR] [--json]\n\
             Reads one test report here, refusing a link or a file over its format's bound.\n\
             --project starts a run with the file name as its declared source and finishes it.\n\
             --run records into that running run and leaves finishing it to its writer.\n",
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
         trackwright run import --format FORMAT --file PATH (--project KEY | --run KEY-Rn)\n\
         Operations:\n",
    );
    for spec in catalog {
        writeln!(out, "  {:<16} {}", spec.kind.as_str(), spec.summary).unwrap();
    }
    writeln!(
        out,
        "Database: --db, then {DB_ENV}, then {FOLDER}/{FILE} here or in the nearest folder above.\n\
         Actor: --actor, then {ACTOR_ENV}. Plain trackwright schema needs neither.\n\
         --json prints the reply envelope as one line.\n\
         Exit status: 0 done, {EXIT_REFUSED} refused, {EXIT_USAGE} usage error."
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
        FieldType::Project => "KEY".to_owned(),
        FieldType::Ticket => "KEY-N".to_owned(),
        FieldType::Test(object) => format!("KEY-{}N", object.letter()),
        FieldType::Line { .. } => "text".to_owned(),
        FieldType::Shape(_) => "JSON".to_owned(),
        FieldType::Actor => "actor".to_owned(),
        FieldType::Text(_) => "text".to_owned(),
        FieldType::Version => "version".to_owned(),
        FieldType::Integer { min, max } => format!("{min}..{max}"),
        FieldType::Choice(named) => named.names().join("|"),
        FieldType::Day => "YYYY-MM-DD".to_owned(),
        FieldType::Flag => "true|false".to_owned(),
        FieldType::Cursor => "cursor".to_owned(),
        FieldType::RequestId => "request id".to_owned(),
        FieldType::Word => "word".to_owned(),
        FieldType::Path => "path".to_owned(),
        FieldType::Keys => "key, repeat the flag".to_owned(),
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
