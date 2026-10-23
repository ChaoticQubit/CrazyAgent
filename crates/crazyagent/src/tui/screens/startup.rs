use super::{Flow, Place, Screen, open};
use crate::tui::ctx::{Ctx, Link, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::styled;
use crate::tui::theme::Role;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};

/// The first screen: connects to the engine, then reads the profile and
/// replaces itself with Welcome on first run or Home afterwards.
/// Input: connection and profile arrivals. Output: "Connecting" with a spinner,
/// or the full "Engine not running" screen with r and c.
#[derive(Default)]
pub struct Startup;

impl Startup {
    /// A new startup screen.
    /// Input: none. Output: the screen.
    pub fn new() -> Self {
        Self
    }

    fn down(ctx: &Ctx) -> bool {
        matches!(ctx.link, Link::Down(_))
    }
}

/// Shortens a path under the home folder to start with `~`.
/// Inputs: the path and the home folder. Output: the display path.
pub fn tilde(path: &str, home: Option<&str>) -> String {
    match home {
        Some(home) if !home.is_empty() && path.starts_with(home) && path[home.len()..].starts_with('/') => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}

/// Support details copied with `c`: the socket path and versions, never secrets.
/// Input: the context. Output: plain text lines.
pub fn support_details(ctx: &Ctx) -> String {
    let problem = match &ctx.link {
        Link::Down(problem) => problem.as_str(),
        _ => "none",
    };
    format!(
        "CrazyAgent {}\nEngine socket: {}\nProblem: {problem}\nTerminal: {}x{}\n",
        env!("CARGO_PKG_VERSION"),
        ctx.socket,
        ctx.width,
        ctx.height
    )
}

fn copy(text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = std::process::Command::new("/usr/bin/pbcopy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    child
        .stdin
        .take()
        .ok_or("clipboard_unavailable")?
        .write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    let status = child.wait().map_err(|error| error.to_string())?;
    status.success().then_some(()).ok_or_else(|| "clipboard_failed".into())
}

impl Screen for Startup {
    fn crumbs(&self) -> Vec<String> {
        Vec::new()
    }

    fn status(&self, ctx: &Ctx) -> Line<'static> {
        if Self::down(ctx) {
            styled("Engine not running", Role::Error, &ctx.theme)
        } else {
            Line::default()
        }
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if Self::down(ctx) {
            vec![
                hint("r", "Try again"),
                hint("c", "Copy details for support"),
                hint("Ctrl-Q", "Quit"),
            ]
        } else {
            vec![hint("Ctrl-Q", "Quit")]
        }
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if !Self::down(ctx) {
            return Flow::Handled;
        }
        match key {
            Key::Char('r') => {
                ctx.link = Link::Connecting;
                ctx.request(Request::Reconnect);
                Flow::Handled
            }
            Key::Char('c') => {
                match copy(&support_details(ctx)) {
                    Ok(()) => ctx.say(Message::success("Copied details for support.", "")),
                    Err(_) => ctx.say(Message::error(
                        "Could not copy.",
                        "Select the socket path with your mouse instead.",
                    )),
                }
                Flow::Handled
            }
            _ => Flow::Handled,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        match arrival {
            Arrival::Connected => {
                ctx.request(Request::Profile);
                Flow::Handled
            }
            Arrival::Profile(Ok(Some(_))) => Flow::Replace(open(&Place::Home, ctx)),
            Arrival::Profile(Ok(None)) => Flow::Replace(open(&Place::Welcome, ctx)),
            Arrival::Profile(Err(problem)) => {
                ctx.store.profile = Load::Failed(problem.clone());
                ctx.say(Message::error(
                    "Could not read your profile.",
                    "Press r to try again.",
                ));
                ctx.link = Link::Down(problem.clone());
                Flow::Handled
            }
            _ => Flow::Handled,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let lines = if Self::down(ctx) {
            vec![
                Line::default(),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled("CrazyAgent's engine isn't running", theme.heading),
                ]),
                Line::default(),
                Line::raw("  Projects, accounts and agents live in the engine."),
                Line::raw("  Start it, then press r to try again."),
                Line::default(),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled("Looked for it at", theme.muted),
                    Span::raw(format!("  {}", tilde(&ctx.socket, ctx.home.as_deref()))),
                ]),
            ]
        } else {
            vec![
                Line::default(),
                Line::from(vec![
                    Span::raw("  Connecting to CrazyAgent "),
                    Span::styled(ctx.spinner(), theme.running),
                ]),
            ]
        };
        draw_lines(frame.buffer_mut(), area, &lines);
        None
    }

    fn busy(&self, ctx: &Ctx) -> bool {
        !Self::down(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_paths_under_home_start_with_tilde() {
        assert_eq!(
            tilde("/Users/mira/.crazyagent/engine.sock", Some("/Users/mira")),
            "~/.crazyagent/engine.sock"
        );
        assert_eq!(tilde("/Users/miranda/e.sock", Some("/Users/mira")), "/Users/miranda/e.sock");
        assert_eq!(tilde("/tmp/e.sock", None), "/tmp/e.sock");
    }
}
