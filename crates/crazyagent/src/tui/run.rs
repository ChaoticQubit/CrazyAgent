use super::client::{Connection, OwnerEvent};
use super::ctx::{Ctx, Link};
use super::glyphs::Glyphs;
use super::keys::Key;
use super::prefs;
use super::seam::{Arrival, NOT_READY, Request, ShellEvent, ShellRequest, TerminalChunk};
use super::shell::Shell;
use super::theme::{self, Appearance, Colors};
use super::{TerminalGuard, TuiError, bounded_area};
use crate::engine::Client;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, Event,
};
use crossterm::execute;
use crossterm::terminal::LeaveAlternateScreen;
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(100);
const IDLE_WAKE: Duration = Duration::from_millis(500);
const REFRESH: Duration = Duration::from_secs(5);
const RECONNECT: Duration = Duration::from_secs(3);
const PASTE_CHARS_MAX: usize = 100_000;

struct Extras;

impl Extras {
    fn enter() -> Result<Self, TuiError> {
        execute!(std::io::stdout(), EnableBracketedPaste, EnableFocusChange)
            .map_err(|error| TuiError(error.to_string()))?;
        Ok(Self)
    }
}

impl Drop for Extras {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableFocusChange, DisableBracketedPaste);
    }
}

/// Writes the sequences that hand the terminal back: stop focus and paste
/// events, show the cursor, leave the alternate screen.
/// Input: the terminal writer. Output: the write result.
fn restore(out: &mut impl std::io::Write) -> std::io::Result<()> {
    execute!(
        out,
        DisableFocusChange,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    )
}

/// Installs a panic hook that restores the terminal before the panic message
/// prints, so it lands on the normal screen, then runs the previous hook.
/// Input: none. Output: none; the hook stays for the rest of the process.
fn restore_on_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = restore(&mut std::io::stdout());
        previous(info);
    }));
}

/// Builds the shell's context from this process's environment.
/// Inputs: an environment lookup and the engine socket shown when it is down.
/// Output: the context, or the one-line refusal for `TERM=dumb`.
pub fn context(var: impl Fn(&str) -> Option<String>, socket: String) -> Result<Ctx, TuiError> {
    let depth = theme::depth(&var)
        .ok_or_else(|| TuiError("CrazyAgent needs a full terminal. TERM is set to dumb.".into()))?;
    let prefs = prefs::default_path(var("HOME").as_deref());
    let colors = prefs
        .as_deref()
        .map_or(Colors::default(), prefs::load_colors);
    let appearance = if colors == Colors::FollowMacos {
        theme::system_appearance()
    } else {
        Appearance::Dark
    };
    let mut ctx = Ctx::new(depth, colors, appearance, Glyphs::detect(&var), socket);
    ctx.prefs = prefs;
    ctx.home = var("HOME").filter(|home| home.starts_with('/'));
    Ok(ctx)
}

/// Runs the rebuilt TUI until the owner quits or a signal arrives.
/// Inputs: the unbound engine client and the stop flag set by signals.
/// Output: Ok after a clean exit, or the terminal or connection error.
pub fn run(client: Client, stopped: &AtomicBool) -> Result<(), TuiError> {
    let (socket, timeout) = client
        .into_unbound_configuration()
        .map_err(|error| TuiError(error.to_string()))?;
    let ctx = context(
        |name| std::env::var(name).ok(),
        socket.display().to_string(),
    )?;
    let client = Client::new(socket, timeout).map_err(|error| TuiError(error.to_string()))?;
    let mut connection = Connection::connect_paired(client).map_err(TuiError)?;
    restore_on_panic();
    let _guard = TerminalGuard::enter()?;
    let _extras = Extras::enter()?;
    let (width, height) =
        crossterm::terminal::size().map_err(|error| TuiError(error.to_string()))?;
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(bounded_area(width, height)),
        },
    )
    .map_err(|error| TuiError(error.to_string()))?;
    let mut shell = Shell::new(ctx);
    shell.resize(width, height);
    drive(&mut terminal, &mut shell, &mut connection, stopped)
}

fn drive(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    shell: &mut Shell,
    connection: &mut Connection,
    stopped: &AtomicBool,
) -> Result<(), TuiError> {
    let mut dirty = true;
    let mut tick = Instant::now();
    let mut refresh = Instant::now() + REFRESH;
    let mut reconnect = Instant::now() + RECONNECT;
    let mut showing_message = false;
    let mut link = shell.ctx.link.clone();
    while !stopped.load(Ordering::Relaxed) && !shell.quitting() {
        while let Some(event) = connection.poll_owner().map_err(TuiError)? {
            shell.arrive(owner_arrival(event));
            dirty = true;
        }
        if let Some(result) = connection.poll().map_err(TuiError)? {
            shell.arrive(Arrival::Wire(result.map_err(|error| error.to_string())));
            dirty = true;
        }
        let now = Instant::now();
        if shell.ctx.link != link {
            if shell.ctx.link == Link::Reconnecting {
                reconnect = now + RECONNECT;
            }
            link = shell.ctx.link.clone();
        }
        if !connection.busy() {
            if shell.ctx.take_reconnect() || (link == Link::Reconnecting && now >= reconnect) {
                connection.reconnect().map_err(TuiError)?;
                reconnect = now + RECONNECT;
            } else if link == Link::Ready {
                if let Some(request) = shell.ctx.next_request() {
                    send(request, shell, connection)?;
                    dirty = true;
                } else if now >= refresh {
                    shell.refresh();
                    refresh = now + REFRESH;
                }
            }
        }
        let message = shell.ctx.message().is_some();
        if message != showing_message {
            showing_message = message;
            dirty = true;
        }
        if shell.pace(now) {
            dirty = true;
        }
        if shell.busy() && now.duration_since(tick) >= TICK {
            shell.ctx.tick = shell.ctx.tick.wrapping_add(1);
            tick = now;
            dirty = true;
        }
        if dirty {
            terminal
                .draw(|frame| shell.draw(frame))
                .map_err(|error| TuiError(error.to_string()))?;
            dirty = false;
        }
        let waiting = shell.ctx.link == Link::Ready && shell.ctx.queued() > 0;
        let wait = if shell.busy() || connection.busy() || waiting {
            TICK
        } else {
            IDLE_WAKE
        };
        if !event::poll(wait).map_err(|error| TuiError(error.to_string()))? {
            continue;
        }
        match event::read().map_err(|error| TuiError(error.to_string()))? {
            Event::Key(key) => {
                if let Some(key) = Key::from_event(key) {
                    shell.key(key);
                    dirty = true;
                }
            }
            Event::Paste(text) => {
                let text: String = text.chars().take(PASTE_CHARS_MAX).collect();
                shell.key(Key::Paste(text));
                dirty = true;
            }
            Event::Resize(width, height) => {
                shell.resize(width, height);
                terminal
                    .resize(bounded_area(width, height))
                    .map_err(|error| TuiError(error.to_string()))?;
                dirty = true;
            }
            Event::FocusGained if shell.ctx.colors == Colors::FollowMacos => {
                let appearance = theme::system_appearance();
                if appearance != shell.ctx.appearance {
                    let colors = shell.ctx.colors;
                    shell.ctx.restyle(colors, appearance);
                    terminal
                        .clear()
                        .map_err(|error| TuiError(error.to_string()))?;
                    dirty = true;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Turns one owner worker event into a shell arrival.
/// Input: the event. Output: the arrival screens react to.
pub(super) fn owner_arrival(event: OwnerEvent) -> Arrival {
    match event {
        OwnerEvent::Access {
            ready: true,
            identity,
            ..
        } => Arrival::Connected(identity),
        OwnerEvent::Access { problem, .. } => Arrival::Disconnected(problem),
        OwnerEvent::Shell(event) => match *event {
            ShellEvent::Profile(result) => Arrival::Profile(result),
            ShellEvent::ProfileSaved(result) => Arrival::ProfileSaved(result),
            ShellEvent::Pools(result) => Arrival::Pools(result),
            ShellEvent::PoolProjects(result) => Arrival::PoolProjects(result),
            ShellEvent::Projects(result) => Arrival::Projects(result),
            ShellEvent::PoolEdited(result) => Arrival::PoolEdited(result),
        },
        other => Arrival::Owner(Box::new(other)),
    }
}

/// Sends one queued request on the idle, connected engine connection.
/// Inputs: the request, the shell and the connection. Output: Ok, or the
/// connection error that ends the TUI.
fn send(request: Request, shell: &mut Shell, connection: &mut Connection) -> Result<(), TuiError> {
    assert!(!connection.busy());
    assert!(shell.ctx.link == Link::Ready);
    let result = match request {
        Request::Profile => connection.shell(ShellRequest::Profile),
        Request::SaveProfile { profile } => connection.shell(ShellRequest::SaveProfile {
            profile,
            generation: shell.ctx.next_generation(),
        }),
        Request::Pools => connection.shell(ShellRequest::Pools),
        Request::PoolProjects => connection.shell(ShellRequest::PoolProjects),
        Request::Projects => connection.shell(ShellRequest::Projects),
        Request::EditPool { change } => connection.shell(ShellRequest::EditPool {
            change,
            generation: shell.ctx.next_generation(),
        }),
        Request::Setup(query) => connection.project_setup(query),
        Request::Creation(job) => connection.creation(*job),
        Request::Conversation(job) => connection.conversation(*job),
        Request::Master(job) => connection.master(*job),
        Request::Wire { command, revision } => connection.start(command, revision).map(|_| ()),
        Request::Settings(job) => connection.settings(*job),
        Request::TeamGraph { project } => {
            shell.arrive(Arrival::TeamGraph {
                project,
                result: Err(NOT_READY.into()),
            });
            Ok(())
        }
        Request::TerminalAttach { agent } => {
            shell.arrive(Arrival::Terminal {
                agent,
                chunk: TerminalChunk::Closed(NOT_READY.into()),
            });
            Ok(())
        }
        Request::TerminalInput { .. }
        | Request::TerminalResize { .. }
        | Request::TerminalDetach { .. } => Ok(()),
        Request::Intake(job) => connection.intake(*job),
        Request::Reconnect => connection.reconnect(),
    };
    result.map_err(TuiError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restore_hands_the_terminal_back_in_full() {
        let mut out = Vec::new();
        restore(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        for sequence in ["\x1b[?1004l", "\x1b[?2004l", "\x1b[?25h", "\x1b[?1049l"] {
            assert!(text.contains(sequence), "missing {sequence:?} in {text:?}");
        }
        assert!(
            text.ends_with("\x1b[?1049l"),
            "the alternate screen is left last"
        );
    }
}
