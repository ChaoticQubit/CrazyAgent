use super::text::{pad, safe, wrap};
use super::wire::{Refused, Shown, ShownPart, ask, unreadable};
use super::{
    Client, Key, Theme, check_label, criterion_label, criterion_mark, evidence_label, kind_label,
    put, status_label,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use serde_json::json;
use std::cell::Cell;
use trackwright_core::model::{
    CRITERIA_PER_TICKET_MAX, CriterionState, LINKS_PER_TICKET_MAX, OpKind,
};
use trackwright_core::ops::SHOWN_EVENTS_MAX;
use trackwright_core::text::BODY_BYTES_MAX;

const STATE_CELLS: usize = 8;
const EVIDENCE_CELLS: usize = 12;
const LINK_KIND_CELLS: usize = 12;
const LINK_STATE_CELLS: usize = 10;
const EVENT_OP_CELLS: usize = 18;
const SHOW_PAGES_MAX: usize =
    1 + BODY_BYTES_MAX / 1024 + CRITERIA_PER_TICKET_MAX + LINKS_PER_TICKET_MAX + SHOWN_EVENTS_MAX;

#[derive(Clone, Copy)]
enum Role {
    Heading,
    Muted,
    Text,
}

/// One ticket's detail: its fields, body, criteria with checks and latest evidence,
/// links with the host's state, and its latest events, scrolled as one column of lines.
pub struct Detail {
    shown: Shown,
    scroll: usize,
    bottom: Cell<usize>,
}

impl Detail {
    /// Reads one ticket with `show`, following the cursor until every page is in: each
    /// page must move the cursor and continue the body where the last one stopped. Inputs:
    /// the short id and the client. Output: the detail, or the refusal.
    pub fn load(id: &str, client: &mut dyn Client) -> Result<Self, Refused> {
        let (mut shown, mut cursor) = ask::<Shown>(client, OpKind::Show, json!({ "id": id }))?;
        let mut pages = 1;
        while let Some(at) = cursor {
            if pages == SHOW_PAGES_MAX {
                return Err(unreadable(OpKind::Show));
            }
            let (part, next) =
                ask::<ShownPart>(client, OpKind::Show, json!({ "id": id, "cursor": at }))?;
            let joins = part.body.is_empty() || part.body_at == shown.part.body.len();
            if !joins || next.as_deref() == Some(at.as_str()) {
                return Err(unreadable(OpKind::Show));
            }
            shown.part.body.push_str(&part.body);
            shown.part.criteria.extend(part.criteria);
            shown.part.links.extend(part.links);
            shown.part.events.extend(part.events);
            cursor = next;
            pages += 1;
        }
        assert!(pages <= SHOW_PAGES_MAX);
        Ok(Self {
            shown,
            scroll: 0,
            bottom: Cell::new(0),
        })
    }

    /// The ticket's short id. Output: the id as the reply gave it.
    pub fn id(&self) -> &str {
        &self.shown.ticket.id
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

    fn lines(&self, limit: usize) -> Vec<(String, Role)> {
        let ticket = &self.shown.ticket;
        let shown = &self.shown.part;
        let mut lines = vec![(
            format!(" {}  {}", safe(&ticket.id), safe(&ticket.title)),
            Role::Heading,
        )];
        let mut facts = vec![
            kind_label(ticket.kind).to_owned(),
            status_label(ticket.status).to_owned(),
            format!("P{}", ticket.priority),
        ];
        facts.extend(ticket.assignee.as_deref().map(safe));
        facts.extend(ticket.release.as_deref().map(safe));
        facts.extend(ticket.cycle.map(|number| format!("Cycle {number}")));
        lines.push((format!(" {}", facts.join("  ")), Role::Muted));
        if let Some(by) = &ticket.closed_by {
            let reason = ticket.close_reason.as_deref().unwrap_or("");
            lines.push((
                format!(" Closed by {}: {}", safe(by), safe(reason)),
                Role::Muted,
            ));
        }
        lines.push((String::new(), Role::Text));
        let mut body: Vec<String> = shown
            .body
            .lines()
            .flat_map(|paragraph| {
                let wrapped = wrap(&safe(paragraph), limit - 2);
                if wrapped.is_empty() {
                    vec![String::new()]
                } else {
                    wrapped
                }
            })
            .collect();
        while body.last().is_some_and(String::is_empty) {
            body.pop();
        }
        if body.is_empty() {
            lines.push((" No description.".to_owned(), Role::Muted));
        }
        lines.extend(
            body.into_iter()
                .map(|line| (format!(" {line}"), Role::Text)),
        );
        lines.push((String::new(), Role::Text));
        let met = shown
            .criteria
            .iter()
            .filter(|criterion| criterion.state == CriterionState::Met)
            .count();
        lines.push((
            if shown.criteria.is_empty() {
                " Criteria  none".to_owned()
            } else {
                format!(" Criteria  {met} of {} met", shown.criteria.len())
            },
            Role::Heading,
        ));
        for criterion in &shown.criteria {
            lines.push((
                format!(
                    "   {} {}{}",
                    criterion_mark(criterion.state),
                    pad(&safe(&criterion.text), limit - 7 - STATE_CELLS),
                    pad(criterion_label(criterion.state), STATE_CELLS),
                ),
                Role::Text,
            ));
            for check in &criterion.checks {
                lines.push((
                    format!(
                        "       {}{}",
                        pad(
                            &format!("{}  {}", check_label(check.kind), safe(&check.reference)),
                            limit - 7 - EVIDENCE_CELLS
                        ),
                        pad(
                            evidence_label(check.latest.as_ref().map(|latest| latest.result)),
                            EVIDENCE_CELLS
                        ),
                    ),
                    Role::Muted,
                ));
            }
        }
        lines.push((String::new(), Role::Text));
        lines.push((" Links".to_owned(), Role::Heading));
        if shown.links.is_empty() {
            lines.push(("   none".to_owned(), Role::Muted));
        }
        for link in &shown.links {
            let shown_as = if link.label.is_empty() {
                &link.target
            } else {
                &link.label
            };
            let state = match (link.live, link.state.is_empty()) {
                (false, true) => "missing",
                (true, true) => "-",
                (_, false) => link.state.as_str(),
            };
            lines.push((
                format!(
                    "   {}{}{}",
                    pad(&safe(&link.kind), LINK_KIND_CELLS),
                    pad(
                        &safe(shown_as),
                        limit - 3 - LINK_KIND_CELLS - LINK_STATE_CELLS
                    ),
                    pad(&safe(state), LINK_STATE_CELLS),
                ),
                Role::Text,
            ));
        }
        lines.push((String::new(), Role::Text));
        lines.push((" History".to_owned(), Role::Heading));
        if shown.events.is_empty() {
            lines.push(("   none".to_owned(), Role::Muted));
        }
        for event in &shown.events {
            lines.push((
                format!(
                    "   {}{}",
                    pad(&safe(&event.op), EVENT_OP_CELLS),
                    safe(&event.actor)
                ),
                Role::Text,
            ));
        }
        lines
    }

    /// Draws the lines that fit from the scroll position. Inputs: the area, the buffer and
    /// the theme.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let lines = self.lines(usize::from(area.width));
        let visible = usize::from(area.height);
        self.bottom.set(lines.len().saturating_sub(visible));
        let start = self.scroll.min(self.bottom.get());
        for (row, (text, role)) in lines.iter().skip(start).take(visible).enumerate() {
            let style: Style = match role {
                Role::Heading => theme.heading,
                Role::Muted => theme.muted,
                Role::Text => theme.text,
            };
            let row = u16::try_from(row).expect("visible rows fit a u16");
            put(area, buf, row, text, style);
        }
    }
}
