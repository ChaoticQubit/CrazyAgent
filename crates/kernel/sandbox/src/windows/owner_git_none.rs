use crate::error::SandboxError;
use std::path::Path;
use std::process::Command;

/// Owner git's own container, which only Windows has (V244-WIN-GIT-UNC-SIBLINGS): elsewhere
/// this type has no value, so a publish or `@` list there never routes its git anywhere and
/// runs it as the owner, as before.
#[derive(Debug)]
pub enum OwnerGit {}

impl OwnerGit {
    /// Never runs: no value of this type exists outside Windows. Input: the command.
    /// Output: none.
    pub fn wrap(&self, _git: &Command) -> Result<Command, SandboxError> {
        match *self {}
    }

    /// Never runs: no value of this type exists outside Windows. Output: none.
    pub fn tmp(&self) -> &Path {
        match *self {}
    }

    /// Never runs: no value of this type exists outside Windows. Output: none.
    pub fn git(&self) -> &Path {
        match *self {}
    }
}
