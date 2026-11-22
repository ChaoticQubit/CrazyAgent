#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use sdk::settings::{LeftRunning, OnQuit, QuitSettings};

/// Builds the values the new `.cap` types map to: a text, a byte payload and a list of a
/// generated type. Input: the hours. Output: the three values.
pub fn build(hours: u16) -> (String, Vec<u8>, Vec<QuitSettings>) {
    let label = String::from("quit");
    let bytes: Vec<u8> = label.as_bytes().to_vec();
    let mut list: Vec<QuitSettings> = Vec::new();
    list.push(QuitSettings {
        on_quit: OnQuit::Stop,
        left_running: LeftRunning::Never,
        hours,
    });
    (label, bytes, list)
}
