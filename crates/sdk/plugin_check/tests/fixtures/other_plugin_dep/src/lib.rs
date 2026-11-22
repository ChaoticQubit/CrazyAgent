#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use sdk::settings::{self, QuitSettings};

/// Saves the settings through the SDK and names them. Inputs: the host and the settings.
/// Output: the refusal, or a line naming the hours.
pub fn save(host: &dyn sdk::Host, settings: QuitSettings) -> Result<String, sdk::Refusal> {
    settings::save_quit(host, settings)?;
    let mut seen: BTreeMap<u16, u32> = BTreeMap::new();
    *seen.entry(settings.hours).or_default() += 1;
    let mut line = format!("hours {}", settings.hours);
    let _ = write!(line, " seen {}", seen.len());
    Ok(line)
}
