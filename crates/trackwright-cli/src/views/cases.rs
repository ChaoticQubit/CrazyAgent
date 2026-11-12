use super::text::{pad, safe};
use super::wire::{Refused, ask};
use super::{Client, Key, Theme, fit, put, title_row};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde::Deserialize;
use serde_json::json;
use std::cell::Cell;
use trackwright_core::model::{OpKind, ProjectKey};
use trackwright_core::tests_model::{CASES_PER_PROJECT_MAX, CaseRow, TestId};

const HEAD_ROWS: u16 = 3;
const MARKER_CELLS: usize = 3;
const ID_CELLS: usize = 12;
const KIND_CELLS: usize = 13;
const STATUS_CELLS: usize = 12;
const LATEST_CELLS: usize = 9;

#[derive(Debug, Deserialize)]
struct Cases {
    cases: Vec<CaseRow>,
}

/// The case list of one project: one page of case rows per reply, the next page fetched
/// when the selection moves past the last row. The host opens the selected case.
pub struct CaseList {
    project: ProjectKey,
    rows: Vec<CaseRow>,
    cursor: Option<String>,
    selected: usize,
    top: Cell<usize>,
    visible: Cell<usize>,
}

impl CaseList {
    /// An empty list. Input: the project. Output: the list; `load` fills it.
    pub fn new(project: &ProjectKey) -> Self {
        Self {
            project: project.clone(),
            rows: Vec::new(),
            cursor: None,
            selected: 0,
            top: Cell::new(0),
            visible: Cell::new(1),
        }
    }

    /// Reads the first page again, keeping the selected case selected when it is still on
    /// it. Input: the client. Output: Ok, or the refusal.
    pub fn load(&mut self, client: &mut dyn Client) -> Result<(), Refused> {
        let input = json!({ "project": self.project.as_str() });
        let (page, cursor) = ask::<Cases>(client, OpKind::CaseList, input)?;
        let kept = self.selected().cloned();
        self.rows = page.cases;
        self.rows.truncate(CASES_PER_PROJECT_MAX);
        self.cursor = cursor;
        self.selected = kept
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
        assert!(self.rows.is_empty() || self.selected < self.rows.len());
        Ok(())
    }

    fn more(&mut self, client: &mut dyn Client) -> Result<bool, Refused> {
        let Some(cursor) = self.cursor.clone() else {
            return Ok(false);
        };
        if self.rows.len() >= CASES_PER_PROJECT_MAX {
            return Ok(false);
        }
        let input = json!({ "project": self.project.as_str(), "cursor": cursor });
        let (page, next) = ask::<Cases>(client, OpKind::CaseList, input)?;
        let before = self.rows.len();
        self.rows.extend(page.cases);
        self.rows.truncate(CASES_PER_PROJECT_MAX);
        self.cursor = next;
        assert!(self.rows.len() >= before);
        Ok(self.rows.len() > before)
    }

    /// The selected case. Output: its id, or None while the list is empty.
    pub fn selected(&self) -> Option<&TestId> {
        self.rows.get(self.selected).map(|row| &row.id)
    }

    /// Acts on a key: Up and Down select (Down past the last row fetches the next page),
    /// PageUp and PageDown jump a screen. Enter is left to the host, which opens
    /// `selected`. Inputs: the key and the client. Output: whether it was used, or the
    /// refusal of the next page.
    pub fn key(&mut self, key: Key, client: &mut dyn Client) -> Result<bool, Refused> {
        let last = self.rows.len().saturating_sub(1);
        let screen = self.visible.get().max(1);
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down if self.selected < last => self.selected += 1,
            Key::Down => {
                if self.more(client)? {
                    self.selected += 1;
                }
            }
            Key::PageUp => self.selected = self.selected.saturating_sub(screen),
            Key::PageDown => self.selected = (self.selected + screen).min(last),
            _ => return Ok(false),
        }
        assert!(self.rows.is_empty() || self.selected < self.rows.len());
        Ok(true)
    }

    /// Draws the title row, the column header and the rows that fit. Inputs: the area,
    /// the buffer and the theme.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let more = if self.cursor.is_some() { "+" } else { "" };
        let count = self.rows.len();
        let plural = if count == 1 && more.is_empty() {
            ""
        } else {
            "s"
        };
        let total = format!("{count}{more} case{plural}");
        title_row(area, buf, theme, &self.project, "Cases", &total);
        let fixed = MARKER_CELLS + ID_CELLS + KIND_CELLS + STATUS_CELLS + LATEST_CELLS;
        let title_cells = usize::from(area.width).saturating_sub(fixed);
        let header = format!(
            "{}{}{}{}{}{}",
            " ".repeat(MARKER_CELLS),
            pad("ID", ID_CELLS),
            pad("Title", title_cells),
            pad("Kind", KIND_CELLS),
            pad("Status", STATUS_CELLS),
            pad("Latest", LATEST_CELLS),
        );
        put(area, buf, 2, &header, theme.muted);
        if self.rows.is_empty() {
            put(area, buf, HEAD_ROWS, "   No cases yet.", theme.muted);
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
            let line = case_line(row, chosen, title_cells);
            let style = if chosen { theme.selected } else { theme.text };
            let row_index = HEAD_ROWS + u16::try_from(offset).expect("visible rows fit a u16");
            put(area, buf, row_index, &line, style);
        }
    }
}

fn case_line(row: &CaseRow, chosen: bool, title_cells: usize) -> String {
    let elided = if row.elided { "…" } else { "" };
    format!(
        "{}{}{}{}{}{}",
        if chosen { " > " } else { "   " },
        pad(&row.id.to_string(), ID_CELLS),
        pad(&format!("{}{elided}", safe(&row.title)), title_cells),
        pad(row.kind.map_or("-", |kind| kind.as_str()), KIND_CELLS),
        pad(
            row.status.map_or("-", |status| status.as_str()),
            STATUS_CELLS
        ),
        pad(
            row.latest.map_or("none", |latest| latest.as_str()),
            LATEST_CELLS
        ),
    )
}
