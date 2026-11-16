use super::words::{
    ADD_HELP, ADD_TITLE, CATALOG, CATALOG_ABOUT, HARNESSES, HARNESSES_ABOUT, KEY_HELP,
    LOCKED_LABEL, LOCKED_REASON, LOCKED_VALUE, NATIVE_CHOICES, NATIVE_HELP, NATIVE_LABEL, PURPOSE,
    REFUSED_FILE, REFUSED_LISTED, REFUSED_PASTED, REMOVE_BODY, SETTINGS, SIGN_IN_WITH,
    SIGN_IN_WITH_HELP, SIZE_CHOICES, SIZE_HELP, SIZE_LABEL, WAIT_CHOICES, WAIT_HELP, WAIT_LABEL,
    WRITE_CHOICES, WRITE_HELP, WRITE_LABEL, chosen_way, keys_shown, origin_line, reaches_line,
    refusal, secret_line, state, trust_lines, way_word,
};
use super::{AddForm, ConnectorsPage, KeyForm, NOT_READ, Open, Row, TITLE, refused};
use crate::app_plugins::{RunsView, TrustView};
use crate::connectors::launch::reach;
use crate::connectors::{ConnectorView, ConnectorsView};
use crate::coord::settings::HarnessKind;
use crate::tui::ctx::{Ctx, Load};
use crate::tui::frame::{Hint, Modal, hint};
use crate::tui::screens::settings::app_plugins::{problem_text, safe};
use crate::tui::text::{list_row, row, styled, wrap};
use crate::tui::theme::Role;
use ratatui::layout::Position;
use ratatui::text::{Line, Span};

const MODAL_WIDTH: u16 = 66;
const UNDER: &str = "    ";
const LABEL_WIDTH: usize = NATIVE_LABEL.len();

const _: () = assert!(NATIVE_LABEL.len() >= SIZE_LABEL.len());
const _: () = assert!(NATIVE_LABEL.len() >= WRITE_LABEL.len());
const _: () = assert!(NATIVE_LABEL.len() >= WAIT_LABEL.len());

/// The page's lines, the first and last line the cursor's part needs in view, and where
/// a typing cursor sits.
type Content = (
    Vec<Line<'static>>,
    Option<(usize, usize)>,
    Option<Position>,
);

/// The first line to draw so the focused part stays in view: its last line at the
/// window's bottom when it would fall below, never past its first line. Inputs: the top
/// drawn before, the part's first and last line, the rows shown and the line count.
/// Output: the first line to draw.
pub(super) fn scrolled(top: usize, first: usize, last: usize, room: usize, total: usize) -> usize {
    assert!(first <= last, "a part starts before it ends");
    assert!(last < total.max(1), "the part is on the page");
    let mut top = top.min(first);
    if room > 0 && last >= top + room {
        top = last + 1 - room;
    }
    top.min(total.saturating_sub(1))
}

/// The body rows a dialog box has, so it stays below the header and its rule and above
/// the message line and the help bar (rv-ds241 T5 F1). Input: the context with the
/// terminal size. Output: the rows, at least 3.
pub(super) fn dialog_rows(ctx: &Ctx) -> usize {
    let chrome = 5 + usize::from(ctx.hint_rows);
    let rows = usize::from(ctx.height).saturating_sub(chrome).max(3);
    assert!(rows >= 3);
    rows
}

/// The part of a trust screen its box shows: the first line, how many lines show and how
/// many are left below. A screen taller than the box keeps its last row for the line
/// that says how many are below. Inputs: the first line asked for, the line count and the
/// box rows. Output: the first line, the lines shown and the lines below.
pub(super) fn window(top: usize, total: usize, rows: usize) -> (usize, usize, usize) {
    assert!(rows >= 2, "a box shows a line and the line below it");
    if total <= rows {
        return (0, total, 0);
    }
    let shown = rows - 1;
    let top = top.min(total - shown);
    let below = total - top - shown;
    assert_eq!(top + shown + below, total);
    (top, shown, below)
}

fn muted(text: &str, room: usize, ctx: &Ctx) -> Vec<Line<'static>> {
    wrap(text, room)
        .into_iter()
        .map(|part| styled(part, Role::Muted, &ctx.theme))
        .collect()
}

fn under(text: &str, room: usize, ctx: &Ctx) -> Vec<Line<'static>> {
    wrap(text, room.saturating_sub(UNDER.len()).max(1))
        .into_iter()
        .map(|part| styled(format!("{UNDER}{part}"), Role::Muted, &ctx.theme))
        .collect()
}

/// A setting's label and its choices, the pick in brackets. Choices that do not fit in the
/// room go on further lines, lined up under the first choice. Inputs: the label, the
/// choices, the pick, the room in cells and the context. Output: the label line and any
/// further choice lines.
fn choices<T: Copy + PartialEq>(
    label: &str,
    options: &[(T, &str)],
    now: T,
    room: usize,
    ctx: &Ctx,
) -> Vec<Line<'static>> {
    assert!(label.len() <= LABEL_WIDTH);
    assert!(options.iter().any(|(choice, _)| *choice == now), "the pick is offered");
    let indent = LABEL_WIDTH + 2;
    let mut lines = Vec::new();
    let mut spans = vec![Span::raw(format!("{label:<LABEL_WIDTH$}  "))];
    let mut used = indent;
    for (choice, word) in options {
        let (text, style) = if *choice == now {
            (format!("[{word}] "), ctx.theme.heading)
        } else {
            (format!(" {word}  "), ctx.theme.muted)
        };
        let cells = crate::tui::text::width(&text);
        if used > indent && used + cells > room {
            lines.push(Line::from(std::mem::take(&mut spans)));
            spans.push(Span::raw(" ".repeat(indent)));
            used = indent;
        }
        used += cells;
        spans.push(Span::styled(text, style));
    }
    lines.push(Line::from(spans));
    assert!(lines.len() <= options.len(), "each line holds a choice");
    lines
}

impl ConnectorsPage {
    pub(super) fn content(&self, width: usize, ctx: &Ctx) -> Content {
        let view = match &self.stored {
            Load::Ready(view) => view,
            Load::Failed(code) => {
                let mut lines = vec![styled(NOT_READ, Role::Warning, &ctx.theme)];
                lines.extend(muted(&refusal(code), width, ctx));
                return (lines, None, None);
            }
            Load::Idle | Load::Loading | Load::NotReady => {
                let reading = format!("Reading connectors{}", ctx.glyphs.ellipsis);
                return (vec![styled(reading, Role::Muted, &ctx.theme)], None, None);
            }
        };
        match &self.open {
            Open::Add(form) => return self.add_lines(form, width, ctx),
            Open::Key(form) => return self.key_lines(form, width, ctx),
            Open::None | Open::Trust { .. } | Open::Refused { .. } | Open::Remove { .. } => {}
        }
        let (lines, spans) = match (&self.detail, self.opened()) {
            (Some(_), Some(connector)) => self.detail_lines(connector, width, ctx),
            (Some(name), None) => {
                let missing = format!("No connector named {}.", safe(name));
                (vec![styled(missing, Role::Warning, &ctx.theme)], vec![(0, 0)])
            }
            (None, _) => self.list_lines(view, width, ctx),
        };
        let at = if self.detail.is_some() {
            self.secret
        } else {
            self.selected
        };
        assert!(at < spans.len(), "every row has its lines");
        (lines, Some(spans[at]), None)
    }

    fn failure_lines(&self, lines: &mut Vec<Line<'static>>, width: usize, ctx: &Ctx) {
        if let Some(failure) = &self.failure {
            lines.extend(
                wrap(failure, width)
                    .into_iter()
                    .map(|part| styled(part, Role::Error, &ctx.theme)),
            );
        }
    }

    fn list_lines(
        &self,
        view: &ConnectorsView,
        width: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let theme = &ctx.theme;
        let mut lines = vec![styled(TITLE, Role::Heading, theme)];
        lines.extend(muted(PURPOSE, width, ctx));
        lines.push(Line::default());
        lines.push(styled(CATALOG, Role::Heading, theme));
        lines.extend(muted(CATALOG_ABOUT, width, ctx));
        self.failure_lines(&mut lines, width, ctx);
        let mut spans = Vec::new();
        for (at, connector) in view.connectors.iter().enumerate() {
            let first = if at == 0 { 0 } else { lines.len() };
            lines.extend(self.catalog_row(at, connector, width, ctx));
            spans.push((first, lines.len() - 1));
        }
        if view.more {
            let more = format!("Showing the first {} connectors.", view.connectors.len());
            lines.push(styled(more, Role::Muted, theme));
        }
        lines.push(Line::default());
        lines.push(styled(HARNESSES, Role::Heading, theme));
        lines.extend(muted(HARNESSES_ABOUT, width, ctx));
        lines.extend(
            HarnessKind::ALL
                .iter()
                .map(|kind| Line::raw(reach(*kind).to_owned())),
        );
        lines.push(Line::default());
        spans.extend(self.settings_lines(&mut lines, width, ctx));
        assert_eq!(spans.len(), view.connectors.len() + super::SETTING_ROWS);
        (lines, spans)
    }

    fn catalog_row(
        &self,
        at: usize,
        connector: &ConnectorView,
        width: usize,
        ctx: &Ctx,
    ) -> Vec<Line<'static>> {
        let theme = &ctx.theme;
        let (word, role, line) = state(connector);
        let selected = self.rows()[self.selected] == Row::Connector(at);
        let head = row(
            width.saturating_sub(2),
            Line::raw(safe(&connector.title)),
            styled(word, role, theme),
            ctx.glyphs,
        );
        let mut lines = vec![list_row(head, width, selected, true, theme, ctx.glyphs)];
        let mut texts = vec![origin_line(connector)];
        texts.extend(line);
        if refused(connector) {
            texts.push(REFUSED_FILE.to_owned());
        }
        if !connector.description.is_empty() {
            texts.push(safe(&connector.description));
        }
        texts.extend(connector.runs.as_ref().map(reaches_line));
        for text in texts {
            lines.extend(under(&text, width, ctx));
        }
        assert!(lines.len() >= 2, "a title and its origin");
        lines
    }

    fn settings_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        width: usize,
        ctx: &Ctx,
    ) -> Vec<(usize, usize)> {
        let theme = &ctx.theme;
        let current = self.rows()[self.selected];
        let heading = lines.len();
        lines.push(styled(SETTINGS, Role::Heading, theme));
        let room = width.saturating_sub(2);
        let settings = [
            (
                Row::Write,
                choices(WRITE_LABEL, &WRITE_CHOICES, self.write, room, ctx),
                WRITE_HELP,
            ),
            (
                Row::Wait,
                choices(WAIT_LABEL, &WAIT_CHOICES, self.wait, room, ctx),
                WAIT_HELP,
            ),
            (
                Row::Native,
                choices(NATIVE_LABEL, &NATIVE_CHOICES, self.native, room, ctx),
                NATIVE_HELP,
            ),
            (
                Row::Size,
                choices(SIZE_LABEL, &SIZE_CHOICES, self.size_cap, room, ctx),
                SIZE_HELP,
            ),
        ];
        let last = settings.len() - 1;
        let mut spans = Vec::new();
        for (at, (row, picks, help)) in settings.into_iter().enumerate() {
            let first = if at == 0 { heading } else { lines.len() };
            for (index, line) in picks.into_iter().enumerate() {
                let selected = index == 0 && current == row;
                lines.push(list_row(line, width, selected, true, theme, ctx.glyphs));
            }
            lines.extend(under(help, width, ctx));
            if at == last {
                lines.push(Line::raw(format!("  {LOCKED_LABEL}  {LOCKED_VALUE}")));
                lines.extend(under(LOCKED_REASON, width, ctx));
            }
            spans.push((first, lines.len() - 1));
        }
        assert_eq!(spans.len(), super::SETTING_ROWS);
        spans
    }

    fn detail_lines(
        &self,
        connector: &ConnectorView,
        width: usize,
        ctx: &Ctx,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let theme = &ctx.theme;
        let (word, role, line) = state(connector);
        let title = styled(safe(&connector.title), Role::Heading, theme);
        let mut lines = vec![row(width, title, styled(word, role, theme), ctx.glyphs)];
        lines.extend(muted(&origin_line(connector), width, ctx));
        if !connector.description.is_empty() {
            let description = wrap(&safe(&connector.description), width);
            lines.extend(description.into_iter().map(Line::raw));
        }
        if let Some(line) = line {
            lines.extend(wrap(&line, width).into_iter().map(|part| styled(part, role, theme)));
        }
        if refused(connector) {
            lines.extend(wrap(REFUSED_FILE, width).into_iter().map(Line::raw));
        }
        if let Some(runs) = &connector.runs {
            lines.extend(wrap(&reaches_line(runs), width).into_iter().map(Line::raw));
        }
        if let Some(RunsView::Connector { url, .. }) = &connector.runs {
            let address = format!("Address: {}", safe(url));
            lines.extend(wrap(&address, width).into_iter().map(Line::raw));
        }
        if let Some(way) = chosen_way(connector) {
            let chosen = format!("{SIGN_IN_WITH}: {}", way_word(way));
            lines.extend(wrap(&chosen, width).into_iter().map(Line::raw));
            lines.extend(muted(SIGN_IN_WITH_HELP, width, ctx));
        }
        self.failure_lines(&mut lines, width, ctx);
        lines.push(Line::default());
        let secrets = if keys_shown(connector) {
            connector.secrets.as_slice()
        } else {
            &[]
        };
        let mut spans = Vec::new();
        for (at, secret) in secrets.iter().enumerate() {
            let first = if at == 0 { 0 } else { lines.len() };
            spans.push((first, lines.len()));
            let text = Line::raw(secret_line(secret));
            lines.push(list_row(text, width, at == self.secret, true, theme, ctx.glyphs));
        }
        if spans.is_empty() {
            spans.push((0, lines.len() - 1));
        }
        assert!(!lines.is_empty() && !spans.is_empty());
        (lines, spans)
    }

    fn add_lines(&self, form: &AddForm, width: usize, ctx: &Ctx) -> Content {
        let theme = &ctx.theme;
        let mut lines = vec![styled(ADD_TITLE, Role::Heading, theme)];
        lines.extend(muted(ADD_HELP, width, ctx));
        self.failure_lines(&mut lines, width, ctx);
        let mut cursor = None;
        for (field, focused) in [(&form.yaml, !form.on_path), (&form.path, form.on_path)] {
            let top = u16::try_from(lines.len()).unwrap_or(u16::MAX);
            let (drawn, at) = field.lines(width, focused, theme, ctx.glyphs);
            if let Some(at) = at {
                cursor = Some(Position::new(at.x, at.y.saturating_add(top)));
            }
            lines.extend(drawn);
        }
        assert!(cursor.is_none() || lines.len() > 2);
        (lines, None, cursor)
    }

    fn key_lines(&self, form: &KeyForm, width: usize, ctx: &Ctx) -> Content {
        let theme = &ctx.theme;
        let heading = format!("Key for {}", safe(&form.secret));
        let mut lines = vec![styled(heading, Role::Heading, theme)];
        let sends = format!(
            "{} sends this key to {}. Only the engine keeps it.",
            form.title,
            form.hosts.join(", ")
        );
        lines.extend(muted(&sends, width, ctx));
        self.failure_lines(&mut lines, width, ctx);
        let top = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let (drawn, at) = form.key.lines(width, true, theme, ctx.glyphs);
        let cursor = at.map(|at| Position::new(at.x, at.y.saturating_add(top)));
        lines.extend(drawn);
        lines.push(styled(KEY_HELP, Role::Muted, theme));
        assert!(lines.len() >= 4, "the heading, the line, the field and the helper");
        (lines, None, cursor)
    }

    /// The trust screen's lines before any scroll: a failure first, then every trust line
    /// wrapped to the box. Inputs: the trust view and the context. Output: the lines.
    pub(super) fn trust_body(&self, trust: &TrustView, ctx: &Ctx) -> Vec<Line<'static>> {
        let room = usize::from(MODAL_WIDTH) - 4;
        let mut body = Vec::new();
        self.failure_lines(&mut body, room, ctx);
        let lines = trust_lines(trust);
        body.extend(lines.iter().flat_map(|text| wrap(text, room)).map(Line::raw));
        assert!(body.len() >= lines.len());
        body
    }

    /// The trust screen in its box (rv-ds241 T5 F1): the lines that fit from the first one
    /// scrolled to, a line saying how many are below, and y only once the end was on
    /// screen. Inputs: whether a pasted manifest is added, the trust view and the context.
    /// Output: the title, the body and the hints.
    fn trust_dialog(
        &self,
        adding: bool,
        trust: &TrustView,
        ctx: &Ctx,
    ) -> (String, Vec<Line<'static>>, Vec<Hint>) {
        let body = self.trust_body(trust, ctx);
        let rows = dialog_rows(ctx);
        let (top, shown, below) = window(self.trust_top, body.len(), rows);
        let mut lines: Vec<Line<'static>> = body.into_iter().skip(top).take(shown).collect();
        if below > 0 {
            let more = match below {
                1 => "1 more line below".to_owned(),
                _ => format!("{below} more lines below"),
            };
            lines.push(styled(more, Role::Muted, &ctx.theme));
        }
        let mut hints = Vec::new();
        if top > 0 || below > 0 {
            let keys = format!("{}{}", ctx.glyphs.up, ctx.glyphs.down);
            hints.push(hint(&keys, "Scroll"));
        }
        if self.trust_end || below == 0 {
            hints.push(hint("y", "Confirm"));
        }
        hints.push(hint("Esc", "Cancel"));
        let title = if adding {
            format!("Trust {}?", safe(&trust.title))
        } else {
            format!("Review {}", safe(&trust.title))
        };
        assert!(!lines.is_empty() && lines.len() <= rows);
        (title, lines, hints)
    }

    pub(super) fn dialog(&self, ctx: &Ctx) -> Option<(Modal, Vec<Hint>)> {
        let room = usize::from(MODAL_WIDTH) - 4;
        let mut body = Vec::new();
        self.failure_lines(&mut body, room, ctx);
        let (title, hints) = match &self.open {
            Open::Trust { yaml, trust } => {
                let (title, lines, hints) = self.trust_dialog(yaml.is_some(), trust, ctx);
                body = lines;
                (title, hints)
            }
            Open::Refused { yaml, problem } => {
                body.extend(
                    wrap(&problem_text(problem), room)
                        .into_iter()
                        .map(|part| styled(part, Role::Error, &ctx.theme)),
                );
                let mut hints = Vec::new();
                if yaml.is_some() {
                    hints.push(hint("e", "Edit manifest"));
                }
                hints.push(hint("Esc", "Cancel"));
                let title = if yaml.is_some() {
                    REFUSED_PASTED
                } else {
                    REFUSED_LISTED
                };
                (title.to_owned(), hints)
            }
            Open::Remove { title, .. } => {
                body.extend(wrap(REMOVE_BODY, room).into_iter().map(Line::raw));
                let hints = vec![hint("y", "Remove"), hint("Esc", "Cancel")];
                (format!("Remove {title}?"), hints)
            }
            Open::None | Open::Add(_) | Open::Key(_) => return None,
        };
        assert!(!body.is_empty(), "a dialog says something");
        let hints = if self.pending.is_some() {
            Vec::new()
        } else {
            hints
        };
        let modal = Modal {
            width: MODAL_WIDTH,
            title,
            body,
            top: None,
        };
        Some((modal, hints))
    }
}
