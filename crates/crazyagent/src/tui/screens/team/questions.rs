use crate::coord::{BODY_BYTES_MAX, Severity, Waiting};
use crate::engine::owner::OwnerResult;
use crate::project_setup::{SetupQuery, SetupView};
use crate::protocol::ProjectId;
use crate::team::TeamAction;
use crate::team::questions::{Inbox, OwnerQuestion, WaitingAgent};
use crate::terminal::AgentId;
use crate::tui::client::OwnerEvent;
use crate::tui::ctx::Ctx;
use crate::tui::frame::{Hint, Message, hint};
use crate::tui::keys::Key;
use crate::tui::screens::new_project_input::TextInput;
use crate::tui::screens::new_project_page::key_event;
use crate::tui::seam::{Arrival, NOT_READY, Request};
use crate::tui::text::{cut, styled, wrap};
use crate::tui::theme::Role;
use ratatui::text::Line;

type QuestionLines = (String, Vec<Line<'static>>, Option<(usize, usize)>);

/// The most lines one question's text takes in the box when it is not the picked one.
pub(super) const QUESTION_LINES_MAX: usize = 3;

/// What the Questions box asks the Team screen to do after a key.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Took {
    Handled,
    Open(AgentId),
}

/// One row the arrows move over: an open question to the owner or an agent waiting on the
/// owner in its own terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Question(usize),
    Waiting(usize),
}

/// The answer the owner is writing: the question it answers, its asker and the text.
struct Answer {
    message: i64,
    label: String,
    input: TextInput,
    sending: bool,
}

/// The Team screen's Questions box (Q14, Q14b): `q` lists the open questions agents put to
/// the owner, the most urgent first, and the running agents that wait on the owner in their
/// own terminal. Enter on a question writes its answer, which goes to the asker as the
/// owner's reply; Enter on a waiting agent, or `o` on a question, opens that agent's
/// terminal. Input: keys and setup replies. Output: Setup requests, one message per outcome
/// and the terminal to open.
pub(super) struct Questions {
    open: bool,
    inbox: Option<Inbox>,
    problem: Option<String>,
    cursor: usize,
    answer: Option<Answer>,
}

impl Questions {
    /// A closed box with nothing read.
    pub(super) fn new() -> Self {
        Self {
            open: false,
            inbox: None,
            problem: None,
            cursor: 0,
            answer: None,
        }
    }

    /// Says whether the box is open, so it takes every key.
    pub(super) fn open(&self) -> bool {
        self.open
    }

    /// Says whether the owner is writing an answer, so letters type instead of acting.
    pub(super) fn typing(&self) -> bool {
        self.open && self.answer.is_some()
    }

    /// How many open questions wait on the owner and how many agents wait in a terminal, as
    /// last read; None before the first read.
    pub(super) fn counts(&self) -> Option<(usize, usize)> {
        self.inbox.as_ref().map(|inbox| {
            let more = usize::try_from(inbox.more).unwrap_or(usize::MAX);
            (
                inbox.questions.len().saturating_add(more),
                inbox.waiting.len(),
            )
        })
    }

    /// Asks the engine for the project's Questions inbox. Inputs: the project and the
    /// context. Output: one Setup request.
    pub(super) fn read(project: &ProjectId, ctx: &mut Ctx) {
        ctx.request(Request::Setup(SetupQuery::CoordQuestions {
            project: project.clone(),
        }));
    }

    fn rows(&self) -> Vec<Row> {
        let Some(inbox) = &self.inbox else {
            return Vec::new();
        };
        (0..inbox.questions.len())
            .map(Row::Question)
            .chain((0..inbox.waiting.len()).map(Row::Waiting))
            .collect()
    }

    fn question(&self, at: usize) -> Option<&OwnerQuestion> {
        self.inbox
            .as_ref()
            .and_then(|inbox| inbox.questions.get(at))
    }

    fn waiting(&self, at: usize) -> Option<&WaitingAgent> {
        self.inbox.as_ref().and_then(|inbox| inbox.waiting.get(at))
    }

    /// Takes a key first while the box is open or for `q`. Inputs: the key, the project and
    /// the context. Output: what the Team screen does next, or None to leave the key to it.
    pub(super) fn key(&mut self, key: &Key, project: &ProjectId, ctx: &mut Ctx) -> Option<Took> {
        if !self.open {
            if !matches!(key, Key::Char('q')) {
                return None;
            }
            self.open = true;
            self.cursor = 0;
            Self::read(project, ctx);
            return Some(Took::Handled);
        }
        if self.answer.is_some() {
            return Some(self.answer_key(key, project, ctx));
        }
        let rows = self.rows();
        let took = match key {
            Key::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Took::Handled
            }
            Key::Down => {
                self.cursor = (self.cursor + 1).min(rows.len().saturating_sub(1));
                Took::Handled
            }
            Key::Enter => match rows.get(self.cursor) {
                Some(Row::Question(at)) => {
                    self.start_answer(*at);
                    Took::Handled
                }
                Some(Row::Waiting(at)) => {
                    self.terminal(self.waiting(*at).and_then(|row| row.terminal.clone()), ctx)
                }
                None => Took::Handled,
            },
            Key::Char('o') => match rows.get(self.cursor) {
                Some(Row::Question(at)) => {
                    self.terminal(self.question(*at).and_then(|row| row.terminal.clone()), ctx)
                }
                Some(Row::Waiting(at)) => {
                    self.terminal(self.waiting(*at).and_then(|row| row.terminal.clone()), ctx)
                }
                None => Took::Handled,
            },
            Key::Char('r') => {
                Self::read(project, ctx);
                Took::Handled
            }
            Key::Char('q') | Key::Esc => {
                self.open = false;
                Took::Handled
            }
            _ => Took::Handled,
        };
        assert!(self.cursor <= rows.len().saturating_sub(1));
        Some(took)
    }

    fn terminal(&mut self, terminal: Option<AgentId>, ctx: &mut Ctx) -> Took {
        let Some(terminal) = terminal else {
            ctx.say(Message::warning(
                "This agent has no terminal now.",
                "It is not running.",
            ));
            return Took::Handled;
        };
        self.open = false;
        Took::Open(terminal)
    }

    fn start_answer(&mut self, at: usize) {
        let Some(question) = self.question(at) else {
            return;
        };
        self.answer = Some(Answer {
            message: question.id,
            label: super::plain(&question.label),
            input: TextInput::multi(BODY_BYTES_MAX),
            sending: false,
        });
    }

    fn answer_key(&mut self, key: &Key, project: &ProjectId, ctx: &mut Ctx) -> Took {
        let Some(answer) = self.answer.as_mut() else {
            return Took::Handled;
        };
        if answer.sending {
            return Took::Handled;
        }
        match key {
            Key::Esc => self.answer = None,
            Key::Enter => {
                let text = answer.input.value().trim().to_owned();
                if text.is_empty() {
                    ctx.say(Message::warning(
                        "Write an answer first.",
                        "Esc leaves without answering.",
                    ));
                    return Took::Handled;
                }
                answer.sending = true;
                let generation = ctx.next_generation();
                ctx.request(Request::Setup(SetupQuery::TeamChange {
                    action: TeamAction::AnswerQuestion {
                        project_id: project.clone(),
                        message: answer.message,
                        text,
                    },
                    generation,
                }));
            }
            Key::Paste(text) => answer.input.insert_str(text),
            other => {
                if let Some(event) = key_event(other) {
                    answer.input.handle(event);
                }
            }
        }
        Took::Handled
    }

    /// Applies the replies to the reads and answers this box sent for the project: a read
    /// fills the list, an answer says how it went and reads the inbox again. Inputs: the
    /// arrival, the project and the context.
    pub(super) fn arrive(&mut self, arrival: &Arrival, project: &ProjectId, ctx: &mut Ctx) {
        let Arrival::Owner(event) = arrival else {
            return;
        };
        let OwnerEvent::Setup(event) = event.as_ref() else {
            return;
        };
        match (&event.query, &event.result) {
            (
                SetupQuery::CoordQuestions { project: asked },
                Ok(SetupView::CoordQuestions(inbox)),
            ) if asked == project => {
                assert_eq!(&inbox.project_id, project);
                self.inbox = Some((**inbox).clone());
                self.problem = None;
                self.cursor = self.cursor.min(self.rows().len().saturating_sub(1));
            }
            (SetupQuery::CoordQuestions { project: asked }, Err(code)) if asked == project => {
                self.problem = Some(code.clone());
            }
            (
                SetupQuery::TeamChange {
                    action:
                        TeamAction::AnswerQuestion {
                            project_id,
                            message,
                            ..
                        },
                    ..
                },
                result,
            ) if project_id == project => {
                let Some(answer) = self
                    .answer
                    .as_mut()
                    .filter(|answer| answer.message == *message)
                else {
                    return;
                };
                answer.sending = false;
                match result {
                    Ok(SetupView::Changed(OwnerResult::Applied)) => {
                        ctx.say(Message::success(
                            &format!("Answer sent to {}.", answer.label),
                            "It reaches the agent as your reply.",
                        ));
                        self.answer = None;
                        Self::read(project, ctx);
                    }
                    Ok(SetupView::Changed(OwnerResult::Rejected { code })) | Err(code) => {
                        ctx.say(Message::warning("The answer was not sent.", words(code)));
                        if matches!(
                            code.as_str(),
                            "coord_question_closed" | "coord_question_missing"
                        ) {
                            self.answer = None;
                            Self::read(project, ctx);
                        }
                    }
                    Ok(_) => {}
                }
            }
            _ => {}
        }
    }

    /// The keys the open box offers.
    pub(super) fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        if !self.open {
            return Vec::new();
        }
        if self.answer.is_some() {
            return vec![
                hint("Enter", "Send"),
                hint("Ctrl-J", "New line"),
                hint("Esc", "Cancel"),
            ];
        }
        let vertical = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
        let mut hints = vec![hint(&vertical, "Move")];
        match self.rows().get(self.cursor) {
            Some(Row::Question(_)) => {
                hints.push(hint("Enter", "Answer"));
                hints.push(hint("o", "Terminal"));
            }
            Some(Row::Waiting(_)) => hints.push(hint("Enter", "Terminal")),
            None => {}
        }
        hints.push(hint("r", "Refresh"));
        hints.push(hint("q", "Close"));
        hints
    }

    /// The box lines while it is open: the answer being written, or each open question with
    /// its asker, urgency, text and stated assumption, then each agent waiting on the owner
    /// in its terminal; the picked row is marked. Rows past the box's height are counted in
    /// one "and N more" line. Inputs: the line width, the box's inner height and the
    /// context. Output: the title, at most `room` lines (one when `room` is zero) and the
    /// answer's cursor as line and column, or None while closed.
    pub(super) fn lines(&self, limit: usize, room: usize, ctx: &Ctx) -> Option<QuestionLines> {
        if !self.open {
            return None;
        }
        let theme = &ctx.theme;
        let free = room.max(1);
        if let Some(answer) = &self.answer {
            let mut lines = vec![styled(
                cut(
                    &format!("Answer to {}:", answer.label),
                    limit,
                    ctx.glyphs.ellipsis,
                ),
                Role::Heading,
                theme,
            )];
            let (input, (row, column)) = answer.input.wrap(limit.max(1));
            let shown = free.saturating_sub(2).max(1);
            let top = row.saturating_sub(shown.saturating_sub(1));
            lines.extend(input.into_iter().skip(top).take(shown).map(Line::raw));
            if answer.sending {
                lines.push(ctx.loading("Sending..."));
            }
            lines.truncate(free);
            let cursor = (1 + row - top).min(free.saturating_sub(1));
            assert!(lines.len() <= free);
            return Some(("Answer".to_owned(), lines, Some((cursor, column))));
        }
        let Some(inbox) = &self.inbox else {
            let line = match self.problem.as_deref() {
                Some(code) => styled(
                    cut(words(code), limit, ctx.glyphs.ellipsis),
                    Role::Warning,
                    theme,
                ),
                None => ctx.loading("Reading questions..."),
            };
            return Some((title(None), vec![line], None));
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        if let Some(code) = self.problem.as_deref() {
            lines.push(styled(
                cut(
                    &format!("Could not refresh: {}", words(code)),
                    limit,
                    ctx.glyphs.ellipsis,
                ),
                Role::Warning,
                theme,
            ));
        }
        if inbox.questions.is_empty() && inbox.more == 0 {
            lines.push(styled(
                cut("No questions for you.", limit, ctx.glyphs.ellipsis),
                Role::Muted,
                theme,
            ));
        }
        let rows = self.rows();
        let mut cut_off = None;
        for (index, row) in rows.iter().enumerate() {
            let block = match row {
                Row::Question(at) => {
                    question_lines(&inbox.questions[*at], index == self.cursor, limit, ctx)
                }
                Row::Waiting(at) => {
                    waiting_lines(&inbox.waiting[*at], index == self.cursor, limit, ctx)
                }
            };
            let heading = matches!(row, Row::Waiting(0));
            let need = block.len() + usize::from(heading) * 2;
            if lines.len() + need + 1 > free {
                cut_off = Some(rows.len() - index);
                break;
            }
            if heading {
                lines.push(Line::default());
                lines.push(styled(
                    cut(
                        "Waiting on you in their terminal:",
                        limit,
                        ctx.glyphs.ellipsis,
                    ),
                    Role::Heading,
                    theme,
                ));
            }
            lines.extend(block);
        }
        let hidden = cut_off.unwrap_or(0) + usize::try_from(inbox.more).unwrap_or(usize::MAX);
        if hidden > 0 {
            lines.push(Line::raw(cut(
                &format!("and {hidden} more"),
                limit,
                ctx.glyphs.ellipsis,
            )));
        }
        lines.truncate(free);
        assert!(lines.len() <= free);
        Some((title(self.counts()), lines, None))
    }
}

/// The box title. Input: the open question and waiting counts, None before a read.
/// Output: "Questions for you (N)".
pub(super) fn title(counts: Option<(usize, usize)>) -> String {
    match counts {
        Some((questions, _)) => format!("Questions for you ({questions})"),
        None => "Questions for you".to_owned(),
    }
}

/// The plain word for a question's urgency (Q14b). Input: the severity. Output: the word.
pub(super) fn urgency(severity: Severity) -> &'static str {
    match severity {
        Severity::High => "Urgent, the agent is paused",
        Severity::Normal => "The agent waits for you",
        Severity::Low => "The agent carries on",
    }
}

fn question_lines(
    question: &OwnerQuestion,
    picked: bool,
    limit: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let theme = &ctx.theme;
    let mark = if picked { "> " } else { "  " };
    let head = format!(
        "{mark}{}  For: {}  {}",
        super::plain(&question.label),
        super::plain(&question.for_label),
        urgency(question.severity)
    );
    let role = match (picked, question.severity) {
        (true, _) => Role::Accent,
        (false, Severity::High) => Role::Warning,
        (false, _) => Role::Heading,
    };
    let mut lines = vec![styled(cut(&head, limit, ctx.glyphs.ellipsis), role, theme)];
    let body = wrap(
        &super::plain(&question.text),
        limit.saturating_sub(4).max(1),
    );
    let shown = if picked {
        body.len()
    } else {
        QUESTION_LINES_MAX
    };
    let more = body.len() > shown;
    lines.extend(
        body.into_iter()
            .take(shown)
            .map(|text| Line::raw(format!("    {text}"))),
    );
    if more {
        lines.push(styled(
            cut("    ...", limit, ctx.glyphs.ellipsis),
            Role::Muted,
            theme,
        ));
    }
    match (&question.assumption, question.severity) {
        (Some(assumption), _) => lines.push(styled(
            cut(
                &format!("    Assumes: {}", super::plain(assumption)),
                limit,
                ctx.glyphs.ellipsis,
            ),
            Role::Muted,
            theme,
        )),
        (None, Severity::Low) => lines.push(styled(
            cut("    It stated no assumption.", limit, ctx.glyphs.ellipsis),
            Role::Muted,
            theme,
        )),
        (None, _) => {}
    }
    lines
}

fn waiting_lines(
    agent: &WaitingAgent,
    picked: bool,
    limit: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    let mark = if picked { "> " } else { "  " };
    let what = match agent.waiting {
        Waiting::Permission => "a permission prompt",
        Waiting::Question => "a question in its terminal",
    };
    let text = format!("{mark}{}  {what}", super::plain(&agent.label));
    let line = cut(&text, limit, ctx.glyphs.ellipsis);
    vec![if picked {
        styled(line, Role::Accent, &ctx.theme)
    } else {
        styled(line, Role::Warning, &ctx.theme)
    }]
}

/// One plain sentence for a refused answer or read. Input: the code. Output: the sentence.
fn words(code: &str) -> &'static str {
    match code {
        "coord_question_closed" => "That question was already answered or has ended.",
        "coord_question_missing" => "That question is no longer there.",
        "team_answer_invalid" => {
            "Write an answer of at most 4000 bytes, without control characters."
        }
        NOT_READY => "Questions are not available in this engine yet.",
        "trusted_connection_unavailable" | "controller_disconnected" => {
            "The engine connection is not ready yet."
        }
        other => crate::config::plain_problem(other).unwrap_or("The engine could not do this."),
    }
}
