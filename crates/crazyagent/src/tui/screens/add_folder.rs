use super::intake::{AddWarning, COPY_WARN_BYTES, ask, copy, event};
use super::new_project::{complete, expand, size_label};
use super::{Flow, Place, Screen, open};
use crate::engine::owner::OwnerResult;
use crate::projects_home::icloud::Progress;
use crate::projects_home::{AddMode, AddOverride, GitLink, Measured, PATH_BYTES_MAX};
use crate::tui::client::intake::{IntakeJob, IntakeQuery, IntakeView};
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Message, draw_lines, hint};
use crate::tui::keys::Key;
use crate::tui::seam::{Arrival, Request};
use crate::tui::text::{join, styled, wrap};
use crate::tui::theme::Role;
use crate::tui::widgets::{TextField, buttons, radio};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const FIELD_WIDTH: usize = 72;
const WORKTREE_PATHS_MAX: usize = 16;
const SOFT_SIZE_CODES: [&str; 3] = [
    "folder_size_timeout",
    "folder_size_too_large",
    "folder_size_unreadable",
];
const ICLOUD_CHOICES: [&str; 3] = ["Download and move", "Download and copy", "Cancel"];
const ICLOUD_STOPPED: &str = "The download stopped. Nothing was moved or copied.";

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

/// Something about a folder the owner is asked about before a Move or Copy goes ahead,
/// in the order the question names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Issue {
    Large,
    TooMany,
    Unreadable,
    Worktrees,
    LinkedWorktree,
    Submodule,
    GitDataUnread,
    LinkedCheckout,
    NoGit,
}

/// What a Move or Copy of the typed folder meets, read as the engine reads it: the git link
/// a Move breaks, whether a Copy keeps its own git data, whether it has git, whether it shares
/// the projects folder's disk, its linked worktree folders and its size or the soft reason it
/// could not be measured.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Facts {
    git_link: GitLink,
    copyable: bool,
    git: bool,
    same_volume: Option<bool>,
    worktree_paths: Vec<String>,
    bytes: Option<u64>,
    size_problem: Option<String>,
}

/// One warning block of the question: whether it is about losing data, its lines and the
/// folders it names.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Block {
    loss: bool,
    lines: Vec<String>,
    paths: Vec<String>,
}

/// The open "Move anyway?" or "Copy anyway?" question: the folder and way it is about, the
/// overrides yes sends, the warnings and the selected answer (0 Use where it is, 1 anyway).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Question {
    source: String,
    mode: AddMode,
    overrides: Vec<AddOverride>,
    blocks: Vec<Block>,
    choice: usize,
}

/// The open iCloud question for a folder whose files are not all on this Mac: the folder, how
/// many of its files the read found only in iCloud (none when only the engine's refusal said
/// so), the selected answer (Download and move, Download and copy, Cancel) and, once the owner
/// chose, the running download.
#[derive(Clone, Debug)]
struct Icloud {
    source: String,
    placeholders: Option<u64>,
    choice: usize,
    download: Option<Downloading>,
}

/// One running iCloud download: the way the folder is added after it, the flag Esc sets to
/// stop it and the last progress the engine reported.
#[derive(Clone, Debug)]
struct Downloading {
    mode: AddMode,
    cancel: Arc<AtomicBool>,
    progress: Option<Progress>,
}

/// The Existing folder page: sets up a folder on this computer where it is
/// by default, or moves or copies it into the projects folder with the target
/// shown first, a Move confirmed with a second Enter, and a Copy with its size
/// shown first, a warning above 1 GB and the linked git worktrees it leaves with
/// the original. A Move or Copy with issues asks "Move anyway?" first, data loss
/// first: yes sends an override for exactly those issues, Use where it is sets the
/// folder up in place. A folder with files only in iCloud asks first whether to download
/// them and then move or copy, shows the download's progress, stops it on Esc, and goes on
/// with the chosen way once every file is on this Mac. The folder then opens its setup.
pub struct AddFolder {
    field: TextField,
    way: Way,
    confirming: bool,
    focus: Focus,
    home: Option<String>,
    size: Option<(String, Load<Measured>)>,
    worktrees: usize,
    question: Option<Question>,
    icloud: Option<Icloud>,
    downloaded: Option<String>,
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
            worktrees: 0,
            question: None,
            icloud: None,
            downloaded: None,
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

    /// Reads what a Move or Copy of `source` meets. Takes the folder. Returns the facts.
    fn facts(&self, source: &str) -> Facts {
        let folder = Path::new(source);
        let (bytes, size_problem) = match self.sized(source) {
            Some(Load::Ready(measured)) => (Some(measured.bytes), None),
            Some(Load::Failed(code)) if SOFT_SIZE_CODES.contains(&code.as_str()) => {
                (None, Some(code.clone()))
            }
            _ => (None, None),
        };
        let worktree_paths: Vec<String> =
            crate::projects_home::linked_worktree_paths(folder, WORKTREE_PATHS_MAX)
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
        assert!(worktree_paths.len() <= WORKTREE_PATHS_MAX);
        Facts {
            git_link: crate::projects_home::git_link(folder),
            copyable: !crate::projects_home::linked_checkout(folder),
            git: std::fs::symlink_metadata(folder.join(".git")).is_ok(),
            same_volume: self
                .home
                .as_deref()
                .and_then(|home| crate::projects_home::same_volume(folder, Path::new(home))),
            worktree_paths,
            bytes,
            size_problem,
        }
    }

    /// Opens the question for `source` when its facts or the engine refusal raise an issue.
    /// Takes the folder, the way and the refusal code, if any. Returns true when it opened.
    fn ask_anyway(&mut self, source: &str, mode: AddMode, refused: Option<&str>) -> bool {
        let facts = self.facts(source);
        let issues = issues(mode, &facts, refused);
        if issues.is_empty() {
            return false;
        }
        self.question = Some(Question {
            source: source.to_owned(),
            mode,
            overrides: overrides(mode, &issues, &facts),
            blocks: warnings(mode, &issues, &facts),
            choice: 0,
        });
        self.confirming = false;
        true
    }

    /// The ways offered: all three, since what a folder's read finds is asked about first.
    fn ways(&self) -> Vec<(Way, &'static str)> {
        WAYS.to_vec()
    }

    fn step(&mut self, forward: bool) {
        let ways = self.ways();
        let at = ways
            .iter()
            .position(|(way, _)| *way == self.way)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % ways.len()
        } else {
            (at + ways.len() - 1) % ways.len()
        };
        self.way = ways[next].0;
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

    fn sized(&self, source: &str) -> Option<&Load<Measured>> {
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
        match self.sized(&source) {
            Some(Load::Ready(_)) => {}
            Some(Load::Failed(code)) if SOFT_SIZE_CODES.contains(&code.as_str()) => {}
            Some(Load::Loading) => return Flow::Handled,
            _ => {
                self.size = Some((source.clone(), Load::Loading));
                ask(ctx, IntakeQuery::FolderSize { path: source });
                return Flow::Handled;
            }
        }
        self.proceed(source, mode, ctx)
    }

    /// Goes on with a measured folder: the iCloud question while some of its files are only
    /// in iCloud, then the question for its issues, then the Move confirmation, then the add.
    /// Takes the folder, the way and the context. Returns the flow.
    fn proceed(&mut self, source: String, mode: AddMode, ctx: &mut Ctx) -> Flow {
        let placeholders = match self.sized(&source) {
            Some(Load::Ready(measured)) if measured.placeholders > 0 => Some(measured.placeholders),
            _ => None,
        };
        let fetched = self.downloaded.as_deref() == Some(source.as_str());
        if !self.confirming && placeholders.is_some() && !fetched {
            self.ask_icloud(&source, mode, placeholders);
            return Flow::Handled;
        }
        if !self.confirming && self.ask_anyway(&source, mode, None) {
            return Flow::Handled;
        }
        if mode == AddMode::Move && !self.confirming {
            self.confirming = true;
            return Flow::Handled;
        }
        self.send(source, mode, Vec::new(), ctx)
    }

    /// Opens the iCloud question for `source` with the owner's way selected. Takes the folder,
    /// the way and how many files the read found only in iCloud, if it counted them.
    fn ask_icloud(&mut self, source: &str, mode: AddMode, placeholders: Option<u64>) {
        assert!(placeholders != Some(0));
        assert!(self.downloaded.as_deref() != Some(source));
        self.icloud = Some(Icloud {
            source: source.to_owned(),
            placeholders,
            choice: match mode {
                AddMode::Move => 0,
                AddMode::Copy => 1,
            },
            download: None,
        });
        self.question = None;
        self.confirming = false;
    }

    /// Starts the download the owner chose; the folder is moved or copied only after it.
    /// Takes the way and the context. Returns Handled; progress and the end arrive later.
    fn download(&mut self, mode: AddMode, ctx: &mut Ctx) -> Flow {
        let Some(icloud) = &mut self.icloud else {
            return Flow::Ignored;
        };
        assert!(icloud.download.is_none());
        let cancel = Arc::new(AtomicBool::new(false));
        icloud.download = Some(Downloading {
            mode,
            cancel: Arc::clone(&cancel),
            progress: None,
        });
        let path = icloud.source.clone();
        self.way = Way::Into(mode);
        self.problem = None;
        ctx.request(Request::Intake(Box::new(IntakeJob::IcloudDownload {
            path,
            cancel,
        })));
        Flow::Handled
    }

    /// Takes one key while the iCloud question is open: arrows and Tab pick the answer,
    /// Enter gives it and Esc closes it. While the download runs, Esc stops it and every
    /// other key waits. Takes the key and the context. Returns the flow.
    fn answer_icloud(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(icloud) = &mut self.icloud else {
            return Flow::Ignored;
        };
        let count = ICLOUD_CHOICES.len();
        assert!(icloud.choice < count);
        if let Some(download) = &icloud.download {
            if *key == Key::Esc {
                download.cancel.store(true, Ordering::Relaxed);
            }
            return Flow::Handled;
        }
        match key {
            Key::Right | Key::Tab => icloud.choice = (icloud.choice + 1) % count,
            Key::Left | Key::BackTab => icloud.choice = (icloud.choice + count - 1) % count,
            Key::Esc => self.icloud = None,
            Key::Enter => {
                return match icloud.choice {
                    0 => self.download(AddMode::Move, ctx),
                    1 => self.download(AddMode::Copy, ctx),
                    _ => {
                        self.icloud = None;
                        Flow::Handled
                    }
                };
            }
            _ => {}
        }
        Flow::Handled
    }

    /// Applies one download reply: progress updates the line, the end goes on with the chosen
    /// way, and a failure or the owner's stop closes the question with its sentence. Takes the
    /// folder the reply is for, the result and the context. Returns the flow.
    fn download_reply(
        &mut self,
        path: &str,
        result: &Result<IntakeView, String>,
        ctx: &mut Ctx,
    ) -> Flow {
        let Some(icloud) = self.icloud.as_mut().filter(|icloud| icloud.source == path) else {
            return Flow::Handled;
        };
        let Some(download) = &mut icloud.download else {
            return Flow::Handled;
        };
        let (mode, stopped) = (download.mode, download.cancel.load(Ordering::Relaxed));
        match result {
            Ok(IntakeView::IcloudProgress(progress)) => {
                assert!(progress.done <= progress.total);
                download.progress = Some(*progress);
                return Flow::Handled;
            }
            Ok(IntakeView::IcloudDownloaded(_)) => {
                self.icloud = None;
                self.downloaded = Some(path.to_owned());
                return self.proceed(path.to_owned(), mode, ctx);
            }
            Err(code) if stopped && code == "home_icloud_not_downloaded" => {
                self.problem = Some(ICLOUD_STOPPED.into());
            }
            Err(code) => self.problem = Some(copy(code)),
            Ok(_) => self.problem = Some(copy("icloud_outcome_unknown")),
        }
        self.icloud = None;
        Flow::Handled
    }

    /// Sends the add with the overrides the owner agreed to. Takes the folder, the way, the
    /// overrides and the context. Returns Handled; the reply arrives later.
    fn send(
        &mut self,
        source: String,
        mode: AddMode,
        overrides: Vec<AddOverride>,
        ctx: &mut Ctx,
    ) -> Flow {
        self.confirming = false;
        self.question = None;
        self.sent = Some(source.clone());
        let generation = ctx.next_generation();
        ask(
            ctx,
            IntakeQuery::AddExisting {
                source,
                mode,
                overrides,
                generation,
            },
        );
        Flow::Handled
    }

    /// Takes one key while the question is open: arrows and Tab pick the answer, Enter
    /// gives it and Esc closes the question without either. Takes the key and the context.
    /// Returns the flow.
    fn answer(&mut self, key: &Key, ctx: &mut Ctx) -> Flow {
        let Some(question) = &mut self.question else {
            return Flow::Ignored;
        };
        match key {
            Key::Left | Key::Right | Key::Tab | Key::BackTab => {
                question.choice = 1 - question.choice;
                Flow::Handled
            }
            Key::Esc => {
                self.question = None;
                Flow::Handled
            }
            Key::Enter if question.choice == 0 => {
                let source = question.source.clone();
                self.question = None;
                self.way = Way::Here;
                self.here(&source, ctx)
            }
            Key::Enter => {
                let Question {
                    source,
                    mode,
                    overrides,
                    ..
                } = question.clone();
                self.send(source, mode, overrides, ctx)
            }
            _ => Flow::Handled,
        }
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
        if let Some(icloud) = &self.icloud {
            return match icloud.download {
                Some(_) => "Wait",
                None => ICLOUD_CHOICES[icloud.choice],
            };
        }
        if let Some(question) = &self.question {
            return match (question.choice, question.mode) {
                (0, _) => "Use where it is",
                (_, AddMode::Move) => "Move anyway",
                (_, AddMode::Copy) => "Copy anyway",
            };
        }
        let Way::Into(mode) = self.way else {
            return "Set up here";
        };
        let size = self
            .source(ctx)
            .ok()
            .and_then(|source| self.sized(&source).cloned());
        let measured = match &size {
            Some(Load::Ready(_)) => true,
            Some(Load::Failed(code)) => SOFT_SIZE_CODES.contains(&code.as_str()),
            _ => false,
        };
        match mode {
            _ if !measured => "Check size",
            AddMode::Move if self.confirming => "Move it now",
            AddMode::Move => "Move into projects folder",
            AddMode::Copy if matches!(size, Some(Load::Ready(measured)) if measured.bytes > COPY_WARN_BYTES) => {
                "Copy anyway"
            }
            AddMode::Copy => "Copy into projects folder",
        }
    }
}

impl Screen for AddFolder {
    fn crumbs(&self) -> Vec<String> {
        vec!["Add project".into(), "Existing folder".into()]
    }

    fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let arrows = format!("{}{}", ctx.glyphs.left, ctx.glyphs.right);
        match self.icloud.as_ref().map(|icloud| icloud.download.is_some()) {
            Some(true) => return vec![hint("Esc", "Stop the download")],
            Some(false) => {
                return vec![
                    hint(&arrows, "Choose"),
                    hint("Enter", self.button(ctx)),
                    hint("Esc", "Close the question"),
                ];
            }
            None => {}
        }
        if self.question.is_some() {
            return vec![
                hint(&arrows, "Choose"),
                hint("Enter", self.button(ctx)),
                hint("Esc", "Close the question"),
            ];
        }
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
        if self.icloud.is_some() {
            return self.answer_icloud(key, ctx);
        }
        if self.question.is_some() {
            return self.answer(key, ctx);
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
                    self.worktrees = crate::projects_home::linked_worktrees(Path::new(path));
                    *size = match result {
                        Ok(IntakeView::FolderSize(measured)) => Load::Ready(*measured),
                        Ok(_) => Load::Failed("intake_change_outcome_unknown".into()),
                        Err(code) => Load::Failed(code.clone()),
                    };
                }
                Flow::Handled
            }
            (
                IntakeQuery::AddExisting {
                    source,
                    mode,
                    overrides,
                    ..
                },
                result,
            ) if self.sent.as_ref() == Some(source) => {
                self.sent = None;
                match result {
                    Ok(IntakeView::Changed(OwnerResult::Applied)) => self.added(source, *mode, ctx),
                    Ok(_) => {
                        self.problem = Some(copy("intake_change_outcome_unknown"));
                        Flow::Handled
                    }
                    Err(code) if asks_again(code, overrides) => {
                        self.ask_anyway(source, *mode, Some(code));
                        Flow::Handled
                    }
                    Err(code)
                        if code == "home_icloud_not_downloaded"
                            && self.downloaded.as_ref() != Some(source) =>
                    {
                        self.ask_icloud(source, *mode, None);
                        Flow::Handled
                    }
                    Err(code) => {
                        self.problem = Some(copy(code));
                        Flow::Handled
                    }
                }
            }
            (IntakeQuery::IcloudDownload { path }, result) => {
                self.download_reply(path, result, ctx)
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
        for (way, label) in self.ways() {
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
            let same = source
                .as_deref()
                .zip(self.home.as_deref())
                .and_then(|(source, home)| {
                    crate::projects_home::same_volume(Path::new(source), Path::new(home))
                });
            let mut warnings = vec![(AddWarning::CloseApps, true)];
            if same == Some(false) {
                warnings.push((AddWarning::AcrossDisks, true));
            }
            warnings.push((AddWarning::OldPlace, false));
            if same == Some(true) {
                warnings.push((AddWarning::SameDisk, false));
            }
            for (warning, loss) in warnings {
                lines.extend(wrap(warning.text(), limit).into_iter().map(|text| {
                    if loss {
                        styled(text, Role::Warning, theme)
                    } else {
                        Line::raw(text)
                    }
                }));
            }
            lines.extend(
                wrap(
                    "The folder won't stay at its old place. Press Enter again to move it, or any other key to keep it here.",
                    limit,
                )
                .into_iter()
                .map(|text| styled(text, Role::Warning, theme)),
            );
        }
        if let Way::Into(mode) = self.way {
            let copying = mode == AddMode::Copy;
            let size = self
                .source(ctx)
                .ok()
                .and_then(|source| self.sized(&source).cloned());
            match size {
                Some(Load::Ready(Measured { bytes, .. })) => {
                    lines.push(Line::raw(format!("Size: {}", size_label(bytes))));
                    if copying && self.worktrees > 0 {
                        lines.extend(
                            wrap(&worktrees_line(self.worktrees), limit)
                                .into_iter()
                                .map(Line::raw),
                        );
                    }
                    if copying && bytes > COPY_WARN_BYTES {
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
                    if copying {
                        "CrazyAgent shows the size before it copies."
                    } else {
                        "CrazyAgent shows the size before it moves."
                    },
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
        if let Some(icloud) = &self.icloud {
            icloud_lines(icloud, limit, ctx, &mut lines);
        } else if let Some(question) = &self.question {
            question_lines(question, limit, theme, &mut lines);
        } else if self.sent.is_some() {
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
        self.sent.is_some()
            || matches!(&self.size, Some((_, Load::Loading)))
            || self
                .icloud
                .as_ref()
                .is_some_and(|icloud| icloud.download.is_some())
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

/// The issues the question names for `mode`: what the folder's facts show, plus the issue
/// of a soft engine refusal. A Move on the same disk is a rename, so size matters only to a
/// Copy or a Move that may cross disks. Takes the way, the facts and the refusal code, if any.
/// Returns the issues in their fixed order.
fn issues(mode: AddMode, facts: &Facts, refused: Option<&str>) -> Vec<Issue> {
    let mut found = Vec::with_capacity(6);
    match refused {
        Some("home_move_worktrees") => found.push(move_link_issue(facts.git_link)),
        Some("home_copy_linked_checkout") => found.push(Issue::LinkedCheckout),
        Some("home_copy_too_large") => found.push(Issue::TooMany),
        _ => {}
    }
    if mode == AddMode::Move && facts.git_link != GitLink::None {
        found.push(move_link_issue(facts.git_link));
    }
    if mode == AddMode::Copy && !facts.copyable {
        found.push(Issue::LinkedCheckout);
    }
    if !facts.git {
        found.push(Issue::NoGit);
    }
    let problem = facts.size_problem.as_deref();
    if problem == Some("folder_size_unreadable") {
        found.push(Issue::Unreadable);
    }
    let copies = mode == AddMode::Copy || facts.same_volume != Some(true);
    if copies
        && matches!(
            problem,
            Some("folder_size_timeout" | "folder_size_too_large")
        )
    {
        found.push(Issue::Large);
    }
    if copies && problem == Some("folder_size_too_large") {
        found.push(Issue::TooMany);
    }
    if copies && facts.bytes.is_some_and(|bytes| bytes > COPY_WARN_BYTES) {
        found.push(Issue::Large);
    }
    found.sort();
    found.dedup();
    found
}

/// The issue a Move of a folder with git links outside it is asked about: the link the read
/// named, or linked worktrees for an engine refusal the read did not explain. Takes the link.
/// Returns the issue.
fn move_link_issue(link: GitLink) -> Issue {
    match link {
        GitLink::LinkedWorktree => Issue::LinkedWorktree,
        GitLink::Submodule => Issue::Submodule,
        GitLink::Unreadable => Issue::GitDataUnread,
        GitLink::None | GitLink::Worktrees => Issue::Worktrees,
    }
}

/// The overrides yes sends: one for each issue the engine itself refuses. Takes the way,
/// the issues and the facts. Returns them in the engine's order.
fn overrides(mode: AddMode, issues: &[Issue], facts: &Facts) -> Vec<AddOverride> {
    let unmeasured = facts.size_problem.as_deref() == Some("folder_size_timeout");
    let mut overrides = Vec::with_capacity(crate::projects_home::ADD_OVERRIDES_MAX);
    let links = [
        Issue::Worktrees,
        Issue::LinkedWorktree,
        Issue::Submodule,
        Issue::GitDataUnread,
    ];
    if mode == AddMode::Move && issues.iter().any(|issue| links.contains(issue)) {
        overrides.push(AddOverride::Worktrees);
    }
    if mode == AddMode::Copy && issues.contains(&Issue::LinkedCheckout) {
        overrides.push(AddOverride::LinkedCheckout);
    }
    if issues.contains(&Issue::TooMany) || (issues.contains(&Issue::Large) && unmeasured) {
        overrides.push(AddOverride::TooLarge);
    }
    assert!(overrides.len() <= crate::projects_home::ADD_OVERRIDES_MAX);
    overrides
}

/// The warnings the question shows: data loss blocks first (a copy sharing another checkout's
/// git data, a moved worktree whose git data its repository can prune, apps still using the
/// folder, a copy across disks, a very large or deep folder, unreadable files), then the other
/// effects (the old place, git worktrees with their folders, the folder being a submodule, git
/// data not read, no git, a quick rename). Takes the way, the issues and the facts. Returns the
/// blocks in that order.
fn warnings(mode: AddMode, issues: &[Issue], facts: &Facts) -> Vec<Block> {
    let moving = mode == AddMode::Move;
    let copy_line = if moving {
        AddWarning::AcrossDisks
    } else {
        AddWarning::CopyAll
    };
    let block = |loss: bool, lines: Vec<String>| Block {
        loss,
        lines,
        paths: Vec::new(),
    };
    let text = |warning: AddWarning| warning.text().to_owned();
    let mut blocks = Vec::with_capacity(9);
    if issues.contains(&Issue::LinkedCheckout) {
        blocks.push(block(true, vec![text(AddWarning::LinkedCheckout)]));
    }
    if issues.contains(&Issue::LinkedWorktree) {
        blocks.push(block(true, vec![text(AddWarning::LinkedWorktree)]));
    }
    if moving {
        blocks.push(block(true, vec![text(AddWarning::CloseApps)]));
    }
    let mut copying = false;
    if issues.contains(&Issue::Large) {
        let size = facts.bytes.map_or_else(
            || text(AddWarning::Unmeasured),
            |bytes| format!("It is about {}.", size_label(bytes)),
        );
        blocks.push(block(
            true,
            vec![text(AddWarning::Large), size, text(copy_line)],
        ));
        copying = true;
    }
    if issues.contains(&Issue::TooMany) {
        let mut lines = Vec::with_capacity(2);
        if !copying {
            lines.push(text(copy_line));
        }
        lines.push(text(AddWarning::TooMany));
        blocks.push(block(true, lines));
        copying = true;
    }
    if moving && !copying && facts.same_volume == Some(false) {
        blocks.push(block(true, vec![text(AddWarning::AcrossDisks)]));
        copying = true;
    }
    if issues.contains(&Issue::Unreadable) {
        let unreadable = if moving {
            AddWarning::Unreadable
        } else {
            AddWarning::UnreadableCopy
        };
        blocks.push(block(true, vec![text(unreadable)]));
    }
    if moving {
        blocks.push(block(false, vec![text(AddWarning::OldPlace)]));
    }
    if issues.contains(&Issue::Worktrees) {
        blocks.push(Block {
            loss: false,
            lines: vec![text(AddWarning::Worktrees)],
            paths: facts.worktree_paths.clone(),
        });
    }
    for (issue, warning) in [
        (Issue::Submodule, AddWarning::Submodule),
        (Issue::GitDataUnread, AddWarning::GitDataUnread),
    ] {
        if issues.contains(&issue) {
            blocks.push(block(false, vec![text(warning)]));
        }
    }
    if issues.contains(&Issue::NoGit) {
        blocks.push(block(false, vec![text(AddWarning::NoGit)]));
    }
    if moving && !copying && facts.same_volume == Some(true) {
        blocks.push(block(false, vec![text(AddWarning::SameDisk)]));
    }
    let first_effect = blocks
        .iter()
        .position(|block| !block.loss)
        .unwrap_or(blocks.len());
    assert!(blocks[first_effect..].iter().all(|block| !block.loss));
    blocks
}

/// Whether an engine refusal turns into the question for that issue: only a soft refusal
/// whose override was not already sent. Takes the code and the overrides sent. Returns true
/// to ask.
fn asks_again(code: &str, sent: &[AddOverride]) -> bool {
    match code {
        "home_move_worktrees" => !sent.contains(&AddOverride::Worktrees),
        "home_copy_linked_checkout" => !sent.contains(&AddOverride::LinkedCheckout),
        "home_copy_too_large" => !sent.contains(&AddOverride::TooLarge),
        _ => false,
    }
}

/// Draws the question: its title, each warning block (data loss as warnings) with the
/// folders it names, and the two answers, Use where it is first. Takes the question, the
/// width, the theme and the lines to add to.
fn question_lines(
    question: &Question,
    limit: usize,
    theme: &crate::tui::theme::Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let (verb, anyway) = match question.mode {
        AddMode::Move => ("Move", "Move anyway"),
        AddMode::Copy => ("Copy", "Copy anyway"),
    };
    let name = AddFolder::name(&question.source);
    lines.push(styled(
        format!("{verb} {name} anyway?"),
        Role::Heading,
        theme,
    ));
    for block in &question.blocks {
        for line in &block.lines {
            lines.extend(wrap(line, limit).into_iter().map(|text| {
                if block.loss {
                    styled(text, Role::Warning, theme)
                } else {
                    Line::raw(text)
                }
            }));
        }
        for path in &block.paths {
            lines.extend(wrap(&format!("- {path}"), limit).into_iter().map(Line::raw));
        }
    }
    lines.push(Line::default());
    lines.push(buttons(
        &["Use where it is", anyway],
        Some(question.choice),
        theme,
    ));
}

/// Draws the iCloud question: how many files are only in iCloud, that a move takes the folder
/// out of iCloud on the owner's other Macs, and the three answers; while the download runs,
/// its progress instead of the answers. Takes the question, the width, the context and the
/// lines to add to.
fn icloud_lines(icloud: &Icloud, limit: usize, ctx: &Ctx, lines: &mut Vec<Line<'static>>) {
    let theme = &ctx.theme;
    let name = AddFolder::name(&icloud.source);
    lines.push(styled(
        format!("Download {name} from iCloud first?"),
        Role::Heading,
        theme,
    ));
    let place = icloud_place(&icloud.source, ctx.home.as_deref());
    for text in [
        icloud_count(icloud.placeholders),
        format!("Moving takes the folder out of {place} on your other Macs too."),
    ] {
        lines.extend(wrap(&text, limit).into_iter().map(Line::raw));
    }
    lines.push(Line::default());
    let Some(download) = &icloud.download else {
        lines.push(buttons(&ICLOUD_CHOICES, Some(icloud.choice), theme));
        return;
    };
    if download.cancel.load(Ordering::Relaxed) {
        lines.push(ctx.loading("Stopping the download"));
        return;
    }
    lines.push(ctx.loading(&match download.progress {
        Some(Progress { total, done }) if total > 0 => {
            format!("Downloading from iCloud: {done} of {total} files")
        }
        _ => "Downloading from iCloud".to_owned(),
    }));
}

/// Says how many of a folder's files are only in iCloud and that CrazyAgent downloads them
/// first. Takes the count, none when only the engine's refusal said some are. Returns the
/// sentence.
fn icloud_count(placeholders: Option<u64>) -> String {
    let files = match placeholders {
        Some(1) => "1 file is".to_owned(),
        Some(count) => format!("{count} files are"),
        None => "Some files are".to_owned(),
    };
    format!(
        "{files} only in iCloud; CrazyAgent downloads them first (needs internet, can take a while)."
    )
}

/// Names the iCloud place a folder is in, for the move line: iCloud Desktop for a folder in
/// ~/Desktop, iCloud Documents for one in ~/Documents and iCloud Drive otherwise. Takes the
/// folder and the user's home folder. Returns the name.
fn icloud_place(source: &str, home: Option<&str>) -> &'static str {
    let inside = |folder: &str| {
        home.is_some_and(|home| Path::new(source).starts_with(Path::new(home).join(folder)))
    };
    if inside("Desktop") {
        "iCloud Desktop"
    } else if inside("Documents") {
        "iCloud Documents"
    } else {
        "iCloud Drive"
    }
}

/// Says that a Copy leaves the folder's linked git worktrees with the original.
/// Takes how many linked worktrees the folder has.
/// Returns the sentence.
fn worktrees_line(count: usize) -> String {
    if count == 1 {
        "This folder has 1 linked git worktree. It stays with the original folder; the copy gets the main checkout and every branch.".into()
    } else {
        format!(
            "This folder has {count} linked git worktrees. They stay with the original folder; the copy gets the main checkout and every branch."
        )
    }
}
