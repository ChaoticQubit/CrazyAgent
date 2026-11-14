use super::client::editor;
use super::client::terminal::Terminals;
use super::client::{Connection, OwnerEvent};
use super::ctx::{Ctx, Link};
use super::glyphs::Glyphs;
use super::keys::{Key, ShiftEnter};
use super::prefs;
use super::seam::{Arrival, Request, ShellEvent, ShellRequest};
use super::shell::Shell;
use super::theme::{self, Appearance, Colors};
use super::{TerminalGuard, TuiError, bounded_area};
use crate::engine::Client;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
    Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::LeaveAlternateScreen;
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(100);
const IDLE_WAKE: Duration = Duration::from_millis(500);
const STREAM_WAKE: Duration = Duration::from_millis(16);
const FRAMES_PER_PASS: usize = 64;
const REFRESH: Duration = Duration::from_secs(5);
const REPLY_WATCH: Duration = Duration::from_secs(1);
const RECONNECT: Duration = Duration::from_secs(3);
/// The most characters of one paste the TUI takes; the rest is dropped.
pub(in crate::tui) const PASTE_CHARS_MAX: usize = 100_000;

struct Extras;

impl Extras {
    fn enter(shift_enter: ShiftEnter) -> Result<Self, TuiError> {
        execute!(std::io::stdout(), EnableBracketedPaste, EnableFocusChange)
            .map_err(|error| TuiError(error.to_string()))?;
        let extras = Self;
        push_keyboard(
            &mut std::io::stdout(),
            shift_enter,
            crossterm::terminal::supports_keyboard_enhancement,
            &KEYBOARD_PUSHED,
        )
        .map_err(|error| TuiError(error.to_string()))?;
        Ok(extras)
    }
}

impl Drop for Extras {
    fn drop(&mut self) {
        let _ = leave(&mut std::io::stdout(), &KEYBOARD_PUSHED);
    }
}

/// Whether this TUI pushed the enhanced keyboard protocol and has not popped it yet.
pub(in crate::tui) static KEYBOARD_PUSHED: AtomicBool = AtomicBool::new(false);

/// Turns on the enhanced keyboard protocol (disambiguated escape codes) under Auto when the
/// terminal says it has it, so Shift-Enter arrives as its own key; Off never asks. The flag
/// is set before the push is written, so every restore pops it.
/// Inputs: the writer, the choice, the support query and the pushed flag. Output: the write
/// result; a failed or timed-out query leaves the terminal as it is.
fn push_keyboard(
    out: &mut impl std::io::Write,
    choice: ShiftEnter,
    supported: impl FnOnce() -> std::io::Result<bool>,
    pushed: &AtomicBool,
) -> std::io::Result<()> {
    if choice == ShiftEnter::Off || !matches!(supported(), Ok(true)) {
        return Ok(());
    }
    pushed.store(true, Ordering::SeqCst);
    execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

/// Pushes the keyboard protocol again on the screen a child that took the terminal handed
/// back, when the restore before the child popped it, so Shift-Enter stays its own key. The
/// terminal already said it has the protocol, so it is not asked again; the flag is set
/// before the push is written, so the next restore pops it. Inputs: the writer, whether it
/// was pushed before the handoff and the pushed flag. Output: the write result.
pub(in crate::tui) fn push_back(
    out: &mut impl std::io::Write,
    was_pushed: bool,
    pushed: &AtomicBool,
) -> std::io::Result<()> {
    if !was_pushed || pushed.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

/// Pops the keyboard protocol when this TUI pushed it, once; the terminal keeps one stack
/// per screen, so it runs before the alternate screen is left.
/// Inputs: the writer and the pushed flag. Output: the write result.
fn pop_keyboard(out: &mut impl std::io::Write, pushed: &AtomicBool) -> std::io::Result<()> {
    if pushed.swap(false, Ordering::SeqCst) {
        execute!(out, PopKeyboardEnhancementFlags)?;
    }
    Ok(())
}

/// Makes the terminal follow a Shift-Enter choice saved in Settings, at once: Off pops the
/// protocol this TUI pushed without asking the terminal; Auto asks and pushes it when it is
/// not pushed yet. Inputs: the writer, the saved choice, the support query and the pushed
/// flag. Output: the write result.
fn switch_keyboard(
    out: &mut impl std::io::Write,
    choice: ShiftEnter,
    supported: impl FnOnce() -> std::io::Result<bool>,
    pushed: &AtomicBool,
) -> std::io::Result<()> {
    if choice == ShiftEnter::Off {
        return pop_keyboard(out, pushed);
    }
    if pushed.load(Ordering::SeqCst) {
        return Ok(());
    }
    push_keyboard(out, choice, supported, pushed)
}

/// Writes what the normal exit hands back before the screen guard leaves the alternate
/// screen: the keyboard protocol popped when this TUI pushed it, then focus and paste
/// events stopped. Inputs: the writer and the pushed flag. Output: the write result.
fn leave(out: &mut impl std::io::Write, pushed: &AtomicBool) -> std::io::Result<()> {
    let popped = pop_keyboard(out, pushed);
    execute!(out, DisableFocusChange, DisableBracketedPaste)?;
    popped
}

/// Writes the sequences that hand the terminal back: the keyboard protocol popped when
/// this TUI pushed it, focus and paste events stopped, the cursor shown, the alternate
/// screen left. Input: the terminal writer. Output: the write result.
pub(super) fn restore(out: &mut impl std::io::Write) -> std::io::Result<()> {
    restore_with(out, &KEYBOARD_PUSHED)
}

/// Writes what `restore` writes, popping the keyboard protocol when the given flag says
/// it was pushed. Inputs: the writer and the pushed flag. Output: the write result.
pub(in crate::tui) fn restore_with(
    out: &mut impl std::io::Write,
    pushed: &AtomicBool,
) -> std::io::Result<()> {
    let popped = pop_keyboard(out, pushed);
    execute!(
        out,
        DisableFocusChange,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    )?;
    popped
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
    let shift_arrows = prefs.as_deref().map_or_else(Default::default, |path| {
        prefs::load_shift_arrows(&prefs::shift_arrows_path(path))
    });
    let shift_enter = prefs.as_deref().map_or_else(Default::default, |path| {
        prefs::load_shift_enter(&prefs::shift_enter_path(path))
    });
    let mut ctx = Ctx::new(depth, colors, appearance, Glyphs::detect(&var), socket);
    ctx.prefs = prefs;
    ctx.shift_arrows = shift_arrows;
    ctx.shift_enter = shift_enter;
    ctx.home = var("HOME").filter(|home| home.starts_with('/'));
    ctx.editor = editor::configured(var("EDITOR"));
    Ok(ctx)
}

/// Runs the rebuilt TUI until the owner quits or a signal arrives.
/// Inputs: the unbound engine client, the stop flag set by signals and the
/// editor-running flag the signal listener reads.
/// Output: Ok after a clean exit, or the terminal or connection error.
pub fn run(
    client: Client,
    stopped: &AtomicBool,
    editor_running: Arc<AtomicBool>,
) -> Result<(), TuiError> {
    let (socket, timeout) = client
        .into_unbound_configuration()
        .map_err(|error| TuiError(error.to_string()))?;
    let mut ctx = context(
        |name| std::env::var(name).ok(),
        socket.display().to_string(),
    )?;
    ctx.editor_running = editor_running;
    let mut terminals = Terminals::new(socket.clone(), timeout);
    let client = Client::new(socket, timeout).map_err(|error| TuiError(error.to_string()))?;
    let mut connection = Connection::connect_paired(client).map_err(TuiError)?;
    restore_on_panic();
    let _guard = TerminalGuard::enter()?;
    let _extras = Extras::enter(ctx.shift_enter)?;
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
    drive(
        &mut terminal,
        &mut shell,
        &mut connection,
        &mut terminals,
        stopped,
    )
}

/// How long the loop waits for a key before it looks again: a frame while an agent terminal
/// streams or a fresh engine reply is due, since a reply never wakes the key poll and would
/// otherwise sit out a tick; a tick while anything else waits or the shell animates; otherwise
/// the idle wake. Inputs: whether a terminal streams, whether a fresh reply is due, and
/// whether anything else waits. Output: the wait.
fn wake(streaming: bool, replying: bool, waiting: bool) -> Duration {
    if streaming || replying {
        STREAM_WAKE
    } else if waiting {
        TICK
    } else {
        IDLE_WAKE
    }
}

/// Whether a fresh engine reply is due: a request is in flight, not a failed create kept for
/// Retry, and it went out less than REPLY_WATCH ago, so a long job such as a sign-in falls back
/// to the tick. Inputs: whether a request is in flight, when the last one went out and now.
/// Output: true while the reply is due.
fn replying(in_flight: bool, asked: Option<Instant>, now: Instant) -> bool {
    in_flight && asked.is_some_and(|at| now.saturating_duration_since(at) < REPLY_WATCH)
}

fn drive(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    shell: &mut Shell,
    connection: &mut Connection,
    terminals: &mut Terminals,
    stopped: &AtomicBool,
) -> Result<(), TuiError> {
    let mut dirty = true;
    let mut asked: Option<Instant> = None;
    let mut tick = Instant::now();
    let mut refresh = Instant::now() + REFRESH;
    let mut reconnect = Instant::now() + RECONNECT;
    let mut showing_message = false;
    let mut link = shell.ctx.link.clone();
    let mut shift_enter = shell.ctx.shift_enter;
    while !stopped.load(Ordering::Relaxed) && !shell.quitting() {
        while let Some(event) = connection.poll_owner().map_err(TuiError)? {
            shell.arrive(owner_arrival(event));
            dirty = true;
        }
        if let Some(result) = connection.poll().map_err(TuiError)? {
            shell.arrive(Arrival::Wire(result.map_err(|error| error.to_string())));
            dirty = true;
        }
        for _ in 0..FRAMES_PER_PASS {
            let Some((agent, frame)) = terminals.poll() else {
                break;
            };
            shell.arrive(Arrival::Terminal { agent, frame });
            dirty = true;
        }
        while let Some(request) = shell.ctx.next_terminal_request() {
            terminals.handle(request);
            dirty = true;
        }
        let now = Instant::now();
        if shell.ctx.link != link {
            if shell.ctx.link == Link::Reconnecting {
                reconnect = now + RECONNECT;
            }
            link = shell.ctx.link.clone();
        }
        if shell.ctx.shift_enter != shift_enter {
            shift_enter = shell.ctx.shift_enter;
            switch_keyboard(
                &mut std::io::stdout(),
                shift_enter,
                crossterm::terminal::supports_keyboard_enhancement,
                &KEYBOARD_PUSHED,
            )
            .map_err(|error| TuiError(error.to_string()))?;
        }
        if !connection.busy() {
            if shell.ctx.take_reconnect() || (link == Link::Reconnecting && now >= reconnect) {
                connection.reconnect().map_err(TuiError)?;
                reconnect = now + RECONNECT;
            } else if link == Link::Ready {
                if let Some(request) = shell.ctx.next_request() {
                    asked = Some(now);
                    send(request, shell, connection, terminals)?;
                    dirty = true;
                } else if now >= refresh {
                    shell.tick();
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
        if editor::open_waiting(&mut shell.ctx, &mut std::io::stdout())
            .map_err(|error| TuiError(error.to_string()))?
        {
            let (width, height) =
                crossterm::terminal::size().map_err(|error| TuiError(error.to_string()))?;
            shell.resize(width, height);
            terminal
                .resize(bounded_area(width, height))
                .map_err(|error| TuiError(error.to_string()))?;
            terminal
                .clear()
                .map_err(|error| TuiError(error.to_string()))?;
            dirty = true;
        }
        if dirty {
            terminal
                .draw(|frame| shell.draw(frame))
                .map_err(|error| TuiError(error.to_string()))?;
            dirty = false;
        }
        let waiting = shell.ctx.link == Link::Ready && shell.ctx.queued() > 0;
        let in_flight = connection.busy() && !connection.uncertain();
        let wait = wake(
            terminals.live(),
            replying(in_flight, asked, now),
            shell.busy() || connection.busy() || waiting,
        );
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
        OwnerEvent::TeamGraph { project, result } => Arrival::TeamGraph { project, result },
        OwnerEvent::ProjectFiles {
            project,
            query,
            result,
        } => Arrival::ProjectFiles {
            project,
            query,
            result,
        },
        other => Arrival::Owner(Box::new(other)),
    }
}

/// Turns a request the full request queue refused into the reply the engine worker gives
/// a request that failed, with `owner_busy`, so the screen that asked stops waiting on it
/// and says so; a refused terminal attach comes back as a terminal the engine refused, so
/// the view closes and Enter attaches again. A refused creation Reconcile or Retry stays
/// retained: the worker never saw it and still holds that submission. Reads of the shared
/// profile, pools and projects and terminal keys, sizes and detaches get none: a refused
/// read leaves the data alone, and the message line says the queue is full.
/// Input: the refused request. Output: its failed arrival, or none.
pub(super) fn refused_arrival(request: Request) -> Option<Arrival> {
    use super::client::{ConversationEvent, ConversationJob, CreationEvent, MutationDelivery};
    use super::master_session::{Event as MasterEvent, Job as MasterJob};
    use super::seam::OWNER_BUSY;
    let busy = || OWNER_BUSY.to_owned();
    let event = match request {
        Request::Profile
        | Request::Pools
        | Request::PoolProjects
        | Request::Projects
        | Request::Reconnect
        | Request::TerminalInput { .. }
        | Request::TerminalResize { .. }
        | Request::TerminalDetach { .. } => return None,
        Request::TerminalAttach { agent } | Request::TerminalAttachPane { agent, .. } => {
            return Some(Arrival::Terminal {
                agent,
                frame: crate::terminal::TerminalFrame::Rejected { code: busy() },
            });
        }
        Request::SaveProfile { .. } => return Some(Arrival::ProfileSaved(Err(busy()))),
        Request::EditPool { .. } => return Some(Arrival::PoolEdited(Err(busy()))),
        Request::Wire { .. } => return Some(Arrival::Wire(Err(busy()))),
        Request::TeamGraph { project } => {
            return Some(Arrival::TeamGraph {
                project,
                result: Err(busy()),
            });
        }
        Request::ProjectFiles { project, query } => {
            return Some(Arrival::ProjectFiles {
                project,
                query,
                result: Err(busy()),
            });
        }
        Request::Setup(query) => OwnerEvent::Setup(Box::new(super::client::SetupEvent {
            query,
            result: Err(busy()),
        })),
        Request::Creation(job) => {
            let retained = !matches!(job.delivery, MutationDelivery::Submit);
            OwnerEvent::Creation(Box::new(CreationEvent {
                submission: job.submission,
                operation: None,
                result: Err(busy()),
                retained,
            }))
        }
        Request::Conversation(job) => OwnerEvent::Conversation(Box::new(match *job {
            ConversationJob::Read { context, .. } => ConversationEvent::Read {
                context,
                result: Err(busy()),
            },
            ConversationJob::Mutation { submission, .. } => ConversationEvent::Mutation {
                submission,
                operation: None,
                result: Err(busy()),
                retained: false,
            },
        })),
        Request::Master(job) => OwnerEvent::Master(Box::new(match *job {
            MasterJob::Catalog { context } => MasterEvent::Catalog {
                context,
                result: Err(busy()),
            },
            MasterJob::Read { context, .. } => MasterEvent::Read {
                context,
                result: Err(busy()),
            },
            MasterJob::Mutation { submission, .. } => MasterEvent::Mutation {
                submission,
                operation: None,
                result: Err(busy()),
                retained: false,
            },
        })),
        Request::Settings(job) => OwnerEvent::Settings(Box::new(
            super::client::settings::SettingsEvent::failed(&job, OWNER_BUSY),
        )),
        Request::Intake(job) => OwnerEvent::Intake(Box::new(
            super::client::intake::IntakeEvent::failed(&job, OWNER_BUSY),
        )),
        Request::Changes(query) => OwnerEvent::Changes(Box::new(
            super::client::changes::ChangesEvent::failed(*query, OWNER_BUSY),
        )),
        Request::Process(query) => OwnerEvent::Process(Box::new(
            super::client::process::ProcessEvent::failed(*query, OWNER_BUSY),
        )),
        Request::Cleanup(query) => OwnerEvent::Cleanup(Box::new(
            super::client::cleanup::CleanupEvent::failed(*query, OWNER_BUSY),
        )),
        Request::Learning(query) => OwnerEvent::Learning(Box::new(
            super::client::learning::LearningEvent::failed(*query, OWNER_BUSY),
        )),
        Request::Recovery(query) => {
            OwnerEvent::Recovery(Box::new(super::client::recovery::RecoveryEvent {
                query: *query,
                result: Err(busy()),
            }))
        }
        Request::Roadmap(query) => {
            OwnerEvent::Roadmap(Box::new(super::client::roadmap::RoadmapEvent {
                query: *query,
                result: Err(busy()),
            }))
        }
        Request::Demo(query) => OwnerEvent::Demo(Box::new(super::client::demo::DemoEvent {
            query: *query,
            result: Err(busy()),
        })),
        Request::Questions(query) => {
            OwnerEvent::Questions(Box::new(super::client::questions::QuestionsEvent {
                query: *query,
                result: Err(busy()),
            }))
        }
        Request::Reveal(query) => OwnerEvent::Reveal(Box::new(super::client::ide::RevealEvent {
            query: *query,
            result: Err(busy()),
        })),
    };
    Some(owner_arrival(event))
}

/// Sends one queued request on the idle, connected engine connection, or to
/// the agent terminal on its own connection.
/// Inputs: the request, the shell, the connection and the terminals.
/// Output: Ok, or the connection error that ends the TUI.
fn send(
    request: Request,
    shell: &mut Shell,
    connection: &mut Connection,
    terminals: &mut Terminals,
) -> Result<(), TuiError> {
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
        Request::TeamGraph { project } => connection.team_graph(project),
        Request::ProjectFiles { project, query } => connection.project_files(project, query),
        request @ (Request::TerminalAttach { .. }
        | Request::TerminalAttachPane { .. }
        | Request::TerminalInput { .. }
        | Request::TerminalResize { .. }
        | Request::TerminalDetach { .. }) => {
            terminals.handle(request);
            Ok(())
        }
        Request::Intake(job) => connection.intake(*job),
        Request::Changes(query) => connection.changes(*query),
        Request::Process(query) => connection.process(*query),
        Request::Recovery(query) => connection.recovery(*query),
        Request::Roadmap(query) => connection.roadmap(*query),
        Request::Demo(query) => connection.demo(*query),
        Request::Questions(query) => connection.questions(*query),
        Request::Cleanup(query) => connection.cleanup(*query),
        Request::Learning(query) => connection.learning(*query),
        Request::Reveal(query) => connection.reveal(*query),
        Request::Reconnect => connection.reconnect(),
    };
    result.map_err(TuiError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_engine_reply_the_loop_waits_for_is_read_within_a_frame_not_a_tick() {
        assert!(STREAM_WAKE < TICK);
        assert!(TICK < IDLE_WAKE);
        for waiting in [false, true] {
            assert_eq!(
                wake(false, true, waiting),
                STREAM_WAKE,
                "a fresh reply is due"
            );
            assert_eq!(
                wake(true, false, waiting),
                STREAM_WAKE,
                "a terminal streams"
            );
        }
        assert_eq!(wake(false, false, true), TICK, "something else waits");
        assert_eq!(wake(false, false, false), IDLE_WAKE, "nothing waits");
    }

    #[test]
    fn only_a_request_in_flight_for_under_a_second_counts_as_a_fresh_reply() {
        let asked = Instant::now();
        assert!(replying(true, Some(asked), asked));
        assert!(replying(true, Some(asked), asked + REPLY_WATCH / 2));
        assert!(
            !replying(true, Some(asked), asked + REPLY_WATCH),
            "a long job ticks"
        );
        assert!(
            !replying(false, Some(asked), asked),
            "the reply landed or a create failed"
        );
        assert!(
            !replying(true, None, asked),
            "nothing went out from this loop"
        );
    }

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

    #[test]
    fn the_keyboard_protocol_is_turned_on_only_under_auto_on_a_terminal_that_has_it() {
        let pushed = AtomicBool::new(false);
        let mut out = Vec::new();
        push_keyboard(&mut out, ShiftEnter::Auto, || Ok(true), &pushed).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[>1u");
        assert!(pushed.load(Ordering::SeqCst));
        for answer in [Ok(false), Err(std::io::Error::other("no reply within 2 s"))] {
            let pushed = AtomicBool::new(false);
            let mut out = Vec::new();
            push_keyboard(&mut out, ShiftEnter::Auto, move || answer, &pushed).unwrap();
            assert!(out.is_empty() && !pushed.load(Ordering::SeqCst));
        }
        let pushed = AtomicBool::new(false);
        let mut out = Vec::new();
        push_keyboard(
            &mut out,
            ShiftEnter::Off,
            || panic!("Off never asks the terminal"),
            &pushed,
        )
        .unwrap();
        assert!(out.is_empty() && !pushed.load(Ordering::SeqCst));
    }

    #[test]
    fn every_restore_turns_a_pushed_keyboard_protocol_off_before_the_screen_is_left() {
        let pushed = AtomicBool::new(true);
        let mut out = Vec::new();
        restore_with(&mut out, &pushed).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[<1u"), "{text:?}");
        assert!(text.ends_with("\x1b[?1049l"), "{text:?}");
        assert!(!pushed.load(Ordering::SeqCst));
        let mut again = Vec::new();
        restore_with(&mut again, &pushed).unwrap();
        assert!(
            !String::from_utf8(again).unwrap().contains("\x1b[<"),
            "a pushed protocol is popped once"
        );

        let pushed = AtomicBool::new(true);
        let mut out = Vec::new();
        leave(&mut out, &pushed).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[<1u"), "{text:?}");
        assert!(text.contains("\x1b[?2004l"), "{text:?}");
        assert!(!pushed.load(Ordering::SeqCst));

        let never = AtomicBool::new(false);
        let mut out = Vec::new();
        restore_with(&mut out, &never).unwrap();
        leave(&mut out, &never).unwrap();
        assert!(
            !String::from_utf8(out).unwrap().contains("\x1b[<"),
            "nothing pushed, nothing popped"
        );
    }

    #[test]
    fn a_saved_shift_enter_choice_switches_the_keyboard_protocol_at_once() {
        let pushed = AtomicBool::new(true);
        let mut out = Vec::new();
        switch_keyboard(
            &mut out,
            ShiftEnter::Auto,
            || panic!("a pushed protocol is not asked for again"),
            &pushed,
        )
        .unwrap();
        assert!(out.is_empty() && pushed.load(Ordering::SeqCst));
        switch_keyboard(
            &mut out,
            ShiftEnter::Off,
            || panic!("Off never asks the terminal"),
            &pushed,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[<1u");
        assert!(!pushed.load(Ordering::SeqCst));
        let mut out = Vec::new();
        switch_keyboard(
            &mut out,
            ShiftEnter::Off,
            || panic!("Off never asks the terminal"),
            &pushed,
        )
        .unwrap();
        switch_keyboard(&mut out, ShiftEnter::Auto, || Ok(false), &pushed).unwrap();
        assert!(out.is_empty() && !pushed.load(Ordering::SeqCst));
        switch_keyboard(&mut out, ShiftEnter::Auto, || Ok(true), &pushed).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[>1u");
        assert!(pushed.load(Ordering::SeqCst));
    }
}
