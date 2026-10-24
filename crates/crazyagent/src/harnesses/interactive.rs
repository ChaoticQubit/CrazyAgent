use std::path::PathBuf;

/// What the terminal host runs for one interactive agent: the full argument list, starting
/// with the project sandbox wrapper, the working folder and the environment, plus where the
/// harness keeps its own session record. No secret is ever in `argv` or `env`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractiveCommand {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub transcript: Transcript,
}

/// Where a harness writes its own session record, which the engine reads for status,
/// replies and DELEGATE lines instead of reading the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transcript {
    CodexRollouts { sessions: PathBuf },
    ClaudeSession { file: PathBuf },
    Unavailable,
}

/// Whether a harness has an interactive app the engine can run in a terminal pane.
/// `HeadlessOnly` keeps the existing piped launch and carries the plain reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Terminal,
    HeadlessOnly { reason: &'static str },
}
