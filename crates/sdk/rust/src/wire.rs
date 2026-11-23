use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The standard base64 alphabet of RFC 4648, in sextet order.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The most digits a `u64` has in decimal.
const U64_DIGITS_MAX: usize = 20;

/// A `u64` on the wire: its canonical decimal string, since a JSON number loses
/// precision above 2^53 in a TS plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct U64(pub u64);

/// Bytes on the wire: their standard padded base64 string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

/// A handle on the wire: its kernel id as a canonical decimal string. Only the generated
/// envelope turns it back into a handle, through the handle's `from_kernel`, so the bus
/// checks it against the caller as for any other call; it has no `Debug`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Id(pub u64);

/// Why a stored event body did not convert to its wire form: the body is not the stored
/// shape of its kind, and the reason is the JSON decoder's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventError {
    pub reason: String,
}

/// Converts one stored event body to its wire JSON through the kind's two generated forms.
/// Input: the stored body bytes. Output: the wire JSON, or the [`EventError`] when the
/// bytes are not the stored shape; never a panic.
pub fn event_body<S, W>(body: &[u8]) -> Result<serde_json::Value, EventError>
where
    S: serde::de::DeserializeOwned,
    W: Serialize + From<S>,
{
    let refuse = |error: serde_json::Error| EventError {
        reason: alloc::string::ToString::to_string(&error),
    };
    let stored: S = serde_json::from_slice(body).map_err(refuse)?;
    serde_json::to_value(W::from(stored)).map_err(refuse)
}

/// Writes a `u64` as its decimal string. Input: the value. Output: the string.
pub fn encode_u64(value: u64) -> String {
    alloc::format!("{value}")
}

/// Reads a canonical decimal `u64`: ASCII digits only, no sign, no leading zero but in
/// `0`, below 2^64. Input: the text. Output: the value, or `None` for any other text.
pub fn decode_u64(text: &str) -> Option<u64> {
    let digits = text.as_bytes();
    if digits.is_empty() || digits.len() > U64_DIGITS_MAX {
        return None;
    }
    if !digits.iter().all(u8::is_ascii_digit) || (digits[0] == b'0' && digits.len() > 1) {
        return None;
    }
    let value = text.parse::<u64>().ok()?;
    assert_eq!(encode_u64(value), text);
    Some(value)
}

/// Writes bytes as standard padded base64. Input: the bytes. Output: the string, four
/// characters per three bytes or part of three.
pub fn encode_bytes(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied();
        let c = chunk.get(2).copied();
        let sextet = |value: u8| char::from(ALPHABET[usize::from(value & 63)]);
        text.push(sextet(a >> 2));
        text.push(sextet(((a & 3) << 4) | (b.unwrap_or(0) >> 4)));
        text.push(b.map_or('=', |b| sextet(((b & 15) << 2) | (c.unwrap_or(0) >> 6))));
        text.push(c.map_or('=', sextet));
    }
    assert_eq!(text.len(), bytes.len().div_ceil(3) * 4);
    text
}

/// Reads canonical standard padded base64: a length that is a multiple of four, `=` only
/// as the last one or two characters, and zero bits after the last byte. Input: the text.
/// Output: the bytes, or `None` for any other text.
pub fn decode_bytes(text: &str) -> Option<Vec<u8>> {
    let chars = text.as_bytes();
    if !chars.len().is_multiple_of(4) {
        return None;
    }
    let pad = chars
        .iter()
        .rev()
        .take_while(|symbol| **symbol == b'=')
        .count();
    if pad > 2 {
        return None;
    }
    let mut bytes = Vec::with_capacity(chars.len() / 4 * 3);
    for (index, quad) in chars.chunks(4).enumerate() {
        let last = (index + 1) * 4 == chars.len();
        let kept = if last { 4 - pad } else { 4 };
        let mut triple: u32 = 0;
        for (place, symbol) in quad.iter().enumerate() {
            let sextet = if place < kept { sextet_of(*symbol)? } else { 0 };
            triple |= u32::from(sextet) << (18 - 6 * place);
        }
        let [_, first, second, third] = triple.to_be_bytes();
        bytes.extend_from_slice(&[first, second, third][..kept - 1]);
    }
    if encode_bytes(&bytes) != text {
        return None;
    }
    Some(bytes)
}

/// The sextet of one base64 character. Input: the character. Output: its value, or `None`
/// for a character outside the alphabet, `=` included.
fn sextet_of(symbol: u8) -> Option<u8> {
    let at = ALPHABET.iter().position(|known| *known == symbol)?;
    Some(u8::try_from(at).expect("the alphabet has 64 characters"))
}

/// Reads one wire string with a decoder. Inputs: the deserializer, what the string must
/// be, for the error, and the decoder. Output: the decoded value, or the serde error.
fn read<'de, D, T>(
    deserializer: D,
    expected: &'static str,
    decode: fn(&str) -> Option<T>,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
{
    /// Accepts only a string the decoder takes, refusing every other JSON value.
    struct Text<T> {
        expected: &'static str,
        decode: fn(&str) -> Option<T>,
    }

    impl<T> Visitor<'_> for Text<T> {
        type Value = T;

        /// Names what the wire value must be. Input: the formatter. Output: the write result.
        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.expected)
        }

        /// Decodes one string. Input: the string. Output: the value, or an invalid-value error.
        fn visit_str<E: de::Error>(self, text: &str) -> Result<T, E> {
            (self.decode)(text).ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))
        }
    }

    deserializer.deserialize_str(Text { expected, decode })
}

impl Serialize for U64 {
    /// Writes the decimal string. Input: the serializer. Output: its result.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_u64(self.0))
    }
}

impl<'de> Deserialize<'de> for U64 {
    /// Reads a canonical decimal string. Input: the deserializer. Output: the value.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        read(
            deserializer,
            "sdk_wire_u64: a canonical decimal string",
            decode_u64,
        )
        .map(Self)
    }
}

impl Serialize for Bytes {
    /// Writes the padded base64 string. Input: the serializer. Output: its result.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_bytes(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    /// Reads canonical padded base64. Input: the deserializer. Output: the bytes.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        read(
            deserializer,
            "sdk_wire_bytes: canonical padded base64",
            decode_bytes,
        )
        .map(Self)
    }
}

impl Serialize for Id {
    /// Writes the kernel id as a decimal string. Input: the serializer. Output: its result.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_u64(self.0))
    }
}

impl<'de> Deserialize<'de> for Id {
    /// Reads a kernel id as a canonical decimal string. Input: the deserializer.
    /// Output: the id.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        read(
            deserializer,
            "sdk_wire_handle: a canonical decimal string",
            decode_u64,
        )
        .map(Self)
    }
}
