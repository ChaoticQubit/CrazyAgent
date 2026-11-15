use super::super::launch::{LaunchError, LaunchEvent, PROMPT_BYTES_MAX, failure};
use super::super::manifest::{self, DriverKind, Manifest, Refused};
use super::jsonl_turn::{MODEL_BYTES_MAX, Phase, arguments, plain_session};
use super::rules;
use serde_json::Value;
use std::path::Path;

/// The most bytes of output one turn keeps; a turn whose output passes it is stopped.
pub const OUTPUT_BYTES_MAX: usize = 1024 * 1024;

/// The piped settings of a harness on the `text-turn` driver, read from its manifest: the
/// arguments of every turn, the ones added to continue a session, the ones added with tools
/// off, and, for a harness that ends its output with one JSON object, the paths of the
/// reply and of the session id in it. With no reply path, the reply is the whole output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    argv: Vec<String>,
    resume: Vec<String>,
    tools_off: Option<Vec<String>>,
    reply: Option<String>,
    session: Option<String>,
}

impl Config {
    /// Reads the settings of one manifest. Input: a manifest whose `piped.driver` is
    /// `text-turn`. Output: the settings, or a refusal that names the setting: `piped.argv`
    /// missing or without `{prompt}`, `piped.resume` without `{session}`, an argument that is
    /// not 1 to `ARG_BYTES_MAX` bytes of text, a `piped.reply` or `piped.session` that is not
    /// a plain dotted path, or `piped.session` without `piped.reply`.
    pub fn read(manifest: &Manifest) -> Result<Self, Refused> {
        let Some(piped) = manifest
            .block("piped")
            .filter(|_| manifest.piped == Some(DriverKind::TextTurn))
        else {
            return refuse(
                manifest::KIND,
                "piped.driver: this harness does not run on the text-turn driver.",
            );
        };
        let argv = arguments(piped, "argv")?.unwrap_or_default();
        if argv.is_empty() {
            return refuse(manifest::MISSING, "piped.argv: this setting is needed.");
        }
        if !argv.iter().any(|argument| argument.contains("{prompt}")) {
            return refuse(
                manifest::VALUE,
                "piped.argv: {prompt} is needed, so each turn's prompt reaches the harness.",
            );
        }
        let resume = arguments(piped, "resume")?.unwrap_or_default();
        if !resume.is_empty() && !resume.iter().any(|argument| argument.contains("{session}")) {
            return refuse(
                manifest::VALUE,
                "piped.resume: {session} is needed, so a turn continues its session.",
            );
        }
        let tools_off = arguments(piped, "tools_off")?;
        let reply = rules::path_of(piped, "reply", "piped.reply")?;
        let session = rules::path_of(piped, "session", "piped.session")?;
        if session.is_some() && reply.is_none() {
            return refuse(
                manifest::VALUE,
                "piped.session: a session id is read only from the final JSON a reply path names.",
            );
        }
        Ok(Self {
            argv,
            resume,
            tools_off,
            reply,
            session,
        })
    }
}

/// What the running turn has done that decides how it ends.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Turn {
    over: bool,
    interrupted: bool,
}

/// One conversation with a harness that runs one process per turn and gives no event
/// stream, without any IO: it gives each turn's arguments, keeps the turn's output within
/// [`OUTPUT_BYTES_MAX`], and when the process exits gives the reply, the whole output or one
/// field of its final JSON object, and the turn's end. It never reports usage, so a run on
/// it reads "not reported". A session id the final JSON gives is passed to the next turn
/// through `piped.resume`.
#[derive(Debug)]
pub struct TextTurn {
    config: Config,
    home: String,
    cwd: String,
    model: Option<String>,
    tools: bool,
    session: Option<String>,
    phase: Phase,
    turn: Turn,
    output: Vec<u8>,
}

impl TextTurn {
    /// Starts a conversation. Inputs: the settings, the harness's private home and working
    /// folder, both absolute UTF-8, the model and the tools switch. Output: the conversation
    /// and its `Ready` event, or `launch_account_home_unsafe`,
    /// `launch_working_dir_unavailable`, `launch_model_unsupported` for a model that is not
    /// 1 to `MODEL_BYTES_MAX` bytes without a control character, or
    /// `launch_tools_off_unsupported` for tools off on a manifest with no `tools_off`.
    pub fn new(
        config: Config,
        home: &Path,
        cwd: &Path,
        model: Option<&str>,
        tools: bool,
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
        if !tools && config.tools_off.is_none() {
            return Err(failure("launch_tools_off_unsupported"));
        }
        let conversation = Self {
            config,
            home,
            cwd,
            model: model.map(str::to_owned),
            tools,
            session: None,
            phase: Phase::Idle,
            turn: Turn::default(),
            output: Vec::new(),
        };
        conversation.assert_consistent();
        let ready = LaunchEvent::Ready {
            native_session: None,
        };
        Ok((conversation, vec![ready]))
    }

    /// Where the conversation stands. Output: the phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The harness session id the next turn continues. Output: the id the last final JSON
    /// gave, or none.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Whether the running turn's output passed [`OUTPUT_BYTES_MAX`], so its caller must stop
    /// the turn's process. Output: true from then until the turn ends.
    pub fn over(&self) -> bool {
        self.turn.over
    }

    /// Starts a turn. Input: the prompt. Output: the turn's arguments, each placeholder
    /// filled once and never again inside a filled value: `piped.argv`, then `piped.resume`
    /// when a session id is known, then `piped.tools_off` with tools off. Refusals: as on
    /// jsonl-turn, `launch_prompt_invalid`, `launch_busy`, `launch_protocol_failed` once
    /// failed, `launch_model_unsupported`, `launch_session_id_invalid` and
    /// `launch_harness_unsupported` for an account value this driver does not hand out yet.
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
        let resume: &[String] = if self.session.is_some() {
            self.config.resume.as_slice()
        } else {
            &[]
        };
        let off: &[String] = if self.tools {
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
            .map(|argument| super::fill(argument, |name| self.value(name, prompt)))
            .collect::<Result<Vec<String>, LaunchError>>()?;
        assert!(argv.len() >= self.config.argv.len());
        self.phase = Phase::Running;
        self.turn = Turn::default();
        self.assert_consistent();
        Ok(argv)
    }

    /// Keeps one line of the running turn's output, up to [`OUTPUT_BYTES_MAX`] bytes in all;
    /// the line that passes it is kept only in part and marks the turn for stopping. Lines
    /// outside a turn, after an interrupt or past the bound are dropped. Input: the line, with
    /// its newline. Output: no events, as the reply comes when the process exits, or
    /// `launch_protocol_failed` once failed.
    pub fn receive(&mut self, line: &[u8]) -> Result<Vec<LaunchEvent>, LaunchError> {
        self.assert_consistent();
        if self.phase == Phase::Failed {
            return Err(failure("launch_protocol_failed"));
        }
        if self.phase != Phase::Running || self.turn.over || self.turn.interrupted {
            return Ok(Vec::new());
        }
        let room = OUTPUT_BYTES_MAX - self.output.len();
        if line.len() > room {
            self.output.extend_from_slice(&line[..room]);
            self.turn.over = true;
        } else {
            self.output.extend_from_slice(line);
        }
        self.assert_consistent();
        Ok(Vec::new())
    }

    /// Counts a line too long for the output reader as output past the bound, which marks
    /// the running turn for stopping.
    pub fn skipped(&mut self) {
        if self.phase == Phase::Running {
            self.turn.over = true;
        }
    }

    /// Marks the running turn as interrupted; the caller then stops the turn's process.
    /// Output: Ok, or `launch_not_running` with no turn, an interrupt already asked or
    /// output past the bound, whose turn the caller already stops.
    pub fn interrupt(&mut self) -> Result<(), LaunchError> {
        self.assert_consistent();
        if self.phase != Phase::Running || self.turn.interrupted || self.turn.over {
            return Err(failure("launch_not_running"));
        }
        self.turn.interrupted = true;
        Ok(())
    }

    /// Ends the running turn when its process exits, with exactly one `TurnFinished`: after
    /// an interrupt, `Interrupted` and an unsuccessful end; after output past the bound, what
    /// fit as the reply when the reply is the whole output, then a `TurnError` and an
    /// unsuccessful end; for an exit code other than 0 or a signal, a `TurnError` and an
    /// unsuccessful end, with no reply; else the reply, when it is not empty, and a
    /// successful end, or a `TurnError` and an unsuccessful end when the final JSON holds no
    /// text at the reply path. Input: the exit code, none when a signal ended the process.
    /// Output: the events, `launch_not_running` with no turn, or `launch_session_id_invalid`
    /// for a session id that is not plain, which fails the conversation for good.
    pub fn exited(&mut self, code: Option<i32>) -> Result<Vec<LaunchEvent>, LaunchError> {
        self.assert_consistent();
        if self.phase != Phase::Running {
            return Err(failure("launch_not_running"));
        }
        let output = std::mem::take(&mut self.output);
        let turn = std::mem::take(&mut self.turn);
        self.phase = Phase::Idle;
        let unsuccessful = LaunchEvent::TurnFinished { success: false };
        let error = LaunchEvent::TurnError {
            rate_limited: false,
        };
        let mut events = Vec::new();
        if turn.interrupted {
            events.extend([LaunchEvent::Interrupted, unsuccessful]);
        } else if turn.over {
            if self.config.reply.is_none() {
                events.extend(text(plain(&output)));
            }
            events.extend([error, unsuccessful]);
        } else if code != Some(0) {
            events.extend([error, unsuccessful]);
        } else {
            match self.reply(&output) {
                Some((_, Some(id))) if !plain_session(&id) => {
                    self.phase = Phase::Failed;
                    self.assert_consistent();
                    return Err(failure("launch_session_id_invalid"));
                }
                Some((reply, session)) => {
                    self.session = session.or(self.session.take());
                    events.extend(text(reply));
                    events.push(LaunchEvent::TurnFinished { success: true });
                }
                None => events.extend([error, unsuccessful]),
            }
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, LaunchEvent::TurnFinished { .. }))
                .count(),
            1
        );
        self.assert_consistent();
        Ok(events)
    }

    fn reply(&self, output: &[u8]) -> Option<(String, Option<String>)> {
        let Some(path) = &self.config.reply else {
            return Some((plain(output), None));
        };
        let object = final_json(output)?;
        let Value::String(reply) = rules::at(&object, path)? else {
            return None;
        };
        assert!(reply.len() <= OUTPUT_BYTES_MAX);
        let session =
            self.config
                .session
                .as_deref()
                .and_then(|path| match rules::at(&object, path) {
                    Some(Value::String(id)) => Some(id.clone()),
                    _ => None,
                });
        Some((reply.clone(), session))
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
        assert!(!self.config.argv.is_empty());
        assert!(self.config.session.is_none() || self.config.reply.is_some());
        assert!(self.tools || self.config.tools_off.is_some());
        assert!(self.output.len() <= OUTPUT_BYTES_MAX);
        assert!(
            self.phase == Phase::Running
                || (self.turn == Turn::default() && self.output.is_empty())
        );
    }
}

/// The text event of a reply, none for an empty one. Input: the reply. Output: the event.
fn text(reply: String) -> Option<LaunchEvent> {
    (!reply.is_empty()).then_some(LaunchEvent::Text { text: reply })
}

/// The reply of a turn whose whole output is its reply: the output with bytes that are not
/// UTF-8 replaced, without its leading blank lines and trailing white space, cut at a
/// character boundary within [`OUTPUT_BYTES_MAX`]. Input: the output. Output: the reply.
fn plain(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(output);
    let text = text.trim_end();
    let start = text
        .find(|letter: char| !letter.is_whitespace())
        .map_or(text.len(), |first| {
            text[..first].rfind('\n').map_or(0, |line| line + 1)
        });
    let mut reply = text[start..].to_owned();
    if reply.len() > OUTPUT_BYTES_MAX {
        let mut end = OUTPUT_BYTES_MAX;
        while !reply.is_char_boundary(end) {
            end -= 1;
        }
        reply.truncate(end);
    }
    assert!(reply.len() <= OUTPUT_BYTES_MAX);
    reply
}

/// The JSON object a turn's output ends with: the whole output, or else its last line that
/// is not blank. Output that is not UTF-8 holds none, so no text read from the object is
/// longer than the output. Input: the output. Output: the object, or none.
fn final_json(output: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(output).ok()?.trim();
    if let Ok(object @ Value::Object(_)) = serde_json::from_str::<Value>(text) {
        return Some(object);
    }
    let last = text.lines().rev().find(|line| !line.trim().is_empty())?;
    match serde_json::from_str::<Value>(last.trim()) {
        Ok(object @ Value::Object(_)) => Some(object),
        _ => None,
    }
}

fn refuse<T>(code: &'static str, message: &str) -> Result<T, Refused> {
    Err(Refused {
        code,
        message: message.to_owned(),
    })
}

#[cfg(test)]
#[path = "text_turn_tests.rs"]
mod tests;
