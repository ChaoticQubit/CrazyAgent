use super::launch::{LaunchError, failure};
use super::manifest::{self, DriverKind, PLACEHOLDERS, Refused};
use super::registry::{Harness, registry};

pub mod acp;
pub mod jsonl_rpc;
pub mod jsonl_turn;
pub mod rules;
pub mod text_turn;

/// The piped drivers the engine runs: its own adapters for Codex and Claude Code and the
/// Agent Client Protocol client ([`acp::Acp`]) for any harness whose manifest names `acp`,
/// which run in a [`super::launch::Session`], the shared `jsonl-turn` and `text-turn`
/// drivers, which run one process per turn in a [`super::launch::TurnSession`], and the
/// shared `jsonl-rpc` driver ([`jsonl_rpc::JsonlRpc`]) for one long-lived process with JSON
/// lines both ways, whose launch arguments land with the launch task. Each other shared
/// driver kind a manifest names joins this list when its task lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Piped {
    Codex,
    Claude,
    Acp,
    JsonlTurn,
    JsonlRpc,
    TextTurn,
}

/// The one dispatch point of piped launches: the driver the engine runs for a driver kind.
/// Input: the kind a manifest names. Output: the driver, or `launch_harness_unsupported`
/// for RCLI, which runs only through its master host, and for every kind whose driver has
/// not landed. An ACP or jsonl-rpc harness gets its driver here, while its launch arguments
/// land with the launch task, so a launch of one is still refused when it is built.
pub fn of(kind: DriverKind) -> Result<Piped, LaunchError> {
    match kind {
        DriverKind::CodexAppServer => Ok(Piped::Codex),
        DriverKind::ClaudeStreamJson => Ok(Piped::Claude),
        DriverKind::Rcli => Err(failure("launch_harness_unsupported")),
        DriverKind::Acp => Ok(Piped::Acp),
        DriverKind::JsonlTurn => Ok(Piped::JsonlTurn),
        DriverKind::JsonlRpc => Ok(Piped::JsonlRpc),
        DriverKind::TextTurn => Ok(Piped::TextTurn),
    }
}

/// Picks the driver a piped launch of one harness runs under, from the driver kind its
/// manifest names. Input: a catalog id or an alias, such as `claude`. Output: the driver,
/// or `launch_harness_unsupported` for a name outside the catalog, a harness with no piped
/// driver, or a kind [`of`] refuses.
pub fn piped(harness: &str) -> Result<Piped, LaunchError> {
    registry()
        .get(harness)
        .and_then(Harness::piped)
        .ok_or_else(|| failure("launch_harness_unsupported"))
        .and_then(of)
}

/// Fills the placeholders of one manifest argument for a turn, each once and never again
/// inside a filled value; a `{` that opens no known placeholder stays as written. Inputs:
/// the argument and the value of each placeholder it names. Output: the filled argument,
/// or the first refusal `value` gives.
fn fill<'a>(
    argument: &str,
    value: impl Fn(&str) -> Result<&'a str, LaunchError>,
) -> Result<String, LaunchError> {
    let mut filled = String::with_capacity(argument.len());
    let mut rest = argument;
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name = after
            .find('}')
            .map(|close| &after[..close])
            .filter(|name| PLACEHOLDERS.contains(name));
        match name {
            Some(name) => {
                filled.push_str(value(name)?);
                rest = &after[name.len() + 1..];
            }
            None => {
                filled.push('{');
                rest = after;
            }
        }
    }
    filled.push_str(rest);
    Ok(filled)
}

/// Where a per-turn driver puts `piped.resume` and `piped.tools_off` among the arguments of
/// `piped.argv`: before its first `--` entry, or at its end with none, checked so a prompt is
/// never read as an option. Inputs: the `piped.argv`, `piped.resume` and `piped.tools_off`
/// of one manifest. Output: that index, or a refusal for a `{prompt}` that begins an argument
/// before it, or for `{prompt}` in `piped.resume` or `piped.tools_off`.
fn positional(argv: &[String], resume: &[String], tools_off: &[String]) -> Result<usize, Refused> {
    let positional = argv
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(argv.len());
    if argv[..positional]
        .iter()
        .any(|argument| argument.starts_with("{prompt}"))
    {
        return Err(Refused {
            code: manifest::VALUE,
            message: "piped.argv: {prompt} begins an argument; write it after text such as \
                      --prompt= or after a -- entry, so a prompt is never read as an option."
                .into(),
        });
    }
    for (key, added) in [("resume", resume), ("tools_off", tools_off)] {
        if added.iter().any(|argument| argument.contains("{prompt}")) {
            return Err(Refused {
                code: manifest::VALUE,
                message: format!(
                    "piped.{key}: {{prompt}} belongs in piped.argv only, so a prompt is never \
                     read as an option."
                ),
            });
        }
    }
    assert!(positional <= argv.len());
    Ok(positional)
}

/// The arguments of one turn in order, before their placeholders are filled: `piped.argv` up
/// to `positional`, then `resume`, then `off`, then the rest of `piped.argv` from its `--`
/// entry on, so a prompt after that entry can neither end nor add an option. Inputs:
/// `piped.argv`, the index [`positional`] gave for it and this turn's added arguments.
/// Output: the arguments.
fn ordered<'a>(
    argv: &'a [String],
    positional: usize,
    resume: &'a [String],
    off: &'a [String],
) -> impl Iterator<Item = &'a String> {
    let (options, rest) = argv.split_at(positional);
    options.iter().chain(resume).chain(off).chain(rest)
}

#[cfg(test)]
#[path = "drivers_tests.rs"]
mod tests;
