use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use trackwright_core::catalog;

/// The CLI's own subcommands beside the catalog's operations.
const CLI_COMMANDS: [&str; 3] = ["init", "mcp", "help"];
/// The CLI's own flags; every command takes them.
const CLI_FLAGS: [&str; 4] = ["json", "db", "actor", "help"];
/// The CLI's own flags that take no value.
const CLI_SWITCHES: [&str; 2] = ["json", "help"];
/// The flags `trackwright init` takes beside the CLI's own.
const INIT_FLAGS: [&str; 2] = ["key", "name"];
/// The shell words that end one command on a line.
const SEPARATORS: [&str; 4] = ["&&", "||", "|", ";"];
/// The code block languages the docs use; a block in any other one fails the test until
/// the test learns to read it.
const LANGUAGES: [&str; 5] = ["sh", "json", "toml", "rust", "text"];
/// The text a Rust snippet names an operation with.
const PARSE_CALL: &str = "Request::parse(\"";
/// The commands the docs must show at least once, so a test that reads no snippet fails.
const SHOWN_COMMANDS: [&str; 6] = ["init", "mcp", "create", "ready", "claim", "close"];

/// The generated schema reference.
const SCHEMA_REFERENCE: &str = include_str!("../docs/schema.md");
/// The harness integration guide.
const HARNESS_GUIDE: &str = include_str!("../docs/harness-integration.md");
/// Every tracked doc of the trackwright crates, by path from the crates folder.
const DOCS: [(&str, &str); 5] = [
    ("trackwright-core/README.md", include_str!("../README.md")),
    ("trackwright-core/docs/schema.md", SCHEMA_REFERENCE),
    (
        "trackwright-core/docs/harness-integration.md",
        HARNESS_GUIDE,
    ),
    (
        "trackwright-cli/README.md",
        include_str!("../../trackwright-cli/README.md"),
    ),
    (
        "trackwright-mcp/README.md",
        include_str!("../../trackwright-mcp/README.md"),
    ),
];

/// The schema reference's opening note, under its title.
const GENERATED_NOTE: &str = "Generated from `trackwright_core::catalog::schema_v1()` by the \
    test `the_schema_reference_is_generated_from_the_catalog` in \
    `crates/trackwright-core/tests/docs.rs`. Change the catalog, never this file. The same \
    schema as JSON Schema is `crates/trackwright-core/schema/v1.json`.";
/// The schema reference's line above the reply envelope.
const REPLY_NOTE: &str = "Every operation answers with this envelope: `ok` with `data`, the \
    `next` actions and a `cursor`, or a refusal with its `code` and `message`.";

/// One fenced code block of a doc. Fields: the doc's path, the fence's language and the
/// block's text.
struct Block {
    doc: &'static str,
    language: String,
    text: String,
}

/// What the snippets name, and every name that is not in the catalog. Fields: the
/// `trackwright` commands, the MCP tools called, and the problems found.
#[derive(Default)]
struct Findings {
    commands: BTreeSet<String>,
    tools: BTreeSet<String>,
    problems: Vec<String>,
}

impl Findings {
    /// Checks every `trackwright` call on one shell line. Inputs: the doc and the line.
    fn line(&mut self, doc: &str, line: &str) {
        for call in calls_in(&shell_words(line)) {
            match checked_call(&call) {
                Ok(command) => {
                    self.commands.insert(command);
                }
                Err(problem) => self.problems.push(format!("{doc}: {problem}")),
            }
        }
    }

    /// Checks one operation a snippet names and, when given, the argument names it passes.
    /// Inputs: the doc, the operation and the argument names.
    fn op(&mut self, doc: &str, op: &str, arguments: &[&str]) {
        let Some(fields) = fields_of(op) else {
            self.problems
                .push(format!("{doc}: {op} is no operation of the catalog"));
            return;
        };
        for argument in arguments {
            if !fields.contains(argument) {
                self.problems
                    .push(format!("{doc}: {argument} is no field of {op}"));
            }
        }
    }
}

/// The field names of one catalog operation. Input: the operation's name. Output: its
/// fields, or None for a name the catalog does not hold.
fn fields_of(op: &str) -> Option<Vec<&'static str>> {
    let spec = catalog::catalog()
        .iter()
        .find(|spec| spec.kind.as_str() == op)?;
    let fields: Vec<&'static str> = spec.fields.iter().map(|field| field.name).collect();
    assert!(!fields.is_empty(), "{op} has fields");
    Some(fields)
}

/// Splits one doc into its fenced code blocks and the inline code spans of its prose.
/// Inputs: the doc's path and text. Output: the blocks and the spans.
fn snippets(doc: &'static str, text: &str) -> (Vec<Block>, Vec<String>) {
    let mut blocks = Vec::new();
    let mut spans = Vec::new();
    let mut open: Option<Block> = None;
    for line in text.lines() {
        let fence = line.trim_start().strip_prefix("```");
        if let Some(block) = open.as_mut() {
            if let Some(rest) = fence {
                assert!(rest.trim().is_empty(), "{doc}: a closing fence is bare");
                blocks.push(open.take().expect("a block is open"));
            } else {
                block.text.push_str(line);
                block.text.push('\n');
            }
        } else if let Some(language) = fence {
            open = Some(Block {
                doc,
                language: language.trim().to_owned(),
                text: String::new(),
            });
        } else {
            spans.extend(line.split('`').skip(1).step_by(2).map(str::to_owned));
        }
    }
    assert!(open.is_none(), "{doc}: every code fence is closed");
    (blocks, spans)
}

/// Splits one shell line into words as a POSIX shell does for plain words and single or
/// double quotes, the only quoting the docs use. Input: the line. Output: the words.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    assert!(quote.is_none(), "every quote in {line:?} is closed");
    if started {
        words.push(word);
    }
    words
}

/// Tells whether a shell word sets an environment variable, as `TRACKWRIGHT_ACTOR=owner`
/// does before a command. Input: the word. Output: whether it is an assignment.
fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    })
}

/// Finds every `trackwright` call among one line's words: the program word at the start,
/// after `$`, after `--` (as in `claude mcp add trackwright -- trackwright mcp`), after an
/// environment assignment that is not a flag's value, or after a separator. Input: the
/// words. Output: each call's words after the program name, up to the next separator.
fn calls_in(words: &[String]) -> Vec<Vec<String>> {
    let mut calls = Vec::new();
    for (at, word) in words.iter().enumerate() {
        if word != "trackwright" {
            continue;
        }
        let starts = at == 0 || {
            let before = words[at - 1].as_str();
            let flag_value = at >= 2 && words[at - 2].starts_with('-');
            before == "--"
                || before == "$"
                || SEPARATORS.contains(&before)
                || (assignment(before) && !flag_value)
        };
        if !starts {
            continue;
        }
        let rest = &words[at + 1..];
        let end = rest
            .iter()
            .position(|word| SEPARATORS.contains(&word.as_str()))
            .unwrap_or(rest.len());
        calls.push(rest[..end].to_vec());
    }
    assert!(calls.len() <= words.len());
    calls
}

/// Checks one `trackwright` call as the CLI reads it: one command, an operation of the
/// catalog or one of the CLI's own (`--help` alone asks for help), and every flag the
/// CLI's own or a field of that command, each flag but `--json` and `--help` followed by
/// its value. Input: the words after the program name. Output: the command, or what is
/// wrong.
fn checked_call(argv: &[String]) -> Result<String, String> {
    let shown = format!("trackwright {}", argv.join(" "));
    let mut command: Option<String> = None;
    let mut flags = Vec::new();
    let mut at = 0;
    while at < argv.len() {
        let word = argv[at].as_str();
        at += 1;
        let Some(flag) = word.strip_prefix("--") else {
            if command.is_some() || word.starts_with('-') {
                return Err(format!("unexpected word {word} in {shown}"));
            }
            command = Some(word.replace('-', "_"));
            continue;
        };
        let (name, inline) = flag
            .split_once('=')
            .map_or((flag, false), |(name, _)| (name, true));
        let name = name.replace('-', "_");
        if !inline && !CLI_SWITCHES.contains(&name.as_str()) {
            if argv.get(at).is_none_or(|value| value.starts_with("--")) {
                return Err(format!("--{name} has no value in {shown}"));
            }
            at += 1;
        }
        flags.push(name);
    }
    assert_eq!(at, argv.len());
    let command = match command {
        Some(command) => command,
        None if flags.iter().any(|flag| flag == "help") => "help".to_owned(),
        None => return Err(format!("{shown} names no command")),
    };
    let fields: Vec<&str> = match command.as_str() {
        "init" => INIT_FLAGS.to_vec(),
        "mcp" | "help" => Vec::new(),
        op => fields_of(op).ok_or_else(|| format!("{op} in {shown} is no operation"))?,
    };
    for flag in &flags {
        if !CLI_FLAGS.contains(&flag.as_str()) && !fields.contains(&flag.as_str()) {
            return Err(format!("--{flag} is no flag of trackwright {command}"));
        }
    }
    Ok(command)
}

/// Joins a shell block's lines that end in a backslash with the next one. Input: the
/// block's text. Output: the logical lines.
fn shell_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pending = String::new();
    for line in text.lines() {
        match line.strip_suffix('\\') {
            Some(head) => pending.push_str(head),
            None => {
                pending.push_str(line);
                lines.push(std::mem::take(&mut pending));
            }
        }
    }
    assert!(pending.is_empty(), "a shell block ends without a backslash");
    lines
}

/// Reads one JSON block: the whole block as one value, or else one value per line as a
/// transcript of messages. Input: the block. Output: the values.
fn json_values(block: &Block) -> Vec<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(&block.text) {
        return vec![value];
    }
    let values: Vec<Value> = block
        .text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: JSON line {line:?}: {error}", block.doc))
        })
        .collect();
    assert!(
        !values.is_empty(),
        "{}: a JSON block holds a value",
        block.doc
    );
    values
}

/// Checks what one JSON value names, at any depth: an MCP client entry whose command is
/// `trackwright` (its `args`), a `tools/call` request (its tool and argument names) and a
/// reply's next action (its `op`). Inputs: the doc, the value and the findings.
fn json_calls(doc: &str, value: &Value, found: &mut Findings) {
    let object = match value {
        Value::Array(items) => {
            for item in items {
                json_calls(doc, item, found);
            }
            return;
        }
        Value::Object(object) => object,
        _ => return,
    };
    if object.get("command").and_then(Value::as_str) == Some("trackwright") {
        let args = object["args"]
            .as_array()
            .expect("an MCP entry lists its args");
        let words: Vec<String> = args.iter().map(|arg| text(arg).to_owned()).collect();
        found.line(doc, &format!("trackwright {}", words.join(" ")));
    }
    if object.get("method").and_then(Value::as_str) == Some("tools/call") {
        let params = &object["params"];
        let tool = text(&params["name"]);
        let arguments = params["arguments"]
            .as_object()
            .expect("a tool call has arguments");
        let names: Vec<&str> = arguments.keys().map(String::as_str).collect();
        found.op(doc, tool, &names);
        found.tools.insert(tool.to_owned());
    }
    if let Some(op) = object.get("op").and_then(Value::as_str) {
        found.op(doc, op, &[]);
    }
    for inner in object.values() {
        json_calls(doc, inner, found);
    }
}

/// Checks every MCP client entry of one TOML table, at any depth: a table whose
/// `command` is `trackwright` with its `args`. Inputs: the doc, the table and the findings.
fn toml_calls(doc: &str, table: &toml::Table, found: &mut Findings) {
    if table.get("command").and_then(toml::Value::as_str) == Some("trackwright") {
        let args = table["args"]
            .as_array()
            .expect("an MCP entry lists its args");
        let words: Vec<&str> = args
            .iter()
            .map(|arg| arg.as_str().expect("an arg is a string"))
            .collect();
        assert_eq!(words.len(), args.len());
        found.line(doc, &format!("trackwright {}", words.join(" ")));
    }
    for inner in table.values() {
        if let toml::Value::Table(inner) = inner {
            toml_calls(doc, inner, found);
        }
    }
}

/// Checks every operation a Rust snippet parses a request for. Inputs: the block and the
/// findings.
fn rust_ops(block: &Block, found: &mut Findings) {
    for (at, _) in block.text.match_indices(PARSE_CALL) {
        let rest = &block.text[at + PARSE_CALL.len()..];
        let end = rest.find('"').expect("a parsed operation's name is closed");
        found.op(block.doc, &rest[..end], &[]);
    }
}

/// Checks everything one code block names. Inputs: the block and the findings.
fn read_block(block: &Block, found: &mut Findings) {
    assert!(
        LANGUAGES.contains(&block.language.as_str()),
        "{}: the test reads code blocks in {LANGUAGES:?}, not {:?}",
        block.doc,
        block.language
    );
    match block.language.as_str() {
        "sh" => {
            for line in shell_lines(&block.text) {
                found.line(block.doc, &line);
            }
        }
        "json" => {
            for value in json_values(block) {
                json_calls(block.doc, &value, found);
            }
        }
        "toml" => {
            let table: toml::Table = block
                .text
                .parse()
                .unwrap_or_else(|error| panic!("{}: TOML block: {error}", block.doc));
            toml_calls(block.doc, &table, found);
        }
        "rust" => rust_ops(block, found),
        _ => assert_eq!(block.language, "text"),
    }
}

/// One string of a published schema. Input: the value. Output: its text.
fn text(value: &Value) -> &str {
    value.as_str().expect("a schema text is a string")
}

/// One flag of a published schema as the reference words it. Input: the value. Output:
/// `yes` or `no`.
fn yes(value: &Value) -> &'static str {
    if value.as_bool().expect("a schema flag is a boolean") {
        "yes"
    } else {
        "no"
    }
}

/// One table cell's text, its pipes escaped. Input: one line of text. Output: the cell.
fn cell(text: &str) -> String {
    assert!(!text.contains('\n'), "a table cell is one line");
    text.replace('|', "\\|")
}

/// A copy of a JSON value with every object's keys in sorted order, so the reference does
/// not depend on serde_json's map order. Input: the value. Output: the sorted copy.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&object[key.as_str()])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// One operation's section of the reference: its summary, whether it writes, and a row
/// per field, sorted by name, with whether it is required, its JSON Schema and its
/// meaning. Inputs: the name and the operation's schema. Output: the lines.
fn op_section(name: &str, op: &Value) -> Vec<String> {
    let args = &op["args"];
    let properties = args["properties"]
        .as_object()
        .expect("arguments are an object");
    let required: Vec<&str> = args["required"]
        .as_array()
        .expect("required names are a list")
        .iter()
        .map(text)
        .collect();
    let mut fields: Vec<&String> = properties.keys().collect();
    fields.sort();
    let mut lines = vec![
        String::new(),
        format!("## `{name}`"),
        String::new(),
        text(&op["summary"]).to_owned(),
        String::new(),
        format!("Writes: {}.", yes(&op["write"])),
        String::new(),
        "| Field | Required | Schema | Meaning |".to_owned(),
        "| --- | --- | --- | --- |".to_owned(),
    ];
    for field in fields {
        let mut schema = properties[field.as_str()]
            .as_object()
            .expect("a field's schema is an object")
            .clone();
        let meaning = schema
            .remove("description")
            .expect("every field is described");
        let compact = serde_json::to_string(&sorted(&Value::Object(schema))).unwrap();
        assert!(
            !compact.contains('`'),
            "{name}.{field}: no backtick in a schema"
        );
        let needed = if required.contains(&field.as_str()) {
            "yes"
        } else {
            "no"
        };
        let (compact, meaning) = (cell(&compact), cell(text(&meaning)));
        lines.push(format!(
            "| `{field}` | {needed} | `{compact}` | {meaning} |"
        ));
    }
    assert!(lines.len() > 9, "{name} has fields");
    lines
}

/// Builds the schema reference from a published schema: the bounds, a table of every
/// operation, a section per operation, the reply envelope and the refusal codes.
/// Operations and fields are sorted by name, so the text does not depend on map order.
/// Input: the schema. Output: the Markdown text.
fn schema_reference(schema: &Value) -> String {
    let ops = schema["ops"]
        .as_object()
        .expect("the schema lists its operations");
    let mut names: Vec<&String> = ops.keys().collect();
    names.sort();
    let mut lines = vec![
        format!("# Schema reference: {}", text(&schema["title"])),
        String::new(),
        GENERATED_NOTE.to_owned(),
        String::new(),
        format!("- API version: {}", schema["api"]),
        format!("- Request bound: {} bytes", schema["request_bytes_max"]),
        format!(
            "- Reply bound: {} bytes by default, at most {}",
            schema["reply_bytes_default"], schema["reply_bytes_max"]
        ),
        String::new(),
        "## Operations".to_owned(),
        String::new(),
        "| Operation | Writes | Summary |".to_owned(),
        "| --- | --- | --- |".to_owned(),
    ];
    for name in &names {
        let op = &ops[name.as_str()];
        let summary = cell(text(&op["summary"]));
        lines.push(format!("| `{name}` | {} | {summary} |", yes(&op["write"])));
    }
    for name in &names {
        lines.extend(op_section(name, &ops[name.as_str()]));
    }
    lines.extend(["", "## Reply envelope", "", REPLY_NOTE, "", "```json"].map(str::to_owned));
    lines.push(serde_json::to_string_pretty(&sorted(&schema["reply"])).unwrap());
    lines.extend(["```", "", "## Refusal codes", ""].map(str::to_owned));
    for code in schema["codes"]
        .as_array()
        .expect("the schema lists its codes")
    {
        lines.push(format!("- `{}`", text(code)));
    }
    assert!(
        lines.len() > names.len() * 10,
        "every operation has its section"
    );
    let reference = lines.join("\n") + "\n";
    assert!(
        !reference.contains("\n\n\n"),
        "one blank line between parts"
    );
    reference
}

/// Every command, MCP tool and operation the docs' snippets name exists: a `trackwright`
/// call names an operation of the catalog or one of the CLI's own commands, with flags
/// that command takes; an MCP client entry starts `trackwright mcp`; a `tools/call` names
/// an operation with its fields; a reply's next action and a Rust `Request::parse` name an
/// operation.
#[test]
fn every_op_named_in_the_docs_exists_in_the_catalog() {
    let mut found = Findings::default();
    for (doc, text) in DOCS {
        let (blocks, spans) = snippets(doc, text);
        for block in &blocks {
            read_block(block, &mut found);
        }
        for span in spans.iter().filter(|span| span.starts_with("trackwright ")) {
            found.line(doc, span);
        }
    }
    assert_eq!(found.problems, Vec::<String>::new());
    for command in SHOWN_COMMANDS {
        assert!(
            found.commands.contains(command),
            "the docs show trackwright {command}"
        );
    }
    assert!(
        found.tools.contains("ready"),
        "the docs show an MCP tool call"
    );
    for command in &found.commands {
        assert!(CLI_COMMANDS.contains(&command.as_str()) || fields_of(command).is_some());
    }
}

/// `docs/schema.md` is the reference built from the catalog's published schema, so it
/// never drifts from the operations. On a difference the expected text is written under
/// the target folder, and the failure names the file to copy over it.
#[test]
fn the_schema_reference_is_generated_from_the_catalog() {
    let generated = schema_reference(&catalog::schema_v1());
    if generated != SCHEMA_REFERENCE {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("schema.md");
        std::fs::write(&path, &generated).unwrap();
        panic!(
            "docs/schema.md is generated, never hand-edited: copy {} over it",
            path.display()
        );
    }
    for spec in catalog::catalog() {
        let heading = format!("\n## `{}`\n", spec.kind.as_str());
        assert_eq!(generated.matches(&heading).count(), 1, "{heading:?} once");
    }
}

/// The harness guide says what standalone authority is worth: actors name themselves, so
/// the project's policy binds honest agents only.
#[test]
fn the_harness_guide_says_standalone_identity_is_self_declared() {
    assert!(HARNESS_GUIDE.contains("self-declared"));
    assert!(HARNESS_GUIDE.contains("honest agents only"));
}
