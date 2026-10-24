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

fn launch(_request: &Request) -> ExitCode {
    refuse("this helper cannot start agents yet")
}
