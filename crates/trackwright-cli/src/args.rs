use crate::import_cmd::{self, Import};
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::PathBuf;
use trackwright_core::catalog::{FieldSpec, FieldType, OpSpec};

/// The most command line words one call takes; more is a usage error, never truncated.
pub const ARGUMENTS_MAX: usize = 256;
/// The CLI's own flags. No catalog field may take one of these names.
const RESERVED: [&str; 4] = ["json", "db", "actor", "help"];
/// The CLI's own subcommands beside the catalog's operations.
const COMMANDS: [&str; 4] = ["init", "mcp", "help", "run"];
/// The longest caller word a usage line repeats back.
const SHOWN_BYTES_MAX: usize = 64;

/// A usage error: one line, without a line break, saying what to change. It exits 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage(String);

impl Usage {
    /// Builds the one line. Input: the message. Output: the error.
    pub fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        assert!(!message.is_empty());
        assert!(!message.contains('\n'), "a usage error is one line");
        Self(message)
    }

    /// The line. Output: the message.
    pub fn message(&self) -> &str {
        &self.0
    }
}

/// One parsed call: the CLI's own flags and what to run.
pub struct Invocation {
    pub json: bool,
    pub db: Option<PathBuf>,
    pub actor: Option<String>,
    pub command: Command,
}

/// What a call runs.
pub enum Command {
    Help(Topic),
    Init {
        key: String,
        name: Option<String>,
    },
    Mcp,
    Op {
        spec: &'static OpSpec,
        args: Map<String, Value>,
    },
    Import(Import),
}

/// What a help call explains.
#[derive(Clone, Copy)]
pub enum Topic {
    Overview,
    Init,
    Mcp,
    Import,
    Op(&'static OpSpec),
}

#[derive(Default)]
struct Scan<'a> {
    json: bool,
    help: bool,
    db: Option<&'a str>,
    actor: Option<&'a str>,
    op: Option<&'a str>,
    sub: Option<&'a str>,
    fields: Vec<(String, &'a str)>,
}

/// Parses the command line against the catalog's field tables: `<op> [--field value ...]`
/// with the CLI's own `--json`, `--db`, `--actor` and `--help` anywhere. Field values become
/// the JSON type their field declares; the core validates everything else. Inputs: the words
/// after the program name, the catalog. Output: the call, or a one-line usage error.
pub fn parse(args: &[OsString], catalog: &'static [OpSpec]) -> Result<Invocation, Usage> {
    assert_catalog_leaves_cli_names_free(catalog);
    if args.len() > ARGUMENTS_MAX {
        return Err(Usage::new(format!("at most {ARGUMENTS_MAX} arguments")));
    }
    let words = args
        .iter()
        .map(|arg| arg.to_str())
        .collect::<Option<Vec<&str>>>()
        .ok_or_else(|| Usage::new("arguments must be UTF-8"))?;
    let scan = scan(&words)?;
    let command = command(&scan, catalog)?;
    Ok(Invocation {
        json: scan.json,
        db: scan.db.map(PathBuf::from),
        actor: scan.actor.map(str::to_owned),
        command,
    })
}

fn scan<'a>(words: &[&'a str]) -> Result<Scan<'a>, Usage> {
    let mut scan = Scan::default();
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        index += 1;
        if word == "--json" {
            scan.json = true;
        } else if word == "--help" || word == "-h" {
            scan.help = true;
        } else if let Some(flag) = word.strip_prefix("--") {
            let (name, value) = match flag.split_once('=') {
                Some(pair) => pair,
                None => {
                    let value = words.get(index).copied().filter(|v| !v.starts_with("--"));
                    index += 1;
                    let value = value.ok_or_else(|| {
                        Usage::new(format!(
                            "--{} needs a value; a value starting with -- goes as --{0}=VALUE",
                            shown(flag)
                        ))
                    })?;
                    (flag, value)
                }
            };
            flag_into(&mut scan, name.replace('-', "_"), value)?;
        } else if word.len() > 1 && word.starts_with('-') {
            return Err(Usage::new(format!("unknown flag {}", shown(word))));
        } else if scan.op.is_none() {
            scan.op = Some(word);
        } else if scan.op == Some("run") && scan.sub.is_none() {
            scan.sub = Some(word);
        } else {
            return Err(Usage::new(format!("unexpected argument {}", shown(word))));
        }
    }
    assert_eq!(index, words.len());
    Ok(scan)
}

fn flag_into<'a>(scan: &mut Scan<'a>, name: String, value: &'a str) -> Result<(), Usage> {
    let slot = match name.as_str() {
        "json" | "help" => return Err(Usage::new(format!("--{name} takes no value"))),
        "db" => &mut scan.db,
        "actor" => &mut scan.actor,
        _ => {
            scan.fields.push((name, value));
            return Ok(());
        }
    };
    if slot.replace(value).is_some() {
        return Err(Usage::new(format!("--{name} given twice")));
    }
    Ok(())
}

fn command(scan: &Scan<'_>, catalog: &'static [OpSpec]) -> Result<Command, Usage> {
    let Some(op) = scan.op else {
        if scan.help {
            return Ok(Command::Help(Topic::Overview));
        }
        return Err(Usage::new(
            "name an operation; trackwright --help lists them",
        ));
    };
    let op = op.replace('-', "_");
    match op.as_str() {
        "help" => Ok(Command::Help(Topic::Overview)),
        "init" if scan.help => Ok(Command::Help(Topic::Init)),
        "init" => init(&scan.fields),
        "run" if scan.help => Ok(Command::Help(Topic::Import)),
        "run" => import_cmd::command(scan.sub, &scan.fields).map(Command::Import),
        "mcp" if scan.help => Ok(Command::Help(Topic::Mcp)),
        "mcp" => match scan.fields.first() {
            Some((name, _)) => Err(Usage::new(format!(
                "unknown flag --{} for mcp",
                shown(name)
            ))),
            None => Ok(Command::Mcp),
        },
        _ => {
            let spec = catalog
                .iter()
                .find(|spec| spec.kind.as_str() == op)
                .ok_or_else(|| {
                    Usage::new(format!(
                        "unknown operation {}; trackwright --help lists them",
                        shown(&op)
                    ))
                })?;
            if scan.help {
                return Ok(Command::Help(Topic::Op(spec)));
            }
            let args = arguments(spec, &scan.fields)?;
            Ok(Command::Op { spec, args })
        }
    }
}

fn init(fields: &[(String, &str)]) -> Result<Command, Usage> {
    let mut key = None;
    let mut name = None;
    for (field, value) in fields {
        let slot = match field.as_str() {
            "key" => &mut key,
            "name" => &mut name,
            _ => {
                return Err(Usage::new(format!(
                    "unknown flag --{} for init",
                    shown(field)
                )));
            }
        };
        if slot.replace((*value).to_owned()).is_some() {
            return Err(Usage::new(format!("--{field} given twice")));
        }
    }
    let key = key.ok_or_else(|| Usage::new("init needs --key KEY (2 to 8 letters A-Z)"))?;
    Ok(Command::Init { key, name })
}

fn arguments(spec: &OpSpec, fields: &[(String, &str)]) -> Result<Map<String, Value>, Usage> {
    let op = spec.kind.as_str();
    let mut args = Map::new();
    for (name, text) in fields {
        let field = spec
            .fields
            .iter()
            .find(|field| field.name == name)
            .ok_or_else(|| {
                Usage::new(format!(
                    "unknown flag --{} for {op}; trackwright {op} --help lists its fields",
                    shown(name)
                ))
            })?;
        let value = value_of(field, text)?;
        if matches!(field.ty, FieldType::Keys) {
            let list = args
                .entry(field.name)
                .or_insert_with(|| Value::Array(Vec::new()));
            list.as_array_mut()
                .expect("a list field holds an array")
                .push(value);
        } else if args.insert(field.name.to_owned(), value).is_some() {
            return Err(Usage::new(format!("--{} given twice", field.name)));
        }
    }
    assert!(args.len() <= fields.len());
    Ok(args)
}

fn value_of(field: &FieldSpec, text: &str) -> Result<Value, Usage> {
    let value = match field.ty {
        FieldType::Project
        | FieldType::Ticket
        | FieldType::Test(_)
        | FieldType::Line { .. }
        | FieldType::Actor
        | FieldType::Text(_)
        | FieldType::Choice(_)
        | FieldType::Day
        | FieldType::Cursor
        | FieldType::RequestId
        | FieldType::Word
        | FieldType::Path
        | FieldType::Keys => Value::String(text.to_owned()),
        FieldType::Version | FieldType::Integer { .. } => text
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| Usage::new(format!("--{} takes an integer", field.name)))?,
        FieldType::Flag => match text {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return Err(Usage::new(format!("--{} takes true or false", field.name))),
        },
        FieldType::Shape(_) => serde_json::from_str::<Value>(text)
            .ok()
            .filter(|value| value.is_array() || value.is_object())
            .ok_or_else(|| Usage::new(format!("--{} takes a JSON array or object", field.name)))?,
    };
    let whole = matches!(field.ty, FieldType::Shape(_));
    assert!(whole || value.is_string() || value.is_i64() || value.is_boolean());
    assert!(!whole || value.is_array() || value.is_object());
    Ok(value)
}

fn assert_catalog_leaves_cli_names_free(catalog: &[OpSpec]) {
    for spec in catalog {
        assert!(
            !COMMANDS.contains(&spec.kind.as_str()),
            "{} is a CLI command",
            spec.kind
        );
        for field in spec.fields {
            assert!(
                !RESERVED.contains(&field.name),
                "--{} is a CLI flag",
                field.name
            );
            assert!(
                !field.name.contains('-'),
                "field names use _ so - can stand for it"
            );
        }
    }
}

fn shown(word: &str) -> String {
    let plain = word.len() <= SHOWN_BYTES_MAX
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    if plain {
        word.to_owned()
    } else {
        "(a long or unusual word)".to_owned()
    }
}
