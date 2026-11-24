#![forbid(unsafe_code)]

use std::process::ExitCode;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    kernel_sandbox::linux::helper::main(std::env::args_os().skip(1).collect())
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("crazyagent-landlock runs only on Linux");
    ExitCode::from(2)
}
