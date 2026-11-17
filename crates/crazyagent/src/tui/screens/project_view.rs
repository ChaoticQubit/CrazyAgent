use super::console;
use super::new_project::{any_harness_total, display_path, harness_name, size_label, team_summary};
use super::new_project_input::cells;
use super::new_project_view::{
    Look, Row, Screen, columns, list_row, modal, research_glyph, shown_tail, spread, tab_strip,
    wrap_words,
};
use super::planning_line::{planning, planning_value};
use super::project::{
    Field, GENERAL_FIELDS, GeneralEdit, MasterStatus, OVERVIEW_TAB_LABELS, Picks, ProjectScreen,
    REVIEW_ROWS, RemoveChoice, Speaker, Tab, Transcript, memory_source, overview_pending,
    plain_problem, research_kind,
};
use super::project_activity::{TEAM_FROM, TEAM_GAP, TEAM_SIDE, activity_lines};
use super::project_markdown::{MdLine, Role, render, sanitize};
use super::project_master::{activity_rows, last_activity};
use super::project_publish::{PROMPT_KEY, PROMPT_LINE};
use super::project_team::window;
use super::project_time::{clock, duration};
use super::sessions::Sessions;
use crate::project_overview::{
    BLOCKED_BLANK, BLOCKED_NOT_A_FILE, BLOCKED_NOT_A_FOLDER, BLOCKED_NOT_UTF8, BLOCKED_TOO_LARGE,
    NEEDS_HOME, Overview, TAB_BYTES_MAX, Tab as OverviewTab,
};
use crate::project_setup::{
    ContextItem, ContextKind, Platform, ResearchJob, ResearchKind, ResearchStatus,
};
use crate::roadmap_status::Refresh;
use ratatui::style::Style;

mod context_settings;
mod overview_conversation_agents;

use context_settings::{HIDDEN_SEERS_COVER, context, settings};
use overview_conversation_agents::{agents, conversation, md_row, overview};

const SPINNER: &str = "\u{283c}";
const REMOVE_BOX: usize = 64;
const REMOVE_BOX_MIN: usize = 32;
const REMOVE_FOLDERS_SHOWN: usize = 3;
const REMOVE_LOSSES_SHOWN: usize = 6;
const REMOVE_WORDS: &str = "Its master conversation ends, its agents stop and what its research found is forgotten. CrazyAgent keeps your notes, imports and saved selections for it, and brings them back if you add this same folder again. The project folder stays on disk, with every file in it.";
const DELETE_WORDS: &str = "Stops its agents, moves its folders to the Trash and removes its records, your notes, imports and saved selections for it included. Its token counts stay under Removed projects.";
const DELETE_NOTE: &str = "Your notes, imports and saved selections go too.";

/// The project tabs for the tab strip: each label after its section glyph, with the count of
/// running agents on Agents. When the labels with glyphs and the narrowest gaps do not fit
/// the width, the glyphs go. Inputs: the running count, the width and the look. Output: the
/// labels and counts in tab order.
pub(in crate::tui) fn tab_items(
    running: usize,
    width: usize,
    look: &Look,
) -> Vec<(String, String)> {
    let glyphed: usize = Tab::ALL
        .iter()
        .map(|tab| cells(look.glyph(tab.glyph())) + 1 + cells(tab.label()))
        .sum::<usize>()
        + 2 * (Tab::ALL.len() - 1)
        + 2;
    Tab::ALL
        .iter()
        .map(|tab| {
            let count = if *tab == Tab::Agents && running > 0 {
                running.to_string()
            } else {
                String::new()
            };
            let label = if glyphed <= width {
                format!("{} {}", look.glyph(tab.glyph()), tab.label())
            } else {
                tab.label().to_owned()
            };
            (label, count)
        })
        .collect()
}

/// Draws one project tab with its tab strip, header status and help keys.
/// Takes the screen state, the frame size and the look; the reader caches rendered reports.
/// Returns the body rows, cursor, breadcrumb and help keys.
pub(in crate::tui) fn project_screen(
    screen: &mut ProjectScreen,
    width: u16,
    height: u16,
    look: &Look,
) -> Screen {
    let width = usize::from(width);
    let body_height = usize::from(height).saturating_sub(4);
    let content = width.saturating_sub(4);
    let running = screen.counts().0;
    let items = tab_items(running, width, look);
    let mut rows: Vec<Row> = tab_strip(width, &items, screen.tab as usize, look).into();
    let composer = screen.tab == Tab::Conversation && screen.typing;
    let banner = screen.recovery.rows(content, look, composer);
    let room = body_height.saturating_sub(2 + banner.len());
    for line in banner {
        rows.push(Row::of("  ", Style::new()).cat(line).pad(width));
    }
    let mut crumbs = vec!["Home".to_owned(), screen.name.clone()];
    if screen.tab != Tab::Overview {
        crumbs.push(screen.tab.label().to_owned());
    }
    let (lines, cursor, help) = match screen.tab {
        Tab::Overview => overview(screen, width, content, room, look),
        Tab::Conversation => conversation(screen, content, room, look),
        Tab::Agents => agents(screen, width, content, room, look),
        Tab::Changes => screen.changes.rows(content, room, screen.now_ms, look),
        Tab::Questions => screen.questions.rows(content, room, screen.offset, look),
        Tab::Context => {
            if screen.reading
                && width < 100
                && let Some(name) = screen
                    .preview
                    .as_ref()
                    .and_then(|p| p.path.rsplit('/').next())
            {
                crumbs.push(name.to_owned());
            }
            context(screen, width, content, room, look)
        }
        Tab::Settings => settings(screen, content, room, look),
    };
    for line in lines.into_iter().take(room) {
        rows.push(Row::of("  ", Style::new()).cat(line).pad(width));
    }
    while rows.len() < body_height {
        rows.push(Row::default().pad(width));
    }
    let mut help = help;
    if let Some(edit) = &screen.edit {
        let mut body: Vec<Row> = wrap_words(
            "Edit it before saving if you like. Every project can read saved memories.",
            52,
        )
        .into_iter()
        .map(|line| Row::of(line, look.fg))
        .collect();
        body.push(Row::default());
        body.push(Row::of("Memory", look.accent));
        let (lines, (line, column)) = edit.input.wrap(47);
        let first = line.saturating_sub(3);
        let top = 4;
        let mut cursor_at = None;
        for (index, text) in lines.iter().enumerate().skip(first).take(4) {
            if index == line {
                cursor_at = Some(body.len());
            }
            body.push(
                Row::of("[", look.accent)
                    .push(" ", Style::new())
                    .cat(Row::of(text.clone(), look.fg).pad(48))
                    .push(" ", Style::new())
                    .push("]", look.accent),
            );
        }
        body.push(Row::default());
        body.push(Row::of("From", look.muted).pad(8).push("Research", look.fg));
        body.push(Row::default());
        body.push(Row::of("[> Save to memory ]", look.selection).push("   [ Cancel ]", look.fg));
        let box_left = width.saturating_sub(58) / 2;
        rows = modal(&rows, width, 58, "Save to memory", body, top, look);
        help = vec![
            ("Enter", "Save to memory"),
            ("Ctrl-J", "New line"),
            ("Esc", "Cancel"),
        ];
        let cursor = cursor_at.map(|row| ((box_left + 4 + column) as u16, (top + 1 + row) as u16));
        return Screen {
            rows,
            cursor,
            crumbs,
            help,
            status: header_status(running, look),
        };
    }
    if let Some(review) = &screen.review {
        let lines = review.lines();
        let end = (review.scroll + REVIEW_ROWS).min(lines.len());
        let mut body: Vec<Row> = lines[review.scroll.min(end)..end]
            .iter()
            .map(|line| Row::of(line.clone(), look.fg))
            .collect();
        body.resize(REVIEW_ROWS, Row::default());
        body.push(Row::default());
        let at_end = review.at_end();
        body.push(if at_end {
            Row::of("End of the memory. y saves all of it.", look.muted)
        } else {
            Row::of(
                format!(
                    "Lines {}-{end} of {}. Read to the end to save or edit it.",
                    review.scroll + 1,
                    lines.len()
                ),
                look.muted,
            )
        });
        rows = modal(&rows, width, 58, "Save to memory", body, 2, look);
        help = if at_end {
            vec![
                ("y", "Save to memory"),
                ("e", "Edit first"),
                ("n", "Dismiss"),
                ("Esc", "Back"),
            ]
        } else {
            vec![("Down", "Read on"), ("n", "Dismiss"), ("Esc", "Back")]
        };
        return Screen {
            rows,
            cursor: None,
            crumbs,
            help,
            status: header_status(running, look),
        };
    }
    if let Some(adding) = &screen.adding {
        let field = 52;
        let (_, (_, column)) = adding.input.wrap(4096);
        let value: Vec<char> = adding.input.value().chars().collect();
        let start = column.saturating_sub(field - 1);
        let shown: String = value.iter().skip(start).take(field).collect();
        let mut body: Vec<Row> = wrap_words(
            "Type the full path of a file on this computer. It is copied into the project's documents.",
            56,
        )
        .into_iter()
        .map(|line| Row::of(line, look.muted))
        .collect();
        body.push(Row::default());
        let at = body.len();
        body.push(
            Row::of("[", look.accent)
                .push(" ", Style::new())
                .cat(Row::of(shown, look.fg).pad(field))
                .push(" ", Style::new())
                .push("]", look.accent),
        );
        body.push(Row::default());
        body.extend(
            adding
                .error
                .iter()
                .flat_map(|error| wrap_words(error, 56))
                .map(|line| Row::of(line, look.error)),
        );
        body.push(if adding.busy {
            Row::of(format!("{SPINNER} Adding..."), look.running)
        } else {
            Row::of("[> Add file ]", look.selection).push("   [ Cancel ]", look.fg)
        });
        let top = 4;
        let box_left = width.saturating_sub(62) / 2;
        rows = modal(&rows, width, 62, "Add file", body, top, look);
        help = if adding.busy {
            vec![("Esc", "Close")]
        } else {
            vec![("Enter", "Add file"), ("Esc", "Cancel")]
        };
        let cursor = (!adding.busy).then(|| {
            (
                (box_left + 4 + column - start) as u16,
                (top + 1 + at) as u16,
            )
        });
        return Screen {
            rows,
            cursor,
            crumbs,
            help,
            status: header_status(running, look),
        };
    }
    if let Some(folders) = &screen.folders {
        let field = 52;
        let (_, (_, column)) = folders.input.wrap(16384);
        let value: Vec<char> = folders.input.value().chars().collect();
        let start = column.saturating_sub(field - 1);
        let shown: String = value.iter().skip(start).take(field).collect();
        let mut body: Vec<Row> = wrap_words(
            "Folders agents may read, besides the toolchains found for this project. Separate them with a colon. They never read credential files inside them.",
            56,
        )
        .into_iter()
        .map(|line| Row::of(line, look.muted))
        .collect();
        body.push(Row::default());
        let at = body.len();
        body.push(
            Row::of("[", look.accent)
                .push(" ", Style::new())
                .cat(Row::of(shown, look.fg).pad(field))
                .push(" ", Style::new())
                .push("]", look.accent),
        );
        body.push(Row::default());
        body.extend(
            folders
                .error
                .iter()
                .flat_map(|error| wrap_words(error, 56))
                .map(|line| Row::of(line, look.error)),
        );
        body.push(if folders.busy {
            Row::of(format!("{SPINNER} Saving..."), look.running)
        } else {
            Row::of("[> Save folders ]", look.selection).push("   [ Cancel ]", look.fg)
        });
        let top = 4;
        let box_left = width.saturating_sub(62) / 2;
        rows = modal(&rows, width, 62, "Read-only folders", body, top, look);
        help = if folders.busy {
            vec![("Esc", "Close")]
        } else {
            vec![("Enter", "Save folders"), ("Esc", "Cancel")]
        };
        let cursor = (!folders.busy).then(|| {
            (
                (box_left + 4 + column - start) as u16,
                (top + 1 + at) as u16,
            )
        });
        return Screen {
            rows,
            cursor,
            crumbs,
            help,
            status: header_status(running, look),
        };
    }
    if let Some(hidden) = &screen.hidden {
        let field = 52;
        let (_, (_, column)) = hidden.input.wrap(16384);
        let value: Vec<char> = hidden.input.value().chars().collect();
        let start = column.saturating_sub(field - 1);
        let shown: String = value.iter().skip(start).take(field).collect();
        let mut body: Vec<Row> = wrap_words(
            "Folders inside the project that agents, the master and research jobs can't read, write or run. Separate them with a colon. Each must be a folder inside the project that git does not track.",
            56,
        )
        .into_iter()
        .chain(std::iter::once(String::new()))
        .chain(wrap_words(HIDDEN_SEERS_COVER, 56))
        .map(|line| Row::of(line, look.muted))
        .collect();
        if screen
            .agent_policy
            .as_ref()
            .is_some_and(|view| view.access == crate::sandbox::policy::AgentAccess::Full)
        {
            body.push(Row::default());
            body.extend(
                wrap_words(
                    "Full access runs agents without a sandbox, so hidden folders do not apply.",
                    56,
                )
                .into_iter()
                .map(|line| Row::of(line, look.warning)),
            );
        }
        body.push(Row::default());
        let at = body.len();
        body.push(
            Row::of("[", look.accent)
                .push(" ", Style::new())
                .cat(Row::of(shown, look.fg).pad(field))
                .push(" ", Style::new())
                .push("]", look.accent),
        );
        body.push(Row::default());
        body.extend(
            hidden
                .error
                .iter()
                .flat_map(|error| wrap_words(error, 56))
                .map(|line| Row::of(line, look.error)),
        );
        body.push(if hidden.busy {
            Row::of(format!("{SPINNER} Saving..."), look.running)
        } else {
            Row::of("[> Save hidden folders ]", look.selection).push("   [ Cancel ]", look.fg)
        });
        let top = 4;
        let box_left = width.saturating_sub(62) / 2;
        rows = modal(&rows, width, 62, "Hidden folders", body, top, look);
        help = if hidden.busy {
            vec![("Esc", "Close")]
        } else {
            vec![("Enter", "Save hidden folders"), ("Esc", "Cancel")]
        };
        let cursor = (!hidden.busy).then(|| {
            (
                (box_left + 4 + column - start) as u16,
                (top + 1 + at) as u16,
            )
        });
        return Screen {
            rows,
            cursor,
            crumbs,
            help,
            status: header_status(running, look),
        };
    }
    if let Some(access) = &screen.access {
        let top = 4;
        let body = access.rows(look, body_height.saturating_sub(top + 2));
        rows = modal(&rows, width, 62, "Agent access", body, top, look);
        help = if access.busy {
            vec![("Esc", "Close")]
        } else if access.confirm_full {
            vec![("y", "Allow Full access"), ("n", "Keep")]
        } else {
            vec![
                ("Up/Down", "Move"),
                ("Left/Right", "Change"),
                ("Enter", "Save"),
                ("Esc", "Cancel"),
            ]
        };
        return Screen {
            rows,
            cursor: None,
            crumbs,
            help,
            status: header_status(running, look),
        };
    }
    if screen.confirm_remove {
        let box_width = REMOVE_BOX.min(width).max(REMOVE_BOX_MIN);
        let (body, keys) = remove_ask(screen, box_width - 4, body_height.saturating_sub(4), look);
        rows = modal(&rows, width, box_width, "Remove project", body, 2, look);
        help = keys;
    }
    if screen.confirm_move {
        let box_width = REMOVE_BOX.min(width).max(REMOVE_BOX_MIN);
        let body = move_ask(screen, box_width - 4, look);
        rows = modal(
            &rows,
            width,
            box_width,
            "Move into CrazyAgent folder",
            body,
            2,
            look,
        );
        help = vec![
            ("m", "Move"),
            ("c", "Copy"),
            ("n", "Keep it here"),
            ("Esc", "Keep it here"),
        ];
    }
    if let Some(path) = &screen.confirm_delete {
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        let body = vec![
            Row::of(format!("Delete {name}?"), look.fg),
            Row::of("It is removed from the project folder.", look.muted),
            Row::default(),
            Row::of("[ Delete ]", look.fg)
                .push("   ", Style::new())
                .push("[> Keep ]", look.selection),
        ];
        rows = modal(&rows, width, 46, "Delete file", body, 5, look);
        help = vec![("y", "Delete"), ("n", "Keep"), ("Esc", "Keep")];
    }
    if let Some(job) = screen.confirm_stop {
        let name = screen
            .jobs
            .iter()
            .flatten()
            .find(|known| known.id == job)
            .map_or_else(
                || "this research".to_owned(),
                |known| known.kind_label.clone(),
            );
        let body = vec![
            Row::of(format!("Stop {name}?"), look.fg),
            Row::of("The agent ends. You can retry it later.", look.muted),
            Row::default(),
            Row::of("[ Stop ]", look.fg)
                .push("   ", Style::new())
                .push("[> Keep running ]", look.selection),
        ];
        rows = modal(&rows, width, 46, "Stop agent", body, 5, look);
        help = vec![
            ("y", "Stop"),
            ("n", "Keep running"),
            ("Esc", "Keep running"),
        ];
    }
    if screen.confirm_plan.is_some() {
        let mut body = vec![Row::of("Replace the roadmap?", look.fg)];
        body.extend(
            wrap_words(
                "Planning writes roadmap.md and release-process.md again from the brief and research. Your edits to them are lost.",
                50,
            )
            .into_iter()
            .map(|line| Row::of(line, look.muted)),
        );
        body.push(Row::default());
        body.push(
            Row::of("[ Replace ]", look.fg)
                .push("   ", Style::new())
                .push("[> Keep ]", look.selection),
        );
        rows = modal(&rows, width, 56, "Plan roadmap", body, 5, look);
        help = vec![("y", "Replace"), ("n", "Keep"), ("Esc", "Keep")];
    }
    if let Some(cursor) = screen.pool_picker {
        let mut body: Vec<Row> = wrap_words("Agents of this project use its pool's accounts.", 46)
            .into_iter()
            .map(|line| Row::of(line, look.muted))
            .collect();
        body.push(Row::default());
        let first = cursor.saturating_sub(PICKER_ROWS - 1);
        for (index, choice) in screen
            .pool_choices
            .iter()
            .enumerate()
            .skip(first)
            .take(PICKER_ROWS)
        {
            let current = screen
                .pool
                .as_ref()
                .is_some_and(|pool| pool.assigned && pool.id == choice.id);
            let row = Row::of(
                format!("{} {}", if current { "(*)" } else { "( )" }, choice.name),
                look.fg,
            )
            .pad(32)
            .push(
                format!(
                    "{} {}",
                    choice.accounts,
                    if choice.accounts == 1 {
                        "account"
                    } else {
                        "accounts"
                    }
                ),
                look.muted,
            )
            .pad(44);
            body.push(if index == cursor {
                row.restyle(look.selection)
            } else {
                row
            });
        }
        let none = screen.pool_choices.len();
        if none < first + PICKER_ROWS {
            let unassigned = !screen.pool.as_ref().is_some_and(|pool| pool.assigned);
            let row = Row::of(
                format!("{} No pool", if unassigned { "(*)" } else { "( )" }),
                look.fg,
            )
            .pad(32)
            .push("Agents wait", look.muted)
            .pad(44);
            body.push(if cursor == none {
                row.restyle(look.selection)
            } else {
                row
            });
        }
        rows = modal(&rows, width, 50, "Pick a pool", body, 4, look);
        help = vec![
            ("\u{2191}\u{2193}", "Move"),
            ("Enter", "Assign"),
            ("Esc", "Cancel"),
        ];
    }
    Screen {
        rows,
        cursor: cursor.map(|(x, y)| ((2 + x) as u16, (2 + y) as u16)),
        crumbs,
        help,
        status: header_status(running, look),
    }
}

fn header_status(running: usize, look: &Look) -> Option<Row> {
    match running {
        0 => None,
        1 => Some(Row::of("1 agent running", look.running)),
        n => Some(Row::of(format!("{n} agents running"), look.running)),
    }
}

type Help = Vec<(&'static str, &'static str)>;

/// What the move ask says about the team and its work before a project moves.
pub(in crate::tui) const MOVE_ASK_TEAM: &str = "Stop the team first (Stop all on the Team tab). Work not landed yet stays in the agents' copies, which follow the move.";

/// What the move ask says about its two ways.
pub(in crate::tui) const MOVE_ASK_WAYS: &str = "Move takes the folder into your CrazyAgent folder. Copy leaves the old folder where it is and the project uses the copy.";

/// Draws the move ask: that the team must be stopped and its work follows, then Move, Copy
/// or keeping the project where it is. Inputs: the screen, the box's inner width and the
/// look. Output: the box rows.
fn move_ask(screen: &ProjectScreen, inner: usize, look: &Look) -> Vec<Row> {
    assert!(inner >= REMOVE_BOX_MIN - 4);
    let mut body = vec![
        Row::of(
            format!("Move {} into your CrazyAgent folder?", screen.name),
            look.fg,
        ),
        Row::default(),
    ];
    for text in [MOVE_ASK_TEAM, MOVE_ASK_WAYS] {
        body.extend(
            wrap_words(text, inner)
                .iter()
                .map(|line| Row::of(line.as_str(), look.muted)),
        );
        body.push(Row::default());
    }
    body.push(
        Row::of("[ m Move ]", look.fg)
            .push("   ", Style::new())
            .push("[ c Copy ]", look.fg)
            .push("   ", Style::new())
            .push("[> Keep it here ]", look.selection),
    );
    body
}

/// Draws the remove ask in at most `room` rows: with Remove from dashboard chosen, both choices
/// in the owner's words; with Delete everything chosen, the folders it moves to the Trash, what
/// would be lost and the name typed so far, cut to the rows left; or the engine's reason Delete
/// everything is not offered. Inputs: the screen, the box's inner width and rows, and the look.
/// Output: the box rows and the help keys.
fn remove_ask(screen: &ProjectScreen, inner: usize, room: usize, look: &Look) -> (Vec<Row>, Help) {
    assert!(inner >= REMOVE_BOX_MIN - 4);
    let removal = screen.removal.as_ref();
    let can_delete = removal.is_some_and(|removal| removal.can_delete);
    let everything = screen.remove_choice == RemoveChoice::Everything;
    assert!(!everything || can_delete);
    let name = removal.map_or(screen.name.as_str(), |removal| removal.name.as_str());
    let pick = |chosen: bool, label: &str| {
        let mark = if chosen { "(*)" } else { "( )" };
        let style = if chosen { look.selection } else { look.fg };
        Row::of(format!("{mark} {label}"), style)
    };
    let indented = |line: &str, style: Style| Row::of(format!("    {line}"), style);
    let words = |text: &str| -> Vec<Row> {
        wrap_words(text, inner - 4)
            .iter()
            .map(|line| indented(line, look.muted))
            .collect()
    };
    let mut body = vec![
        Row::of(format!("Remove {name} from CrazyAgent?"), look.fg),
        Row::default(),
        pick(!everything, "Remove from dashboard (keep files)"),
    ];
    if !everything {
        body.extend(words(REMOVE_WORDS));
    }
    match removal {
        None => {
            body.push(Row::default());
            body.push(Row::of("Reading what removing would do...", look.muted));
        }
        Some(removal) if removal.can_delete => {
            body.push(Row::default());
            body.push(pick(everything, "Delete everything"));
            if everything {
                let mut rows = room.saturating_sub(body.len() + 2);
                if rows >= 3 {
                    body.push(indented(DELETE_NOTE, look.muted));
                    rows -= 1;
                }
                let typed = &screen.remove_typed;
                body.extend(delete_details(removal, typed, inner, rows, look));
            } else {
                body.extend(words(DELETE_WORDS));
            }
        }
        Some(removal) => {
            let blocked = removal.blocked.as_deref().unwrap_or_default();
            if !blocked.is_empty() {
                body.push(Row::default());
                for line in wrap_words(blocked, inner - 4) {
                    body.push(indented(&line, look.muted));
                }
            }
        }
    }
    body.push(Row::default());
    let send = if everything {
        "[ Delete everything ]"
    } else {
        "[ Remove project ]"
    };
    body.push(
        Row::of(send, look.fg)
            .push("   ", Style::new())
            .push("[> Keep project ]", look.selection),
    );
    let help = match (everything, can_delete) {
        (true, _) => vec![
            ("Enter", "Delete everything"),
            ("Up/Down", "Choose"),
            ("Esc", "Keep"),
        ],
        (false, true) => vec![
            ("y", "Remove"),
            ("Up/Down", "Choose"),
            ("n", "Keep"),
            ("Esc", "Keep"),
        ],
        (false, false) => vec![("y", "Remove"), ("n", "Keep"), ("Esc", "Keep")],
    };
    (body, help)
}

/// Lists what Delete everything does in at most `rows` rows: the folders it moves to the Trash,
/// what would be lost and the name typed so far, wrapped when every line fits, else one line
/// each, the two lists sharing the rows and each counting what it does not show. Inputs: the
/// preview, the typed name, the box's inner width, the rows and the look. Output: the rows.
fn delete_details(
    removal: &crate::team::cleanup::RemovalPreview,
    typed: &str,
    inner: usize,
    rows: usize,
    look: &Look,
) -> Vec<Row> {
    let indented = |line: &str, style: Style| Row::of(format!("    {line}"), style);
    let losses = crate::team::cleanup::loss_lines(removal);
    let wrapped = wrapped_details(removal, &losses, typed, inner, look);
    if wrapped.len() <= rows {
        return wrapped;
    }
    let lists = rows.saturating_sub(2);
    let loss_need = losses.len().clamp(1, REMOVE_LOSSES_SHOWN + 1);
    let folder_need = removal.folders.len().min(REMOVE_FOLDERS_SHOWN + 1);
    let folder_room = folder_need.min(lists.saturating_sub(loss_need.min(lists / 2)));
    let loss_room = (lists - folder_room).min(REMOVE_LOSSES_SHOWN + 1);
    let mut body = vec![indented(&trash_header(removal), look.muted)];
    let (shown, more) = fitted(removal.folders.len(), folder_room);
    for folder in removal.folders.iter().take(shown) {
        let tail = shown_tail(folder, inner - 6);
        body.push(indented(&format!("  {tail}"), look.fg));
    }
    if more {
        let more = removal.folders.len() - shown;
        body.push(indented(&format!("  and {more} more"), look.muted));
    }
    if losses.is_empty() && loss_room > 0 {
        body.push(indented(
            "No agent is running and no work is unlanded.",
            look.muted,
        ));
    }
    let (shown, more) = fitted(losses.len(), loss_room);
    for loss in losses.iter().take(shown) {
        body.push(indented(loss, look.warning));
    }
    if more {
        let more = losses.len() - shown;
        body.push(indented(&format!("and {more} more"), look.warning));
    }
    body.push(indented(
        &format!("Type the project name: {typed}"),
        look.fg,
    ));
    assert!(body.len() <= rows.max(2));
    body
}

/// Lists what Delete everything does with every folder and loss wrapped to the box: up to
/// three folders and six losses, each list counting the rest, then the name typed so far.
/// Inputs: the preview, its loss lines, the typed name, the box's inner width and the look.
/// Output: the rows, however many they take.
fn wrapped_details(
    removal: &crate::team::cleanup::RemovalPreview,
    losses: &[String],
    typed: &str,
    inner: usize,
    look: &Look,
) -> Vec<Row> {
    let indented = |line: &str, style: Style| Row::of(format!("    {line}"), style);
    let mut body = vec![indented(&trash_header(removal), look.muted)];
    for folder in removal.folders.iter().take(REMOVE_FOLDERS_SHOWN) {
        for line in wrap_words(folder, inner - 6) {
            body.push(indented(&format!("  {line}"), look.fg));
        }
    }
    if removal.folders.len() > REMOVE_FOLDERS_SHOWN {
        let more = removal.folders.len() - REMOVE_FOLDERS_SHOWN;
        body.push(indented(&format!("  and {more} more"), look.muted));
    }
    if losses.is_empty() {
        body.push(indented(
            "No agent is running and no work is unlanded.",
            look.muted,
        ));
    }
    for loss in losses.iter().take(REMOVE_LOSSES_SHOWN) {
        for line in wrap_words(loss, inner - 4) {
            body.push(indented(&line, look.warning));
        }
    }
    if losses.len() > REMOVE_LOSSES_SHOWN {
        let more = losses.len() - REMOVE_LOSSES_SHOWN;
        body.push(indented(&format!("and {more} more"), look.warning));
    }
    body.push(indented(
        &format!("Type the project name: {typed}"),
        look.fg,
    ));
    body
}

/// The heading over the folders Delete everything moves to the Trash, counting them all, so a
/// list cut short still says how many go. Input: the preview. Output: the heading.
fn trash_header(removal: &crate::team::cleanup::RemovalPreview) -> String {
    let count = removal.folders.len();
    let noun = if count == 1 { "folder" } else { "folders" };
    format!("Moves to the Trash ({count} {noun}):")
}

/// How many of `len` lines fit in `rows`, one row kept to count the rest when not all fit.
/// Inputs: the line count and the rows. Output: the lines shown and whether a count follows.
fn fitted(len: usize, rows: usize) -> (usize, bool) {
    if len <= rows {
        (len, false)
    } else {
        (rows.saturating_sub(1), rows > 0)
    }
}
type Drawn = (Vec<Row>, Option<(usize, usize)>, Help);

const PICKER_ROWS: usize = 8;

const TABS: (&str, &str) = ("Shift-\u{2190}\u{2192}", "Tabs");
const OVERVIEW_TAB_KEYS: [(&str, &str); 3] = [
    ("\u{2190}\u{2192}", "Overview tab"),
    ("PgUp/PgDn", "Scroll"),
    ("Shift-R", "Refresh tabs"),
];

/// Draws the status of one research job: glyph, word and time.
/// Takes the job, the current time, the UTC offset and the look.
/// Returns the status row.
pub(in crate::tui) fn job_status(job: &ResearchJob, now_ms: i64, offset: i64, look: &Look) -> Row {
    match job.status {
        ResearchStatus::Done => Row::of(
            format!(
                "{} Done{}",
                look.glyph("\u{2713}"),
                job.finished_ms
                    .map(|at| format!(" {}", clock(at, offset)))
                    .unwrap_or_default()
            ),
            look.success,
        ),
        ResearchStatus::Running => {
            Row::of(format!("{} Running", look.glyph("\u{25b8}")), look.running).push(
                job.started_ms
                    .map(|at| format!("  {}", duration(now_ms - at)))
                    .unwrap_or_default(),
                look.muted,
            )
        }
        ResearchStatus::Queued => {
            let queued = Row::of(format!("{} Queued", look.glyph("\u{25cc}")), look.muted);
            match job.error.as_deref().map(str::trim) {
                Some(reason) if !reason.is_empty() => {
                    queued.push(format!("  {reason}"), look.muted)
                }
                _ => queued,
            }
        }
        ResearchStatus::NeedsPool => match job.rate_limited() {
            Some((until, _)) => Row::of(
                format!("! Rate limited until {}", clock(until, offset)),
                look.warning,
            ),
            None => Row::of("! Needs a pool", look.warning),
        },
        ResearchStatus::Failed => Row::of(format!("{} Failed", look.glyph("\u{2717}")), look.error)
            .push(
                match job.attempts {
                    0 | 1 => String::new(),
                    n => format!("  after {n} tries"),
                },
                look.muted,
            ),
        ResearchStatus::Cancelled => {
            Row::of(format!("{} Stopped", look.glyph("\u{2298}")), look.muted)
        }
    }
}

fn research_label(kind: crate::project_setup::ResearchKind, look: &Look) -> String {
    format!("{} {}", look.glyph(research_glyph(kind)), kind.label())
}

fn about_line(screen: &ProjectScreen) -> String {
    screen
        .setup
        .as_ref()
        .and_then(|setup| setup.about.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or("")
        .to_owned()
}

fn folder(screen: &ProjectScreen) -> String {
    screen
        .setup
        .as_ref()
        .map(|setup| {
            display_path(
                &setup.project_folder().display().to_string(),
                screen.home.as_deref(),
            )
        })
        .unwrap_or_default()
}

/// Draws the Overview title: the project name, then its folder at the right edge.
/// Takes the screen, the content width and the look; a folder too long for the row is cut, never the name.
/// Returns one row exactly the content width.
fn title(screen: &ProjectScreen, content: usize, look: &Look) -> Row {
    let name = Row::of(screen.name.clone(), look.heading);
    let room = content.saturating_sub(name.width() + 2);
    let mut path = Row::of(folder(screen), look.muted);
    if path.width() > room {
        path = path.pad(room);
    }
    spread(content, name, path)
}

/// The fewest rows the picked Overview tab's text gets, however full the tab is.
const OVERVIEW_TEXT_ROWS_MIN: usize = 4;

/// What a generated Overview tab with no text says, by what will write it.
/// Takes the overview. Returns the line.
fn overview_unwritten(overview: &Overview) -> &'static str {
    if overview.running {
        return "Writing now. This tab fills in when the run ends.";
    }
    match overview.refresh {
        Refresh::Off => "Overview tabs are off in Project settings.",
        Refresh::RefreshOnly => "Not written yet. It is written when you refresh the tabs.",
        Refresh::AfterLanding => {
            "Not written yet. It is written after the next landed change set, or when you refresh the tabs."
        }
    }
}

/// Why a tab's file can't be shown, where the trouble is and what to do about it: the tab's
/// file, or the tabs' folder when that is what is wrong. An unknown code gets a plain line
/// and is never printed. Takes the tab and the engine's reason code. Returns the line.
fn overview_blocked(tab: OverviewTab, code: &str) -> String {
    assert!(!code.is_empty(), "a blocked tab carries a reason code");
    assert_eq!(TAB_BYTES_MAX % 1024, 0, "the bound reads as whole KB");
    let large = format!("its file is larger than {} KB", TAB_BYTES_MAX / 1024);
    let why = match code {
        BLOCKED_BLANK => "its file is empty",
        BLOCKED_TOO_LARGE => large.as_str(),
        BLOCKED_NOT_UTF8 => "its file is not plain text",
        BLOCKED_NOT_A_FILE => "a link or folder is where its file goes",
        BLOCKED_NOT_A_FOLDER => "the folder for the tabs is a link or a file",
        _ => "its file can't be read",
    };
    let file = format!(".crazyagent/overview/{}.md", tab.stem());
    let (place, path, fix) = match code {
        BLOCKED_NOT_A_FOLDER => ("The folder", ".crazyagent/overview", "Remove it"),
        BLOCKED_NOT_A_FILE => ("The path", file.as_str(), "Remove it"),
        _ => ("The file", file.as_str(), "Fix or delete the file"),
    };
    format!(
        "CrazyAgent can't show this tab: {why}. {place} is {path} in the project. \
         {fix}, then refresh the tabs."
    )
}

/// What a tab whose file came with the project's files says above its text, the desktop's
/// words (RV-DS241-2, DEC-141): CrazyAgent didn't write it and the owner didn't save it here.
const FOREIGN_TAB: &str = "This tab's file came with the project's files: CrazyAgent didn't write it and you didn't save it. Check it before you use its commands.";

/// The lines above the picked Overview tab's text and the text itself, as Markdown.
/// A project outside the CrazyAgent folder and a blocked tab get their one line and no text;
/// the README is shown either way, and a tab whose file came with the project's files says
/// so first. Takes the overview and the picked tab's index in `OVERVIEW_TAB_LABELS`.
/// Returns the plain notes to show first and the Markdown, None when there is none.
fn overview_picked(overview: &Overview, picked: usize) -> (Vec<String>, Option<&str>) {
    assert!(picked < OVERVIEW_TAB_LABELS.len());
    let Some(tab) = OverviewTab::ALL.get(picked) else {
        let Some(readme) = &overview.readme else {
            return (vec!["This project has no README file.".to_owned()], None);
        };
        let mut note = format!("{}  This is the project's own file.", readme.path);
        if readme.cut {
            note.push_str(" Showing the first part of a long file.");
        }
        return (vec![note], Some(&readme.text));
    };
    if overview.needs_home {
        return (vec![plain_problem(NEEDS_HOME)], None);
    }
    let Some(state) = overview.tabs.iter().find(|state| state.tab == *tab) else {
        return (vec![overview_unwritten(overview).to_owned()], None);
    };
    if let Some(code) = &state.blocked {
        return (vec![overview_blocked(*tab, code)], None);
    }
    let mut notes = Vec::new();
    if state.foreign && state.text.is_some() {
        notes.push(FOREIGN_TAB.to_owned());
    }
    if state.proposal.is_some() {
        notes.push("A refresh proposes changes. Review them in the desktop app.".to_owned());
    }
    if state.text.is_none() {
        notes.push(overview_unwritten(overview).to_owned());
    }
    (notes, state.text.as_deref())
}

/// The Overview tabs under a project's work: the tab row with the picked tab in brackets,
/// why the last run failed, the picked tab's notes and its Markdown from the scroll line on.
/// The scroll line is kept within the text, so the last page stays full.
/// Takes the screen, the width, the rows left for the section and the look.
/// Returns no rows before the tabs were read.
fn overview_tab_rows(
    screen: &mut ProjectScreen,
    width: usize,
    room: usize,
    look: &Look,
) -> Vec<Row> {
    let Some(overview) = &screen.overview_tabs else {
        return Vec::new();
    };
    let mut strip = Row::default();
    for (index, label) in OVERVIEW_TAB_LABELS.iter().enumerate() {
        if index > 0 {
            strip = strip.push("  ", Style::new());
        }
        strip = if index == screen.overview_tab {
            strip.push(format!("[{label}]"), look.selection)
        } else {
            strip.push(*label, look.muted)
        };
    }
    let mut rows = vec![Row::default(), strip];
    if let Some(failure) = &overview.failure {
        let (line, style) = if overview.waiting_for_account && overview_pending(overview) {
            (sanitize(failure), look.muted)
        } else {
            (
                format!("Couldn't refresh the tabs. {}", sanitize(failure)),
                look.warning,
            )
        };
        rows.extend(
            wrap_words(&line, width)
                .into_iter()
                .map(|line| Row::of(line, style)),
        );
    }
    let (notes, text) = overview_picked(overview, screen.overview_tab);
    for note in notes {
        let note = sanitize(&note);
        rows.extend(
            wrap_words(&note, width)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
    }
    let Some(text) = text else {
        return rows;
    };
    let lines = render(text, width);
    let window = room.saturating_sub(rows.len()).max(OVERVIEW_TEXT_ROWS_MIN);
    let whole = lines.len() <= window;
    let last_first = if whole { 0 } else { lines.len() - window };
    let first = screen.overview_scroll.min(last_first);
    let more = !whole && first < last_first;
    let shown = if more { window - 1 } else { window };
    rows.extend(
        lines
            .iter()
            .skip(first)
            .take(shown)
            .map(|line| md_row(line, look.fg, look)),
    );
    if more {
        rows.push(Row::of("PgDn for more", look.muted));
    }
    assert!(first + shown >= lines.len() || more);
    screen.overview_scroll = first;
    rows
}

/// Draws the roadmap planning line under the Work rows once the jobs are read.
/// Takes the screen and the look. Returns the row, or none before the jobs are read.
fn roadmap_row(screen: &ProjectScreen, look: &Look) -> Option<Row> {
    let jobs = screen.jobs.as_ref()?;
    Some(
        Row::of("  ", Style::new())
            .cat(Row::of("Roadmap", look.fg).pad(24))
            .cat(planning_value(&planning(jobs), look)),
    )
}

/// Draws the roadmap status line under the planning line once the jobs are read.
/// Takes the screen and the look. Returns the row, or none before the jobs are read.
fn roadmap_status_row(screen: &ProjectScreen, look: &Look) -> Option<Row> {
    screen.jobs.as_ref()?;
    Some(
        Row::of("  ", Style::new())
            .cat(Row::of("Roadmap status", look.fg).pad(24))
            .cat(screen.roadmap.value(look, screen.now_ms)),
    )
}

fn work_rows(screen: &ProjectScreen, look: &Look, paths: bool, width: usize) -> Vec<Row> {
    if screen.setup.is_none() {
        return vec![Row::of("Loading...", look.muted)];
    }
    let kinds = screen.work_kinds();
    if kinds.is_empty() {
        return vec![Row::of(
            "  No research was picked for this project.",
            look.muted,
        )];
    }
    let suggested = screen.suggested().len();
    kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let path = kind.report_path();
            let status = match (&screen.jobs, screen.job_for(&path)) {
                (None, _) => Row::of("Loading...", look.muted),
                (Some(_), Some(job)) => job_status(job, screen.now_ms, screen.offset, look),
                (Some(_), None) => {
                    Row::of(format!("{} Queued", look.glyph("\u{25cc}")), look.muted)
                }
            };
            let waits = screen.job_for(&path).is_some_and(|job| {
                job.status == ResearchStatus::Queued
                    && job
                        .error
                        .as_deref()
                        .is_some_and(|reason| !reason.trim().is_empty())
            });
            let mut row = Row::of(research_label(*kind, look), look.fg)
                .pad(24)
                .cat(if waits { status } else { status.pad(28) });
            if paths
                && screen
                    .job_for(&path)
                    .is_some_and(|job| job.status == ResearchStatus::Done)
            {
                row = row.push(path.trim_start_matches("context/"), look.muted);
            }
            let selected =
                screen.tab == Tab::Overview && screen.overview_cursor == suggested + index;
            if selected {
                Row::of("> ", Style::new())
                    .cat(row)
                    .pad(width)
                    .restyle(look.selection)
            } else {
                Row::of("  ", Style::new()).cat(row)
            }
        })
        .collect()
}

/// Picks the line that stands for a memory in one-line lists.
/// Takes the memory text, which may be a whole Markdown report.
/// Returns its first line of prose, else its first heading without the marks.
pub(in crate::tui) fn memory_quote(text: &str) -> String {
    let lines = || text.lines().map(str::trim).filter(|line| !line.is_empty());
    lines()
        .find(|line| !line.starts_with('#'))
        .or_else(|| lines().next())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim()
        .to_owned()
}

fn needs_you(screen: &ProjectScreen, look: &Look) -> Vec<Row> {
    let suggested = screen.suggested();
    if suggested.is_empty() {
        return Vec::new();
    }
    let index = screen.overview_cursor.min(suggested.len() - 1);
    let memory = suggested[index];
    let bar = Row::of(look.glyph("\u{258c}"), look.accent).push(" ", Style::new());
    let from = memory_source(memory).map_or("From research".to_owned(), |kind| {
        format!("From {}", kind.label())
    });
    vec![
        Row::of("Needs you", look.accent).push(format!("  {}", suggested.len()), look.muted),
        bar.clone().push("Save to memory?", look.heading),
        bar.clone()
            .cat(Row::of(format!("\"{}\"", memory_quote(&memory.text)), look.fg).pad(70)),
        bar.push(from, look.muted)
            .push("   ", Style::new())
            .push("y", look.heading)
            .push(" Save   ", look.fg)
            .push("n", look.heading)
            .push(" Dismiss   ", look.fg)
            .push("e", look.heading)
            .push(" Edit first", look.fg),
        Row::default(),
    ]
}

/// Says whether the Overview shows the GitHub publish question and takes its G key: the
/// engine said to ask, and the project has its setup. Takes the screen. Returns the answer.
pub(in crate::tui) fn asks_github(screen: &ProjectScreen) -> bool {
    screen.github_notice && !screen.setup_missing
}

fn github_notice(screen: &ProjectScreen, look: &Look) -> Vec<Row> {
    if !asks_github(screen) {
        return Vec::new();
    }
    vec![
        Row::default(),
        Row::of(PROMPT_LINE, look.muted)
            .push("   ", Style::new())
            .push(PROMPT_KEY.0, look.heading)
            .push(format!(" {}", PROMPT_KEY.1), look.fg),
    ]
}

fn pool_line(screen: &ProjectScreen, look: &Look) -> Row {
    match &screen.pool {
        _ if !screen.pool_ready => Row::of("Not ready yet", look.muted),
        Some(pool) if pool.assigned => Row::of(pool.name.clone(), look.fg).push(
            format!(
                "  {} {}",
                pool.accounts,
                if pool.accounts == 1 {
                    "account"
                } else {
                    "accounts"
                }
            ),
            look.muted,
        ),
        Some(pool) => Row::of(pool.name.clone(), look.fg)
            .push("  ", Style::new())
            .push("! Not assigned yet", look.warning),
        None => Row::of("! Needs a pool", look.warning),
    }
}

fn team_line(screen: &ProjectScreen, look: &Look) -> Row {
    match &screen.setup {
        Some(setup) => Row::of(team_summary(&setup.team, None), look.fg),
        None => Row::of("Loading...", look.muted),
    }
}

/// Lists what the engine recorded for one research job, oldest first.
/// Takes the job and the look; only times and counts the engine stored are used.
/// Returns each step's time when known, its plain text and its style.
pub(in crate::tui) fn activity(
    job: &ResearchJob,
    look: &Look,
) -> Vec<(Option<i64>, String, Style)> {
    let mut steps = vec![(Some(job.created_ms), "Queued".to_owned(), look.fg)];
    if job.status == ResearchStatus::NeedsPool {
        let waiting = job
            .rate_limited()
            .map_or("Waiting for a pool", |(_, reason)| reason);
        steps.push((None, waiting.to_owned(), look.warning));
    }
    if let Some(at) = job.started_ms {
        let harness = harness_name(&job.harness, None);
        let text = match job.attempts {
            0 | 1 => format!("Started on {harness}"),
            tries => format!("Started on {harness}, try {tries}"),
        };
        steps.push((Some(at), text, look.fg));
    }
    if job.status == ResearchStatus::Running {
        if let Some(at) = job.heartbeat_ms {
            steps.push((
                Some(at),
                format!(
                    "Still working, {} of output so far",
                    size_label(job.output_bytes)
                ),
                look.fg,
            ));
        }
        if job.cancel_requested {
            steps.push((
                None,
                "Stop asked, waiting for the agent to end".to_owned(),
                look.warning,
            ));
        }
    }
    if let Some(at) = job.finished_ms {
        let (text, style) = match job.status {
            ResearchStatus::Done => ("Finished, report written", look.success),
            ResearchStatus::Failed => ("Failed", look.error),
            ResearchStatus::Cancelled => ("Stopped", look.muted),
            _ => ("Finished", look.fg),
        };
        steps.push((Some(at), text.to_owned(), style));
    }
    assert!(!steps.is_empty() && steps.len() <= 6);
    steps
}
