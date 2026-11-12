#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .take(trackwright_cli::ARGUMENTS_MAX + 1)
        .collect();
    trackwright_cli::run(&args)
}
