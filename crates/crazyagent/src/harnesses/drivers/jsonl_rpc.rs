use super::super::launch::{LaunchError, LaunchEvent, PROMPT_BYTES_MAX, failure};
use super::super::manifest::{self, DriverKind, Fact, Manifest, PLACEHOLDERS, Refused, Rule};
use super::jsonl_turn::{
    Phase, Turn, arguments, fill, inputs, plain_session, positional_start, refuse, rule_set,
};
use super::rules;
use crate::adapters::LINE_BYTES_MAX;
use crate::adapters::codex::FRAME_BYTES_MAX;
use crate::app_plugins::yaml::Value as Yaml;
use serde_json::Value;
use std::path::Path;

/// The most bytes of the prompt template or the interrupt frame a manifest writes.
pub const TEMPLATE_BYTES_MAX: usize = 1024;

/// The piped settings of a harness on the `jsonl-rpc` driver, read from its manifest: the
/// arguments of its one process and where their first `--` entry stands, the ones added to
/// continue a session, the ones added with tools off, the JSON line template each prompt is
/// framed in, the JSON line that interrupts a turn, and the rules that turn its output lines
/// into facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    argv: Vec<String>,
    positional: usize,
    resume: Vec<String>,
    tools_off: Option<Vec<String>>,
    prompt: String,
    interrupt: String,
    rules: Vec<Rule>,
}

impl Config {
    /// Reads the settings of one manifest. Input: a manifest whose `piped.driver` is
    /// `jsonl-rpc`. Output: the settings, or a refusal that names the setting: `piped.argv`
    /// missing, `{prompt}` in `piped.argv`, `piped.resume` or `piped.tools_off`,
    /// `piped.resume` without `{session}`, `piped.prompt` or `piped.interrupt` missing or not
    /// one line of 1 to `TEMPLATE_BYTES_MAX` bytes, a prompt template without exactly one
    /// `{prompt}` inside a JSON string of one JSON object or with another value, an interrupt
    /// that is not one JSON object or holds a value to fill, an argument that is not 1 to
    /// `ARG_BYTES_MAX` bytes of text, `piped.rules` missing, or a refused rule.
    pub fn read(manifest: &Manifest) -> Result<Self, Refused> {
        let Some(piped) = manifest
            .block("piped")
            .filter(|_| manifest.piped == Some(DriverKind::JsonlRpc))
        else {
            return refuse(
                manifest::KIND,
                "piped.driver: this harness does not run on the jsonl-rpc driver.".into(),
            );
        };
        let argv = arguments(piped, "argv")?.unwrap_or_default();
        if argv.is_empty() {
            return refuse(
                manifest::MISSING,
                "piped.argv: this setting is needed.".into(),
            );
        }
        let resume = arguments(piped, "resume")?.unwrap_or_default();
        let tools_off = arguments(piped, "tools_off")?;
        for (key, listed) in [
            ("argv", argv.as_slice()),
            ("resume", resume.as_slice()),
            ("tools_off", tools_off.as_deref().unwrap_or_default()),
        ] {
            if listed.iter().any(|argument| argument.contains("{prompt}")) {
                return refuse(
                    manifest::VALUE,
                    format!(
                        "piped.{key}: {{prompt}} is not an argument on this driver; each prompt \
                         goes to the harness's input through piped.prompt."
                    ),
                );
            }
        }
        if !resume.is_empty() && !resume.iter().any(|argument| argument.contains("{session}")) {
            return refuse(
                manifest::VALUE,
                "piped.resume: {session} is needed, so a launch continues its session.".into(),
            );
        }
        let prompt = template(piped, "prompt")?;
        let other = PLACEHOLDERS
            .iter()
            .any(|name| *name != "prompt" && holds(&prompt, name) > 0);
        if holds(&prompt, "prompt") != 1 || other {
            return refuse(
                manifest::VALUE,
                "piped.prompt: this holds {prompt} once and no other value.".into(),
            );
        }
        if !object(prompt.replacen("{prompt}", "\\\"", 1).as_bytes()) {
            return refuse(
                manifest::VALUE,
                "piped.prompt: this is one JSON object with {prompt} inside a JSON string.".into(),
            );
        }
        let interrupt = template(piped, "interrupt")?;
        if PLACEHOLDERS.iter().any(|name| holds(&interrupt, name) > 0)
            || !object(interrupt.as_bytes())
        {
            return refuse(
                manifest::VALUE,
                "piped.interrupt: this is one JSON object with no value to fill.".into(),
            );
        }
        let rules = rule_set(manifest, piped)?;
        let positional = positional_start(&argv);
        assert!(positional <= argv.len());
        Ok(Self {
            argv,
            positional,
            resume,
            tools_off,
            prompt,
            interrupt,
            rules,
        })
    }
}

/// One conversation with a harness that runs one long-lived process and speaks JSON lines
/// both ways, without any IO: it gives the process's arguments, frames each prompt and the
/// interrupt as one JSON line for its input, and turns its output lines into launch events
/// by the manifest's rules. A turn ends on the line a rule reads as its end; a process that
/// dies first leaves it unfinished, and the launch session reports the exit.
#[derive(Debug)]
pub struct JsonlRpc {
    config: Config,
    argv: Vec<String>,
    tools: bool,
    session: Option<String>,
    phase: Phase,
    turn: Turn,
}

impl JsonlRpc {
    /// Starts a conversation. Inputs: the settings, the harness's private home and working
    /// folder, both absolute UTF-8, the model, the tools switch and the session to continue.
    /// Output: the conversation and its `Ready` event, which carries the session to continue,
    /// or `launch_account_home_unsafe`, `launch_working_dir_unavailable`,
    /// `launch_model_unsupported` for a model that is not plain or for `{model}` with no
    /// model, `launch_session_id_invalid` for a session id that is not plain or for
    /// `{session}` with none, `launch_tools_off_unsupported` for tools off on a manifest with
    /// no `tools_off`, and `launch_harness_unsupported` for `{relay_url}`, `{token}` or
    /// `{key_file}`, which this driver does not hand out yet. The arguments are `piped.argv`
    /// up to its first `--` entry, then `piped.resume` with a session to continue, then
    /// `piped.tools_off` with tools off, then the rest of `piped.argv`.
    pub fn new(
        config: Config,
        home: &Path,
        cwd: &Path,
        model: Option<&str>,
        tools: bool,
        resume: Option<&str>,
    ) -> Result<(Self, Vec<LaunchEvent>), LaunchError> {
        let (home, cwd) = inputs(home, cwd, model, resume)?;
        if !tools && config.tools_off.is_none() {
            return Err(failure("launch_tools_off_unsupported"));
        }
        let value = |name: &str| match name {
            "home" => Ok(home.as_str()),
            "cwd" => Ok(cwd.as_str()),
            "model" => model.ok_or_else(|| failure("launch_model_unsupported")),
            "session" => resume.ok_or_else(|| failure("launch_session_id_invalid")),
            _ => Err(failure("launch_harness_unsupported")),
        };
        let resumed: &[String] = if resume.is_some() {
            &config.resume
        } else {
            &[]
        };
        let off: &[String] = if tools {
            &[]
        } else {
            config.tools_off.as_deref().unwrap_or_default()
        };
        let (options, rest) = config.argv.split_at(config.positional);
        let argv = options
            .iter()
            .chain(resumed)
            .chain(off)
            .chain(rest)
            .map(|argument| fill(argument, &value))
            .collect::<Result<Vec<String>, LaunchError>>()?;
        assert_eq!(argv.len(), config.argv.len() + resumed.len() + off.len());
        let conversation = Self {
            config,
            argv,
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

    /// The arguments the harness's process starts with. Output: the filled arguments.
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The harness session id. Output: the id the harness last reported, else the one the
    /// conversation started from, or none.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Whether the running turn used a tool with tools off, so its caller must make sure the
    /// process stops it. Output: true from the breach until the turn ends.
    pub fn breached(&self) -> bool {
        self.turn.breached
    }

    /// Starts a turn. Input: the prompt. Output: the frame to write to the process,
    /// `piped.prompt` with the prompt as JSON string text in place of `{prompt}` and never
    /// filled again, one line ending in a newline; or `launch_prompt_invalid` for an empty
    /// prompt, one with a NUL byte, one over `PROMPT_BYTES_MAX` or one whose frame would be
    /// over `FRAME_BYTES_MAX`, `launch_busy` during a turn and `launch_protocol_failed` once
    /// failed.
    pub fn prompt(&mut self, text: &str) -> Result<Vec<u8>, LaunchError> {
        self.assert_consistent();
        if self.phase == Phase::Failed {
            return Err(failure("launch_protocol_failed"));
        }
        if text.trim().is_empty() || text.contains('\0') || text.len() > PROMPT_BYTES_MAX {
            return Err(failure("launch_prompt_invalid"));
        }
        if self.phase == Phase::Running {
            return Err(failure("launch_busy"));
        }
        let quoted = Value::String(text.to_owned()).to_string();
        let escaped = &quoted[1..quoted.len() - 1];
        let mut frame = self
            .config
            .prompt
            .replacen("{prompt}", escaped, 1)
            .into_bytes();
        if frame.len() >= FRAME_BYTES_MAX {
            return Err(failure("launch_prompt_invalid"));
        }
        assert!(!frame.contains(&b'\n') && object(&frame));
        frame.push(b'\n');
        self.phase = Phase::Running;
        self.turn = Turn::default();
        self.assert_consistent();
        Ok(frame)
    }

    /// Asks the running turn to stop; the turn ends as interrupted on the line the harness
    /// then ends it with. Output: the `piped.interrupt` frame to write to the process, or
    /// `launch_not_running` with no turn, an interrupt already asked or a breach, whose
    /// interrupt was already written.
    pub fn interrupt(&mut self) -> Result<Vec<u8>, LaunchError> {
        self.assert_consistent();
        if self.phase != Phase::Running || self.turn.interrupted || self.turn.breached {
            return Err(failure("launch_not_running"));
        }
        self.turn.interrupted = true;
        self.assert_consistent();
        Ok(self.interrupt_frame())
    }

    /// Reads one line the process wrote. A line over `LINE_BYTES_MAX`, a line that is not
    /// one JSON object and a line no rule matches give nothing; a line outside a turn gives
    /// nothing but may carry the session id. With tools off, a tool the harness reports
    /// using is a breach: one `ApprovalDenied` instead of the tool use and the interrupt
    /// frame to write, unless an interrupt was already asked; after it only a line that ends
    /// the turn is read. A finished fact, a turn error or an interrupted fact ends the turn
    /// with exactly one `TurnFinished`: after an interrupt, `Interrupted` and an unsuccessful
    /// end; after a breach or a turn error, an unsuccessful end; else the success the rule
    /// read, true when none did. Input: the line, with or without its newline. Output: the
    /// events and the frames to write back, `launch_session_id_invalid` for a session id
    /// that is not plain, which fails the conversation for good, and
    /// `launch_protocol_failed` once failed.
    pub fn receive(
        &mut self,
        line: &[u8],
    ) -> Result<(Vec<LaunchEvent>, Vec<Vec<u8>>), LaunchError> {
        self.assert_consistent();
        if self.phase == Phase::Failed {
            return Err(failure("launch_protocol_failed"));
        }
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() > LINE_BYTES_MAX {
            return Ok((Vec::new(), Vec::new()));
        }
        let Ok(object @ Value::Object(_)) = serde_json::from_slice::<Value>(line) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let Some(found) = rules::matched(&self.config.rules, &object) else {
            return Ok((Vec::new(), Vec::new()));
        };
        if let Some(id) = found.session {
            if !plain_session(&id) {
                self.phase = Phase::Failed;
                self.turn = Turn::default();
                self.assert_consistent();
                return Err(failure("launch_session_id_invalid"));
            }
            self.session = Some(id);
        }
        let ending = matches!(
            found.fact,
            Fact::TurnFinished | Fact::TurnError | Fact::Interrupted
        );
        if self.phase != Phase::Running || (self.turn.breached && !ending) {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut events = Vec::new();
        let mut writes = Vec::new();
        match found.fact {
            Fact::Text => events.extend(found.text.map(|text| LaunchEvent::Text { text })),
            Fact::ToolUse if !self.tools => {
                self.turn.breached = true;
                events.push(LaunchEvent::ApprovalDenied);
                if !self.turn.interrupted {
                    writes.push(self.interrupt_frame());
                }
            }
            Fact::ToolUse => events.extend(found.name.map(|name| LaunchEvent::ToolUse { name })),
            Fact::Usage => events.extend(found.tokens.map(|tokens| LaunchEvent::Usage { tokens })),
            Fact::TurnFinished => {
                self.turn.success = Some(found.success.unwrap_or(true));
                events.extend(found.tokens.map(|tokens| LaunchEvent::Usage { tokens }));
                events.extend(self.end());
            }
            Fact::TurnError => {
                if !self.turn.interrupted && !self.turn.breached {
                    events.push(LaunchEvent::TurnError {
                        rate_limited: found.rate_limited.unwrap_or(false),
                    });
                }
                self.turn.errored = true;
                events.extend(self.end());
            }
            Fact::Interrupted => {
                self.turn.interrupted |= !self.turn.breached;
                events.extend(self.end());
            }
            Fact::Ready | Fact::TurnStarted | Fact::Waiting | Fact::Exited => {}
        }
        self.assert_consistent();
        Ok((events, writes))
    }

    fn end(&mut self) -> Vec<LaunchEvent> {
        assert_eq!(self.phase, Phase::Running);
        let unsuccessful = LaunchEvent::TurnFinished { success: false };
        let events = if self.turn.interrupted {
            vec![LaunchEvent::Interrupted, unsuccessful]
        } else if self.turn.breached || self.turn.errored {
            vec![unsuccessful]
        } else {
            vec![LaunchEvent::TurnFinished {
                success: self.turn.success.unwrap_or(true),
            }]
        };
        self.phase = Phase::Idle;
        self.turn = Turn::default();
        events
    }

    fn interrupt_frame(&self) -> Vec<u8> {
        let mut frame = self.config.interrupt.clone().into_bytes();
        frame.push(b'\n');
        frame
    }

    fn assert_consistent(&self) {
        assert!(!self.argv.is_empty() && self.argv.len() >= self.config.argv.len());
        assert!(self.config.positional <= self.config.argv.len());
        assert!(self.session.as_deref().is_none_or(plain_session));
        assert!(self.tools || self.config.tools_off.is_some());
        assert!(!self.tools || !self.turn.breached);
        assert!(self.phase == Phase::Running || self.turn == Turn::default());
    }
}

/// Reads one JSON line setting. Inputs: the piped block and the key. Output: the text, or a
/// refusal that names the setting: missing, not text, or not one line of 1 to
/// `TEMPLATE_BYTES_MAX` bytes.
fn template(piped: &Yaml, key: &str) -> Result<String, Refused> {
    match piped.get(key) {
        None | Some(Yaml::Null) => refuse(
            manifest::MISSING,
            format!("piped.{key}: this setting is needed."),
        ),
        Some(Yaml::Str(text))
            if !text.is_empty()
                && text.len() <= TEMPLATE_BYTES_MAX
                && !text.contains(['\r', '\n']) =>
        {
            Ok(text.clone())
        }
        Some(Yaml::Str(_)) => refuse(
            manifest::VALUE,
            format!("piped.{key}: this is one line of 1 to {TEMPLATE_BYTES_MAX} bytes."),
        ),
        Some(_) => refuse(
            manifest::TYPE,
            format!("piped.{key}: this must be text in quotes."),
        ),
    }
}

/// How often a text holds one placeholder. Inputs: the text and the placeholder's name.
/// Output: the count.
fn holds(text: &str, name: &str) -> usize {
    text.matches(&format!("{{{name}}}")).count()
}

/// Whether bytes are one JSON object. Input: the bytes. Output: true for one object.
fn object(bytes: &[u8]) -> bool {
    matches!(serde_json::from_slice::<Value>(bytes), Ok(Value::Object(_)))
}

#[cfg(test)]
#[path = "jsonl_rpc_tests.rs"]
mod tests;
