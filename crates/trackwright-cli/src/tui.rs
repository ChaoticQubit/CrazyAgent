use crate::views::text::{cut, pad};
use crate::views::{Client, Key, Page, Theme, Views};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use serde_json::{Value, json};
use std::fmt;
use std::io::{IsTerminal, Write};
use std::path::Path;
use trackwright_core::model::{Actor, OpKind, ProjectKey, codes};
use trackwright_core::ops::dispatch;
use trackwright_core::requests::Request;
use trackwright_core::standalone::Standalone;
use trackwright_core::store::{Store, StoreError};

/// Why the standalone TUI could not start or stopped: one line for the person.
#[derive(Debug)]
pub struct TuiError(pub String);

impl fmt::Display for TuiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TuiError {}

/// The views' client on a standalone database: each call parses the input with the
/// core's bounded request parser and runs `dispatch` with the standalone host in its own
/// transaction, committed only when the reply is ok.
pub struct Local {
    store: Store,
    actor: Actor,
}

impl Local {
    /// Opens a standalone database for one actor. Inputs: the database file and the
    /// self-declared actor. Output: the client, or the store's refusal.
    pub fn open(path: &Path, actor: Actor) -> Result<Self, StoreError> {
        Ok(Self {
            store: Store::open(path)?,
            actor,
        })
    }
}

impl Client for Local {
    fn call(&mut self, op: OpKind, input: Value) -> Value {
        let args = input.to_string();
        let request = match Request::parse(op.as_str(), args.as_bytes()) {
            Ok(request) => request,
            Err(refusal) => return failed(refusal.code, &refusal.message),
        };
        let tx = match self.store.transaction() {
            Ok(tx) => tx,
            Err(error) => return failed(codes::STORE_FAILED, &error.to_string()),
        };
        let reply = dispatch(&tx, &Standalone, &self.actor, request);
        let wire: Value = match serde_json::from_str(reply.json()) {
            Ok(wire) => wire,
            Err(error) => return failed(codes::STORE_FAILED, &error.to_string()),
        };
        assert_eq!(wire["ok"].as_bool(), Some(reply.is_ok()));
        if !reply.is_ok() {
            return wire;
        }
        match tx.commit() {
            Ok(()) => wire,
            Err(error) => failed(codes::STORE_FAILED, &error.to_string()),
        }
    }
}

fn failed(code: &str, message: &str) -> Value {
    json!({ "api": 1, "ok": false, "code": code, "message": message, "next": [] })
}

/// Draws the standalone TUI: the page tabs on the first row, the views in the middle and
/// the key hints on the last row. Inputs: the frame, the views and the theme.
pub fn draw(frame: &mut Frame<'_>, views: &Views, theme: &Theme) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    if area.height < 3 {
        views.render(area, buf, theme);
        return;
    }
    let limit = usize::from(area.width);
    let mut tabs = vec![Span::styled(" trackwright ", theme.heading)];
    for page in Page::ALL {
        let label = if page == views.page() {
            Span::styled(format!(" [{}]", page.label()), theme.accent)
        } else {
            Span::styled(format!("  {} ", page.label()), theme.muted)
        };
        tabs.push(label);
    }
    buf.set_line(area.x, area.y, &Line::from(tabs), area.width);
    let middle = Rect {
        y: area.y + 1,
        height: area.height - 2,
        ..area
    };
    views.render(middle, buf, theme);
    let mut hints: Vec<String> = views
        .hints()
        .iter()
        .map(|hint| format!("{} {}", hint.key, hint.label))
        .collect();
    hints.push("Tab Page".to_owned());
    hints.push("q Quit".to_owned());
    let line = pad(&cut(&format!(" {}", hints.join("  ")), limit), limit);
    buf.set_line(
        area.x,
        area.y + area.height - 1,
        &Line::styled(line, theme.muted),
        area.width,
    );
}

struct Guard;

impl Guard {
    fn enter() -> Result<Self, TuiError> {
        enable_raw_mode().map_err(|error| TuiError(error.to_string()))?;
        let guard = Self;
        execute!(std::io::stdout(), EnterAlternateScreen, Hide)
            .map_err(|error| TuiError(error.to_string()))?;
        Ok(guard)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), Show, LeaveAlternateScreen);
        let _ = std::io::stdout().flush();
    }
}

fn key_of(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc | KeyCode::Backspace => Key::Back,
        KeyCode::Char(character) => Key::Char(character),
        _ => return None,
    })
}

/// Runs `trackwright tui` on a standalone database until the person presses q or
/// Ctrl-C. Inputs: the database file, the self-declared actor and the project. Output:
/// Ok on quit, or why the TUI could not run (no terminal, the store's refusal, a
/// terminal error). The terminal is restored on every exit path.
pub fn run(path: &Path, actor: Actor, project: ProjectKey) -> Result<(), TuiError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(TuiError("trackwright tui needs a terminal".to_owned()));
    }
    let mut client = Local::open(path, actor).map_err(|error| TuiError(error.to_string()))?;
    let theme = if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        Theme::plain()
    } else {
        Theme::color()
    };
    let _guard = Guard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))
        .map_err(|error| TuiError(error.to_string()))?;
    let mut views = Views::new(project);
    let mut page = 0;
    views.open(Page::ALL[page], &mut client);
    loop {
        terminal
            .draw(|frame| draw(frame, &views, &theme))
            .map_err(|error| TuiError(error.to_string()))?;
        let Event::Key(press) = event::read().map_err(|error| TuiError(error.to_string()))? else {
            continue;
        };
        if press.kind == KeyEventKind::Release {
            continue;
        }
        match press.code {
            KeyCode::Char('c') if press.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Char('q') => break,
            KeyCode::Tab => {
                page = (page + 1) % Page::ALL.len();
                views.open(Page::ALL[page], &mut client);
            }
            KeyCode::BackTab => {
                page = (page + Page::ALL.len() - 1) % Page::ALL.len();
                views.open(Page::ALL[page], &mut client);
            }
            code => {
                if let Some(key) = key_of(code) {
                    views.key(key, &mut client);
                }
            }
        }
    }
    Ok(())
}
