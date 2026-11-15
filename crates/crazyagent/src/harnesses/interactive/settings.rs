use super::super::launch::{LaunchError, failure};
use super::super::manifest::{self, Manifest, RecordKind, Refused, Rule};

/// The most first-screen steps one manifest gives.
pub const STEPS_MAX: usize = 4;
/// The most bytes of the screen text one first-screen step waits for.
pub const SEEN_BYTES_MAX: usize = 64;

/// One key the engine presses in a harness's terminal, from a fixed list, so a manifest
/// never hands the pane bytes of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    CtrlC,
    Tab,
}

impl Key {
    /// Reads a key by its manifest name. Input: the name, such as `ctrl-c`. Output: the key,
    /// or none for a name outside the list.
    pub fn of(_name: &str) -> Option<Self> {
        None
    }

    /// The bytes the pane gets for this key. Output: the bytes.
    pub fn bytes(self) -> &'static [u8] {
        b""
    }
}

/// One step of a harness's first screen: the text the engine waits to see on the screen
/// and the key it then presses, once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub seen: String,
    pub press: Key,
}

/// The terminal settings of a harness whose live terminal the engine runs from its
/// manifest: the arguments of a fresh and of a resumed run, the submit and interrupt keys,
/// the first-screen steps, and the record kind and rules its turns are read back by.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalSettings {
    manifest: Manifest,
    argv: Vec<String>,
    resume: Vec<String>,
    submit: Key,
    interrupt: Key,
    first_screen: Vec<Step>,
    record: RecordKind,
    rules: Vec<Rule>,
}

impl TerminalSettings {
    /// Reads the terminal settings of one manifest. Input: the manifest. Output: the
    /// settings, or a refusal that names the setting.
    pub fn read(_manifest: &Manifest) -> Result<Self, Refused> {
        Err(Refused {
            code: manifest::KIND,
            message: String::new(),
        })
    }

    /// The catalog id of the harness these settings are for. Output: the id.
    pub fn harness(&self) -> &str {
        &self.manifest.id
    }

    /// The key that sends a typed prompt. Output: the key.
    pub fn submit(&self) -> Key {
        self.submit
    }

    /// The key that stops the running turn. Output: the key.
    pub fn interrupt(&self) -> Key {
        self.interrupt
    }

    /// The first-screen steps, in the order the engine answers them. Output: the steps.
    pub fn first_screen(&self) -> &[Step] {
        &self.first_screen
    }

    /// The kind of record the harness writes. Output: the kind.
    pub fn record(&self) -> RecordKind {
        self.record
    }

    /// The rules that turn the record's objects into facts. Output: the rules.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Gives the arguments of one run. Inputs: whether the run resumes, the private home,
    /// the session id, the working folder and the model. Output: the arguments, or a
    /// plain refusal code.
    pub fn arguments(
        &self,
        _resume: bool,
        _home: &str,
        _session: &str,
        _cwd: &str,
        _model: Option<&str>,
    ) -> Result<Vec<String>, LaunchError> {
        Err(failure("launch_harness_unsupported"))
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
