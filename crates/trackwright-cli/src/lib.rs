#![forbid(unsafe_code)]

mod args;
mod human;
mod import_cmd;
mod locate;
pub mod tui;
pub mod views;

pub use args::ARGUMENTS_MAX;

use args::{Command, Invocation};
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use trackwright_core::catalog::{self, OpSpec, catalog};
use trackwright_core::model::{Actor, OpKind, ProjectKey, Refusal, codes};
use trackwright_core::ops;
use trackwright_core::reply::Reply;
use trackwright_core::requests::Request;
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{self, Store, StoreError};

/// The environment variable naming the actor when `--actor` is absent.
pub const ACTOR_ENV: &str = "TRACKWRIGHT_ACTOR";
/// Exit status of a reply the core or the CLI refused; the reply says why.
pub const EXIT_REFUSED: u8 = 1;
/// Exit status of a command line the CLI could not parse; one line on stderr says why.
pub const EXIT_USAGE: u8 = 2;
/// The code of a reply or MCP session that could not use stdin or stdout.
const STDIO_FAILED: &str = "stdio_failed";
/// The code of a call refused because the system clock reads before 1970.
const CLOCK_INVALID: &str = "clock_invalid";

/// A refusal the CLI replies itself around the core, such as a database it cannot find.
/// Fields: a stable code and one line saying why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    /// Builds a failure. Inputs: the code, the message. Output: the failure.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        assert!(!code.is_empty());
        Self {
            code,
            message: message.into(),
        }
    }

    /// The reply envelope for this failure, shaped like the core's refusals.
    /// Output: the envelope.
    fn envelope(&self) -> Value {
        json!({"api": 1, "ok": false, "code": self.code, "message": self.message, "next": []})
    }
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::new(refusal.code, refusal.message)
    }
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Refused(refusal) => refusal.into(),
            StoreError::Sqlite(error) => Self::new(
                codes::STORE_FAILED,
                format!("the tracker database failed: {error}"),
            ),
            StoreError::Corrupt(detail) => Self::new(
                codes::STORE_FAILED,
                format!("the tracker database holds a row it cannot read: {detail}"),
            ),
        }
    }
}

/// Runs one `trackwright` call: parses the words, runs the operation on the located
/// database as the named actor, and prints the envelope (`--json`) or short human text.
/// Input: the words after the program name. Output: 0 done, 1 refused, 2 usage error.
pub fn run(args: &[OsString]) -> ExitCode {
    let invocation = match args::parse(args, catalog()) {
        Ok(invocation) => invocation,
        Err(usage) => return usage_exit(usage.message()),
    };
    match &invocation.command {
        Command::Help(topic) => match write_stdout(&human::help(*topic, catalog())) {
            Ok(()) => ExitCode::SUCCESS,
            Err(failure) => stderr_exit(&failure),
        },
        Command::Op { spec, args }
            if spec.kind == OpKind::Schema && args.is_empty() && !invocation.json =>
        {
            schema()
        }
        Command::Init { .. } | Command::Mcp | Command::Op { .. } | Command::Import(_) => {
            as_actor(invocation)
        }
    }
}

fn as_actor(invocation: Invocation) -> ExitCode {
    let named = invocation.actor.or_else(|| {
        std::env::var(ACTOR_ENV)
            .ok()
            .filter(|value| !value.is_empty())
    });
    let Some(actor) = named else {
        return usage_exit(&format!("name the actor with --actor ACTOR or {ACTOR_ENV}"));
    };
    let place = Place::here(invocation.db);
    let envelope = match invocation.command {
        Command::Mcp => {
            return match place.and_then(|place| serve(&actor, &place)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(failure) => stderr_exit(&failure),
            };
        }
        Command::Init { key, name } => {
            place.and_then(|place| init(&actor, &place, &key, name.as_deref()))
        }
        Command::Op { spec, args } => place.and_then(|place| operation(&actor, &place, spec, args)),
        Command::Import(import) => place.and_then(|place| import_cmd::run(&actor, &place, &import)),
        Command::Help(_) => unreachable!("help is answered before an actor is needed"),
    };
    reply(
        invocation.json,
        &envelope.unwrap_or_else(|failure| failure.envelope()),
    )
}

/// Where a call looks for its database: the `--db` flag, `TRACKWRIGHT_DB` and the working
/// folder, read once.
struct Place {
    flag: Option<PathBuf>,
    env: Option<OsString>,
    cwd: PathBuf,
}

impl Place {
    fn here(flag: Option<PathBuf>) -> Result<Self, Failure> {
        let cwd = std::env::current_dir().map_err(|error| {
            Failure::new(
                locate::codes::STORE_UNREADABLE,
                format!("cannot read the working folder: {}", error.kind()),
            )
        })?;
        assert!(cwd.is_absolute());
        Ok(Self {
            flag,
            env: std::env::var_os(locate::DB_ENV),
            cwd,
        })
    }

    fn existing(&self) -> Result<PathBuf, Failure> {
        locate::existing(self.flag.as_deref(), self.env.as_deref(), &self.cwd)
    }

    fn for_init(&self) -> Result<(PathBuf, bool), Failure> {
        locate::for_init(self.flag.as_deref(), self.env.as_deref(), &self.cwd)
    }
}

fn operation(
    actor: &str,
    place: &Place,
    spec: &'static OpSpec,
    args: Map<String, Value>,
) -> Result<Value, Failure> {
    let actor = Actor::new(actor)?;
    let bytes = serde_json::to_vec(&Value::Object(args)).expect("a JSON object serializes");
    let request = Request::parse(spec.kind.as_str(), &bytes)?;
    let mut store = Store::open(&place.existing()?)?;
    let transaction = store.transaction()?;
    let envelope = envelope_of(ops::dispatch(&transaction, &Standalone, &actor, request));
    if envelope["ok"] == true {
        transaction.commit().map_err(StoreError::from)?;
    } else {
        transaction.rollback().map_err(StoreError::from)?;
    }
    Ok(envelope)
}

fn init(actor: &str, place: &Place, key: &str, name: Option<&str>) -> Result<Value, Failure> {
    let actor = Actor::new(actor)?;
    let key = ProjectKey::new(key)?;
    let (path, existed) = place.for_init()?;
    let mut store = Store::open(&path)?;
    let transaction = store.transaction()?;
    let name = name.unwrap_or(key.as_str());
    store::create_project(&transaction, &actor, &key, name, now_ms()?)?;
    transaction.commit().map_err(StoreError::from)?;
    Ok(json!({
        "api": 1,
        "ok": true,
        "data": {"project": key.as_str(), "db": path.display().to_string(), "created": !existed},
        "next": [{"op": "create", "why": format!("add the first ticket to {key}")}],
        "cursor": null,
    }))
}

/// Serves MCP on stdin and stdout as the launch actor until stdin closes. Nothing but
/// JSON-RPC reaches stdout, so every failure goes to stderr. Inputs: actor, place.
/// Output: done, or the failure.
fn serve(actor: &str, place: &Place) -> Result<(), Failure> {
    let actor = Actor::new(actor)?;
    let store = Store::open(&place.existing()?)?;
    let session = trackwright_mcp::Session::new(store.into_connection(), Standalone, actor);
    trackwright_mcp::serve(io::stdin().lock(), io::stdout().lock(), &session)
        .map_err(|error| Failure::new(STDIO_FAILED, format!("stdio failed: {}", error.kind())))
}

fn schema() -> ExitCode {
    let schema = catalog::schema_v1();
    assert_eq!(schema["api"], 1, "the published schema is API version 1");
    let text = serde_json::to_string_pretty(&schema).expect("a JSON value serializes") + "\n";
    assert!(
        text.starts_with('{'),
        "the published schema is one JSON object"
    );
    match write_stdout(&text) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => stderr_exit(&failure),
    }
}

fn envelope_of(reply: Reply) -> Value {
    let envelope: Value = serde_json::from_str(reply.json()).expect("the core replies JSON");
    assert_eq!(envelope["api"], 1, "the core replies api 1");
    assert_eq!(
        envelope["ok"].as_bool(),
        Some(reply.is_ok()),
        "the envelope agrees with is_ok"
    );
    envelope
}

fn now_ms() -> Result<i64, Failure> {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Failure::new(CLOCK_INVALID, "the system clock is before 1970"))?;
    let now_ms = i64::try_from(since.as_millis()).expect("milliseconds since 1970 fit in i64");
    assert!(now_ms > 0);
    Ok(now_ms)
}

fn reply(json: bool, envelope: &Value) -> ExitCode {
    let ok = envelope["ok"].as_bool().expect("every envelope says ok");
    let text = if json {
        format!("{envelope}\n")
    } else {
        human::render(envelope)
    };
    assert!(
        !json || text.matches('\n').count() == 1,
        "--json prints one line"
    );
    match write_stdout(&text) {
        Ok(()) if ok => ExitCode::SUCCESS,
        Ok(()) => ExitCode::from(EXIT_REFUSED),
        Err(failure) => stderr_exit(&failure),
    }
}

fn write_stdout(text: &str) -> Result<(), Failure> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| Failure::new(STDIO_FAILED, format!("stdout failed: {}", error.kind())))
}

fn stderr_exit(failure: &Failure) -> ExitCode {
    let text = format!(
        "trackwright: {}: {}\n",
        failure.code,
        human::line(&failure.message)
    );
    let _ = io::stderr().write_all(text.as_bytes());
    ExitCode::from(EXIT_REFUSED)
}

fn usage_exit(message: &str) -> ExitCode {
    let text = format!("trackwright: {}\n", human::line(message));
    assert_eq!(text.matches('\n').count(), 1, "a usage error is one line");
    let _ = io::stderr().write_all(text.as_bytes());
    ExitCode::from(EXIT_USAGE)
}
