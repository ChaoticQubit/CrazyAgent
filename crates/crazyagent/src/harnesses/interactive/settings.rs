use super::super::drivers::jsonl_turn::ARG_BYTES_MAX;
use super::super::drivers::rules;
use super::super::launch::{LaunchError, failure};
use super::super::manifest::{self, Manifest, RecordKind, Refused, Rule};
use super::super::records::{self, Reader, Records};
use super::super::records::json_files::Pattern;
use crate::app_plugins::yaml::Value as Yaml;
use std::path::Path;

/// The most first-screen steps one manifest gives.
pub const STEPS_MAX: usize = 4;
/// The most bytes of the screen text one first-screen step waits for.
pub const SEEN_BYTES_MAX: usize = 64;

const KEYS: [&str; 6] = [
    "argv",
    "resume",
    "submit",
    "interrupt",
    "first_screen",
    "record",
];
const STEP_KEYS: [&str; 3] = ["seen", "press", "optional"];
const TYPED_ONLY: [&str; 4] = ["prompt", "relay_url", "token", "key_file"];

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
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "enter" => Some(Self::Enter),
            "escape" => Some(Self::Escape),
            "ctrl-c" => Some(Self::CtrlC),
            "tab" => Some(Self::Tab),
            _ => None,
        }
    }

    /// The bytes the pane gets for this key. Output: the bytes.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Self::Enter => b"\r",
            Self::Escape => b"\x1b",
            Self::CtrlC => b"\x03",
            Self::Tab => b"\t",
        }
    }
}

/// One step of a harness's first screen: the text the engine waits to see on the screen,
/// the key it then presses, once, and whether the step is optional: pressed when its text
/// shows, never waited for, as a screen a harness shows only on its first run in a home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub seen: String,
    pub press: Key,
    pub optional: bool,
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
    /// Reads the `terminal` block of one manifest: only its own keys, `argv` needed and
    /// `resume` holding `{session}`, no placeholder a terminal run does not fill, keys from
    /// the fixed list (Enter and Escape when not given), at most `STEPS_MAX` first-screen
    /// steps, a jsonl-append or json-rewrite record with its path and `rules`. Input: the
    /// manifest. Output: the settings, or a refusal whose sentence starts with the setting.
    pub fn read(manifest: &Manifest) -> Result<Self, Refused> {
        let terminal = match manifest.block("terminal") {
            None | Some(Yaml::Null) => {
                let missing = "terminal: this setting is needed.";
                return refuse(manifest::MISSING, missing.into());
            }
            Some(terminal) => terminal,
        };
        let Yaml::Map(entries) = terminal else {
            let shape = "terminal: this must be a block of settings.";
            return refuse(manifest::TYPE, shape.into());
        };
        let unknown = entries
            .iter()
            .find(|(key, _)| !KEYS.contains(&key.as_str()));
        if let Some((key, _)) = unknown {
            return refuse(
                manifest::UNKNOWN_KEY,
                format!("terminal.{key}: this key is not one a terminal has."),
            );
        }
        if !matches!(
            manifest.record,
            RecordKind::JsonlAppend | RecordKind::JsonRewrite
        ) {
            return refuse(
                manifest::KIND,
                "terminal.record.reader: this must be jsonl-append or json-rewrite.".into(),
            );
        }
        let argv = arguments(terminal, "argv")?.unwrap_or_default();
        if argv.is_empty() {
            return refuse(
                manifest::MISSING,
                "terminal.argv: this setting is needed.".into(),
            );
        }
        let resume = arguments(terminal, "resume")?.unwrap_or_default();
        if !resume.is_empty() && !resume.iter().any(|argument| argument.contains("{session}")) {
            return refuse(
                manifest::VALUE,
                "terminal.resume: {session} is needed, so the run resumes its session.".into(),
            );
        }
        let submit = key(terminal, "submit", "terminal.submit")?;
        let interrupt = key(terminal, "interrupt", "terminal.interrupt")?;
        let first_screen = steps(terminal)?;
        Pattern::read(manifest, Path::new("/"), "s")?;
        let record = terminal.get("record");
        let set = match record.and_then(|record| record.get("rules")) {
            None | Some(Yaml::Null) => {
                return refuse(
                    manifest::MISSING,
                    "terminal.record.rules: this setting is needed.".into(),
                );
            }
            Some(Yaml::Str(set)) => set,
            Some(_) => {
                return refuse(
                    manifest::TYPE,
                    "terminal.record.rules: this must be text in quotes.".into(),
                );
            }
        };
        let rules = rules::read(manifest, set)?;
        assert!(!argv.is_empty() && !rules.is_empty());
        assert!(first_screen.len() <= STEPS_MAX);
        Ok(Self {
            manifest: manifest.clone(),
            argv,
            resume,
            submit: submit.unwrap_or(Key::Enter),
            interrupt: interrupt.unwrap_or(Key::Escape),
            first_screen,
            record: manifest.record,
            rules,
        })
    }

    /// The catalog id of the harness these settings are for. Output: the id.
    pub fn harness(&self) -> &str {
        &self.manifest.id
    }

    /// The harness's name as its manifest gives it, for the sentences an owner reads.
    /// Output: the name.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Tells whether a fresh run takes the engine's session id: whether `terminal.argv`
    /// names `{session}`. A harness started without it picks its own id, which the engine
    /// reads back from the record. Output: the answer.
    pub fn takes_session(&self) -> bool {
        self.argv
            .iter()
            .any(|argument| argument.contains("{session}"))
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

    /// The manifest these settings were read from, for the blocks other launch steps read,
    /// such as its `mcp` render, which decides whether a pane takes a project context.
    /// Output: the manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Fills the record path for one run. Inputs: the run's private home, absolute, and
    /// its session id. Output: the record as the run's transcript follows it, or a plain
    /// refusal code.
    pub fn records(&self, home: &Path, session: &str) -> Result<Records, LaunchError> {
        self.records_for(home, Some(session))
    }

    /// Fills the record path for a fresh run whose harness picks its own session id:
    /// `{session}` matches any name, and the record found names the id. Input: the run's
    /// private home, absolute. Output: the record as the run's transcript follows it, or a
    /// plain refusal code.
    pub fn learned_records(&self, home: &Path) -> Result<Records, LaunchError> {
        self.records_for(home, None)
    }

    fn records_for(&self, home: &Path, session: Option<&str>) -> Result<Records, LaunchError> {
        assert!(home.is_absolute());
        let reader = records::of(self.record)?;
        assert!(matches!(reader, Reader::JsonlAppend | Reader::JsonRewrite));
        let pattern = match session {
            Some(session) => Pattern::read(&self.manifest, home, session),
            None => Pattern::learning(&self.manifest, home),
        }
        .map_err(|_| failure("launch_session_id_invalid"))?;
        Ok(Records {
            harness: self.manifest.id.clone(),
            kind: self.record,
            pattern,
            rules: self.rules.clone(),
            session: session.map(str::to_owned),
        })
    }

    /// Gives the arguments of one run, `terminal.resume` for a resumed run, else
    /// `terminal.argv`, each placeholder filled once and never inside a filled value.
    /// Inputs: whether the run resumes, the private home, the session id, the working
    /// folder and the model. Output: the arguments, `launch_resume_unavailable` with no
    /// `terminal.resume`, or `launch_model_unsupported` for `{model}` with no model.
    pub fn arguments(
        &self,
        resume: bool,
        home: &str,
        session: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<Vec<String>, LaunchError> {
        let written = if resume { &self.resume } else { &self.argv };
        if written.is_empty() {
            return Err(failure("launch_resume_unavailable"));
        }
        let values = [
            ("home", Some(home)),
            ("session", Some(session)),
            ("cwd", Some(cwd)),
            ("model", model),
        ];
        let filled = written
            .iter()
            .map(|argument| fill(argument, &values))
            .collect::<Result<Vec<String>, LaunchError>>()?;
        assert_eq!(filled.len(), written.len());
        Ok(filled)
    }
}

fn fill(argument: &str, values: &[(&str, Option<&str>)]) -> Result<String, LaunchError> {
    let mut filled = String::with_capacity(argument.len());
    let mut rest = argument;
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let named = after.find('}').and_then(|close| {
            let name = &after[..close];
            values.iter().find(|(known, _)| *known == name)
        });
        match named {
            Some((name, Some(value))) => {
                filled.push_str(value);
                rest = &after[name.len() + 1..];
            }
            Some((_, None)) => return Err(failure("launch_model_unsupported")),
            None => {
                filled.push('{');
                rest = after;
            }
        }
    }
    filled.push_str(rest);
    Ok(filled)
}

fn arguments(terminal: &Yaml, key: &str) -> Result<Option<Vec<String>>, Refused> {
    let place = format!("terminal.{key}");
    let items = match terminal.get(key) {
        None | Some(Yaml::Null) => return Ok(None),
        Some(Yaml::List(items)) => items,
        Some(_) => {
            return refuse(
                manifest::TYPE,
                format!("{place}: this must be a list of texts."),
            );
        }
    };
    if items.is_empty() || items.len() > manifest::LIST_MAX {
        return refuse(
            manifest::VALUE,
            format!("{place}: a list holds 1 to {} entries.", manifest::LIST_MAX),
        );
    }
    let mut listed = Vec::with_capacity(items.len());
    for item in items {
        let Yaml::Str(text) = item else {
            return refuse(
                manifest::TYPE,
                format!("{place}: this must be a list of texts."),
            );
        };
        if text.is_empty() || text.len() > ARG_BYTES_MAX {
            return refuse(
                manifest::VALUE,
                format!("{place}: each entry is 1 to {ARG_BYTES_MAX} bytes of text."),
            );
        }
        let typed = TYPED_ONLY
            .iter()
            .find(|name| text.contains(&format!("{{{name}}}")));
        if let Some(name) = typed {
            return refuse(
                manifest::PLACEHOLDER,
                format!("{place}: {{{name}}} is not filled in a terminal run."),
            );
        }
        listed.push(text.clone());
    }
    assert!(!listed.is_empty() && listed.len() <= manifest::LIST_MAX);
    Ok(Some(listed))
}

fn key(map: &Yaml, key: &str, place: &str) -> Result<Option<Key>, Refused> {
    match map.get(key) {
        None | Some(Yaml::Null) => Ok(None),
        Some(Yaml::Str(name)) => match Key::of(name) {
            Some(key) => Ok(Some(key)),
            None => refuse(
                manifest::VALUE,
                format!("{place}: this must be enter, escape, ctrl-c or tab."),
            ),
        },
        Some(_) => refuse(
            manifest::TYPE,
            format!("{place}: this must be text in quotes."),
        ),
    }
}

fn steps(terminal: &Yaml) -> Result<Vec<Step>, Refused> {
    let entries = match terminal.get("first_screen") {
        None | Some(Yaml::Null) => return Ok(Vec::new()),
        Some(Yaml::Map(entries)) => entries,
        Some(_) => {
            return refuse(
                manifest::TYPE,
                "terminal.first_screen: this must be a block of named steps.".into(),
            );
        }
    };
    if entries.is_empty() || entries.len() > STEPS_MAX {
        return refuse(
            manifest::VALUE,
            format!("terminal.first_screen: this holds 1 to {STEPS_MAX} steps."),
        );
    }
    let mut steps = Vec::with_capacity(entries.len());
    for (name, step) in entries {
        let place = format!("terminal.first_screen.{name}");
        let Yaml::Map(keys) = step else {
            return refuse(
                manifest::TYPE,
                format!("{place}: this must be a block of settings."),
            );
        };
        let unknown = keys
            .iter()
            .find(|(key, _)| !STEP_KEYS.contains(&key.as_str()));
        if let Some((key, _)) = unknown {
            return refuse(
                manifest::UNKNOWN_KEY,
                format!("{place}.{key}: this key is not one a step has."),
            );
        }
        let seen = match step.get("seen") {
            None | Some(Yaml::Null) => {
                return refuse(
                    manifest::MISSING,
                    format!("{place}.seen: this setting is needed."),
                );
            }
            Some(Yaml::Str(seen)) if shown(seen) => seen.clone(),
            Some(Yaml::Str(_)) => {
                let most = SEEN_BYTES_MAX;
                return refuse(
                    manifest::VALUE,
                    format!("{place}.seen: this is 1 to {most} bytes on one line."),
                );
            }
            Some(_) => {
                return refuse(
                    manifest::TYPE,
                    format!("{place}.seen: this must be text in quotes."),
                );
            }
        };
        let Some(press) = key(step, "press", &format!("{place}.press"))? else {
            return refuse(
                manifest::MISSING,
                format!("{place}.press: this setting is needed."),
            );
        };
        let optional = match step.get("optional") {
            None | Some(Yaml::Null) => false,
            Some(Yaml::Bool(optional)) => *optional,
            Some(_) => {
                return refuse(
                    manifest::TYPE,
                    format!("{place}.optional: this must be true or false."),
                );
            }
        };
        steps.push(Step {
            seen,
            press,
            optional,
        });
    }
    assert!(!steps.is_empty() && steps.len() <= STEPS_MAX);
    Ok(steps)
}

fn shown(seen: &str) -> bool {
    !seen.trim().is_empty() && seen.len() <= SEEN_BYTES_MAX && !seen.chars().any(char::is_control)
}

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
