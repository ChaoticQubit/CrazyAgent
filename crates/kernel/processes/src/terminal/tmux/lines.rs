use super::*;

pub(super) fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '\\' | '"' | '$' => {
                quoted.push('\\');
                quoted.push(character);
            }
            control if (control as u32) < 0x20 || control as u32 == 0x7f => {
                let _ = write!(quoted, "\\{:03o}", control as u32);
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

pub(super) fn decode(data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len());
    let mut at = 0;
    while at < data.len() {
        let octal = data
            .get(at + 1..at + 4)
            .filter(|digits| data[at] == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d)))
            .and_then(|digits| {
                let value = digits
                    .iter()
                    .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                u8::try_from(value).ok()
            });
        match octal {
            Some(byte) => {
                bytes.push(byte);
                at += 4;
            }
            None => {
                bytes.push(data[at]);
                at += 1;
            }
        }
    }
    assert!(bytes.len() <= data.len());
    bytes
}

pub(super) fn target(agent: &AgentId) -> String {
    format!("{SESSION}:={agent}")
}

/// Checks a command for a new pane: bounded argv and environment, an absolute plain working
/// folder, a first word tmux cannot read as an option or assignment, and variables an agent
/// may carry. Takes the command. Returns its working folder as text, or
/// `terminal_command_invalid`.
pub(super) fn checked(
    command: &PaneCommand,
    variable_allowed: fn(&OsStr, &OsStr) -> bool,
) -> Result<&str, HostError> {
    let invalid = || failure("terminal_command_invalid");
    let cwd = command.cwd.to_str().ok_or_else(invalid)?;
    let first = command.argv.first().ok_or_else(invalid)?;
    let env_name = |name: &str| {
        !name.is_empty()
            && name.len() <= ENV_NAME_BYTES_MAX
            && !name.as_bytes()[0].is_ascii_digit()
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    if command.argv.len() > ARGS_MAX
        || command.env.len() > ENV_MAX
        || !command.cwd.is_absolute()
        || cwd.contains(['#', '\0'])
        || first.is_empty()
        || first.starts_with('-')
        || first.contains('=')
        || command.argv.iter().any(|arg| arg.contains('\0'))
        || command.env.iter().any(|(name, value)| {
            !env_name(name)
                || value.contains('\0')
                || !variable_allowed(OsStr::new(name), OsStr::new(value))
        })
    {
        return Err(invalid());
    }
    Ok(cwd)
}

pub(super) fn spawn_line(
    agent: &AgentId,
    command: &PaneCommand,
    confinement: &Confinement,
) -> Result<String, HostError> {
    let invalid = || failure("terminal_command_invalid");
    let cwd = checked(command, confinement.variable_allowed)?;
    let mut line = format!(
        "new-window -d -P -F '{PLACED}{FORMAT_END}' -n {agent} -c {} -t {SESSION}:",
        quote(cwd)
    );
    line.push_str(" -- ");
    line.push_str(ENV_PROGRAM);
    for (default, value) in TERMINAL_DEFAULTS {
        if !command.env.iter().any(|(name, _)| name == default) {
            line.push(' ');
            line.push_str(&quote(&format!("{default}={value}")));
        }
    }
    for (name, value) in &command.env {
        line.push(' ');
        line.push_str(&quote(&format!("{name}={value}")));
    }
    for arg in &command.argv {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    if line.len() > COMMAND_BYTES_MAX {
        return Err(invalid());
    }
    Ok(line)
}

/// Builds the new-window line that starts a native-mode lead in its own server. The server
/// already runs inside the lead's profile, so the command's leading `sandbox-exec -f
/// <profile>` must name that same profile and is dropped, and the environment comes from the
/// server (see `Tmux::lead_environment`) with `pane_env` set over it in the lead's pane only.
/// The lead's window becomes the session's current one, since tmux puts a split the lead's
/// app runs without a target into the current window, which would otherwise be the holder.
/// A command the engine put under its pane keeper is taken back out of it first
/// (`Confinement::unkept_pane`): the keeper cannot run inside the server's sandbox.
/// Takes the agent, the command, the server's profile and those variables. Returns the line,
/// or `terminal_command_invalid` for any other command or a variable name not plain.
pub(super) fn confined_line(
    agent: &AgentId,
    command: &PaneCommand,
    profile: &Path,
    pane_env: &[(String, String)],
    confinement: &Confinement,
) -> Result<String, HostError> {
    let invalid = || failure("terminal_command_invalid");
    let cwd = checked(command, confinement.variable_allowed)?;
    #[cfg(target_os = "macos")]
    let unkept = (confinement.unkept_pane)(&command.argv);
    #[cfg(not(target_os = "macos"))]
    let unkept: Option<Vec<String>> = None;
    let argv = unkept.as_deref().unwrap_or(&command.argv);
    let profile = profile.to_str().ok_or_else(invalid)?;
    let confined = [confinement.sandbox_exec, "-f", profile];
    if argv.len() <= confined.len()
        || argv[..confined.len()]
            .iter()
            .zip(confined)
            .any(|(arg, wanted)| arg != wanted)
    {
        return Err(invalid());
    }
    let program = &argv[confined.len()..];
    let first = &program[0];
    if first.is_empty() || first.starts_with('-') || first.contains('=') {
        return Err(invalid());
    }
    let mut line = format!(
        "new-window -P -F '{PLACED}{FORMAT_END}' -n {agent} -c {} -t {SESSION}: -- {PANE_PROGRAM}",
        quote(cwd)
    );
    for (name, value) in pane_env {
        let plain = !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit());
        if !plain || value.contains('\0') {
            return Err(invalid());
        }
        line.push(' ');
        line.push_str(&quote(&format!("{name}={value}")));
    }
    for arg in program {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    if line.len() > COMMAND_BYTES_MAX {
        return Err(invalid());
    }
    Ok(line)
}

/// The most pane history rows an attach sends ahead of the screen: all that tmux keeps.
pub(super) const HISTORY_ROWS_MAX: usize = SCROLLBACK_LINES as usize;

pub(super) const BRACKETED_PASTE_ON: &[u8] = b"\x1b[?2004h";

pub(super) const CURSOR_KEYS_ON: &[u8] = b"\x1b[?1h";

pub(super) const ALTERNATE_SCREEN_ON: &[u8] = b"\x1b[?1049h";

pub(super) const MOUSE_STANDARD_ON: &[u8] = b"\x1b[?1000h";

pub(super) const MOUSE_BUTTON_ON: &[u8] = b"\x1b[?1002h";

pub(super) const MOUSE_ALL_ON: &[u8] = b"\x1b[?1003h";

pub(super) const MOUSE_UTF8_ON: &[u8] = b"\x1b[?1005h";

pub(super) const MOUSE_SGR_ON: &[u8] = b"\x1b[?1006h";

/// The pane's modes a new viewer must take on so it shows the screen and sends keys, pastes
/// and mouse reports as a terminal attached from the start would. An app drawing on its
/// alternate screen with mouse reporting on, such as a full-screen agent that keeps its own
/// scrollback, then gets the wheel, instead of the view scrolling history the app no longer
/// shows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Modes {
    pub(super) bracketed_paste: bool,
    pub(super) cursor_keys: bool,
    pub(super) alternate_screen: bool,
    pub(super) mouse_standard: bool,
    pub(super) mouse_button: bool,
    pub(super) mouse_all: bool,
    pub(super) mouse_utf8: bool,
    pub(super) mouse_sgr: bool,
}

pub(super) fn render(
    screen: &[Vec<u8>],
    cols: u16,
    rows: u16,
    cursor: (u16, u16),
    modes: Modes,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (on, sequence) in [
        (modes.alternate_screen, ALTERNATE_SCREEN_ON),
        (modes.bracketed_paste, BRACKETED_PASTE_ON),
        (modes.cursor_keys, CURSOR_KEYS_ON),
        (modes.mouse_standard, MOUSE_STANDARD_ON),
        (modes.mouse_button, MOUSE_BUTTON_ON),
        (modes.mouse_all, MOUSE_ALL_ON),
        (modes.mouse_utf8, MOUSE_UTF8_ON),
        (modes.mouse_sgr, MOUSE_SGR_ON),
    ] {
        if on {
            bytes.extend_from_slice(sequence);
        }
    }
    bytes.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    for (row, line) in screen.iter().take(usize::from(rows)).enumerate() {
        if row > 0 {
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(line);
    }
    let column = cursor.0.min(cols.saturating_sub(1)) + 1;
    let line = cursor.1.min(rows.saturating_sub(1)) + 1;
    bytes.extend_from_slice(format!("\x1b[0m\x1b[{line};{column}H").as_bytes());
    bytes
}

/// Puts the pane's history rows into a fresh view's scrollback before its screen is drawn:
/// each row is written from the top, then `rows - 1` more line feeds scroll every one of
/// them off a view of `rows` rows. Inputs: the history rows, oldest first, and the view
/// height. Output: the bytes, or none for no history.
pub(super) fn scrollback(history: &[Vec<u8>], rows: u16) -> Vec<u8> {
    if history.is_empty() {
        return Vec::new();
    }
    let mut bytes = b"\x1b[0m\x1b[H\x1b[2J".to_vec();
    for line in history {
        bytes.extend_from_slice(line);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"\x1b[0m");
    bytes.extend(std::iter::repeat_n(
        b'\n',
        usize::from(rows.saturating_sub(1)),
    ));
    bytes
}

/// Takes one reply line of a format ending with `FORMAT_END`. Returns the line without the
/// mark, or none when tmux cut its expansion short.
pub(super) fn whole(line: &[u8]) -> Option<&[u8]> {
    line.strip_suffix(FORMAT_END.as_bytes())
}

/// Reads one `PANE_FIELDS` reply line: the pane id, its first process id and whether that
/// process ended. Takes the line without its end mark. Returns the pane, or none for a line
/// of another shape.
pub(super) fn pane_info(line: &[u8]) -> Option<PaneInfo> {
    let text = std::str::from_utf8(line).ok()?;
    let mut fields = text.split(' ');
    let (Some(id), Some(pid), Some(dead), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid > 0)?;
    let dead = match dead {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    pane_id_valid(id).then(|| PaneInfo {
        id: id.to_owned(),
        pid,
        dead,
    })
}

impl Tmux {
    /// Reads an agent terminal's screen and at most `history` rows above it in one tmux
    /// command line, and opens a viewer on its live output when `view` is set. The line ends
    /// by printing a fresh marker, so a capture whose rows cannot be counted is dropped up to
    /// it and fails only this attach, which asks again while time is left.
    /// Takes the name, the history row limit and whether to view. Returns the stream.
    pub(super) fn capture(
        &self,
        agent: &AgentId,
        pane: Option<&str>,
        history: usize,
        view: bool,
    ) -> Result<TerminalStream, HostError> {
        assert!(history <= HISTORY_ROWS_MAX);
        let control = self.live_control()?;
        let pane = match pane {
            Some(pane) => Some(pane.to_owned()),
            None if self.confined.is_some() => Some(self.agent_target(&control, agent)?),
            None => None,
        };
        let (target, filter) = match &pane {
            Some(pane) => {
                assert!(pane_id_valid(pane));
                (pane.clone(), format!("#{{==:#{{pane_id}},{pane}}}"))
            }
            None => (target(agent), "#{pane_active}".to_owned()),
        };
        let start = if history == 0 {
            "0".to_owned()
        } else {
            format!("-{history}")
        };
        let asked = format!(
            "list-panes -t {target} -f '{filter}' -F '#{{pane_height}} #{{history_size}} #{{pane_id}} #{{window_id}} #{{cursor_x}} #{{cursor_y}} #{{pane_width}} #{{bracket_paste_flag}} #{{keypad_cursor_flag}} #{{alternate_on}} #{{mouse_standard_flag}} #{{mouse_button_flag}} #{{mouse_all_flag}} #{{mouse_sgr_flag}} #{{mouse_utf8_flag}}{FORMAT_END}' ; capture-pane {CAPTURE_WRITTEN} -S {start} -t {target}"
        );
        let until = Instant::now() + REPLY_TIME;
        loop {
            let marker = fresh_marker()?;
            let shown = format!("display-message -p '{marker}'");
            let (reply, answer) = sync_channel(1);
            let attaching = Pending::Attach {
                agent: agent.clone(),
                located: None,
                history,
                view,
                reply,
                marker: marker.into_bytes(),
            };
            control.send_all(vec![(&asked, attaching), (&shown, Pending::Spent)])?;
            match control.wait(&answer) {
                Err(cut) if cut.0 == REPLY_CUT && Instant::now() < until => {}
                Err(cut) if cut.0 == REPLY_CUT => return Err(failure("terminal_not_answering")),
                answered => {
                    return answered.map(|mut stream| {
                        assert!(stream.at.is_none());
                        stream.at = pane.clone().map(|pane| PaneAt {
                            pane,
                            serial: control.serial,
                        });
                        stream
                    });
                }
            }
        }
    }
}

/// Whether `send_bytes` pastes a request through a tmux buffer: when it is longer than one
/// `send-keys` run and holds no NUL, which would end the buffer. Takes at most
/// `INPUT_BYTES_MAX` bytes. Returns the choice.
pub(super) fn pastes(bytes: &[u8]) -> bool {
    assert!(bytes.len() <= INPUT_BYTES_MAX);
    bytes.len() > SEND_KEYS_BYTES_MAX && !bytes.contains(&0)
}

/// The `set-option` commands the engine runs on each server it controls, in order: the
/// terminal type, window names and exits, scrollback, the lead shell on a confined host,
/// then `set-clipboard external` and `get-clipboard off`, so no pane's OSC 52 query reads a
/// tmux buffer. Takes whether the host is confined. Returns the commands.
pub(super) fn server_options(confined: bool) -> Vec<String> {
    let mut options = vec![
        format!("set-option -g default-terminal {TERMINAL_TYPE}"),
        "set-option -gw allow-rename off".into(),
        "set-option -gw automatic-rename off".into(),
        "set-option -gw remain-on-exit off".into(),
        format!("set-option -g history-limit {SCROLLBACK_LINES}"),
    ];
    if confined {
        options.push(format!("set-option -g default-shell {LEAD_SHELL}"));
    }
    options.push("set-option -sq set-clipboard external".into());
    options.push("set-option -sq get-clipboard off".into());
    assert!(options.len() >= 7);
    assert!(
        options
            .iter()
            .all(|option| option.starts_with("set-option -"))
    );
    options
}

/// The most commands one `Control::send_all` writes: a paste's `set-buffer` and
/// `paste-buffer`.
pub(super) const COMMANDS_SENT_MAX: usize = 2;

/// The two commands that paste bytes into one target through a named tmux buffer:
/// `set-buffer` with every byte other than an ASCII letter or digit written as a `\ooo`
/// octal escape inside double quotes, so tmux's parser decodes each exactly and expands no
/// `$`, `~` or `#`, then `paste-buffer -d -r -S`, which writes the bytes unchanged, control
/// bytes and line ends included, and deletes the buffer. No `-p`: the pasted bytes carry
/// their own bracket marks. Takes the target, the buffer name and bytes without a NUL.
/// Returns the commands.
pub(super) fn paste_buffer_lines(target: &str, buffer: &str, bytes: &[u8]) -> Vec<String> {
    assert!(!bytes.contains(&0));
    assert!(bytes.len() <= INPUT_BYTES_MAX);
    vec![
        set_buffer_line(buffer, bytes, false),
        format!("paste-buffer -d -r -S -b {buffer} -t {target}"),
    ]
}

/// One `set-buffer` command that writes bytes into a named buffer, appended with `append`,
/// every byte other than an ASCII letter or digit written as a `\ooo` octal escape inside
/// double quotes, so tmux's parser decodes each exactly and expands nothing. Takes the
/// buffer name, at most `INPUT_BYTES_MAX` bytes without a NUL and whether to append.
/// Returns the command.
pub(super) fn set_buffer_line(buffer: &str, bytes: &[u8], append: bool) -> String {
    assert!(!bytes.contains(&0));
    assert!(bytes.len() <= INPUT_BYTES_MAX);
    let flag = if append { "-a " } else { "" };
    let mut set = String::with_capacity(28 + buffer.len() + bytes.len() * 4);
    let _ = write!(set, "set-buffer {flag}-b {buffer} -- \"");
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() {
            set.push(char::from(byte));
        } else {
            let _ = write!(set, "\\{byte:03o}");
        }
    }
    set.push('"');
    assert!(set.len() <= "set-buffer -a -b  -- \"\"".len() + buffer.len() + bytes.len() * 4);
    set
}

/// The commands that paste one owner paste into one target through a named buffer (T7):
/// one `set-buffer` per piece of at most `INPUT_BYTES_MAX` bytes, each piece after the first
/// appended, then `paste-buffer -d -p -r -S`, which brackets the whole only when the pane's
/// app asked, writes the bytes unchanged otherwise and deletes the buffer. Takes the target,
/// the buffer name and 1 to `PASTE_BYTES_MAX` keys without a NUL. Returns the commands.
pub(super) fn owner_paste_lines(target: &str, buffer: &str, keys: &[u8]) -> Vec<String> {
    assert!(!keys.is_empty() && keys.len() <= PASTE_BYTES_MAX);
    let mut lines: Vec<String> = keys
        .chunks(INPUT_BYTES_MAX)
        .enumerate()
        .map(|(at, piece)| set_buffer_line(buffer, piece, at > 0))
        .collect();
    lines.push(format!("paste-buffer -d -p -r -S -b {buffer} -t {target}"));
    assert_eq!(lines.len(), keys.len().div_ceil(INPUT_BYTES_MAX) + 1);
    lines
}

/// A tmux buffer name no other paste of this engine uses, `crazyagent-paste-<pid>-<n>`,
/// plain enough to sit unquoted in a command. Takes nothing. Returns the name.
pub(super) fn paste_buffer_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    assert!(next < u64::MAX);
    let name = format!("crazyagent-paste-{}-{next}", std::process::id());
    assert!(
        name.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    );
    name
}

/// The `send-keys -H` commands that type bytes into one target, in order. Takes the target
/// and the bytes. Returns one command per `input_runs` run of at most `SEND_KEYS_BYTES_MAX`
/// bytes, so none cuts a character or an escape sequence, and none for no bytes.
pub(super) fn send_keys_lines(target: &str, bytes: &[u8]) -> Vec<String> {
    let lines: Vec<String> = super::super::input_runs(bytes, SEND_KEYS_BYTES_MAX)
        .into_iter()
        .map(|run| {
            let mut line = format!("send-keys -t {target} -H");
            for byte in run {
                let _ = write!(line, " {byte:02x}");
            }
            line
        })
        .collect();
    assert!(lines.len() >= bytes.len().div_ceil(SEND_KEYS_BYTES_MAX));
    assert!(lines.len() <= super::super::input_runs_max(bytes.len(), SEND_KEYS_BYTES_MAX));
    lines
}

#[cfg(test)]
mod tests;
