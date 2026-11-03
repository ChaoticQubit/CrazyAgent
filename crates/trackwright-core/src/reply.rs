use crate::catalog::CURSOR_BYTES_MAX;
use crate::model::{OpKind, Refusal, ShortId};
use serde::Serialize;
use serde_json::{Map, Value, json};

pub const API: u32 = 1;
pub const REPLY_BYTES_DEFAULT: usize = 8 * 1024;
pub const REPLY_BYTES_MAX: usize = 64 * 1024;
pub const NEXT_MAX: usize = 5;
pub const PAGE_ROWS_MAX: usize = 50;
pub const LISTED_TITLE_BYTES_MAX: usize = 80;
pub const MESSAGE_BYTES_MAX: usize = 512;
pub const WHY_BYTES_MAX: usize = 120;

const STORED_PREFIX: &str = "{\"api\":1,\"ok\":true,";

const _: () = assert!(REPLY_BYTES_DEFAULT <= REPLY_BYTES_MAX);
const _: () = assert!(API == 1);

/// One action the caller may take next. Fields: the operation, the ticket it acts on when
/// it acts on one, and one line saying why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Next {
    pub op: OpKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<ShortId>,
    pub why: String,
}

#[derive(Serialize)]
struct Success<'a> {
    api: u32,
    ok: bool,
    data: &'a Value,
    next: &'a [Next],
    cursor: Option<&'a str>,
}

#[derive(Serialize)]
struct Failure<'a> {
    api: u32,
    ok: bool,
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a Value>,
    next: &'a [Next],
}

/// One reply envelope as the JSON text a host sends on: success with data, the next
/// actions and a cursor, or a refusal with its code and message. A repeated request id
/// gets the stored text of its first reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    ok: bool,
    json: String,
}

impl Reply {
    /// Wraps a refusal met before dispatch, such as a request that did not parse. Input:
    /// the refusal. Output: the reply, with no next actions.
    pub fn refused(refusal: &Refusal) -> Self {
        Self::failure(
            refusal.code,
            &refusal.message,
            None,
            &[],
            REPLY_BYTES_DEFAULT,
        )
    }

    /// Tells whether the operation succeeded. Output: the envelope's `ok`.
    pub fn is_ok(&self) -> bool {
        self.ok
    }

    /// The envelope's JSON text. Output: the text, within the request's reply bound.
    pub fn json(&self) -> &str {
        &self.json
    }

    pub(crate) fn success(data: &Value, next: &[Next], cursor: Option<&str>, bound: usize) -> Self {
        assert!(next.len() <= NEXT_MAX);
        assert!(cursor.is_none_or(|cursor| !cursor.is_empty() && cursor.len() <= CURSOR_BYTES_MAX));
        let json = encode(&Success {
            api: API,
            ok: true,
            data,
            next,
            cursor,
        });
        assert!(json.starts_with(STORED_PREFIX));
        assert!(
            json.len() <= bound,
            "a success reply of {} bytes stays in its page budget",
            json.len()
        );
        Self { ok: true, json }
    }

    pub(crate) fn failure(
        code: &str,
        message: &str,
        data: Option<&Value>,
        next: &[Next],
        bound: usize,
    ) -> Self {
        assert!(!code.is_empty());
        assert!(next.len() <= NEXT_MAX);
        let json = encode(&Failure {
            api: API,
            ok: false,
            code,
            message: elide(message, MESSAGE_BYTES_MAX).0,
            data,
            next,
        });
        assert!(
            json.len() <= bound,
            "a refusal of {} bytes stays small",
            json.len()
        );
        Self { ok: false, json }
    }

    pub(crate) fn stored(json: String) -> Self {
        assert!(
            json.starts_with(STORED_PREFIX),
            "only successful replies are stored"
        );
        Self { ok: true, json }
    }
}

fn encode<T: Serialize>(envelope: &T) -> String {
    match serde_json::to_string(envelope) {
        Ok(json) => json,
        Err(error) => panic!("a reply envelope always serializes: {error}"),
    }
}

/// The bytes a success reply's data may take under a bound, after the envelope with the
/// longest next actions and cursor. Input: the reply bound. Output: the data budget.
pub(crate) fn data_bytes_max(bound: usize) -> usize {
    assert!(bound >= REPLY_BYTES_DEFAULT);
    let longest_op = OpKind::ALL
        .iter()
        .map(|op| op.as_str())
        .max_by_key(|name| name.len())
        .unwrap_or("schema");
    let why = "w".repeat(WHY_BYTES_MAX);
    let next: Vec<Value> = (0..NEXT_MAX)
        .map(|_| json!({"op": longest_op, "id": "ABCDEFGH-4294967295", "why": why}))
        .collect();
    let envelope = json!({
        "api": API,
        "ok": true,
        "data": {},
        "next": next,
        "cursor": "c".repeat(CURSOR_BYTES_MAX),
    });
    let overhead = envelope.to_string().len() - 2;
    assert!(overhead < bound / 4);
    bound - overhead
}

/// Cuts a text to at most a number of bytes on a character boundary. Inputs: the text and
/// the bound. Output: the kept part and whether anything was cut.
pub(crate) fn elide(text: &str, bytes_max: usize) -> (&str, bool) {
    if text.len() <= bytes_max {
        return (text, false);
    }
    let mut end = bytes_max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    assert!(end <= bytes_max && end < text.len());
    (&text[..end], true)
}

/// The length of a value as compact JSON. Input: the value. Output: its byte length.
pub(crate) fn json_bytes(value: &Value) -> usize {
    match serde_json::to_vec(value) {
        Ok(bytes) => bytes.len(),
        Err(error) => panic!("a JSON value always serializes: {error}"),
    }
}

/// Builds one reply's data object within a byte budget, counting every member and array
/// element as compact JSON adds it, so a page never outgrows its reply.
#[derive(Debug)]
pub(crate) struct Page {
    data: Map<String, Value>,
    bytes: usize,
    bytes_max: usize,
}

impl Page {
    /// Starts an empty object. Input: the data budget. Output: the page.
    pub(crate) fn new(bytes_max: usize) -> Self {
        assert!(bytes_max >= 2);
        Self {
            data: Map::new(),
            bytes: 2,
            bytes_max,
        }
    }

    /// The bytes still free. Output: the count.
    pub(crate) fn left(&self) -> usize {
        assert!(self.bytes <= self.bytes_max);
        self.bytes_max - self.bytes
    }

    /// The bytes one more member would take. Inputs: its key and value. Output: the count.
    pub(crate) fn member_bytes(&self, key: &str, value: &Value) -> usize {
        usize::from(!self.data.is_empty()) + json_bytes(&json!(key)) + 1 + json_bytes(value)
    }

    /// Adds a member when it fits. Inputs: a key not used yet and the value. Output: whether
    /// it was added.
    pub(crate) fn put(&mut self, key: &str, value: Value) -> bool {
        assert!(!self.data.contains_key(key));
        let cost = self.member_bytes(key, &value);
        if cost > self.left() {
            return false;
        }
        self.bytes += cost;
        self.data.insert(key.to_owned(), value);
        true
    }

    /// Appends an element to the array member `key`, creating the array first. Inputs: the
    /// key and the element. Output: whether it fit.
    pub(crate) fn push(&mut self, key: &str, value: Value) -> bool {
        let element = json_bytes(&value);
        let cost = match self.data.get(key) {
            None => self.member_bytes(key, &json!([])) + element,
            Some(Value::Array(items)) if items.is_empty() => element,
            Some(Value::Array(_)) => 1 + element,
            Some(other) => panic!("{key} is an array member, not {other}"),
        };
        if cost > self.left() {
            return false;
        }
        self.bytes += cost;
        match self
            .data
            .entry(key.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            Value::Array(items) => items.push(value),
            other => panic!("{key} is an array member, not {other}"),
        }
        true
    }

    /// Finishes the page. Output: the data object, exactly as many bytes as counted.
    pub(crate) fn into_value(self) -> Value {
        let value = Value::Object(self.data);
        assert_eq!(
            json_bytes(&value),
            self.bytes,
            "the page counts its own bytes"
        );
        assert!(self.bytes <= self.bytes_max);
        value
    }
}
