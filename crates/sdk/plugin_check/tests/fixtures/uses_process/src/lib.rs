#![no_std]
#![forbid(unsafe_code)]

use std::{collections::BTreeMap, process::Command};

/// Runs a program.
pub fn run() -> bool {
    let _ = BTreeMap::<u8, u8>::new();
    Command::new("true").status().is_ok()
}
