use super::intake::{COPY_WARN_BYTES, ask, copy, event};
use super::new_project::{complete, expand, size_label};
use super::{Flow, Place, Screen, open};
use crate::engine::owner::OwnerResult;
use crate::projects_home::{AddMode, PATH_BYTES_MAX};
use crate::tui::client::intake::{IntakeQuery, IntakeView};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::Arrival;
use crate::tui::text::{join, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons, radio};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::path::Path;

const FIELD_WIDTH: usize = 72;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Folder,
    Mode,
    Add,
}

/// How the owner adds the folder: set up where it is (the default, nothing
/// on disk moves), or explicitly moved or copied into the projects folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    Here,
    Into(AddMode),
}

const WAYS: [(Way, &str); 3] = [
    (Way::Here, "Use where it is"),
    (Way::Into(AddMode::Move), "Move"),
    (Way::Into(AddMode::Copy), "Copy"),
];

/// The Existing folder page: sets up a folder on this computer where it is
/// by default, or moves or copies it into the projects folder with the target
/// shown first, a Move confirmed with a second Enter, and a Copy with its size
/// shown first and a warning above 1 GB. The folder then opens its setup.
pub struct AddFolder {
    field: TextField,
    way: Way,
    confirming: bool,
    focus: Focus,
    home: Option<String>,
    size: Option<(String, Load<u64>)>,
    sent: Option<String>,
    problem: Option<String>,
}

impl AddFolder {
    /// An empty Existing folder page set to use the folder where it is.
    /// Takes the context.
    /// Returns the screen; the projects folder path arrives from the engine.
    pub fn new(_ctx: &Ctx) -> Self {
        Self {
            field: TextField::new("Folder", PATH_BYTES_MAX).placeholder("~/Code/my-app"),
            way: Way::Here,
            confirming: false,
            focus: Focus::Folder,
            home: None,
            size: None,
            sent: None,
            problem: None,
        }
    }

    fn user_home(ctx: &Ctx) -> Option<&Path> {
        ctx.home.as_deref().map(Path::new)
    }

    fn source(&self, ctx: &Ctx) -> Result<String, &'static str> {
        let path = expand(self.field.value().trim(), Self::user_home(ctx))
            .ok_or("Type a full folder path, such as ~/Code/my-app.")?;
        let mut path = path.to_string_lossy().into_owned();
        while path.len() > 1 && path.ends_with('/') {
            path.pop();
        }
        if path == "/" {
            return Err("Pick one project's folder.");
        }
        if path.len() > PATH_BYTES_MAX {
            return Err("This path is too long. Pick a shorter one.");
        }
        Ok(path)
    }

    fn name(source: &str) -> String {
        Path::new(source)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned()
    }

    fn target(&self, source: &str) -> Option<String> {
        let name = Self::name(source);
        self.home
            .as_ref()
            .filter(|_| !name.is_empty())
            .map(|home| format!("{home}/{name}"))
    }

    fn step(&mut self, forward: bool) {
        let at = WAYS
            .iter()
            .position(|(way, _)| *way == self.way)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % WAYS.len()
        } else {
            (at + WAYS.len() - 1) % WAYS.len()
        };
        self.way = WAYS[next].0;
        self.confirming = false;
    }

    fn here(&mut self, source: &str, ctx: &mut Ctx) -> Flow {
        let real = match std::fs::canonicalize(source) {
            Ok(real) if real.is_dir() => real,
            _ => {
                self.problem = Some("That folder does not exist.".into());
                self.focus = Focus::Folder;
                return Flow::Handled;
            }
        };
        let path = real.to_string_lossy().into_owned();
        let name = Self::name(&path);
        Flow::Replace(open(
            &Place::Onboarding {
                path,
                name,
                project: None,
            },
            ctx,
        ))
    }

    fn sized(&self, source: &str) -> Option<&Load<u64>> {
        self.size
            .as_ref()
            .filter(|(path, _)| path == source)
            .map(|(_, size)| size)
    }

    fn submit(&mut self, ctx: &mut Ctx) -> Flow {
        let source = match self.source(ctx) {
            Ok(source) => source,
            Err(problem) => {
                self.problem = Some(problem.into());
                self.focus = Focus::Folder;
                return Flow::Handled;
            }
        };
        self.problem = None;
        let mode = match self.way {
            Way::Here => return self.here(&source, ctx),
            Way::Into(mode) => mode,
        };
        if mode == AddMode::Move && !self.confirming {
            self.confirming = true;
            return Flow::Handled;
        }
        if mode == AddMode::Copy {
            match self.sized(&source) {
                Some(Load::Ready(_)) => {}
                Some(Load::Loading) => return Flow::Handled,
                _ => {
                    self.size = Some((source.clone(), Load::Loading));
                    ask(ctx, IntakeQuery::FolderSize { path: source });
                    return Flow::Handled;
                }
            }
        }
        self.confirming = false;
        self.sent = Some(source.clone());
        let generation = ctx.next_generation();
        ask(
            ctx,
            IntakeQuery::AddExisting {
                source,
                mode,
                generation,
            },
        );
        Flow::Handled
    }

    fn added(&self, source: &str, mode: AddMode, ctx: &mut Ctx) -> Flow {
        let name = Self::name(source);
        let verb = match mode {
            AddMode::Move => "Moved",
            AddMode::Copy => "Copied",
        };
        ctx.say(Message::success(
            &format!("{verb} {name}."),
            "Now set it up.",
        ));
        match &self.home {
            Some(home) if !name.is_empty() => Flow::Replace(open(
                &Place::Onboarding {
                    path: format!("{home}/{name}"),
                    name,
                    project: None,
                },
                ctx,
            )),
            _ => Flow::Back,
        }
    }

    fn button(&self, ctx: &Ctx) -> &'static str {
        match self.way {
            Way::Here => "Set up here",
            Way::Into(AddMode::Move) if self.confirming => "Move it now",
            Way::Into(AddMode::Move) => "Move into projects folder",
            Way::Into(AddMode::Copy) => {
                let size = self
                    .source(ctx)
                    .ok()
                    .and_then(|source| match self.sized(&source) {
                        Some(Load::Ready(bytes)) => Some(*bytes),
                        _ => None,
                    });
                match size {
                    Some(bytes) if bytes > COPY_WARN_BYTES => "Copy anyway",
                    Some(_) => "Copy into projects folder",
                    None => "Check size",
                }
            }
        }
    }
}

impl Screen for AddFolder {
    fn crumbs(&self) -> Vec<String> {
        vec!["Add project".into(), "Existing folder".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        match self.focus {
            Focus::Folder => vec![
                hint("Tab", "Complete or next"),
                hint("Enter", self.button(ctx)),
                hint("Esc", "Back"),
            ],
            Focus::Mode => vec![
                hint(&arrows, "How to add it"),
                hint("Enter", self.button(ctx)),
                hint("Esc", "Back"),
            ],
            Focus::Add => vec![
                hint("Enter", self.button(ctx)),
                hint("Shift-Tab", "Previous"),
                hint("Esc", "Back"),
            ],
        }
    }

    fn typing(&self) -> bool {
        self.focus == Focus::Folder
    }

    fn key(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        if self.sent.is_some() {
            return Flow::Handled;
        }
        if self.confirming && *key != Key::Enter {
            self.confirming = false;
        }
        match (self.focus, key) {
            (_, Key::Enter) => self.submit(ctx),
            (Focus::Folder, Key::Tab) => {
                match complete(self.field.value(), Self::user_home(ctx), true) {
                    Some(completed) => self.field.set(&completed),
                    None => self.focus = Focus::Mode,
                }
                Flow::Handled
            }
            (Focus::Mode, Key::Tab) => {
                self.focus = Focus::Add;
                Flow::Handled
            }
            (Focus::Add, Key::BackTab) => {
                self.focus = Focus::Mode;
                Flow::Handled
            }
            (Focus::Mode, Key::BackTab) => {
                self.focus = Focus::Folder;
                Flow::Handled
            }
            (Focus::Mode, Key::Right | Key::Char(' ')) => {
                self.step(true);
                Flow::Handled
            }
            (Focus::Mode, Key::Left) => {
                self.step(false);
                Flow::Handled
            }
            (_, Key::Esc) => Flow::Ignored,
            (Focus::Folder, other) => {
                if self.field.key(other) {
                    self.problem = None;
                }
                Flow::Handled
            }
            _ => Flow::Ignored,
        }
    }

    fn arrive(&mut self, arrival: &Arrival, ctx: &mut Ctx) -> Flow {
        let Some(reply) = event(arrival) else {
            return Flow::Handled;
        };
        match (&reply.query, &reply.result) {
            (IntakeQuery::Home, Ok(IntakeView::Home(home))) => {
                self.home = home.path.clone();
                Flow::Handled
            }
            (IntakeQuery::FolderSize { path }, result) => {
                if let Some((sized, size)) = &mut self.size
                    && sized.as_str() == path.as_str()
                {
                    *size = match result {
                        Ok(IntakeView::FolderSize(bytes)) => Load::Ready(*bytes),
                        Ok(_) => Load::Failed("intake_change_outcome_unknown".into()),
                        Err(code) => Load::Failed(code.clone()),
                    };
                }
                Flow::Handled
            }
            (IntakeQuery::AddExisting { source, mode, .. }, result)
                if self.sent.as_ref() == Some(source) =>
            {
                self.sent = None;
                match result {
                    Ok(IntakeView::Changed(OwnerResult::Applied)) => self.added(source, *mode, ctx),
                    Ok(_) => {
                        self.problem = Some(copy("intake_change_outcome_unknown"));
                        Flow::Handled
                    }
                    Err(code) => {
                        self.problem = Some(copy(code));
                        Flow::Handled
                    }
                }
            }
            _ => Flow::Handled,
        }
    }

    fn shown(&mut self, ctx: &mut Ctx) {
        ask(ctx, IntakeQuery::Home);
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, ctx: &Ctx) -> Option<Position> {
        let theme = &ctx.theme;
        let limit = (area.width as usize)
            .saturating_sub(4)
            .clamp(6, FIELD_WIDTH);
        let mut lines = vec![
            styled("Existing folder", Role::Heading, theme),
            Line::default(),
        ];
        lines.extend(
            wrap(
                "Pick a folder on this computer. CrazyAgent sets it up where it is. Move puts it in your projects folder instead; Copy leaves the original where it is.",
                limit,
            )
            .into_iter()
            .map(Line::raw),
        );
        lines.push(Line::default());
        let top = lines.len() as u16;
        let editing = self.focus == Focus::Folder && self.sent.is_none();
        let (field, cursor) = self.field.lines(limit, editing, theme, ctx.glyphs);
        lines.extend(field);
        lines.push(Line::default());
        let mode_style = if self.focus == Focus::Mode {
            theme.accent
        } else {
            theme.text
        };
        let mut ways = Vec::with_capacity(WAYS.len() * 2);
        for (way, label) in WAYS {
            if !ways.is_empty() {
                ways.push(Span::raw("   "));
            }
            ways.push(Span::styled(radio(self.way == way, label), mode_style));
        }
        lines.push(Line::from(ways));
        let source = self.source(ctx).ok();
        match (
            self.way,
            source.as_deref().and_then(|source| self.target(source)),
        ) {
            (Way::Here, _) if source.is_some() => {
                lines.push(Line::raw("Stays where it is. Nothing is moved or copied."));
            }
            (Way::Into(_), Some(target)) => lines.extend(
                wrap(&format!("Goes to {target}"), limit)
                    .into_iter()
                    .map(Line::raw),
            ),
            _ => {}
        }
        if self.confirming {
            lines.extend(
                wrap(
                    "The folder won't stay at its old place. Press Enter again to move it, or any other key to keep it here.",
                    limit,
                )
                .into_iter()
                .map(|text| styled(text, Role::Warning, theme)),
            );
        }
        if self.way == Way::Into(AddMode::Copy) {
            let size = self
                .source(ctx)
                .ok()
                .and_then(|source| self.sized(&source).cloned());
            match size {
                Some(Load::Ready(bytes)) => {
                    lines.push(Line::raw(format!("Size: {}", size_label(bytes))));
                    if bytes > COPY_WARN_BYTES {
                        lines.extend(
                            wrap(
                                "This folder is over 1 GB. Copying takes a while and uses that much more disk space.",
                                limit,
                            )
                            .into_iter()
                            .map(|text| styled(text, Role::Warning, theme)),
                        );
                    }
                }
                Some(Load::Loading) => lines.push(ctx.loading("Measuring the folder")),
                Some(Load::Failed(code)) => lines.push(styled(copy(&code), Role::Error, theme)),
                _ => lines.push(styled(
                    "CrazyAgent shows the size before it copies.",
                    Role::Muted,
                    theme,
                )),
            }
        }
        lines.push(Line::default());
        if let Some(problem) = &self.problem {
            lines.extend(
                wrap(problem, limit)
                    .into_iter()
                    .map(|text| styled(text, Role::Error, theme)),
            );
            lines.push(Line::default());
        }
        if self.sent.is_some() {
            lines.push(ctx.loading(match self.way {
                Way::Into(AddMode::Copy) => "Copying the folder",
                _ => "Moving the folder",
            }));
        } else {
            lines.push(buttons(
                &[self.button(ctx)],
                (self.focus == Focus::Add).then_some(0),
                theme,
            ));
        }
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| join([Line::raw("  "), line]))
            .collect();
        draw_lines(frame.buffer_mut(), area, &lines);
        cursor
            .filter(|_| editing)
            .map(|at| Position::new(area.x + 2 + at.x, area.y + top + at.y))
    }

    fn busy(&self, _ctx: &Ctx) -> bool {
        self.sent.is_some() || matches!(&self.size, Some((_, Load::Loading)))
    }

    fn draft(&self) -> bool {
        self.sent.is_some()
    }

    fn loss(&self) -> &'static str {
        "The folder is still being added. Quitting may leave it half copied."
    }

    fn place(&self) -> Option<Place> {
        Some(Place::AddFolder)
    }
}
