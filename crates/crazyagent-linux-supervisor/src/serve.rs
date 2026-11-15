#![forbid(unsafe_code)]

use crate::filter::Guards;
use crate::rules::{GitOwn, Reach};
use std::io;
use std::process::Command;

/// What one agent's supervisor holds: the ports it may reach and where, when its connects
/// are guarded; its own git folders, when new entries are guarded; and whether its
/// signals stay inside the programs it started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supervised {
    pub reach: Option<Reach>,
    pub git: Option<GitOwn>,
    pub signals: bool,
}

impl Supervised {
    /// The guards the harness's filter carries for this agent. Output: one per rule set.
    pub fn guards(&self) -> Guards {
        Guards {
            connect: self.reach.is_some(),
            create: self.git.is_some(),
            signal: self.signals,
        }
    }
}

/// Starts the harness with its guarded calls trapped and answers them until it ends.
/// Inputs: what to hold and the harness command. Output: the harness's exit code, 128
/// plus the signal that ended it, or the reason no harness was supervised.
pub fn run(config: Supervised, command: &mut Command) -> io::Result<i32> {
    assert!(config.guards() != Guards::default());
    let harness = command.get_program().to_string_lossy();
    Err(io::Error::other(format!("no call supervisor starts {harness} yet")))
}
