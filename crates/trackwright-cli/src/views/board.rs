use super::text::{pad, safe};
use super::wire::{Refused, Row, Rows, Shown, ask};
use super::{Client, Key, Notice, Outcome, Theme, Tone, fit, put, status_label, title_row};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;
use std::cell::Cell;
use trackwright_core::model::{OpKind, ProjectKey, TicketStatus, codes};

const COLUMNS: [TicketStatus; 4] = [
    TicketStatus::Todo,
    TicketStatus::InProgress,
    TicketStatus::InReview,
    TicketStatus::Done,
];
const HEAD_ROWS: u16 = 4;
const CARD_ROWS: usize = 4;

struct Column {
    status: TicketStatus,
    rows: Vec<Row>,
    more: bool,
    selected: usize,
    top: Cell<usize>,
}

/// The board page: one column per working status, each holding the first `list` page of
/// that status. A move to the next or previous column carries the version `show` read
/// when the card was selected: into Done it is `close`, out of Done `reopen`, otherwise
/// `move`, so the core's close policy decides the first two.
pub struct Board {
    project: ProjectKey,
    columns: Vec<Column>,
    focus: usize,
}

impl Board {
    /// An empty board. Input: the project. Output: the board; `load` fills it.
    pub fn new(project: &ProjectKey) -> Self {
        Self {
            project: project.clone(),
            columns: COLUMNS
                .iter()
                .map(|&status| Column {
                    status,
                    rows: Vec::new(),
                    more: false,
                    selected: 0,
                    top: Cell::new(0),
                })
                .collect(),
            focus: 0,
        }
    }

    /// Reads every column again, keeping each column's selected ticket selected while it
    /// stays in that column, then reads the selected card's version. Input: the client.
    /// Output: Ok, or the first refusal.
    pub fn load(&mut self, client: &mut dyn Client) -> Result<(), Refused> {
        self.read_columns(client)?;
        self.read_selected(client)
    }

    fn read_columns(&mut self, client: &mut dyn Client) -> Result<(), Refused> {
        for column in &mut self.columns {
            let (page, cursor) = ask::<Rows>(
                client,
                OpKind::List,
                json!({ "project": self.project.as_str(), "status": column.status.as_str() }),
            )?;
            let kept = column.rows.get(column.selected).map(|row| row.id.clone());
            column.rows = page.rows;
            column.more = cursor.is_some();
            column.selected = kept
                .and_then(|id| column.rows.iter().position(|row| row.id == id))
                .unwrap_or(0)
                .min(column.rows.len().saturating_sub(1));
        }
        Ok(())
    }

    fn read_selected(&mut self, client: &mut dyn Client) -> Result<(), Refused> {
        let column = &mut self.columns[self.focus];
        let Some(row) = column.rows.get_mut(column.selected) else {
            return Ok(());
        };
        if row.version.is_none() {
            let (shown, _) = ask::<Shown>(client, OpKind::Show, json!({ "id": row.id }))?;
            row.version = Some(shown.ticket.version);
        }
        Ok(())
    }

    /// Acts on a key: Left and Right change column, Up and Down select a card, `<` and
    /// `>` move the selected card one column, Enter opens it. Inputs: the key and the
    /// client. Output: what happened.
    pub fn key(&mut self, key: Key, client: &mut dyn Client) -> Outcome {
        let count = self.columns.len();
        let column = &mut self.columns[self.focus];
        let last = column.rows.len().saturating_sub(1);
        match key {
            Key::Left if self.focus > 0 => self.focus -= 1,
            Key::Right if self.focus + 1 < count => self.focus += 1,
            Key::Up => column.selected = column.selected.saturating_sub(1),
            Key::Down => column.selected = (column.selected + 1).min(last),
            Key::Enter => {
                return match column.rows.get(column.selected) {
                    Some(row) => Outcome::Open(row.id.clone()),
                    None => Outcome::Used,
                };
            }
            Key::Char('>') => return self.shift(client, true),
            Key::Char('<') => return self.shift(client, false),
            _ => return Outcome::Unused,
        }
        match self.read_selected(client) {
            Ok(()) => Outcome::Used,
            Err(refused) => Outcome::Said(Notice::refused(&refused)),
        }
    }

    fn shift(&mut self, client: &mut dyn Client, forward: bool) -> Outcome {
        let from = self.focus;
        let to = match (forward, from) {
            (true, from) if from + 1 == self.columns.len() => {
                return Outcome::Said(Notice::info(format!(
                    "{} is the last column.",
                    status_label(self.columns[from].status)
                )));
            }
            (false, 0) => {
                return Outcome::Said(Notice::info(format!(
                    "{} is the first column.",
                    status_label(self.columns[from].status)
                )));
            }
            (true, from) => from + 1,
            (false, from) => from - 1,
        };
        let Some(card) = self.columns[from]
            .rows
            .get(self.columns[from].selected)
            .cloned()
        else {
            return Outcome::Said(Notice::info("Nothing to move."));
        };
        let Some(version) = card.version else {
            return Outcome::Said(Notice::info(format!(
                "Select {} again to read its version.",
                safe(&card.id)
            )));
        };
        let status = self.columns[to].status;
        let op = if status.is_closed() {
            OpKind::Close
        } else if self.columns[from].status.is_closed() {
            OpKind::Reopen
        } else {
            OpKind::Move
        };
        let moved = ask::<serde_json::Value>(
            client,
            op,
            json!({
                "id": card.id,
                "status": status.as_str(),
                "version": version,
            }),
        );
        match moved {
            Ok(_) => {
                self.focus = to;
                let loaded = self.read_columns(client);
                let column = &mut self.columns[to];
                if let Some(index) = column.rows.iter().position(|row| row.id == card.id) {
                    column.selected = index;
                }
                match loaded.and_then(|()| self.read_selected(client)) {
                    Ok(()) => Outcome::Used,
                    Err(refused) => Outcome::Said(Notice::refused(&refused)),
                }
            }
            Err(refused) if refused.code == codes::STALE_VERSION => {
                let said = Notice {
                    tone: Tone::Error,
                    text: format!(
                        "{} changed since it was read. Board reloaded; move it again if still wanted.",
                        safe(&card.id)
                    ),
                };
                match self.load(client) {
                    Ok(()) => Outcome::Said(said),
                    Err(refused) => Outcome::Said(Notice::refused(&refused)),
                }
            }
            Err(refused) => Outcome::Said(Notice::refused(&refused)),
        }
    }

    /// Draws the title row, the column headers and the cards that fit. Inputs: the area,
    /// the buffer and the theme.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        title_row(area, buf, theme, &self.project, "Board", "");
        let cell = usize::from(area.width) / self.columns.len();
        assert!(cell >= 12);
        let content = cell - 2;
        let mut heads = String::new();
        let mut rules = String::new();
        for (index, column) in self.columns.iter().enumerate() {
            let more = if column.more { "+" } else { "" };
            let head = format!(
                "{} ({}{more})",
                status_label(column.status),
                column.rows.len()
            );
            heads.push_str(&format!(" {} ", pad(&head, content)));
            let rule = if index == self.focus { "━" } else { "─" };
            rules.push_str(&format!(" {} ", rule.repeat(content)));
        }
        put(area, buf, 2, &heads, theme.heading);
        put(area, buf, 3, &rules, theme.muted);
        let room = usize::from(area.height.saturating_sub(HEAD_ROWS));
        let fitting = ((room + 1) / CARD_ROWS).max(1);
        for (index, column) in self.columns.iter().enumerate() {
            let x = area.x + u16::try_from(index * cell).expect("board columns fit a u16");
            let strip = Rect {
                x,
                y: area.y + HEAD_ROWS,
                width: u16::try_from(cell).expect("a board column fits a u16"),
                height: area.height.saturating_sub(HEAD_ROWS),
            };
            if column.rows.is_empty() {
                put(
                    strip,
                    buf,
                    0,
                    &format!(" {}", pad("Empty", content)),
                    theme.muted,
                );
                continue;
            }
            let top = fit(column.top.get(), column.selected, fitting);
            column.top.set(top);
            for (slot, (card_index, row)) in column
                .rows
                .iter()
                .enumerate()
                .skip(top)
                .take(fitting)
                .enumerate()
            {
                let chosen = index == self.focus && card_index == column.selected;
                let style = if chosen { theme.selected } else { theme.text };
                let first = u16::try_from(slot * CARD_ROWS).expect("card rows fit a u16");
                let lead = if chosen { ">" } else { " " };
                let heading = format!("{}  P{}", safe(&row.id), row.priority);
                put(
                    strip,
                    buf,
                    first,
                    &format!("{lead}{} ", pad(&heading, content)),
                    style,
                );
                let elided = if row.elided { "…" } else { "" };
                let title = format!("{}{elided}", safe(&row.title));
                put(
                    strip,
                    buf,
                    first + 1,
                    &format!(" {} ", pad(&title, content)),
                    style,
                );
                let assignee = safe(row.assignee.as_deref().unwrap_or(""));
                put(
                    strip,
                    buf,
                    first + 2,
                    &format!(" {} ", pad(&assignee, content)),
                    style,
                );
            }
        }
    }
}
