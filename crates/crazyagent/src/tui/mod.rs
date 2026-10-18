pub mod app;
mod client;
pub mod views;

use crate::engine::Client;
use app::{App, Focus, UiEvent};
use client::Connection;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct TuiError(pub String);

impl std::fmt::Display for TuiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TuiError {}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self, TuiError> {
        enable_raw_mode().map_err(|e| TuiError(e.to_string()))?;
        let guard = Self;
        execute!(std::io::stdout(), EnterAlternateScreen, Hide)
            .map_err(|e| TuiError(e.to_string()))?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), Show, LeaveAlternateScreen);
        let _ = std::io::stdout().flush();
    }
}

pub fn run_tui(client: Client) -> Result<(), TuiError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(TuiError("TUI requires terminal stdin and stdout".into()));
    }
    let signals = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| TuiError(e.to_string()))?;
    let _enter = signals.enter();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| TuiError(e.to_string()))?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|e| TuiError(e.to_string()))?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(|e| TuiError(e.to_string()))?;
    drop(_enter);
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let signal_thread = std::thread::spawn(move || {
        signals.block_on(async {
            loop {
                tokio::select! {
                    _ = terminate.recv() => { stop.store(true, Ordering::Relaxed); break; }
                    _ = interrupt.recv() => { stop.store(true, Ordering::Relaxed); break; }
                    _ = hangup.recv() => { stop.store(true, Ordering::Relaxed); break; }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        if stop.load(Ordering::Relaxed) { break; }
                    }
                }
            }
        });
    });
    let result = (|| {
        let mut connection = Connection::connect_paired(client).map_err(TuiError)?;
        let _guard = TerminalGuard::enter()?;
        let (width, height) = crossterm::terminal::size().map_err(|e| TuiError(e.to_string()))?;
        let area = bounded_area(width, height);
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(std::io::stdout()),
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )
        .map_err(|e| TuiError(e.to_string()))?;
        let mut app = App::new();
        app.reduce(UiEvent::Resize(width, height))
            .map_err(TuiError)?;
        event_loop(&mut terminal, &mut app, &mut connection, &stopped)
    })();
    stopped.store(true, Ordering::Relaxed);
    signal_thread
        .join()
        .map_err(|_| TuiError("TUI signal worker stopped unexpectedly".into()))?;
    result
}

fn bounded_area(width: u16, height: u16) -> Rect {
    Rect::new(0, 0, width.min(300), height.min(120))
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    connection: &mut Connection,
    stopped: &AtomicBool,
) -> Result<(), TuiError> {
    let mut refresh = Instant::now();
    let mut dirty = true;
    let mut snapshot = true;
    let mut owner_refresh = Instant::now();
    loop {
        if stopped.load(Ordering::Relaxed) {
            break;
        }
        if let Some(event) = connection.poll_owner().map_err(TuiError)? {
            app.apply_owner(event).map_err(TuiError)?;
            owner_refresh = if app.owner_continuation() {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(1)
            };
            snapshot = true;
            refresh = Instant::now();
            dirty = true;
        }
        if let Some(result) = connection.poll().map_err(TuiError)? {
            match result {
                Ok(reply) => app.apply_reply(reply).map_err(TuiError)?,
                Err(error) => {
                    app.connected = false;
                    app.notice(&format!(
                        "Disconnected: {error}{}",
                        if connection.uncertain() {
                            "; creation outcome unknown, Ctrl-R retries original request"
                        } else {
                            "; reconnecting to configured endpoint"
                        }
                    ));
                }
            }
            dirty = true;
        }
        if !connection.busy() {
            if let Some(command) = app.take_command() {
                let request_id = connection
                    .start(command, app.revision())
                    .map_err(TuiError)?;
                app.bind_project_request(request_id);
                snapshot = true;
                refresh = Instant::now();
            } else if Instant::now() >= refresh {
                let command = if snapshot {
                    crate::protocol::Command::Snapshot
                } else {
                    app.refresh_command()
                };
                snapshot = !snapshot;
                connection
                    .start(command, app.revision())
                    .map_err(TuiError)?;
                refresh = Instant::now() + Duration::from_secs(1);
            } else if Instant::now() >= owner_refresh {
                if let Some((project, generation, first)) =
                    app.evidence_request().map_err(TuiError)?
                {
                    connection
                        .read_evidence(project, generation, first)
                        .map_err(TuiError)?;
                    dirty = true;
                }
                owner_refresh = Instant::now() + Duration::from_secs(1);
            }
        }
        if dirty {
            terminal
                .draw(|frame| views::draw(frame, app))
                .map_err(|e| TuiError(e.to_string()))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(50)).map_err(|e| TuiError(e.to_string()))? {
            continue;
        }
        match event::read().map_err(|e| TuiError(e.to_string()))? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('q' | 'c'))
                {
                    break;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
                    let result = if connection.uncertain() {
                        connection.retry()
                    } else {
                        connection.reconnect().and_then(|()| app.reconnect_owner())
                    };
                    if let Err(error) = result {
                        app.notice(&error);
                    }
                } else if let Some(event) = key_event(key, app) {
                    let previous_focus = app.focus();
                    if matches!(event, UiEvent::Submit)
                        && app.is_creating_project()
                        && (connection.mutation_pending() || app.has_command())
                    {
                        app.notice(
                            "Request pending; wait, or Ctrl-R retry if creation outcome is unknown",
                        );
                    } else if let Err(error) = app.reduce(event) {
                        app.notice(&error);
                    }
                    if previous_focus != Focus::Evidence && app.focus() == Focus::Evidence {
                        owner_refresh = Instant::now();
                    }
                    refresh = Instant::now();
                }
                dirty = true;
            }
            Event::Resize(width, height) => {
                app.reduce(UiEvent::Resize(width, height))
                    .map_err(TuiError)?;
                terminal
                    .resize(bounded_area(width, height))
                    .map_err(|e| TuiError(e.to_string()))?;
                dirty = true;
            }
            _ => {}
        }
    }
    Ok(())
}

fn key_event(key: KeyEvent, app: &App) -> Option<UiEvent> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }
    Some(match key.code {
        KeyCode::Tab => UiEvent::FocusNext,
        KeyCode::BackTab => UiEvent::FocusPrevious,
        KeyCode::Up => UiEvent::SelectionPrevious,
        KeyCode::Down => UiEvent::SelectionNext,
        KeyCode::PageUp => UiEvent::Scroll(-5),
        KeyCode::PageDown => UiEvent::Scroll(5),
        KeyCode::Enter => UiEvent::Submit,
        KeyCode::Esc => UiEvent::Cancel,
        KeyCode::Backspace => UiEvent::Backspace,
        KeyCode::Char('n') if app.focus() == Focus::Projects && !app.is_creating_project() => {
            UiEvent::BeginProject
        }
        KeyCode::Char(']') if !app.is_creating_project() && app.focus() != Focus::Conversation => {
            UiEvent::TaskNext
        }
        KeyCode::Char(character) => UiEvent::Character(character),
        _ => return None,
    })
}
