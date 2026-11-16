//! The lid helper's LaunchDaemon entry point: takes the socket launchd made for it and serves
//! our signed engine until it is idle or told to stop (threat model sections 1 and 5).

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(crazyagent_mac_power::server::main_production())
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("crazyagent-power-helper runs on macOS only");
    std::process::ExitCode::from(2)
}
