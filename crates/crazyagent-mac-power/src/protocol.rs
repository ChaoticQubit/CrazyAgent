//! The helper's wire protocol (threat model section 4): one newline-terminated JSON request per
//! connection, at most 1024 bytes, closed enums and bounded integers only, then one reply line.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const VERSION: u8 = 1;
pub const REQUEST_BYTES_MAX: usize = 1024;
pub const REPLY_BYTES_MAX: usize = 1024;
pub const LEASE_S_MIN: u16 = 30;
pub const LEASE_S_MAX: u16 = 300;

/// A request after its bounds were checked: a hold with its lease, a release of the caller's
/// own lease, a read of the state, or forgetting the pin (Remove lid helper).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Hold { lease_s: u16 },
    Release,
    Read,
    Forget,
}

/// Why a request was refused; sent back as the reply's `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    TooLong,
    Malformed,
    Version,
    LeaseOutOfRange,
    Unpinned,
    HoldersFull,
    Floor,
    JournalUnreadable,
    Pmset,
}

/// The last-resort floor as `Read` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloorView {
    None,
    Battery,
    Heat,
}

/// The uid pin as `Read` reports it to one caller: theirs, another account's, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinView {
    You,
    Other,
    None,
}

/// What `Read` returns (threat model section 4). `None` values were not readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub held: bool,
    pub holders: u8,
    pub lease_left_s: u16,
    pub sleep_disabled: Option<bool>,
    pub foreign: bool,
    pub journal_ok: bool,
    pub floor: FloorView,
    pub pin: PinView,
    pub lid_closed: Option<bool>,
    pub external_display: Option<bool>,
    pub low_power: Option<bool>,
}

/// One reply line, tagged by `reply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Held { lease_left_s: u16 },
    Released,
    Forgotten,
    State(View),
    Refused { reason: Refusal },
}

/// Reads one request: at most `REQUEST_BYTES_MAX` bytes up to and including its newline.
/// Input: the connection. Output: the checked request, or why it was refused.
pub fn read_request(reader: &mut impl Read) -> Result<Request, Refusal> {
    let _ = reader;
    Err(Refusal::Pmset)
}

/// Writes one reply line. Input: the connection and the reply. Output: the write result.
pub fn write_reply(writer: &mut impl Write, reply: &Reply) -> io::Result<()> {
    let _ = (writer, reply);
    Ok(())
}

/// The bytes a client sends for a request, newline included. Input: the request. Output: the
/// line.
pub fn encode_request(request: &Request) -> Vec<u8> {
    let _ = request;
    Vec::new()
}
