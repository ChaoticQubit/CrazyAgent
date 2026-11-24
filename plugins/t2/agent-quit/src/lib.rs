#![no_std]
#![forbid(unsafe_code)]

use sdk::settings::{self, QuitSettings};

/// Saves the owner's agents-on-quit choice through the SDK. Inputs: the host the kernel
/// bound to this plugin and the settings. Output: Ok, or the bus refusal.
pub fn save(host: &dyn sdk::Host, settings: QuitSettings) -> Result<(), sdk::Refusal> {
    settings::save_quit(host, settings)
}
