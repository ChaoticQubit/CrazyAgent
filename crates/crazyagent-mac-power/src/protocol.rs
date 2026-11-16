//! The helper's wire protocol (threat model section 4): one newline-terminated JSON request per
//! connection, at most 1024 bytes, closed enums and bounded integers only, then one reply line.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read, Write};

pub const VERSION: u8 = 1;
pub const REQUEST_BYTES_MAX: usize = 1024;
pub const REPLY_BYTES_MAX: usize = 1024;
pub const LEASE_S_MIN: u16 = 30;
pub const LEASE_S_MAX: u16 = 300;

/// What a holder asks of the display while the lid is closed during its hold (Task 12): sleep
/// it once per close, or leave it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LidDisplay {
    Sleep,
    LeaveOn,
}

/// A request after its bounds were checked: a hold with its lease, its display ask and whether
/// it asks Low Power Mode too, a power-only lease that asks only Low Power Mode (Task 16,
/// threat model 15.1), a release of the caller's own lease, a read of the state, or forgetting
/// the pin (Remove lid helper).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Hold {
        lease_s: u16,
        lid_display: LidDisplay,
        low_power: bool,
    },
    LowPower {
        lease_s: u16,
    },
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

/// What `Read` returns (threat model section 4). `None` values were not readable. `held` says a
/// lease keeps sleep off, `holders` counts every lease, `low_power` is the battery source's
/// Low Power Mode, and `low_power_held` says a lease asks for it (threat model 15.3).
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
    pub low_power_held: bool,
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

/// The calls a request line may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Call {
    Hold,
    LowPower,
    Release,
    Read,
    Forget,
}

/// A request line as it travels: the protocol version, the call, the lease of a hold or a
/// power-only lease, and a hold's display ask and Low Power Mode ask.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    call: Call,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lease_s: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lid_display: Option<LidDisplay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    low_power: Option<bool>,
}

/// Reads one request: at most `REQUEST_BYTES_MAX` bytes up to and including its newline.
/// Input: the connection. Output: the checked request, or why it was refused.
pub fn read_request(reader: &mut impl Read) -> Result<Request, Refusal> {
    let mut line = Vec::with_capacity(REQUEST_BYTES_MAX);
    let mut limited = reader.by_ref().take(REQUEST_BYTES_MAX as u64);
    let read = io::BufReader::new(&mut limited).read_until(b'\n', &mut line);
    assert!(line.len() <= REQUEST_BYTES_MAX);
    if read.is_err() {
        return Err(Refusal::Malformed);
    }
    if line.last() != Some(&b'\n') {
        if line.len() == REQUEST_BYTES_MAX {
            return Err(Refusal::TooLong);
        }
        return Err(Refusal::Malformed);
    }
    let wire: Wire = serde_json::from_slice(&line).map_err(|_| Refusal::Malformed)?;
    checked(wire)
}

/// Checks a parsed line's version, its lease bounds and that only a hold names its lease and
/// its display ask, which it must. Input: the line. Output: the request.
fn checked(wire: Wire) -> Result<Request, Refusal> {
    if wire.version != VERSION {
        return Err(Refusal::Version);
    }
    let in_range = |lease_s: u16| (LEASE_S_MIN..=LEASE_S_MAX).contains(&lease_s);
    let red_stub = wire.low_power.unwrap_or(false);
    let request = match (wire.call, wire.lease_s, wire.lid_display) {
        (Call::Hold, Some(lease_s), Some(lid_display)) if in_range(lease_s) => Request::Hold {
            lease_s,
            lid_display,
            low_power: red_stub,
        },
        (Call::Hold, Some(lease_s), None) if in_range(lease_s) => return Err(Refusal::Malformed),
        (Call::Hold, _, _) => return Err(Refusal::LeaseOutOfRange),
        (Call::LowPower, _, _) => return Err(Refusal::Malformed),
        (_, Some(_), _) | (_, _, Some(_)) => return Err(Refusal::Malformed),
        (Call::Release, None, None) => Request::Release,
        (Call::Read, None, None) => Request::Read,
        (Call::Forget, None, None) => Request::Forget,
    };
    if let Request::Hold { lease_s, .. } = request {
        assert!(lease_s >= LEASE_S_MIN);
        assert!(lease_s <= LEASE_S_MAX);
    }
    Ok(request)
}

/// Writes one reply line. Input: the connection and the reply. Output: the write result.
pub fn write_reply(writer: &mut impl Write, reply: &Reply) -> io::Result<()> {
    let mut line = serde_json::to_vec(reply).map_err(io::Error::from)?;
    line.push(b'\n');
    assert!(line.len() <= REPLY_BYTES_MAX);
    assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one reply line as a client: at most `REPLY_BYTES_MAX` bytes ending in their only
/// newline. Input: the line. Output: the reply, or none for anything else.
pub fn decode_reply(line: &[u8]) -> Option<Reply> {
    if line.len() > REPLY_BYTES_MAX || line.last() != Some(&b'\n') {
        return None;
    }
    if line.iter().filter(|byte| **byte == b'\n').count() != 1 {
        return None;
    }
    let reply: Reply = serde_json::from_slice(line).ok()?;
    Some(reply)
}

/// The bytes a client sends for a request, newline included. Input: the request. Output: the
/// line.
pub fn encode_request(request: &Request) -> Vec<u8> {
    let (call, lease_s, lid_display, low_power) = match *request {
        Request::Hold {
            lease_s,
            lid_display,
            low_power,
        } => (Call::Hold, Some(lease_s), Some(lid_display), Some(low_power)),
        Request::LowPower { lease_s } => (Call::LowPower, Some(lease_s), None, None),
        Request::Release => (Call::Release, None, None, None),
        Request::Read => (Call::Read, None, None, None),
        Request::Forget => (Call::Forget, None, None, None),
    };
    assert_eq!(lid_display.is_some(), low_power.is_some());
    assert!(lease_s.is_some() || lid_display.is_none());
    let wire = Wire {
        version: VERSION,
        call,
        lease_s,
        lid_display,
        low_power,
    };
    let mut line = serde_json::to_vec(&wire).expect("a request always serializes");
    line.push(b'\n');
    assert!(line.len() <= REQUEST_BYTES_MAX);
    assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
    line
}
