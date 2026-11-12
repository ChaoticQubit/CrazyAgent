use super::text::{pad, safe};
use super::wire::{Refused, ask, unreadable};
use super::{Client, Key, Theme, put, title_row};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cell::Cell;
use trackwright_core::coverage::{CoverageCase, CoverageOf, CoverageRow};
use trackwright_core::model::{OpKind, ProjectKey};

const ROWS_SHOWN_MAX: usize = 2_000;
const PAGES_MAX: usize = ROWS_SHOWN_MAX;
const HEAD_ROWS: u16 = 2;
const LABEL_CELLS: usize = 10;
const STATE_CELLS: usize = 8;
const COUNT_CELLS: usize = 11;
const CASE_CELLS: usize = 10;
const PROVES_CELLS: usize = 8;
const LATEST_CELLS: usize = 22;

#[derive(Debug, Deserialize)]
struct Covered {
    criteria_total: u32,
    uncovered_total: u32,
    #[serde(default)]
    rows: Vec<CoverageRow>,
}

/// One ticket's or one release's coverage: each acceptance criterion with how many cases
/// cover it (or `uncovered`), and under it each case with its latest result, scrolled as
/// one column under a title that counts the uncovered criteria.
pub struct Coverage {
    project: ProjectKey,
    scope: String,
    covered: Covered,
    more: bool,
    scroll: usize,
    bottom: Cell<usize>,
}

impl Coverage {
    /// Reads coverage with `coverage`, following the cursor up to 2000 criteria; each later
    /// page must add rows and move the cursor. Inputs: the project, the ticket or release
    /// and the client. Output: the view, or the refusal.
    pub fn load(
        project: &ProjectKey,
        of: &CoverageOf,
        client: &mut dyn Client,
    ) -> Result<Self, Refused> {
        let (mut covered, mut cursor) =
            ask::<Covered>(client, OpKind::Coverage, input(project, of, None))?;
        let mut pages = 1;
        while covered.rows.len() < ROWS_SHOWN_MAX {
            let Some(at) = cursor.clone() else { break };
            if pages == PAGES_MAX {
                return Err(unreadable(OpKind::Coverage));
            }
            let page = input(project, of, Some(&at));
            let (more, next) = ask::<Covered>(client, OpKind::Coverage, page)?;
            if more.rows.is_empty() || next.as_deref() == Some(at.as_str()) {
                return Err(unreadable(OpKind::Coverage));
            }
            covered.rows.extend(more.rows);
            cursor = next;
            pages += 1;
        }
        assert!(pages <= PAGES_MAX);
        covered.rows.truncate(ROWS_SHOWN_MAX);
        if covered.uncovered_total > covered.criteria_total {
            return Err(unreadable(OpKind::Coverage));
        }
        let scope = match of {
            CoverageOf::Ticket(ticket) => ticket.to_string(),
            CoverageOf::Version(name) => safe(name),
        };
        Ok(Self {
            project: project.clone(),
            scope,
            covered,
            more: cursor.is_some(),
            scroll: 0,
            bottom: Cell::new(0),
        })
    }

    /// Acts on a key: Up, Down, PageUp and PageDown scroll, no further than the last
    /// drawn line. Input: the key. Output: true when used.
    pub fn key(&mut self, key: Key) -> bool {
        let bottom = self.bottom.get();
        match key {
            Key::Up => self.scroll = self.scroll.saturating_sub(1),
            Key::Down => self.scroll = (self.scroll + 1).min(bottom),
            Key::PageUp => self.scroll = self.scroll.saturating_sub(10),
            Key::PageDown => self.scroll = (self.scroll + 10).min(bottom),
            _ => return false,
        }
        true
    }

    /// Draws the title row and the criterion and case lines that fit from the scroll
    /// position. Inputs: the area, the buffer and the theme.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let covered = &self.covered;
        let more = if self.more { "+" } else { "" };
        let count = format!(
            "{} of {}{more} uncovered",
            covered.uncovered_total, covered.criteria_total
        );
        let name = format!("Coverage {}", self.scope);
        title_row(area, buf, theme, &self.project, &name, &count);
        let lines = self.lines(usize::from(area.width));
        let visible = usize::from(area.height.saturating_sub(HEAD_ROWS));
        self.bottom.set(lines.len().saturating_sub(visible));
        let start = self.scroll.min(self.bottom.get());
        for (offset, (text, muted)) in lines.iter().skip(start).take(visible).enumerate() {
            let style = if *muted { theme.muted } else { theme.text };
            let row = HEAD_ROWS + u16::try_from(offset).expect("visible rows fit a u16");
            put(area, buf, row, text, style);
        }
    }

    fn lines(&self, limit: usize) -> Vec<(String, bool)> {
        let rows = &self.covered.rows;
        if rows.is_empty() {
            return vec![("   No acceptance criteria in scope.".to_owned(), true)];
        }
        let text_cells = limit.saturating_sub(3 + LABEL_CELLS + STATE_CELLS + COUNT_CELLS);
        let title_cells = limit.saturating_sub(6 + CASE_CELLS + PROVES_CELLS + LATEST_CELLS);
        let mut lines = Vec::new();
        for row in rows {
            let label = format!("{}#{}", row.criterion.ticket, row.criterion.position);
            lines.push((
                format!(
                    "   {}{}{}{}",
                    pad(&label, LABEL_CELLS),
                    pad(&safe(&row.text), text_cells),
                    pad(row.state.as_str(), STATE_CELLS),
                    pad(&cases_count(row.cases_total), COUNT_CELLS),
                ),
                false,
            ));
            lines.extend(
                row.cases
                    .iter()
                    .map(|case| (case_line(case, title_cells), true)),
            );
            let listed = u32::try_from(row.cases.len()).expect("a row lists at most 10 cases");
            assert!(listed <= row.cases_total);
            if row.cases_total > listed {
                let rest = row.cases_total - listed;
                lines.push((format!("      and {rest} more"), true));
            }
        }
        lines
    }
}

fn input(project: &ProjectKey, of: &CoverageOf, cursor: Option<&str>) -> Value {
    let mut input = json!({ "project": project.as_str() });
    match of {
        CoverageOf::Ticket(ticket) => input["ticket"] = json!(ticket.to_string()),
        CoverageOf::Version(name) => input["release"] = json!(name),
    }
    if let Some(cursor) = cursor {
        input["cursor"] = json!(cursor);
    }
    input
}

fn cases_count(total: u32) -> String {
    match total {
        0 => "uncovered".to_owned(),
        1 => "1 case".to_owned(),
        _ => format!("{total} cases"),
    }
}

fn case_line(case: &CoverageCase, title_cells: usize) -> String {
    let latest = case.latest.as_ref().map_or_else(
        || "no result".to_owned(),
        |latest| format!("{} in {}", latest.outcome.as_str(), latest.run),
    );
    let proves = if case.proves { "proves" } else { "linked" };
    format!(
        "      {}{}{}{}",
        pad(&case.case.to_string(), CASE_CELLS),
        pad(&safe(&case.title), title_cells),
        pad(proves, PROVES_CELLS),
        pad(&latest, LATEST_CELLS),
    )
}
