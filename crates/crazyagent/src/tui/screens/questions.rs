use super::new_project_input::{TextInput, cells};
use super::new_project_view::{Look, Row, columns, list_row, spread, wrap_words};
use super::project::plain_problem;
use super::project_team::{Help, Notice, TABS, window};
use super::project_time::clock;
use crate::protocol::ProjectId;
use crate::questions::{
    ANSWER_BYTES_MAX, Answer, Question, QuestionStatus, Settlement, asker_label,
};
use crate::tui::client::questions::{QuestionsEvent, QuestionsQuery, QuestionsReply};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;

const LIST_POLL_MS: i64 = 5_000;
const LIST_ROWS_MAX: usize = 6;
const FROM_WIDTH: usize = 24;
const STATUS_WIDTH: usize = 10;
const LABEL_WIDTH: usize = 10;
const BOX_LEFT: usize = 14;
const TEXT_LINES_MAX: usize = 4;
const READ_PAGE: usize = 10;
const CUT: &str = "... v reads all of it";
const QUIET_CODES: [&str; 1] = ["trusted_connection_unavailable"];

/// What the owner is doing with the chosen question: typing or picking an answer, or
/// confirming a dismissal. Both hold the id and the version the owner read, so the engine
/// refuses a write about a question that changed since.
enum Settling {
    Answer {
        id: u64,
        version: u64,
        input: TextInput,
        option: Option<usize>,
    },
    Dismiss {
        id: u64,
        version: u64,
    },
}

/// One project's Questions tab: every question an agent or an engine step asked the owner,
/// pending ones first, with the recommended answer. `a` answers the chosen pending question
/// by picking an option or typing, `d` dismisses it after a yes. Each write names the
/// version the owner read. Sends at most one request at a time; never invents data.
pub(in crate::tui) struct ProjectQuestions {
    pub project: ProjectId,
    items: Vec<Question>,
    loaded: bool,
    cursor: usize,
    settling: Option<Settling>,
    reading: Option<usize>,
    notice: Option<Notice>,
    queued: Option<QuestionsQuery>,
    sent: Option<QuestionsQuery>,
    stale: bool,
    read_ms: i64,
}

impl ProjectQuestions {
    /// Opens one project's Questions tab with nothing read yet. Input: the project.
    pub(in crate::tui) fn new(project: ProjectId) -> Self {
        Self {
            project,
            items: Vec::new(),
            loaded: false,
            cursor: 0,
            settling: None,
            reading: None,
            notice: None,
            queued: None,
            sent: None,
            stale: true,
            read_ms: 0,
        }
    }

    /// Asks for a fresh list on the next `next_query`, as when the owner opens the tab.
    pub(in crate::tui) fn refresh(&mut self) {
        self.stale = true;
    }

    /// The next request to send: a queued answer or dismissal first, then the list when it
    /// is stale or older than the poll interval. Input: now in ms. Output: the query, or
    /// none while one is in flight.
    pub(in crate::tui) fn next_query(&mut self, now_ms: i64) -> Option<QuestionsQuery> {
        if self.sent.is_some() {
            return None;
        }
        let query = if let Some(action) = self.queued.take() {
            action
        } else if self.stale || now_ms.saturating_sub(self.read_ms) >= LIST_POLL_MS {
            self.stale = false;
            self.read_ms = now_ms;
            QuestionsQuery::List {
                project: self.project.clone(),
            }
        } else {
            return None;
        };
        self.sent = Some(query.clone());
        Some(query)
    }

    /// Forgets the request in flight after the engine connection dropped. A lost answer or
    /// dismissal is not sent again: the list is read again so the owner sees whether it
    /// happened.
    pub(in crate::tui) fn forget_reads(&mut self) {
        let Some(lost) = self.sent.take() else {
            return;
        };
        self.stale = true;
        if !matches!(lost, QuestionsQuery::List { .. }) {
            self.notice = Some(Notice::problem(
                "The engine connection dropped before it answered. Check the list before trying again.",
            ));
        }
    }

    /// Whether a request is in flight or an answer or dismissal waits to be sent.
    pub(in crate::tui) fn busy(&self) -> bool {
        self.sent.is_some() || self.queued.is_some()
    }

    /// Takes the engine's answer to the request in flight; any other reply is left alone.
    /// A settled or stale write reads the list again. Input: the event.
    pub(in crate::tui) fn apply(&mut self, event: &QuestionsEvent) {
        if self.sent.as_ref() != Some(&event.query) {
            return;
        }
        self.sent = None;
        match (&event.query, &event.result) {
            (QuestionsQuery::List { .. }, Ok(QuestionsReply::Page(page))) => {
                let chosen = self.items.get(self.cursor).map(|question| question.id);
                self.items = page.items.clone();
                self.loaded = true;
                if let Some(at) =
                    chosen.and_then(|id| self.items.iter().position(|question| question.id == id))
                {
                    self.cursor = at;
                }
            }
            (QuestionsQuery::List { .. }, Err(code)) => {
                if !QUIET_CODES.contains(&code.as_str()) {
                    self.notice = Some(Notice::problem(format!(
                        "Couldn't load the questions. {}",
                        sentence(code)
                    )));
                }
            }
            (query, Ok(QuestionsReply::Settled(Settlement::Settled))) => {
                self.stale = true;
                self.notice = Some(Notice::plain(match query {
                    QuestionsQuery::Dismiss { .. } => "Dismissed. The asker is told once.",
                    _ => "Answer sent. The asker gets it once.",
                }));
            }
            (_, Ok(QuestionsReply::Settled(Settlement::Stale))) => {
                self.stale = true;
                self.notice = Some(Notice::problem(
                    "That question was answered or changed since you read it. The list is read again.",
                ));
            }
            (_, Err(code)) => {
                self.stale = true;
                self.notice = Some(Notice::problem(sentence(code)));
            }
            (_, Ok(_)) => {}
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.items.len().saturating_sub(1));
        if self.items.is_empty() {
            self.reading = None;
        }
        assert!(self.items.is_empty() || self.cursor < self.items.len());
    }

    fn chosen(&self) -> Option<&Question> {
        self.items.get(self.cursor)
    }

    /// Whether the owner is typing an answer, so the host leaves typed characters to it.
    pub(in crate::tui) fn editing(&self) -> bool {
        matches!(self.settling, Some(Settling::Answer { .. }))
    }

    /// Inserts pasted text into the answer being typed. Input: untrusted pasted text.
    /// Output: true when the answer box took it.
    pub(in crate::tui) fn paste(&mut self, text: &str) -> bool {
        match self.settling.as_mut() {
            Some(Settling::Answer { input, .. }) => {
                input.insert_str(text);
                true
            }
            _ => false,
        }
    }

    /// Handles one key: move through the questions, `v` to read the chosen one whole, `a` to
    /// answer and `d` to dismiss the chosen pending one; in the answer box ↑↓ pick an option,
    /// letters type, Enter sends and Esc cancels; while reading, ↑↓ and Page keys scroll and
    /// Esc or `v` close. Input: the key. Output: true when the tab used the key; other keys,
    /// including Esc on the list, are left to the host.
    pub(in crate::tui) fn key(&mut self, key: KeyEvent) -> bool {
        if let Some(Settling::Answer { .. }) = self.settling {
            self.answer_key(key);
            return true;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
        {
            return false;
        }
        if let Some(top) = self.reading {
            self.reading = match key.code {
                KeyCode::Esc | KeyCode::Char('v') => None,
                KeyCode::Up => Some(top.saturating_sub(1)),
                KeyCode::Down => Some(top.saturating_add(1)),
                KeyCode::PageUp => Some(top.saturating_sub(READ_PAGE)),
                KeyCode::PageDown => Some(top.saturating_add(READ_PAGE)),
                KeyCode::Home => Some(0),
                _ => return true,
            };
            return true;
        }
        if let Some(Settling::Dismiss { id, version }) = self.settling {
            match key.code {
                KeyCode::Char('y') => {
                    self.settling = None;
                    self.queue(QuestionsQuery::Dismiss {
                        project: self.project.clone(),
                        id,
                        version,
                    });
                }
                KeyCode::Char('n') | KeyCode::Esc => self.settling = None,
                _ => {}
            }
            return true;
        }
        if !self.loaded {
            return false;
        }
        let pending = self
            .chosen()
            .filter(|question| question.status == QuestionStatus::Pending)
            .map(|question| (question.id, question.version, recommended_option(question)));
        match (key.code, pending) {
            (KeyCode::Up, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Down, _) => self.cursor = self.cursor.saturating_add(1),
            (KeyCode::Char('v'), _) if self.chosen().is_some() => self.reading = Some(0),
            (KeyCode::Char('a'), Some((id, version, option))) => {
                self.notice = None;
                self.settling = Some(Settling::Answer {
                    id,
                    version,
                    input: TextInput::single(ANSWER_BYTES_MAX),
                    option,
                });
            }
            (KeyCode::Char('d'), Some((id, version, _))) => {
                self.notice = None;
                self.settling = Some(Settling::Dismiss { id, version });
            }
            _ => return false,
        }
        self.clamp();
        true
    }

    fn answer_key(&mut self, key: KeyEvent) {
        let options = self.chosen().map_or(0, |question| question.options.len());
        let Some(Settling::Answer {
            id,
            version,
            input,
            option,
        }) = self.settling.as_mut()
        else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.settling = None,
            KeyCode::Up if options > 0 => {
                *option = Some(option.map_or(0, |at| at.saturating_sub(1)));
            }
            KeyCode::Down if options > 0 => {
                *option = Some(option.map_or(0, |at| (at + 1).min(options - 1)));
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let typed = input.value().trim();
                let answer = if !typed.is_empty() {
                    Answer::Text(typed.to_owned())
                } else if let Some(at) = *option {
                    Answer::Option(u8::try_from(at).expect("options stay below OPTIONS_MAX"))
                } else {
                    self.notice = Some(Notice::problem("Type an answer or pick an option."));
                    return;
                };
                let (id, version) = (*id, *version);
                self.settling = None;
                self.queue(QuestionsQuery::Answer {
                    project: self.project.clone(),
                    id,
                    version,
                    answer,
                });
            }
            _ => {
                input.handle(key);
            }
        }
    }

    fn queue(&mut self, query: QuestionsQuery) {
        if self.queued.is_some()
            || self
                .sent
                .as_ref()
                .is_some_and(|sent| !matches!(sent, QuestionsQuery::List { .. }))
        {
            self.notice = Some(Notice::problem(
                "Still working on your last choice. Try again in a moment.",
            ));
            return;
        }
        self.notice = None;
        self.queued = Some(query);
    }

    /// Draws the Questions tab: the list, the chosen question with its options and the
    /// recommended one, and the answer box or the dismiss question while one is open; or,
    /// while reading, the whole chosen question scrolled to where the owner is.
    /// Inputs: the content width, the rows available, the UTC offset and the look.
    /// Output: the rows, the answer box cursor as a column and row of those rows, and the
    /// help keys.
    pub(in crate::tui) fn rows(
        &mut self,
        content: usize,
        room: usize,
        offset: i64,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        let mut lines = Vec::new();
        if let (Some(top), Some(question)) = (self.reading, self.chosen()) {
            let (shown, top) = whole(question, content, room, top, offset, look);
            self.reading = Some(top);
            let help = vec![
                ("\u{2191}\u{2193}", "Scroll"),
                ("PgUp/PgDn", "Page"),
                ("Esc", "Back"),
            ];
            return (shown, None, help);
        }
        if !self.loaded {
            lines.push(Row::of("Questions", look.heading));
            lines.push(Row::of("Loading questions...", look.muted));
            lines.extend(self.notice.iter().map(|notice| notice.row(look)));
            return (lines, None, vec![TABS, ("Esc", "Home")]);
        }
        let (footer, at) = self.footer(content, look);
        lines.extend(self.list_rows(content, room.saturating_sub(footer.len()), offset, look));
        let cursor = at
            .map(|(column, row)| (column, lines.len() + row))
            .filter(|(_, row)| *row < room);
        lines.extend(footer);
        (lines, cursor, self.help())
    }

    fn list_rows(&self, content: usize, room: usize, offset: i64, look: &Look) -> Vec<Row> {
        let waiting = self
            .items
            .iter()
            .filter(|question| question.status == QuestionStatus::Pending)
            .count();
        let mut lines = vec![
            Row::of("Questions", look.heading).push(format!("  {waiting} waiting"), look.muted),
        ];
        if self.items.is_empty() {
            lines.extend(
                wrap_words(
                    "No questions yet. When an agent or a setup step needs you to decide something, its question waits here.",
                    content,
                )
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
            );
            return lines;
        }
        let shown = match self.settling {
            Some(Settling::Answer { .. }) => 1,
            _ => LIST_ROWS_MAX.min(room.saturating_sub(10).max(1)),
        };
        let first = window(self.cursor, self.items.len(), shown);
        let widths = [
            content.saturating_sub(2 + FROM_WIDTH + STATUS_WIDTH + 4),
            FROM_WIDTH,
            STATUS_WIDTH,
        ];
        for (index, question) in self.items.iter().enumerate().skip(first).take(shown) {
            let cells = vec![
                Row::of(question.question.replace('\n', " "), look.fg),
                Row::of(asker_label(&question.asker), look.muted),
                Row::of(
                    status_word(question.status),
                    status_style(question.status, look),
                ),
            ];
            lines.push(list_row(
                columns(&widths, cells),
                content,
                index == self.cursor,
                !self.editing(),
                look,
            ));
        }
        lines.push(Row::default());
        if let Some(question) = self.chosen() {
            lines.extend(self.details(question, content, offset, look));
        }
        if lines.len() > room.max(1) {
            lines.truncate(room.max(1) - 1);
            lines.push(Row::of(CUT, look.muted));
        }
        assert!(lines.len() <= room.max(1));
        lines
    }

    fn details(&self, question: &Question, content: usize, offset: i64, look: &Look) -> Vec<Row> {
        let text = content.saturating_sub(LABEL_WIDTH).max(1);
        let asked = i64::try_from(question.created_ms).unwrap_or(i64::MAX);
        let mut rows: Vec<Row> = cut(wrap_words(&question.question, content))
            .into_iter()
            .map(|line| Row::of(line, look.heading))
            .collect();
        rows.push(spread(
            content,
            Row::of("From", look.muted)
                .pad(LABEL_WIDTH)
                .push(asker_label(&question.asker), look.fg),
            Row::of(format!("Asked {}", clock(asked, offset)), look.muted),
        ));
        let picked = match &self.settling {
            Some(Settling::Answer { input, option, .. }) if input.value().trim().is_empty() => {
                *option
            }
            _ => None,
        };
        for (index, choice) in question.options.iter().enumerate() {
            let mut line = format!("{}. {choice}", index + 1);
            if question.recommended.as_deref() == Some(choice.as_str()) {
                line.push_str(" (recommended)");
            }
            let mark = if picked == Some(index) { "> " } else { "  " };
            let lead = if index == 0 { "Options" } else { "" };
            rows.push(
                Row::of(lead, look.muted)
                    .pad(LABEL_WIDTH.saturating_sub(2))
                    .push(mark, look.accent)
                    .push(
                        line,
                        if picked == Some(index) {
                            look.accent
                        } else {
                            look.fg
                        },
                    ),
            );
        }
        if question.options.is_empty()
            && let Some(recommended) = &question.recommended
        {
            rows.extend(field("Suggested", recommended, text, look.fg, look));
        }
        if let Some(answer) = &question.answer {
            rows.extend(field("Answer", answer, text, look.success, look));
        }
        if let Some(context) = &question.context {
            rows.extend(field("Context", context, text, look.fg, look));
        }
        rows
    }

    fn footer(&self, content: usize, look: &Look) -> (Vec<Row>, Option<(usize, usize)>) {
        match &self.settling {
            None => (
                self.notice.iter().map(|notice| notice.row(look)).collect(),
                None,
            ),
            Some(Settling::Dismiss { .. }) => (
                vec![
                    Row::of("Dismiss this question?", look.warning)
                        .push("   y", look.heading)
                        .push(" Dismiss   ", look.fg)
                        .push("n", look.heading)
                        .push(" Keep it", look.fg),
                    Row::of(
                        "The asker is told once that you will not answer it.",
                        look.muted,
                    ),
                ],
                None,
            ),
            Some(Settling::Answer { input, option, .. }) => {
                let room = content.saturating_sub(BOX_LEFT + 2).max(1);
                let typed: Vec<char> = input.value().chars().collect();
                let start = input.cursor().saturating_sub(room - 1);
                let shown: String = typed.iter().skip(start).take(room).collect();
                let before: String = typed[start..input.cursor()].iter().collect();
                let mut rows = vec![
                    Row::of("Your answer", look.heading)
                        .pad(BOX_LEFT - 2)
                        .push("[ ", look.accent)
                        .cat(Row::of(shown, look.fg).pad(room))
                        .push(" ]", look.accent),
                ];
                let hint = if option.is_some() {
                    "Type your own answer, or press Enter to send the marked option."
                } else {
                    "Type your answer, then press Enter to send it."
                };
                rows.push(Row::of(hint, look.muted));
                rows.extend(self.notice.iter().map(|notice| notice.row(look)));
                (rows, Some((BOX_LEFT + cells(&before), 0)))
            }
        }
    }

    fn help(&self) -> Help {
        match &self.settling {
            Some(Settling::Answer { .. }) => {
                return vec![
                    ("Enter", "Send"),
                    ("\u{2191}\u{2193}", "Option"),
                    ("Esc", "Cancel"),
                ];
            }
            Some(Settling::Dismiss { .. }) => return vec![("y", "Dismiss"), ("n", "Keep it")],
            None => {}
        }
        let mut help = Vec::new();
        if self
            .chosen()
            .is_some_and(|question| question.status == QuestionStatus::Pending)
        {
            help.extend([("a", "Answer"), ("d", "Dismiss")]);
        }
        if self.chosen().is_some() {
            help.push(("v", "Read all"));
        }
        help.extend([("\u{2191}\u{2193}", "Move"), TABS, ("Esc", "Home")]);
        help
    }
}

/// Keeps at most `TEXT_LINES_MAX` wrapped lines, the last one saying the text goes on and
/// how to read it whole when more were cut. Input: the wrapped lines. Output: the lines.
fn cut(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > TEXT_LINES_MAX {
        lines.truncate(TEXT_LINES_MAX - 1);
        lines.push(CUT.to_owned());
    }
    assert!(lines.len() <= TEXT_LINES_MAX);
    lines
}

/// One labelled detail: the label, then the value wrapped beside it, at most
/// `TEXT_LINES_MAX` lines with a marker when cut. Inputs: the label, the value, its width,
/// its style and the look. Output: the rows.
fn field(label: &str, value: &str, width: usize, style: Style, look: &Look) -> Vec<Row> {
    cut(wrap_words(value, width))
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let lead = if index == 0 { label } else { "" };
            Row::of(lead, look.muted).pad(LABEL_WIDTH).push(line, style)
        })
        .collect()
}

/// The whole of one question for reading: the question, who asked and when, the options with
/// the recommended one, the suggested answer, the answer and the context, none of them cut,
/// then where the view is. Inputs: the question, the width, the rows available, the first
/// line wanted, the UTC offset and the look. Output: the rows and the first line shown, kept
/// so the last line stays in view.
fn whole(
    question: &Question,
    content: usize,
    room: usize,
    top: usize,
    offset: i64,
    look: &Look,
) -> (Vec<Row>, usize) {
    let text = content.saturating_sub(LABEL_WIDTH).max(1);
    let labelled = |label: &str, value: &str, style: Style| -> Vec<Row> {
        wrap_words(value, text)
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let lead = if index == 0 { label } else { "" };
                Row::of(lead, look.muted).pad(LABEL_WIDTH).push(line, style)
            })
            .collect()
    };
    let asked = i64::try_from(question.created_ms).unwrap_or(i64::MAX);
    let mut all: Vec<Row> = wrap_words(&question.question, content)
        .into_iter()
        .map(|line| Row::of(line, look.heading))
        .collect();
    all.push(Row::default());
    all.extend(labelled("From", &asker_label(&question.asker), look.fg));
    all.push(
        Row::of("Asked", look.muted)
            .pad(LABEL_WIDTH)
            .push(clock(asked, offset), look.fg),
    );
    for (index, choice) in question.options.iter().enumerate() {
        let mut line = format!("{}. {choice}", index + 1);
        if question.recommended.as_deref() == Some(choice.as_str()) {
            line.push_str(" (recommended)");
        }
        all.extend(labelled(
            if index == 0 { "Options" } else { "" },
            &line,
            look.fg,
        ));
    }
    if question.options.is_empty()
        && let Some(recommended) = &question.recommended
    {
        all.extend(labelled("Suggested", recommended, look.fg));
    }
    if let Some(answer) = &question.answer {
        all.extend(labelled("Answer", answer, look.success));
    }
    if let Some(context) = &question.context {
        all.extend(labelled("Context", context, look.fg));
    }
    let page = room.saturating_sub(1).max(1);
    let top = top.min(all.len().saturating_sub(page));
    let end = (top + page).min(all.len());
    let mut rows: Vec<Row> = all[top..end].to_vec();
    rows.push(Row::of(
        format!("Lines {}-{end} of {}", top + 1, all.len()),
        look.muted,
    ));
    assert!(rows.len() <= page + 1 && top <= all.len());
    (rows, top)
}

/// The option the asker recommended, by position, so the answer box starts on it.
fn recommended_option(question: &Question) -> Option<usize> {
    let recommended = question.recommended.as_deref()?;
    question
        .options
        .iter()
        .position(|choice| choice == recommended)
}

fn status_word(status: QuestionStatus) -> &'static str {
    match status {
        QuestionStatus::Pending => "Waiting",
        QuestionStatus::Answered => "Answered",
        QuestionStatus::Dismissed => "Dismissed",
        QuestionStatus::AnsweredInTerminal => "In terminal",
    }
}

fn status_style(status: QuestionStatus, look: &Look) -> Style {
    match status {
        QuestionStatus::Pending => look.warning,
        QuestionStatus::Answered | QuestionStatus::AnsweredInTerminal => look.success,
        QuestionStatus::Dismissed => look.muted,
    }
}

/// One plain sentence for a refused Questions request. Input: the code. Output: the sentence.
fn sentence(code: &str) -> String {
    match code {
        "questions_not_found" => "That question is no longer there. The list is read again.".into(),
        "questions_option_invalid" | "questions_answer_invalid" | "questions_text_invalid" => {
            "That answer can't be sent. Use one line of plain text.".into()
        }
        "questions_in_terminal" => {
            "That question is answered in the agent's own terminal, not here.".into()
        }
        "questions_delivery_full" => {
            "The master has too many messages waiting. Try again in a moment.".into()
        }
        "questions_unavailable" | "questions_page_unavailable" => {
            "CrazyAgent couldn't read its questions. Try again.".into()
        }
        "questions_outcome_unknown" => {
            "CrazyAgent couldn't confirm what happened. The list is read again.".into()
        }
        other => plain_problem(other),
    }
}
