use crate::catalog::{
    self, CURSOR_BYTES_MAX, FieldSpec, FieldType, KEY_BYTES_MAX, KEYS_MAX, PATH_BYTES_MAX, Shape,
    WORD_BYTES_MAX,
};
use crate::model::{
    Actor, CheckKind, CriterionState, EvidenceResult, OpKind, PolicyKey, ProjectKey,
    REQUEST_BYTES_MAX, REQUEST_ID_BYTES_MAX, Refusal, ShortId, TicketKind, TicketStatus,
    VersionStatus, codes,
};
use crate::reply::{REPLY_BYTES_DEFAULT, REPLY_BYTES_MAX, Reply};
use crate::runs::{IntegrityRef, NewResult, PolicyRef, RunEnd, SourceRef, StopReason};
use crate::store::{self, StoreError};
use crate::tests_model::{CaseKind, CaseStatus, PlanStatus, TestId, Verdict};
use crate::text;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const REQUESTS_KEPT_MAX: i64 = 100_000;
pub const REQUESTS_KEPT_MS_MAX: i64 = 7 * 24 * 60 * 60 * 1000;
const FIELD_NAME_SHOWN_BYTES_MAX: usize = 32;

pub(crate) const REQUEST_GET: &str =
    "SELECT payload_sha, reply_json FROM tk_requests WHERE actor = ?1 AND request_id = ?2";
pub(crate) const REQUEST_INSERT: &str = "INSERT INTO tk_requests(actor, request_id, payload_sha, \
     reply_json, at_ms) VALUES (?1, ?2, ?3, ?4, ?5)";
pub(crate) const REQUESTS_EXPIRE: &str = "DELETE FROM tk_requests WHERE at_ms < ?1";
pub(crate) const REQUESTS_COUNT: &str = "SELECT count(*) FROM tk_requests";
pub(crate) const REQUESTS_TRIM: &str = "DELETE FROM tk_requests WHERE (actor, request_id) IN \
     (SELECT actor, request_id FROM tk_requests ORDER BY at_ms, actor, request_id LIMIT ?1)";

/// One argument after its field's checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Arg {
    Project(ProjectKey),
    Ticket(ShortId),
    Test(TestId),
    Actor(Actor),
    Text(String),
    Integer(i64),
    Choice(&'static str),
    Flag(bool),
    Keys(Vec<String>),
    Json(Value),
}

impl Arg {
    fn to_json(&self) -> Value {
        match self {
            Arg::Project(key) => json!(key.as_str()),
            Arg::Ticket(id) => json!(id.to_string()),
            Arg::Test(id) => json!(id.to_string()),
            Arg::Actor(actor) => json!(actor.as_str()),
            Arg::Text(text) => json!(text),
            Arg::Integer(value) => json!(value),
            Arg::Choice(name) => json!(name),
            Arg::Flag(value) => json!(value),
            Arg::Keys(keys) => json!(keys),
            Arg::Json(value) => value.clone(),
        }
    }
}

/// Reads one argument as the type a handler expects; a field read as another type than its
/// catalog type is a programmer error.
pub(crate) trait FromArg<'a>: Sized {
    fn from_arg(arg: &'a Arg) -> Option<Self>;
}

impl<'a> FromArg<'a> for &'a ProjectKey {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Project(key) => Some(key),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a ShortId {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Ticket(id) => Some(id),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a TestId {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Test(id) => Some(id),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a Value {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Json(value) => Some(value),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a Actor {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Actor(actor) => Some(actor),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a str {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Text(text) => Some(text),
            _ => None,
        }
    }
}

impl FromArg<'_> for i64 {
    fn from_arg(arg: &Arg) -> Option<Self> {
        match arg {
            Arg::Integer(value) => Some(*value),
            _ => None,
        }
    }
}

impl FromArg<'_> for bool {
    fn from_arg(arg: &Arg) -> Option<Self> {
        match arg {
            Arg::Flag(value) => Some(*value),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a [String] {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Keys(keys) => Some(keys),
            _ => None,
        }
    }
}

macro_rules! choice_arg {
    ($($name:ident),+) => {
        $(impl FromArg<'_> for $name {
            fn from_arg(arg: &Arg) -> Option<Self> {
                match arg {
                    Arg::Choice(name) => {
                        let value = $name::parse(name);
                        assert!(value.is_some(), "a parsed choice is one of its enum's names");
                        value
                    }
                    _ => None,
                }
            }
        })+
    };
}

choice_arg!(
    TicketKind,
    TicketStatus,
    CriterionState,
    CheckKind,
    EvidenceResult,
    VersionStatus,
    PolicyKey,
    OpKind,
    CaseKind,
    CaseStatus,
    PlanStatus,
    Verdict,
    RunEnd,
    StopReason
);

/// One request's checked arguments in catalog order, the request id kept apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    kind: OpKind,
    values: Vec<(&'static str, Arg)>,
}

impl Args {
    /// Tells whether the operation has a field of that name. Input: the name. Output: true
    /// when the catalog lists it.
    pub(crate) fn has_field(&self, name: &str) -> bool {
        catalog::spec(self.kind)
            .fields
            .iter()
            .any(|field| field.name == name)
    }

    /// The catalog type of one field. Input: the name. Output: its type, or None when the
    /// operation has no field of that name.
    pub(crate) fn field_type(&self, name: &str) -> Option<FieldType> {
        let fields = catalog::spec(self.kind).fields;
        let found = fields.iter().find(|field| field.name == name);
        assert!(found.is_none() || self.has_field(name));
        found.map(|field| field.ty)
    }

    /// Reads an optional field. Input: the name, a field of this operation. Output: its
    /// value, or None when the request left it out.
    pub(crate) fn get<'a, T: FromArg<'a>>(&'a self, name: &str) -> Option<T> {
        assert!(
            self.has_field(name),
            "{}: {name} is not one of its fields",
            self.kind
        );
        let (_, arg) = self.values.iter().find(|(field, _)| *field == name)?;
        let value = T::from_arg(arg);
        assert!(
            value.is_some(),
            "{}: {name} is read as its catalog type",
            self.kind
        );
        value
    }

    /// Reads a required field, which parsing always keeps. Input: the name. Output: its
    /// value.
    pub(crate) fn need<'a, T: FromArg<'a>>(&'a self, name: &str) -> T {
        let required = catalog::spec(self.kind)
            .fields
            .iter()
            .any(|field| field.name == name && field.required);
        assert!(required, "{}: {name} is a required field", self.kind);
        match self.get(name) {
            Some(value) => value,
            None => panic!("{}: parsing keeps the required field {name}", self.kind),
        }
    }

    fn canonical(&self) -> Value {
        let mut args = Map::new();
        for (name, arg) in &self.values {
            args.insert((*name).to_owned(), arg.to_json());
        }
        json!({"op": self.kind.as_str(), "args": args})
    }
}

/// One parsed request: the operation, its checked arguments, the caller's request id, the
/// hash of the operation and arguments a repeat is compared by, and the reply bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    args: Args,
    request_id: Option<String>,
    payload_sha: String,
    reply_bytes_max: usize,
}

impl Request {
    /// Parses one request from an operation name and its JSON arguments object, checking
    /// every field against the catalog. Refuses a request over `REQUEST_BYTES_MAX`, an
    /// unknown operation, arguments that are not one JSON object, an unknown or missing
    /// field, a value of the wrong type or shape, and text the text rules refuse. Inputs:
    /// the operation and the argument bytes. Output: the request, or the refusal.
    pub fn parse(op: &str, args: &[u8]) -> Result<Self, Refusal> {
        if args.len() > REQUEST_BYTES_MAX {
            return Err(Refusal::new(
                codes::REQUEST_TOO_LARGE,
                format!("a request is at most {REQUEST_BYTES_MAX} bytes"),
            ));
        }
        let kind = OpKind::parse(op).ok_or_else(|| {
            Refusal::new(
                codes::UNKNOWN_OP,
                "no such operation; the schema operation lists them",
            )
        })?;
        let object = match serde_json::from_slice::<Value>(args) {
            Ok(Value::Object(object)) => object,
            _ => {
                return Err(Refusal::new(
                    codes::ARGS_INVALID,
                    "the arguments are one JSON object",
                ));
            }
        };
        let spec = catalog::spec(kind);
        if let Some(unknown) = object
            .keys()
            .find(|name| !spec.fields.iter().any(|field| field.name == name.as_str()))
        {
            return Err(Refusal::new(
                codes::UNKNOWN_FIELD,
                format!("{kind} has no field {}", shown(unknown)),
            ));
        }
        let mut values = Vec::with_capacity(spec.fields.len());
        let mut request_id = None;
        for field in spec.fields {
            match object.get(field.name) {
                None if field.required => {
                    return Err(Refusal::new(
                        codes::MISSING_FIELD,
                        format!("{kind} needs the field {}", field.name),
                    ));
                }
                None => {}
                Some(value) => match (field.ty, checked(field, value)?) {
                    (FieldType::RequestId, Arg::Text(text)) => request_id = Some(text),
                    (_, arg) => values.push((field.name, arg)),
                },
            }
        }
        assert!(values.len() + usize::from(request_id.is_some()) == object.len());
        let args = Args { kind, values };
        let payload_sha = sha256_hex(args.canonical().to_string().as_bytes());
        Ok(Self {
            args,
            request_id,
            payload_sha,
            reply_bytes_max: REPLY_BYTES_DEFAULT,
        })
    }

    /// Raises the reply bound for a host that carries larger replies. Input: the bound,
    /// from `REPLY_BYTES_DEFAULT` to `REPLY_BYTES_MAX`. Output: the request.
    pub fn with_reply_bytes_max(mut self, bytes: usize) -> Self {
        assert!(bytes >= REPLY_BYTES_DEFAULT);
        assert!(bytes <= REPLY_BYTES_MAX);
        self.reply_bytes_max = bytes;
        self
    }

    /// The operation. Output: its kind.
    pub fn kind(&self) -> OpKind {
        self.args.kind
    }

    /// The most bytes the reply may hold. Output: the bound.
    pub fn reply_bytes_max(&self) -> usize {
        self.reply_bytes_max
    }

    pub(crate) fn args(&self) -> &Args {
        &self.args
    }

    pub(crate) fn request_id(&self) -> Option<&str> {
        assert!(self.request_id.is_none() || self.kind().is_write());
        self.request_id.as_deref()
    }

    pub(crate) fn payload_sha(&self) -> &str {
        &self.payload_sha
    }
}

fn checked(field: &FieldSpec, value: &Value) -> Result<Arg, Refusal> {
    let invalid = |why: &str| {
        Refusal::new(
            codes::FIELD_INVALID,
            format!("the field {} {why}", field.name),
        )
    };
    let shaped = |result: Result<Arg, Refusal>| {
        result.map_err(|refusal| invalid(&format!("is refused: {}", refusal.message)))
    };
    match field.ty {
        FieldType::Project => shaped(ProjectKey::new(string(value, &invalid)?).map(Arg::Project)),
        FieldType::Ticket => shaped(ShortId::parse(string(value, &invalid)?).map(Arg::Ticket)),
        FieldType::Test(object) => {
            shaped(TestId::parse_of(string(value, &invalid)?, object).map(Arg::Test))
        }
        FieldType::Line { bytes_max } => line(value, bytes_max, &invalid),
        FieldType::Shape(shape) => whole(value, shape, &invalid),
        FieldType::Actor => shaped(Actor::new(string(value, &invalid)?).map(Arg::Actor)),
        FieldType::Text(kind) => {
            let text = string(value, &invalid)?;
            text::check(kind, text)?;
            Ok(Arg::Text(text.to_owned()))
        }
        FieldType::Version => integer(value, 1, i64::MAX, &invalid),
        FieldType::Integer { min, max } => integer(value, min, max, &invalid),
        FieldType::Choice(named) => {
            let text = string(value, &invalid)?;
            match named.names().into_iter().find(|name| *name == text) {
                Some(name) => Ok(Arg::Choice(name)),
                None => Err(invalid(&format!("is one of {}", named.names().join(", ")))),
            }
        }
        FieldType::Day => match string(value, &invalid)? {
            text if calendar_day(text) => Ok(Arg::Text(text.to_owned())),
            _ => Err(invalid("is a calendar day YYYY-MM-DD")),
        },
        FieldType::Flag => match value {
            Value::Bool(flag) => Ok(Arg::Flag(*flag)),
            _ => Err(invalid("is true or false")),
        },
        FieldType::Cursor => word(value, CURSOR_BYTES_MAX, cursor_byte, &invalid),
        FieldType::RequestId => word(value, REQUEST_ID_BYTES_MAX, key_byte, &invalid),
        FieldType::Word => word(value, WORD_BYTES_MAX, word_byte, &invalid),
        FieldType::Path => {
            let text = string(value, &invalid)?;
            if text.is_empty() || text.len() > PATH_BYTES_MAX {
                return Err(invalid(&format!("is 1 to {PATH_BYTES_MAX} bytes")));
            }
            if text.contains(['\0', '\n', '\r']) || text::hidden(text) {
                return Err(invalid("is one line of visible characters"));
            }
            Ok(Arg::Text(text.to_owned()))
        }
        FieldType::Keys => keys(value, &invalid),
    }
}

fn line(
    value: &Value,
    bytes_max: usize,
    invalid: &dyn Fn(&str) -> Refusal,
) -> Result<Arg, Refusal> {
    assert!(bytes_max > 0);
    let text = string(value, invalid)?;
    if text.trim().is_empty() || text.len() > bytes_max {
        return Err(invalid(&format!("is 1 to {bytes_max} bytes")));
    }
    if text.contains(['\n', '\r']) || text::hidden(text) {
        return Err(invalid("is one line of visible characters"));
    }
    assert!(!text.is_empty());
    Ok(Arg::Text(text.to_owned()))
}

fn whole(value: &Value, shape: Shape, invalid: &dyn Fn(&str) -> Refusal) -> Result<Arg, Refusal> {
    let bytes_max = shape.bytes_max();
    let json = value.to_string();
    if json.len() > bytes_max {
        return Err(invalid(&format!("is at most {bytes_max} bytes of JSON")));
    }
    let read = |ok: bool, what: &str| if ok { Ok(()) } else { Err(invalid(what)) };
    match shape {
        Shape::Steps => read(
            serde_json::from_str::<Vec<String>>(&json).is_ok(),
            "is an array of step texts",
        )?,
        Shape::Results => read(
            serde_json::from_str::<Vec<NewResult>>(&json).is_ok(),
            "is an array of results with name and outcome",
        )?,
        Shape::Source => SourceRef::parse(&json).map(|_| ())?,
        Shape::Policy => PolicyRef::parse(&json).map(|_| ())?,
        Shape::Integrity => IntegrityRef::parse(&json).map(|_| ())?,
    }
    assert!(value.is_array() || value.is_object());
    Ok(Arg::Json(value.clone()))
}

fn string<'v>(value: &'v Value, invalid: &dyn Fn(&str) -> Refusal) -> Result<&'v str, Refusal> {
    value.as_str().ok_or_else(|| invalid("is a string"))
}

fn integer(
    value: &Value,
    min: i64,
    max: i64,
    invalid: &dyn Fn(&str) -> Refusal,
) -> Result<Arg, Refusal> {
    assert!(min <= max);
    match value.as_i64() {
        Some(number) if (min..=max).contains(&number) => Ok(Arg::Integer(number)),
        _ => Err(invalid(&format!("is a whole number from {min} to {max}"))),
    }
}

fn word(
    value: &Value,
    bytes_max: usize,
    allowed: fn(u8) -> bool,
    invalid: &dyn Fn(&str) -> Refusal,
) -> Result<Arg, Refusal> {
    word_text(value, bytes_max, allowed, invalid).map(Arg::Text)
}

fn word_text(
    value: &Value,
    bytes_max: usize,
    allowed: fn(u8) -> bool,
    invalid: &dyn Fn(&str) -> Refusal,
) -> Result<String, Refusal> {
    let text = string(value, invalid)?;
    if text.is_empty() || text.len() > bytes_max || !text.bytes().all(allowed) {
        return Err(invalid(&format!(
            "is 1 to {bytes_max} bytes of the characters its schema lists"
        )));
    }
    Ok(text.to_owned())
}

fn keys(value: &Value, invalid: &dyn Fn(&str) -> Refusal) -> Result<Arg, Refusal> {
    let items = value
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= KEYS_MAX)
        .ok_or_else(|| invalid(&format!("is a list of 1 to {KEYS_MAX} keys")))?;
    let mut keys = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    for item in items {
        let key = word_text(item, KEY_BYTES_MAX, key_byte, invalid)?;
        if !seen.insert(key.clone()) {
            return Err(invalid("lists each key once"));
        }
        keys.push(key);
    }
    assert_eq!(keys.len(), items.len());
    Ok(Arg::Keys(keys))
}

fn cursor_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b':' | b'_' | b'-')
}

fn key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')
}

fn word_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
}

/// Tells whether a text is a real calendar day written `YYYY-MM-DD`, years 0001 to 9999.
/// Input: the text. Output: true for a real day.
pub(crate) fn calendar_day(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &text[range];
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year >= 1 && (1..=days).contains(&day)
}

fn shown(name: &str) -> String {
    let plain = !name.is_empty()
        && name.len() <= FIELD_NAME_SHOWN_BYTES_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if plain {
        name.to_owned()
    } else {
        "with that name".to_owned()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(hex.len(), 64);
    hex
}

/// What the store holds for a write's request id: nothing, the first reply of the same
/// write, or a first write with other fields.
pub(crate) enum Remembered {
    New,
    Repeat(Reply),
    Reused,
}

/// Looks up a write's request id for its actor. Inputs: the transaction, the actor and the
/// request. Output: what the store remembers.
pub(crate) fn remembered(
    tx: &Transaction<'_>,
    actor: &Actor,
    request: &Request,
) -> Result<Remembered, StoreError> {
    let Some(request_id) = request.request_id() else {
        return Ok(Remembered::New);
    };
    let stored: Option<(String, String)> = store::prepare(tx, REQUEST_GET)?
        .query_row(params![actor.as_str(), request_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    Ok(match stored {
        None => Remembered::New,
        Some((sha, reply)) if sha == request.payload_sha() => {
            Remembered::Repeat(Reply::stored(reply))
        }
        Some(_) => Remembered::Reused,
    })
}

/// Keeps a successful write's reply under its request id, then drops records older than
/// seven days and the oldest beyond `REQUESTS_KEPT_MAX`. Inputs: the transaction, the
/// actor, the request, the reply and the time. Output: Ok, or the store error.
pub(crate) fn remember(
    tx: &Transaction<'_>,
    actor: &Actor,
    request: &Request,
    reply: &Reply,
    at_ms: i64,
) -> Result<(), StoreError> {
    assert!(reply.is_ok(), "only a successful write is remembered");
    let Some(request_id) = request.request_id() else {
        return Ok(());
    };
    store::prepare(tx, REQUEST_INSERT)?.execute(params![
        actor.as_str(),
        request_id,
        request.payload_sha(),
        reply.json(),
        at_ms
    ])?;
    store::prepare(tx, REQUESTS_EXPIRE)?.execute([at_ms.saturating_sub(REQUESTS_KEPT_MS_MAX)])?;
    let kept: i64 = store::prepare(tx, REQUESTS_COUNT)?.query_row([], |row| row.get(0))?;
    if kept > REQUESTS_KEPT_MAX {
        let trimmed = store::prepare(tx, REQUESTS_TRIM)?.execute([kept - REQUESTS_KEPT_MAX])?;
        assert_eq!(trimmed as i64, kept - REQUESTS_KEPT_MAX);
    }
    Ok(())
}
