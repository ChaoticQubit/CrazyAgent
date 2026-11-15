use super::super::launch::{LaunchError, LaunchEvent, PROMPT_BYTES_MAX, failure};
use super::super::manifest::{self, DriverKind, Fact, Manifest, Refused, Rule};
use super::rules;
use crate::adapters::LINE_BYTES_MAX;
use crate::app_plugins::yaml::Value as Yaml;
use serde_json::Value;
use std::path::Path;

/// The most bytes of one argument a manifest writes for a turn, before it is filled.
pub const ARG_BYTES_MAX: usize = 1024;
/// The most bytes of a harness session id this driver keeps and passes back as an argument.
pub const SESSION_BYTES_MAX: usize = 256;
/// The most bytes of a model name this driver passes as an argument.
pub const MODEL_BYTES_MAX: usize = 256;

/// The piped settings of a harness on the `jsonl-turn` driver, read from its manifest: the
/// arguments of every turn, the ones added to continue a session, the ones added with tools
/// off, and the rules that turn its output lines into facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    argv: Vec<String>,
    resume: Vec<String>,
    tools_off: Option<Vec<String>>,
    rules: Vec<Rule>,
}

impl Config {
    /// Reads the settings of one manifest. Input: a manifest whose `piped.driver` is
    /// `jsonl-turn`. Output: the settings, or a refusal that names the setting: `piped.argv`
    /// missing or without `{prompt}`, `piped.resume` without `{session}`, an argument that is
    /// not 1 to `ARG_BYTES_MAX` bytes of text, `piped.rules` missing, or a refused rule.
    pub fn read(manifest: &Manifest) -> Result<Self, Refused> {
        let Some(piped) = manifest
            .block("piped")
            .filter(|_| manifest.piped == Some(DriverKind::JsonlTurn))
        else {
            return refuse(
                manifest::KIND,
                "piped.driver: this harness does not run on the jsonl-turn driver.".into(),
            );
        };
        let argv = arguments(piped, "argv")?.unwrap_or_default();
        if argv.is_empty() {
            return refuse(
                manifest::MISSING,
                "piped.argv: this setting is needed.".into(),
            );
        }
        if !argv.iter().any(|argument| argument.contains("{prompt}")) {
            return refuse(
                manifest::VALUE,
                "piped.argv: {prompt} is needed, so each turn's prompt reaches the harness.".into(),
            );
        }
        let resume = arguments(piped, "resume")?.unwrap_or_default();
        if !resume.is_empty() && !resume.iter().any(|argument| argument.contains("{session}")) {
            return refuse(
                manifest::VALUE,
                "piped.resume: {session} is needed, so a turn continues its session.".into(),
            );
        }
        let tools_off = arguments(piped, "tools_off")?;
        let set = match piped.get("rules") {
            None | Some(Yaml::Null) => {
                return refuse(
                    manifest::MISSING,
                    "piped.rules: this setting is needed.".into(),
                );
            }
            Some(Yaml::Str(set)) => set,
            Some(_) => {
                return refuse(
                    manifest::TYPE,
                    "piped.rules: this must be text in quotes.".into(),
                );
            }
        };
        let rules = rules::read(manifest, set)?;
        Ok(Self {
            argv,
            resume,
            tools_off,
            rules,
        })
    }
}

/// Where a jsonl-turn conversation stands: waiting for a prompt, a turn's process running,
/// or failed for good after a refused line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Running,
    Failed,
}

/// What the running turn has reported so far that decides how it ends.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Turn {
    success: Option<bool>,
    errored: bool,
    interrupted: bool,
    breached: bool,
}

/// One conversation with a harness that runs one process per turn and writes JSON lines,
/// without any IO: it gives each turn's arguments, turns the process's lines into launch
/// events by the manifest's rules, and ends the turn when the process exits. The harness
/// session id a turn reports is passed back to the next turn through `piped.resume`.
#[derive(Debug)]
pub struct JsonlTurn {
    config: Config,
    home: String,
    cwd: String,
    model: Option<String>,
    tools: bool,
    session: Option<String>,
    phase: Phase,
    turn: Turn,
}

impl JsonlTurn {
    /// Starts a conversation. Inputs: the settings, the harness's private home and working
    /// folder, both absolute UTF-8, the model, the tools switch and the session to continue.
    /// Output: the conversation and its `Ready` event, or `launch_account_home_unsafe`,
    /// `launch_working_dir_unavailable`, `launch_model_unsupported` for a model that is not
    /// 1 to `MODEL_BYTES_MAX` bytes without a control character, `launch_session_id_invalid`,
    /// or `launch_tools_off_unsupported` for tools off on a manifest with no `tools_off`.
    pub fn new(
        config: Config,
        home: &Path,
        cwd: &Path,
        model: Option<&str>,
        tools: bool,
        resume: Option<&str>,
    ) -> Result<(Self, Vec<LaunchEvent>), LaunchError> {
        let absolute = |path: &Path| {
            path.to_str()
                .filter(|_| path.is_absolute())
                .map(str::to_owned)
        };
        let home = absolute(home).ok_or_else(|| failure("launch_account_home_unsafe"))?;
        let cwd = absolute(cwd).ok_or_else(|| failure("launch_working_dir_unavailable"))?;
        if model.is_some_and(|model| {
            model.is_empty() || model.len() > MODEL_BYTES_MAX || model.chars().any(char::is_control)
        }) {
            return Err(failure("launch_model_unsupported"));
        }
        if resume.is_some_and(|id| !plain_session(id)) {
            return Err(failure("launch_session_id_invalid"));
        }
        if !tools && config.tools_off.is_none() {
            return Err(failure("launch_tools_off_unsupported"));
        }
        let conversation = Self {
            config,
            home,
            cwd,
            model: model.map(str::to_owned),
            tools,
            session: resume.map(str::to_owned),
            phase: Phase::Idle,
            turn: Turn::default(),
        };
        conversation.assert_consistent();
        let ready = LaunchEvent::Ready {
            native_session: conversation.session.clone(),
        };
        Ok((conversation, vec![ready]))
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The harness session id the next turn continues. Output: the id the last turn
    /// reported, else the one the conversation started from, or none.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Whether the running turn used a tool with tools off, so its caller must stop the
    /// turn's process. Output: true from the breach until the turn ends.
    pub fn breached(&self) -> bool {
        self.turn.breached
    }

    /// Starts a turn. Input: the prompt. Output: the turn's arguments, each placeholder
    /// filled once and never again inside a filled value: `piped.argv`, then `piped.resume`
    /// when a session id is known, then `piped.tools_off` with tools off. Refusals:
    /// `launch_prompt_invalid` for an empty prompt, one with a NUL byte or one over
    /// `PROMPT_BYTES_MAX`, `launch_busy` during a turn, `launch_protocol_failed` once failed,
    /// `launch_model_unsupported` for `{model}` with no model, `launch_session_id_invalid`
    /// for `{session}` with no id outside `piped.resume`, and `launch_harness_unsupported`
    /// for `{relay_url}`, `{token}` or `{key_file}`, which this driver does not hand out yet.
    pub fn start(&mut self, prompt: &str) -> Result<Vec<String>, LaunchError> {
        self.assert_consistent();
        if self.phase == Phase::Failed {
            return Err(failure("launch_protocol_failed"));
        }
        if prompt.trim().is_empty() || prompt.contains('\0') || prompt.len() > PROMPT_BYTES_MAX {
            return Err(failure("launch_prompt_invalid"));
        }
        if self.phase == Phase::Running {
            return Err(failure("launch_busy"));
        }
        let resume = if self.session.is_some() {
            self.config.resume.as_slice()
        } else {
            &[]
        };
        let off = if self.tools {
            &[]
        } else {
            self.config.tools_off.as_deref().unwrap_or_default()
        };
        let argv = self
            .config
            .argv
            .iter()
            .chain(resume)
            .chain(off)
            .map(|argument| self.fill(argument, prompt))
            .collect::<Result<Vec<String>, LaunchError>>()?;
        assert!(argv.len() >= self.config.argv.len());
        self.phase = Phase::Running;
        self.turn = Turn::default();
        self.assert_consistent();
        Ok(argv)
    }

    /// Reads one line the turn's process wrote. A line over `LINE_BYTES_MAX`, a line that
    /// is not one JSON object, a line no rule matches, a line outside a turn and every line
    /// after a breach give nothing. With tools off, a tool the harness reports using is a
    /// breach: one `ApprovalDenied` instead of the tool use, and [`JsonlTurn::breached`]
    /// tells the caller to stop the turn's process.
    /// Input: the line, with or without its newline. Output: the events it gives, or
    /// `launch_session_id_invalid` for a session id that is not 1 to `SESSION_BYTES_MAX`
    /// bytes without a control character or a leading `-`, which fails the conversation for
    /// good, and `launch_protocol_failed` once failed.
    pub fn receive(&mut self, line: &[u8]) -> Result<Vec<LaunchEvent>, LaunchError> {
        self.assert_consistent();
        if self.phase == Phase::Failed {
            return Err(failure("launch_protocol_failed"));
        }
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if self.phase != Phase::Running || self.turn.breached || line.len() > LINE_BYTES_MAX {
            return Ok(Vec::new());
        }
        let Ok(object @ Value::Object(_)) = serde_json::from_slice::<Value>(line) else {
            return Ok(Vec::new());
        };
        let Some(found) = rules::matched(&self.config.rules, &object) else {
            return Ok(Vec::new());
        };
        if let Some(id) = found.session {
            if !plain_session(&id) {
                self.phase = Phase::Failed;
                return Err(failure("launch_session_id_invalid"));
            }
            self.session = Some(id);
        }
        let mut events = Vec::new();
        match found.fact {
            Fact::Text => events.extend(found.text.map(|text| LaunchEvent::Text { text })),
            Fact::ToolUse if !self.tools => {
                self.turn.breached = true;
                events.push(LaunchEvent::ApprovalDenied);
            }
            Fact::ToolUse => events.extend(found.name.map(|name| LaunchEvent::ToolUse { name })),
            Fact::TurnFinished => {
                self.turn.success = Some(found.success.unwrap_or(true));
                events.extend(found.tokens.map(|tokens| LaunchEvent::Usage { tokens }));
            }
            Fact::TurnError if !self.turn.errored => {
                self.turn.errored = true;
                events.push(LaunchEvent::TurnError {
                    rate_limited: found.rate_limited.unwrap_or(false),
                });
            }
            Fact::Usage => events.extend(found.tokens.map(|tokens| LaunchEvent::Usage { tokens })),
            Fact::Interrupted => self.turn.interrupted = true,
            Fact::Ready | Fact::TurnStarted | Fact::Waiting | Fact::TurnError | Fact::Exited => {}
        }
        self.assert_consistent();
        Ok(events)
    }

    /// Marks the running turn as interrupted; the caller then stops the turn's process.
    /// Output: Ok, or `launch_not_running` with no turn, an interrupt already asked or a
    /// breach, whose turn the caller already stops.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        self.assert_consistent();
        if self.phase != Phase::Running || self.turn.interrupted || self.turn.breached {
            return Err(failure("launch_not_running"));
        }
        self.turn.interrupted = true;
        Ok(())
    }

    /// Ends the running turn when its process exits, with exactly one `TurnFinished`: after
    /// an interrupt, `Interrupted` and an unsuccessful end; after a breach or a turn error,
    /// an unsuccessful end; for an exit code other than 0 or a signal, a `TurnError` and an
    /// unsuccessful end; else the success a `turn_finished` rule read, true when none did.
    /// Input: the exit code, none when a signal ended the process. Output: the events, or
    /// `launch_not_running` with no turn.
    pub fn exited(&mut self, code: Option<i32>) -> Result<Vec<LaunchEvent>, LaunchError> {
        self.assert_consistent();
        if self.phase != Phase::Running {
            return Err(failure("launch_not_running"));
        }
        let unsuccessful = LaunchEvent::TurnFinished { success: false };
        let events = if self.turn.interrupted {
            vec![LaunchEvent::Interrupted, unsuccessful]
        } else if self.turn.breached || self.turn.errored {
            vec![unsuccessful]
        } else if code != Some(0) {
            vec![
                LaunchEvent::TurnError {
                    rate_limited: false,
                },
                unsuccessful,
            ]
        } else {
            vec![LaunchEvent::TurnFinished {
                success: self.turn.success.unwrap_or(true),
            }]
        };
        self.phase = Phase::Idle;
        self.turn = Turn::default();
        self.assert_consistent();
        Ok(events)
    }

    fn fill(&self, argument: &str, prompt: &str) -> Result<String, LaunchError> {
        super::fill(argument, |name| self.value(name, prompt))
    }

    fn value<'a>(&'a self, name: &str, prompt: &'a str) -> Result<&'a str, LaunchError> {
        match name {
            "home" => Ok(&self.home),
            "cwd" => Ok(&self.cwd),
            "prompt" => Ok(prompt),
            "model" => self
                .model
                .as_deref()
                .ok_or_else(|| failure("launch_model_unsupported")),
            "session" => self
                .session
                .as_deref()
                .ok_or_else(|| failure("launch_session_id_invalid")),
            _ => Err(failure("launch_harness_unsupported")),
        }
    }

    fn assert_consistent(&self) {
        assert!(Path::new(&self.home).is_absolute() && Path::new(&self.cwd).is_absolute());
        assert!(self.session.as_deref().is_none_or(plain_session));
        assert!(!self.config.argv.is_empty() && !self.config.rules.is_empty());
        assert!(self.tools || self.config.tools_off.is_some());
        assert!(!self.tools || !self.turn.breached);
        assert!(self.phase == Phase::Running || self.turn == Turn::default());
    }
}

pub(super) fn arguments(piped: &Yaml, key: &str) -> Result<Option<Vec<String>>, Refused> {
    let place = format!("piped.{key}");
    let items = match piped.get(key) {
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
    items
        .iter()
        .map(|item| match item {
            Yaml::Str(text) if !text.is_empty() && text.len() <= ARG_BYTES_MAX => Ok(text.clone()),
            Yaml::Str(_) => refuse(
                manifest::VALUE,
                format!("{place}: each entry is 1 to {ARG_BYTES_MAX} bytes of text."),
            ),
            _ => refuse(
                manifest::TYPE,
                format!("{place}: this must be a list of texts."),
            ),
        })
        .collect::<Result<Vec<String>, Refused>>()
        .map(Some)
}

pub(super) fn plain_session(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= SESSION_BYTES_MAX
        && !id.starts_with('-')
        && !id.chars().any(char::is_control)
}

fn refuse<T>(code: &'static str, message: String) -> Result<T, Refused> {
    Err(Refused { code, message })
}

#[cfg(test)]
#[path = "jsonl_turn_tests.rs"]
mod tests;
