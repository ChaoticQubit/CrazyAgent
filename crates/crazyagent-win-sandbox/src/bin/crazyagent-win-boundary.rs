#![forbid(unsafe_code)]

use crazyagent_win_sandbox::request::{self, REFUSED_EXIT, Request};
use std::fmt::Display;
use std::process::ExitCode;

fn main() -> ExitCode {
    match request::parse(std::env::args_os().skip(1).collect()) {
        Ok(request) => launch(&request),
        Err(reason) => refuse(reason),
    }
}

fn refuse(reason: impl Display) -> ExitCode {
    eprintln!("crazyagent-win-boundary: {reason}");
    ExitCode::from(REFUSED_EXIT)
}

#[cfg(windows)]
fn launch(request: &Request) -> ExitCode {
    match crazyagent_win_sandbox::run(request) {
        Ok(code) => std::process::exit(code as i32),
        Err(error) => refuse(error),
    }
}

#[cfg(not(windows))]
fn launch(_request: &Request) -> ExitCode {
    refuse("this helper runs only on Windows")
}
