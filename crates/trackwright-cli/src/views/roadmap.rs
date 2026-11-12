use super::text::{pad, safe};
use super::wire::{Plan, Refused, ask};
use super::{Client, Key, Theme, cycle_label, put, title_row, version_label};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::json;
use std::cell::Cell;
use trackwright_core::model::{OpKind, ProjectKey};

const HEAD_ROWS: u16 = 2;
const BAR_CELLS: usize = 10;
const TARGET_CELLS: usize = 12;
const STATUS_CELLS: usize = 10;
const COUNT_CELLS: usize = 22;
const CYCLE_CELLS: usize = 10;
const DATES_CELLS: usize = 26;

/// The roadmap page: versions in order with their targets and progress, then the cycles.
pub struct Roadmap {
    project: ProjectKey,
    plan: Plan,
    scroll: usize,
    bottom: Cell<usize>,
}

impl Roadmap {
    /// An empty roadmap. Input: the project. Output: the roadmap; `load` fills it.
    pub fn new(project: &ProjectKey) -> Self {
        Self {
            project: project.clone(),
            plan: Plan::default(),
            scroll: 0,
            bottom: Cell::new(0),
        }
    }

    /// Reads the roadmap again. Input: the client. Output: Ok, or the refusal.
    pub fn load(&mut self, client: &mut dyn Client) -> Result<(), Refused> {
        let (plan, _) = ask::<Plan>(
            client,
            OpKind::Roadmap,
            json!({ "project": self.project.as_str() }),
        )?;
        self.plan = plan;
        Ok(())
    }

    /// Acts on a key: Up and Down scroll, no further than the last drawn line. Input: the
    /// key. Output: true when used.
    pub fn key(&mut self, key: Key) -> bool {
        match key {
            Key::Up => self.scroll = self.scroll.saturating_sub(1),
            Key::Down => self.scroll = (self.scroll + 1).min(self.bottom.get()),
            _ => return false,
        }
        true
    }

    fn lines(&self, limit: usize) -> Vec<(String, bool)> {
        let fixed = 3 + TARGET_CELLS + STATUS_CELLS + BAR_CELLS + 3 + COUNT_CELLS;
        let name_cells = limit.saturating_sub(fixed).max(8);
        let mut lines = vec![(
            format!(
                "   {}{}{}Progress",
                pad("Version", name_cells),
                pad("Target", TARGET_CELLS),
                pad("Status", STATUS_CELLS),
            ),
            true,
        )];
        if self.plan.versions.is_empty() {
            lines.push(("   No versions yet.".to_owned(), true));
        }
        for version in &self.plan.versions {
            lines.push((
                format!(
                    "   {}{}{}{} {}",
                    pad(&safe(&version.name), name_cells),
                    pad(
                        &safe(version.target_day.as_deref().unwrap_or("-")),
                        TARGET_CELLS
                    ),
                    pad(version_label(version.status), STATUS_CELLS),
                    bar(version.closed, version.total),
                    pad(
                        &format!("{} of {} closed", version.closed, version.total),
                        COUNT_CELLS
                    ),
                ),
                false,
            ));
        }
        lines.push((String::new(), false));
        lines.push((
            format!(
                "   {}{}{}Progress",
                pad("Cycle", CYCLE_CELLS),
                pad("Dates", DATES_CELLS),
                pad("Status", STATUS_CELLS),
            ),
            true,
        ));
        if self.plan.cycles.is_empty() {
            lines.push(("   No cycles yet.".to_owned(), true));
        }
        for cycle in &self.plan.cycles {
            lines.push((
                format!(
                    "   {}{}{}{} {} of {} closed",
                    pad(&format!("Cycle {}", cycle.number), CYCLE_CELLS),
                    pad(
                        &format!("{} to {}", safe(&cycle.starts_day), safe(&cycle.ends_day)),
                        DATES_CELLS
                    ),
                    pad(cycle_label(cycle.status), STATUS_CELLS),
                    bar(cycle.closed, cycle.total),
                    cycle.closed,
                    cycle.total,
                ),
                false,
            ));
        }
        lines
    }

    /// Draws the title row and the roadmap lines that fit from the scroll position.
    /// Inputs: the area, the buffer and the theme.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let versions = self.plan.versions.len();
        title_row(
            area,
            buf,
            theme,
            &self.project,
            "Roadmap",
            &format!("{versions} version{}", if versions == 1 { "" } else { "s" }),
        );
        let lines = self.lines(usize::from(area.width));
        let visible = usize::from(area.height.saturating_sub(HEAD_ROWS));
        self.bottom.set(lines.len().saturating_sub(visible));
        let start = self.scroll.min(self.bottom.get());
        for (row, (text, muted)) in lines.iter().skip(start).take(visible).enumerate() {
            let style = if *muted { theme.muted } else { theme.text };
            let row = HEAD_ROWS + u16::try_from(row).expect("visible rows fit a u16");
            put(area, buf, row, text, style);
        }
    }
}

/// A progress bar of closed out of total. Inputs: the counts. Output: `[####......]`,
/// ten cells between brackets, all dots for an empty total.
fn bar(closed: u32, total: u32) -> String {
    let filled = if total == 0 {
        0
    } else {
        usize::try_from(u64::from(closed.min(total)) * BAR_CELLS as u64 / u64::from(total))
            .expect("a bar fill fits a usize")
    };
    assert!(filled <= BAR_CELLS);
    format!("[{}{}]", "#".repeat(filled), ".".repeat(BAR_CELLS - filled))
}
