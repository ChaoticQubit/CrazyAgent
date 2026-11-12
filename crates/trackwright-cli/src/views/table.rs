use super::text::{pad, safe, width};
use super::wire::{Refused, Row, Rows, ask};
use super::{Client, Key, Notice, Outcome, Theme, fit, put, status_label, title_row};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;
use std::cell::Cell;
use trackwright_core::model::{OpKind, ProjectKey, TICKETS_PER_PROJECT_MAX};

const HEAD_ROWS: u16 = 3;
const ID_CELLS_MAX: usize = 14;
const STATUS_CELLS: usize = 12;
const PRIORITY_CELLS: usize = 4;
const ASSIGNEE_CELLS: usize = 16;
const MARKER_CELLS: usize = 3;

/// The list and ready pages: one page of ticket rows per reply, the next page fetched
/// when the selection moves past the last row.
pub struct Table {
    op: OpKind,
    project: ProjectKey,
    rows: Vec<Row>,
    cursor: Option<String>,
    selected: usize,
    top: Cell<usize>,
    visible: Cell<usize>,
}

impl Table {
    /// An empty table. Inputs: `OpKind::List` or `OpKind::Ready`, and the project.
    /// Output: the table; `load` fills it.
    pub fn new(op: OpKind, project: &ProjectKey) -> Self {
        assert!(matches!(op, OpKind::List | OpKind::Ready));
        Self {
            op,
            project: project.clone(),
            rows: Vec::new(),
            cursor: None,
            selected: 0,
            top: Cell::new(0),
            visible: Cell::new(1),
        }
    }

    /// Reads the first page again, keeping the selected ticket selected when it is still
    /// on it. Input: the client. Output: Ok, or the refusal.
    pub fn load(&mut self, client: &mut dyn Client) -> Result<(), Refused> {
        let (page, cursor) =
            ask::<Rows>(client, self.op, json!({ "project": self.project.as_str() }))?;
        let kept = self.rows.get(self.selected).map(|row| row.id.clone());
        self.rows = page.rows;
        self.rows.truncate(TICKETS_PER_PROJECT_MAX);
        self.cursor = cursor;
        self.selected = kept
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
        Ok(())
    }

    fn more(&mut self, client: &mut dyn Client) -> Result<bool, Refused> {
        let Some(cursor) = self.cursor.clone() else {
            return Ok(false);
        };
        if self.rows.len() >= TICKETS_PER_PROJECT_MAX {
            return Ok(false);
        }
        let (page, next) = ask::<Rows>(
            client,
            self.op,
            json!({ "project": self.project.as_str(), "cursor": cursor }),
        )?;
        let before = self.rows.len();
        self.rows.extend(page.rows);
        self.rows.truncate(TICKETS_PER_PROJECT_MAX);
        self.cursor = next;
        Ok(self.rows.len() > before)
    }

    /// Acts on a key: Up and Down select (Down past the last row fetches the next page),
    /// PageUp and PageDown jump a screen, Enter opens the selected ticket. Inputs: the key
    /// and the client. Output: what happened.
    pub fn key(&mut self, key: Key, client: &mut dyn Client) -> Outcome {
        let last = self.rows.len().saturating_sub(1);
        let screen = self.visible.get().max(1);
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down if self.selected < last => self.selected += 1,
            Key::Down => match self.more(client) {
                Ok(true) => self.selected += 1,
                Ok(false) => {}
                Err(refused) => return Outcome::Said(Notice::refused(&refused)),
            },
            Key::PageUp => self.selected = self.selected.saturating_sub(screen),
            Key::PageDown => self.selected = (self.selected + screen).min(last),
            Key::Enter => {
                return match self.rows.get(self.selected) {
                    Some(row) => Outcome::Open(row.id.clone()),
                    None => Outcome::Used,
                };
            }
            _ => return Outcome::Unused,
        }
        assert!(self.rows.is_empty() || self.selected < self.rows.len());
        Outcome::Used
    }

    /// Draws the title row, the column header and the rows that fit. Inputs: the area,
    /// the buffer and the theme.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let more = if self.cursor.is_some() { "+" } else { "" };
        let count = self.rows.len();
        let (name, total, empty) = match self.op {
            OpKind::List => (
                "Tickets",
                format!(
                    "{count}{more} ticket{}",
                    if count == 1 && more.is_empty() {
                        ""
                    } else {
                        "s"
                    }
                ),
                "   No tickets yet.",
            ),
            _ => (
                "Ready",
                format!("{count}{more} ready"),
                "   Nothing is ready.",
            ),
        };
        title_row(area, buf, theme, &self.project, name, &total);
        let id_cells = self
            .rows
            .iter()
            .map(|row| width(&safe(&row.id)))
            .max()
            .unwrap_or(0)
            .clamp(2, ID_CELLS_MAX)
            + 2;
        let limit = usize::from(area.width);
        let fixed = MARKER_CELLS + id_cells + STATUS_CELLS + PRIORITY_CELLS + ASSIGNEE_CELLS;
        assert!(limit >= fixed + 9);
        let title_cells = limit - fixed;
        let header = format!(
            "{}{}{}{}{}{}",
            " ".repeat(MARKER_CELLS),
            pad("ID", id_cells),
            pad("Title", title_cells),
            pad("Status", STATUS_CELLS),
            pad("Pri", PRIORITY_CELLS),
            pad("Assignee", ASSIGNEE_CELLS),
        );
        put(area, buf, 2, &header, theme.muted);
        if self.rows.is_empty() {
            put(area, buf, HEAD_ROWS, empty, theme.muted);
            return;
        }
        let visible = usize::from(area.height.saturating_sub(HEAD_ROWS)).max(1);
        let top = fit(self.top.get(), self.selected, visible);
        self.top.set(top);
        self.visible.set(visible);
        for (offset, (index, row)) in self
            .rows
            .iter()
            .enumerate()
            .skip(top)
            .take(visible)
            .enumerate()
        {
            let chosen = index == self.selected;
            let elided = if row.elided { "…" } else { "" };
            let line = format!(
                "{}{}{} {} {} {}",
                if chosen { " > " } else { "   " },
                pad(&safe(&row.id), id_cells),
                pad(&format!("{}{elided}", safe(&row.title)), title_cells - 1),
                pad(status_label(row.status), STATUS_CELLS - 1),
                pad(&format!("P{}", row.priority), PRIORITY_CELLS - 1),
                pad(&safe(row.assignee.as_deref().unwrap_or("")), ASSIGNEE_CELLS),
            );
            let style = if chosen { theme.selected } else { theme.text };
            let row_index = HEAD_ROWS + u16::try_from(offset).expect("visible rows fit a u16");
            put(area, buf, row_index, &line, style);
        }
    }
}
