use super::console;
use super::new_project::{any_harness_total, display_path, harness_name, size_label, team_summary};
use super::new_project_input::cells;
use super::new_project_view::{
    Look, Row, Screen, columns, list_row, modal, research_glyph, spread, tab_strip, wrap_words,
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

const SPINNER: &str = "\u{283c}";
const REMOVE_BOX: usize = 64;
const REMOVE_BOX_MIN: usize = 32;
const REMOVE_FOLDERS_SHOWN: usize = 3;
const REMOVE_LOSSES_SHOWN: usize = 6;

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
        let (body, keys) = remove_ask(screen, box_width - 4, look);
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
        let mut body = vec![
            Row::of(
                "Agents of this project use its pool's accounts.",
                look.muted,
            ),
            Row::default(),
        ];
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

/// Draws the remove ask: Remove from dashboard, which keeps every file, and Delete everything
/// when the engine's preview allows it, with the folders it moves to the Trash, what would be
/// lost and the name typed so far, or else the engine's reason it is not offered. Inputs: the
/// screen, the box's inner width and the look. Output: the box rows and the help keys.
fn remove_ask(screen: &ProjectScreen, inner: usize, look: &Look) -> (Vec<Row>, Help) {
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
    let mut body = vec![
        Row::of(format!("Remove {name} from CrazyAgent?"), look.fg),
        Row::default(),
        pick(!everything, "Remove from dashboard (keep files)"),
    ];
    body.extend(
        wrap_words(
            "Its master conversation ends, its research and agents stop and CrazyAgent forgets it. The project folder stays on disk, with every file in it.",
            inner - 4,
        )
        .iter()
        .map(|line| indented(line, look.muted)),
    );
    match removal {
        None => {
            body.push(Row::default());
            body.push(Row::of("Reading what removing would do...", look.muted));
        }
        Some(removal) if removal.can_delete => {
            body.push(Row::default());
            body.push(pick(everything, "Delete everything"));
            body.push(indented("Moves to the Trash:", look.muted));
            for folder in removal.folders.iter().take(REMOVE_FOLDERS_SHOWN) {
                for line in wrap_words(folder, inner - 6) {
                    body.push(indented(&format!("  {line}"), look.fg));
                }
            }
            if removal.folders.len() > REMOVE_FOLDERS_SHOWN {
                let more = removal.folders.len() - REMOVE_FOLDERS_SHOWN;
                body.push(indented(&format!("  and {more} more"), look.muted));
            }
            let losses = crate::team::cleanup::loss_lines(removal);
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
            let typed = if everything { look.fg } else { look.muted };
            body.push(indented(
                &format!("Type the project name: {}", screen.remove_typed),
                typed,
            ));
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
        ResearchStatus::Queued => Row::of(format!("{} Queued", look.glyph("\u{25cc}")), look.muted),
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
            let mut row = Row::of(research_label(*kind, look), look.fg)
                .pad(24)
                .cat(status.pad(28));
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

fn overview(
    screen: &mut ProjectScreen,
    width: usize,
    content: usize,
    room: usize,
    look: &Look,
) -> Drawn {
    let assigned = screen.pool.as_ref().is_some_and(|pool| pool.assigned);
    let mut help: Help = Vec::new();
    if screen.setup_missing {
        let lines = vec![
            Row::of(screen.name.clone(), look.heading),
            Row::default(),
            Row::of(
                "This project was made before New project had a brief, research and team.",
                look.fg,
            ),
            Row::of(
                "Its master conversation still works in Conversation.",
                look.fg,
            ),
        ];
        return (lines, None, vec![TABS, ("Esc", "Home")]);
    }
    if !assigned && screen.pool_ready && screen.suggested().is_empty() {
        let picked = screen
            .pool
            .as_ref()
            .filter(|_| screen.setup.as_ref().is_some_and(|s| s.pool.is_some()));
        let (problem, action, button, key) = match picked {
            Some(pool) => (
                format!(
                    "Pool {} is picked but not assigned yet, so no agent can start.",
                    pool.name
                ),
                "Press Enter to assign it. Work starts as soon as you do.",
                format!("[> Assign {} ]", pool.name),
                ("Enter", "Assign pool"),
            ),
            None => (
                "This project has no pool, so no agent can start.".to_owned(),
                "Pick a pool to start. Work starts as soon as you do.",
                "[> Pick a pool ]".to_owned(),
                ("Enter", "Pick a pool"),
            ),
        };
        let mut lines = vec![
            title(screen, content, look),
            Row::of(about_line(screen), look.fg).pad(content),
            Row::default(),
        ];
        lines.extend(
            wrap_words(&problem, content)
                .into_iter()
                .map(|line| Row::of(line, look.warning)),
        );
        lines.extend([
            Row::of(action, look.fg),
            Row::default(),
            Row::of(button, look.selection),
            Row::default(),
            Row::of("Work", look.heading),
        ]);
        if let Some(setup) = &screen.setup {
            for kind in &setup.research {
                lines.push(columns(
                    &[24, 30],
                    vec![
                        Row::of(format!("  {}", kind.label()), look.fg),
                        Row::of("! Needs a pool", look.warning),
                    ],
                ));
            }
        }
        let tabs_room = room.saturating_sub(lines.len());
        lines.extend(overview_tab_rows(screen, content, tabs_room, look));
        help.push(key);
        if screen.overview_tabs.is_some() {
            help.extend(OVERVIEW_TAB_KEYS);
        }
        help.extend([TABS, ("Esc", "Home")]);
        return (lines, None, help);
    }
    if !screen.suggested().is_empty() {
        help.extend([
            ("y", "Save to memory"),
            ("n", "Dismiss"),
            ("e", "Edit first"),
        ]);
    }
    help.extend([("Enter", "Open"), ("o", "Open folder")]);
    if screen.overview_tabs.is_some() {
        help.extend(OVERVIEW_TAB_KEYS);
    }
    help.extend([TABS, ("Esc", "Home")]);
    if width >= 100 {
        let left_width = 77;
        let right_width = content.saturating_sub(80);
        let mut left = vec![
            Row::of(screen.name.clone(), look.heading),
            Row::of(about_line(screen), look.fg),
            Row::of("Folder", look.muted)
                .push(format!("  {}   ", folder(screen)), look.fg)
                .push("o Open folder", look.muted),
            Row::default(),
        ];
        left.extend(needs_you(screen, look));
        left.push(Row::of("Work", look.heading));
        left.extend(work_rows(screen, look, true, left_width));
        left.extend(roadmap_row(screen, look));
        left.extend(roadmap_status_row(screen, look));
        let left_room = room.saturating_sub(left.len());
        left.extend(overview_tab_rows(screen, left_width, left_room, look));
        let mut right = vec![Row::of("Team", look.heading)];
        if let Some(setup) = &screen.setup {
            let team = &setup.team;
            right.push(
                Row::of("Master", look.muted)
                    .pad(12)
                    .push(harness_name(&team.master, None), look.fg),
            );
            if team.sub_harnesses.is_empty() {
                right.push(
                    Row::of("Sub-agents", look.muted)
                        .pad(12)
                        .push(any_harness_total(team), look.fg),
                );
            }
            for (index, slot) in team.sub_harnesses.iter().enumerate() {
                right.push(
                    Row::of(if index == 0 { "Sub-agents" } else { "" }, look.muted)
                        .pad(12)
                        .push(
                            format!("{} x{}", harness_name(&slot.harness, None), slot.agents),
                            look.fg,
                        ),
                );
            }
            let tools = team.master_tools || team.sub_harnesses.iter().any(|s| s.tools);
            right.push(Row::of("Tools", look.muted).pad(12).push(
                if tools {
                    "On, inside the folder"
                } else {
                    "Off"
                },
                look.fg,
            ));
        }
        right.push(Row::default());
        right.push(
            Row::of("Pool", look.heading)
                .push("  ", Style::new())
                .cat(pool_line(screen, look)),
        );
        right.push(Row::default());
        right.push(Row::of("Recent", look.heading));
        right.extend(recent(screen, look));
        let mut lines = Vec::new();
        for index in 0..left.len().max(right.len()) {
            lines.push(
                left.get(index)
                    .cloned()
                    .unwrap_or_default()
                    .pad(left_width)
                    .push(" ", Style::new())
                    .push(look.glyph("\u{2502}"), look.muted)
                    .push(" ", Style::new())
                    .cat(
                        right
                            .get(index)
                            .cloned()
                            .unwrap_or_default()
                            .pad(right_width),
                    ),
            );
        }
        return (lines, None, help);
    }
    let mut lines = vec![
        title(screen, content, look),
        Row::of(about_line(screen), look.fg).pad(content),
        Row::default(),
    ];
    lines.extend(needs_you(screen, look));
    lines.push(Row::of("Work", look.heading));
    lines.extend(work_rows(screen, look, false, content));
    lines.extend(roadmap_row(screen, look));
    lines.extend(roadmap_status_row(screen, look));
    lines.push(Row::default());
    lines.push(
        Row::of("Team", look.heading)
            .pad(6)
            .cat(team_line(screen, look)),
    );
    lines.push(
        Row::of("Pool", look.heading)
            .pad(6)
            .cat(pool_line(screen, look)),
    );
    let tabs_room = room.saturating_sub(lines.len());
    lines.extend(overview_tab_rows(screen, content, tabs_room, look));
    (lines, None, help)
}

fn recent(screen: &ProjectScreen, look: &Look) -> Vec<Row> {
    let mut events: Vec<(i64, String)> = Vec::new();
    if let Some(setup) = &screen.setup {
        events.push((setup.created_ms, "Project created".into()));
    }
    for job in screen.jobs.iter().flatten() {
        if let Some(at) = job.started_ms {
            events.push((at, format!("{} started", job.kind_label)));
        }
        if let Some(at) = job.finished_ms {
            let word = match job.status {
                ResearchStatus::Done => "finished",
                ResearchStatus::Failed => "failed",
                _ => "stopped",
            };
            events.push((at, format!("{} {word}", job.kind_label)));
        }
    }
    events.sort_by_key(|event| std::cmp::Reverse(event.0));
    events
        .into_iter()
        .take(8)
        .map(|(at, text)| {
            Row::of(clock(at, screen.offset), look.muted).push(format!("  {text}"), look.fg)
        })
        .collect()
}

/// Draws the master console's transcript: the owner's lines after `> `, the master's replies
/// as plain wrapped text, engine lines dimmed and team task lines after a bullet, with `@`
/// tags highlighted in the owner's lines.
/// Takes the screen, the transcript, the width and the look.
/// Returns the rows, a blank row between turns.
fn transcript_lines(
    screen: &ProjectScreen,
    transcript: &Transcript,
    content: usize,
    look: &Look,
) -> Vec<Row> {
    let mut lines = Vec::new();
    let width = content.saturating_sub(2).max(1);
    for message in screen.shown_messages(transcript) {
        match message.speaker {
            Speaker::You => {
                if !lines.is_empty() {
                    lines.push(Row::default());
                }
                for (index, line) in wrap_words(&message.text, width).iter().enumerate() {
                    lines.push(tagged(lead(index, ">", look.accent), line, look.fg, look));
                }
            }
            Speaker::Master => {
                let plain = if message.error { look.error } else { look.fg };
                lines.extend(
                    render(&message.text, content)
                        .iter()
                        .map(|line| md_row(line, plain, look)),
                );
            }
            Speaker::Notice => {
                let style = if message.error {
                    look.error
                } else {
                    look.muted
                };
                lines.extend(
                    wrap_words(&message.text, content)
                        .into_iter()
                        .map(|line| Row::of(line, style)),
                );
            }
            Speaker::Task => {
                for (index, line) in wrap_words(&message.text, width).iter().enumerate() {
                    let bullet = look.glyph("\u{2022}");
                    lines.push(lead(index, bullet, look.accent).push(line.clone(), look.muted));
                }
            }
        }
    }
    if let MasterStatus::Thinking { since_ms, activity } = &transcript.status {
        let detail = since_ms
            .map(|since| duration(screen.now_ms - since))
            .into_iter()
            .chain(last_activity(activity, screen.now_ms))
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
        let mut row = Row::of(SPINNER, look.running).push(" Thinking...", look.fg);
        if !detail.is_empty() {
            row = row.push(format!("  {detail}"), look.muted);
        }
        lines.push(row);
        for (age, text) in activity_rows(activity, screen.activity_open, screen.now_ms) {
            lines.push(Row::of(format!("  {age:>10}  {text}"), look.muted));
        }
    }
    lines
}

/// Starts one console row: the marker on a message's first line, two spaces on the rest.
/// Takes the line index, the marker and its style.
/// Returns the row.
fn lead(index: usize, marker: &str, style: Style) -> Row {
    if index == 0 {
        Row::of(marker.to_owned(), style).push(" ", Style::new())
    } else {
        Row::of("  ", Style::new())
    }
}

/// Appends one line to a row with its `@` tags in the accent style.
/// Takes the row, the line, the style for plain text and the look.
/// Returns the row.
fn tagged(row: Row, line: &str, plain: Style, look: &Look) -> Row {
    console::tag_parts(line)
        .into_iter()
        .fold(row, |row, (part, tag)| {
            row.push(part.to_owned(), if tag { look.accent } else { plain })
        })
}

/// Draws the console prompt: `> ` and the typed text, wrapped, at most `PROMPT_LINES_MAX`
/// lines around the cursor; empty, it shows what the prompt takes.
/// Takes the screen, the width and the look.
/// Returns the rows and the cursor's row in them and column.
fn prompt_rows(screen: &ProjectScreen, content: usize, look: &Look) -> (Vec<Row>, (usize, usize)) {
    if screen.input.value().is_empty() {
        let row = lead(0, ">", look.accent)
            .push("Message the master, / for commands, @ to tag", look.muted);
        return (vec![row], (0, 2));
    }
    let (wrapped, (line, column)) = screen.input.wrap(content.saturating_sub(2).max(1));
    let top = (line + 1).saturating_sub(console::PROMPT_LINES_MAX);
    let rows: Vec<Row> = wrapped
        .iter()
        .enumerate()
        .skip(top)
        .take(console::PROMPT_LINES_MAX)
        .map(|(index, text)| tagged(lead(index, ">", look.accent), text, look.fg, look))
        .collect();
    assert!(!rows.is_empty() && rows.len() <= console::PROMPT_LINES_MAX);
    assert!(line >= top && line - top < rows.len());
    (rows, (line - top, 2 + column))
}

/// Draws the prompt's inline menu: at most `MENU_SHOWN_MAX` commands or tags around the
/// chosen one, which is marked, or one line when no command matches.
/// Takes the screen and the look.
/// Returns the rows, none while no menu is open.
fn menu_rows(screen: &ProjectScreen, look: &Look) -> Vec<Row> {
    let picks = screen.picks();
    let at = screen.menu_cursor.min(picks.len().saturating_sub(1));
    let entries: Vec<(String, &str)> = match &picks {
        Picks::None => return Vec::new(),
        Picks::Commands(commands) if commands.is_empty() => {
            return vec![Row::of("  Unknown command", look.muted)];
        }
        Picks::Commands(commands) => commands
            .iter()
            .map(|command| (format!("/{:<8}", command.name), command.about))
            .collect(),
        Picks::Tags { tags, .. } => tags.iter().map(|tag| (tag.clone(), "")).collect(),
    };
    let first = (at + 1).saturating_sub(console::MENU_SHOWN_MAX);
    entries
        .into_iter()
        .enumerate()
        .skip(first)
        .take(console::MENU_SHOWN_MAX)
        .map(|(index, (name, about))| {
            let chosen = index == at;
            let mark = if chosen { look.glyph("\u{25b8}") } else { " " };
            let row = Row::of(format!("{mark} "), look.accent)
                .push(name, if chosen { look.accent } else { look.fg });
            if about.is_empty() {
                row
            } else {
                row.push(format!(" {about}"), look.muted)
            }
        })
        .collect()
}

/// Converts one rendered Markdown line into a styled row.
/// Takes the line, the style for plain text and the look.
/// Returns the row.
pub(in crate::tui) fn md_row(line: &MdLine, plain: Style, look: &Look) -> Row {
    let mut row = Row::default();
    for (text, role) in line {
        let style = match role {
            Role::Plain | Role::Italic => plain,
            Role::Heading | Role::Bold => look.heading,
            Role::Code | Role::Muted => look.muted,
            Role::Link => look.accent,
        };
        row = row.push(text.clone(), style);
    }
    row
}

fn master_status(status: &MasterStatus, look: &Look) -> Row {
    match status {
        MasterStatus::Ready => Row::of(format!("{} Ready", look.glyph("\u{2713}")), look.success),
        MasterStatus::Thinking { .. } => Row::of(
            format!("{} Thinking...", look.glyph("\u{25b8}")),
            look.running,
        ),
        MasterStatus::Replied => {
            Row::of(format!("{} Replied", look.glyph("\u{2713}")), look.success)
        }
        MasterStatus::NeedsYou => Row::of("! Needs you", look.warning),
        MasterStatus::NeedsPool => Row::of("! Needs a pool", look.warning),
        MasterStatus::NotSignedIn => Row::of("! Not signed in", look.warning),
        MasterStatus::Stopped => Row::of(format!("{} Stopped", look.glyph("\u{2298}")), look.error),
        MasterStatus::NotReady => Row::of("Not ready yet", look.muted),
    }
}

/// The Conversation tab: the master conversation, with the team panel beside it once the
/// content is at least `TEAM_FROM` cells wide. Takes the screen, the content width, the body
/// height and the look. Returns the rows, the input cursor and the help keys.
fn conversation(screen: &ProjectScreen, content: usize, room: usize, look: &Look) -> Drawn {
    if content < TEAM_FROM {
        return master_view(screen, content, room, look);
    }
    let main = content - TEAM_SIDE - TEAM_GAP;
    let (lines, cursor, mut help) = master_view(screen, main, room, look);
    let side = activity_lines(&screen.team, room, look);
    let joined = (0..lines.len().max(side.len()))
        .map(|index| {
            lines
                .get(index)
                .cloned()
                .unwrap_or_default()
                .pad(main)
                .push(" ".repeat(TEAM_GAP), Style::new())
                .cat(side.get(index).cloned().unwrap_or_default().pad(TEAM_SIDE))
        })
        .collect();
    if !screen.typing {
        help.insert(help.len().saturating_sub(2), ("t", "Team"));
    }
    (joined, cursor, help)
}

fn master_view(screen: &ProjectScreen, content: usize, room: usize, look: &Look) -> Drawn {
    let assigned = screen.pool.as_ref().is_some_and(|pool| pool.assigned);
    let master = screen
        .transcript
        .as_ref()
        .map(|t| t.harness.clone())
        .or_else(|| {
            screen
                .setup
                .as_ref()
                .map(|s| harness_name(&s.team.master, None))
        })
        .unwrap_or_else(|| "Master".into());
    let mut head = Row::of("Master", look.heading).push(format!("  {master}"), look.fg);
    let status = if !screen.pool_ready {
        MasterStatus::NotReady
    } else if !assigned {
        MasterStatus::NeedsPool
    } else {
        screen
            .transcript
            .as_ref()
            .map_or(MasterStatus::NotReady, |t| t.status.clone())
    };
    if let Some(transcript) = screen.transcript.as_ref().filter(|_| assigned) {
        if let Some(account) = &transcript.account {
            head = head.push(format!("   on {account}"), look.muted);
        }
        head = head.push(
            if transcript.tools {
                "   Tools on"
            } else {
                "   Tools off"
            },
            look.muted,
        );
    }
    let badge = master_status(&status, look);
    let session = screen
        .sessions
        .header(content, screen.now_ms, screen.offset, look);
    let mut lines = vec![
        spread(content, head, badge),
        session.filter(|_| assigned).unwrap_or_default(),
    ];
    let (prompt, at) = prompt_rows(screen, content, look);
    let typed = screen.typing && assigned && screen.transcript.is_some();
    let below: Vec<Row> = if typed {
        let note = screen
            .console_note
            .iter()
            .map(|line| Row::of(line.clone(), look.muted));
        note.chain(menu_rows(screen, look)).collect()
    } else {
        Vec::new()
    };
    let prompt_extra = if typed { prompt.len() - 1 } else { 0 };
    let available = room.saturating_sub(4 + prompt_extra + below.len());
    let (body, top, input, help): (Vec<Row>, bool, Vec<Row>, Help) = if !screen.pool_ready {
        (
            vec![Row::of(
                "The master conversation is not ready yet in this window.",
                look.fg,
            )],
            true,
            vec![Row::of("> Not ready yet", look.muted)],
            vec![TABS, ("Esc", "Home")],
        )
    } else if !assigned {
        (
            vec![
                Row::of(
                    "This project has no pool, so the master cannot start.",
                    look.fg,
                ),
                Row::of("Press s to open project settings and pick one.", look.fg),
            ],
            true,
            vec![Row::of("> Pick a pool to start", look.muted)],
            vec![("s", "Project settings"), TABS, ("Esc", "Home")],
        )
    } else if let Some(transcript) = screen.transcript.as_ref() {
        let body = if transcript.messages.is_empty()
            && !matches!(transcript.status, MasterStatus::Thinking { .. })
        {
            let mut body = vec![Row::of("No messages yet.", look.fg)];
            body.extend(
                wrap_words("Tell the master what to build. It plans the work, starts sub-agents and reports back here.", content)
                    .into_iter()
                    .map(|line| Row::of(line, look.fg)),
            );
            body
        } else {
            transcript_lines(screen, transcript, content, look)
        };
        let empty = transcript.messages.is_empty();
        let picks = screen.picks();
        let mut help = if screen.typing && picks.len() > 0 {
            let enter = if matches!(picks, Picks::Commands(_)) {
                "Run"
            } else {
                "Insert"
            };
            vec![
                ("Up/Down", "Pick"),
                ("Tab", "Complete"),
                ("Enter", enter),
                ("Esc", "Clear"),
            ]
        } else if screen.typing {
            let esc = if screen.input.value().is_empty() {
                "Leave input"
            } else {
                "Clear"
            };
            vec![
                ("Enter", "Send"),
                ("Ctrl-J", "New line"),
                ("/", "Commands"),
                ("@", "Tag"),
                ("PgUp", "Scroll"),
                ("Ctrl-T", "Team"),
                TABS,
                ("Esc", esc),
            ]
        } else if transcript.status.retryable() {
            vec![
                ("r", "Try again"),
                ("Tab", "To input"),
                TABS,
                ("Esc", "Home"),
            ]
        } else {
            vec![
                ("Enter", "Type a message"),
                ("PgUp", "Scroll"),
                TABS,
                ("Esc", "Home"),
            ]
        };
        if transcript.open && !screen.typing {
            help.insert(1, ("e", "End conversation"));
        }
        if !screen.typing {
            let at = help.len().saturating_sub(2);
            help.splice(at..at, Sessions::conversation_help());
        }
        if screen.more_activity() {
            let label = if screen.activity_open {
                "Less activity"
            } else {
                "All activity"
            };
            help.insert(help.len().saturating_sub(2), ("Ctrl-O", label));
        }
        let input = if screen.typing {
            prompt
        } else {
            vec![input_row(screen, look)]
        };
        match screen
            .sessions
            .view(content, available, screen.now_ms, screen.offset, look)
        {
            Some((rows, keys)) if !screen.typing => (rows, true, input, keys),
            _ => (body, empty, input, help),
        }
    } else {
        (
            vec![Row::of(
                "The master conversation is not ready yet.",
                look.muted,
            )],
            true,
            vec![Row::of("> Not ready yet", look.muted)],
            vec![TABS, ("Esc", "Home")],
        )
    };
    let end = body.len().saturating_sub(
        screen
            .transcript_scroll
            .min(body.len().saturating_sub(available)),
    );
    let start = end.saturating_sub(available);
    let shown = &body[start..end];
    let gap = available.saturating_sub(shown.len());
    if !top {
        lines.extend(std::iter::repeat_n(Row::default(), gap));
    }
    lines.extend(shown.iter().cloned());
    if top {
        lines.extend(std::iter::repeat_n(Row::default(), gap));
    }
    lines.extend(below);
    lines.push(Row::of(look.glyph("\u{2500}").repeat(content), look.muted));
    let cursor = typed.then(|| (at.1, lines.len() + at.0));
    lines.extend(input);
    (lines, cursor, help)
}

fn input_row(screen: &ProjectScreen, look: &Look) -> Row {
    let value = screen.input.value();
    if value.is_empty() {
        return Row::of(">", look.accent)
            .push(" ", Style::new())
            .push("Message the master", look.muted);
    }
    let last = value.rsplit('\n').next().unwrap_or(value);
    Row::of(">", look.accent)
        .push(" ", Style::new())
        .push(last.to_owned(), look.fg)
}

fn agents(screen: &ProjectScreen, width: usize, content: usize, room: usize, look: &Look) -> Drawn {
    let wide = width >= 100;
    let (running, waiting, failed, done) = screen.counts();
    let mut title = Row::of("Agents", look.accent);
    for (count, word, style) in [
        (running, "running", look.running),
        (failed, "failed", look.error),
        (waiting, "waiting", look.warning),
        (done, "done", look.success),
    ] {
        if count > 0 {
            title = title
                .push("  ", Style::new())
                .push(format!("{count} {word}"), style)
                .push(" ", Style::new());
        }
    }
    let mut lines = vec![title.pad(content)];
    if wide {
        lines.push(Row::default());
    }
    let mut help = vec![("\u{2191}\u{2193}", "Move")];
    help.extend(screen.focused_job().and_then(|job| match job.status {
        ResearchStatus::Queued | ResearchStatus::NeedsPool | ResearchStatus::Running => {
            Some(("x", "Stop"))
        }
        ResearchStatus::Failed | ResearchStatus::Cancelled => Some(("r", "Retry")),
        ResearchStatus::Done => None,
    }));
    help.extend([
        ("Enter", "Open report"),
        ("t", "Team"),
        TABS,
        ("Esc", "Home"),
        ("s", "Team settings"),
    ]);
    if screen.pool_ready && !screen.pool.as_ref().is_some_and(|pool| pool.assigned) {
        lines.push(Row::of(
            "This project has no pool. Pick a pool to start.",
            look.warning,
        ));
        if wide {
            lines.push(Row::default());
        }
    }
    let Some(jobs) = &screen.jobs else {
        lines.push(Row::of("Loading...", look.muted));
        return (lines, None, help);
    };
    if jobs.is_empty() {
        lines.push(Row::of(
            "No agents yet. Research you pick in New project runs here.",
            look.muted,
        ));
        return (lines, None, help);
    }
    let widths = if wide {
        vec![24, 14, content.saturating_sub(2 + 24 + 14 + 4)]
    } else {
        vec![23, 13, content.saturating_sub(2 + 23 + 13 + 4)]
    };
    lines.push(
        Row::of("  ", Style::new()).cat(
            columns(
                &widths,
                ["Agent", "Harness", "Status"]
                    .iter()
                    .map(|h| Row::of(*h, look.muted))
                    .collect(),
            )
            .restyle(look.muted),
        ),
    );
    for (index, job) in jobs.iter().enumerate() {
        let row = columns(
            &widths,
            vec![
                Row::of(research_label(job.kind, look), look.fg),
                Row::of(harness_name(&job.harness, None), look.fg),
                job_status(job, screen.now_ms, screen.offset, look),
            ],
        );
        lines.push(list_row(
            row,
            content,
            index == screen.agents_cursor,
            true,
            look,
        ));
    }
    lines.push(Row::default());
    if let Some(job) = jobs.get(screen.agents_cursor) {
        let when = match (job.status, job.started_ms, job.finished_ms) {
            (ResearchStatus::Failed, _, Some(at)) => Row::of(
                format!(
                    "{} Failed at {}",
                    look.glyph("\u{2717}"),
                    clock(at, screen.offset)
                ),
                look.error,
            ),
            (_, Some(start), Some(end)) => Row::of(
                format!(
                    "Started {}, took {}",
                    clock(start, screen.offset),
                    duration(end - start)
                ),
                look.muted,
            ),
            (ResearchStatus::Running, Some(start), None) => Row::of(
                format!(
                    "Started {}, running {}",
                    clock(start, screen.offset),
                    duration(screen.now_ms - start)
                ),
                look.muted,
            ),
            (_, Some(start), None) => Row::of(
                format!("Started {}", clock(start, screen.offset)),
                look.muted,
            ),
            _ => Row::of(
                format!("Queued {}", clock(job.created_ms, screen.offset)),
                look.muted,
            ),
        };
        lines.push(spread(
            content,
            Row::of(job.kind_label.clone(), look.heading),
            when,
        ));
        if let Some(error) = job
            .error
            .as_ref()
            .filter(|_| matches!(job.status, ResearchStatus::Failed | ResearchStatus::Queued))
        {
            for line in wrap_words(error, content) {
                lines.push(Row::of(line, look.fg));
            }
        }
        let entry =
            |label: &str, value: String| Row::of(label, look.muted).pad(14).push(value, look.fg);
        lines.push(entry(
            "Report",
            job.report_path.trim_start_matches("context/").to_owned(),
        ));
        lines.push(entry(
            "Tools",
            if job.tools {
                "On, inside the project folder".into()
            } else {
                "Off".into()
            },
        ));
        if job.attempts > 1 {
            lines.push(entry("Tries", job.attempts.to_string()));
        }
        lines.push(Row::default());
        lines.push(Row::of("Activity", look.heading));
        for (at, text, style) in activity(job, look) {
            let at = at.map(|at| clock(at, screen.offset)).unwrap_or_default();
            lines.push(Row::of(at, look.muted).pad(7).push(text, style));
        }
        let left = room.saturating_sub(lines.len());
        lines.extend(
            output_lines(screen, job, look)
                .into_iter()
                .rev()
                .take(left)
                .rev()
                .map(|(text, style)| Row::default().pad(7).push(text, style).pad(content)),
        );
    }
    (lines, None, help)
}

/// Gives the agent output shown under Activity for the focused job.
/// Takes the screen, the job and the look; only lines the engine read for this job are used.
/// Returns the lines oldest first, a plain note while reading or when nothing was written, or nothing for a job that never started.
fn output_lines(screen: &ProjectScreen, job: &ResearchJob, look: &Look) -> Vec<(String, Style)> {
    let log = screen.log.as_ref().filter(|log| log.job == job.id);
    match log.map(|log| log.lines.as_deref()) {
        Some(Some(lines)) if !lines.is_empty() => {
            lines.iter().map(|line| (line.clone(), look.fg)).collect()
        }
        Some(None) => vec![(
            "The agent's output could not be read.".to_owned(),
            look.warning,
        )],
        _ if job.started_ms.is_none() => Vec::new(),
        Some(Some(_)) => vec![("No output yet.".to_owned(), look.muted)],
        None => vec![("Reading the agent's output...".to_owned(), look.muted)],
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

/// Where the Context tab says a file linked from the repository comes from.
pub(in crate::tui) const LINKED_SOURCE: &str = "Linked from your repo";

fn source(screen: &ProjectScreen, item: &ContextItem, look: &Look) -> (Row, Row) {
    if item.linked {
        return (Row::default(), Row::of(LINKED_SOURCE, look.muted));
    }
    match item.kind {
        ContextKind::Brief => (Row::default(), Row::of("Project brief", look.muted)),
        ContextKind::Document => (Row::default(), Row::of("You added it", look.muted)),
        ContextKind::Plan => (
            Row::default(),
            Row::of("Planned after research", look.muted),
        ),
        ContextKind::Research => {
            let label = research_kind(&item.path).map_or("Research", |kind| kind.label());
            match screen.job_for(&item.path) {
                Some(job) if job.status == ResearchStatus::Running => {
                    (Row::of("writing", look.running), Row::of(label, look.muted))
                }
                Some(job) if job.status == ResearchStatus::Done => {
                    let at = job
                        .finished_ms
                        .map(|at| format!(", {}", clock(at, screen.offset)))
                        .unwrap_or_default();
                    let fresh = job
                        .finished_ms
                        .is_some_and(|at| screen.now_ms - at < 30 * 60_000);
                    (
                        if fresh {
                            Row::of("new", look.success)
                        } else {
                            Row::default()
                        },
                        Row::of(format!("{label}{at}"), look.muted),
                    )
                }
                _ => (Row::default(), Row::of(label, look.muted)),
            }
        }
    }
}

/// Draws the Context tree: a row per folder and per file, the selected file marked.
/// Inputs: the screen, the look, whether the tree sits beside the reader and its width.
/// Output: the rows and the index of the selected file's row.
fn tree_rows(screen: &ProjectScreen, look: &Look, wide: bool, width: usize) -> (Vec<Row>, usize) {
    let items = screen.tree();
    let mut rows = Vec::new();
    let mut selected = 0;
    let mut folder = None;
    let focus = !screen.reading || wide;
    for (index, item) in items.iter().enumerate() {
        let relative = item.path.trim_start_matches("context/");
        let (parent, name) = match relative.rsplit_once('/') {
            Some((parent, name)) => (Some(parent), name),
            None => (None, relative),
        };
        if parent != folder {
            if let Some(parent) = parent {
                rows.push(Row::of(format!("  {parent}/"), look.fg));
            }
            folder = parent;
        }
        let indent = if parent.is_some() { "  " } else { "" };
        let (badge, origin) = source(screen, item, look);
        let row = if wide {
            spread(
                width - 4,
                Row::of(format!("{indent}{name}"), look.fg),
                badge,
            )
        } else {
            columns(
                &[26, 8, 36],
                vec![Row::of(format!("{indent}{name}"), look.fg), badge, origin],
            )
        };
        if index == screen.tree_cursor {
            selected = rows.len();
        }
        rows.push(list_row(
            row,
            width,
            index == screen.tree_cursor,
            focus,
            look,
        ));
    }
    assert!(rows.is_empty() || selected < rows.len());
    (rows, selected)
}

/// Keeps the Context tree rows that fit, scrolled so the selected file stays in view.
/// Inputs: the tree rows, the selected file's row and the rows there is room for.
/// Output: at most `size` rows, and at least one when the tree has any.
fn tree_window(rows: Vec<Row>, selected: usize, size: usize) -> Vec<Row> {
    let size = size.max(1);
    if rows.len() <= size {
        return rows;
    }
    let first = window(selected, rows.len(), size);
    let kept: Vec<Row> = rows.into_iter().skip(first).take(size).collect();
    assert_eq!(kept.len(), size);
    kept
}

/// The line the Context tree shows under its files when the engine left docs out of its
/// listing, in the desktop's words (DEC-152), so a cut list never reads as whole.
/// Inputs: how many docs were left out and whether the walk stopped before the whole docs
/// folder. Output: the line, or none when nothing was left out.
fn unlisted_line(unlisted: usize, cut: bool) -> Option<String> {
    let folder = "the docs folder has more files than CrazyAgent reads";
    let counted = (unlisted > 0).then(|| {
        format!(
            "{unlisted} more {} not listed",
            if unlisted == 1 { "doc" } else { "docs" }
        )
    });
    match (counted, cut) {
        (None, false) => None,
        (None, true) => Some(format!("More docs not listed: {folder}")),
        (Some(counted), false) => Some(counted),
        (Some(counted), true) => Some(format!("{counted}, and {folder}")),
    }
}

/// Wraps the left-out docs line to the tree's width. Inputs: the screen, the width and the
/// look. Output: the muted rows, or none when the listing left nothing out.
fn unlisted_rows(screen: &ProjectScreen, width: usize, look: &Look) -> Vec<Row> {
    screen
        .context
        .as_ref()
        .and_then(|context| unlisted_line(context.unlisted_docs, context.docs_folder_cut))
        .map(|line| {
            wrap_words(&line, width.max(1))
                .into_iter()
                .map(|part| Row::of(part, look.muted))
                .collect()
        })
        .unwrap_or_default()
}

fn preview_rows(screen: &mut ProjectScreen, width: usize, room: usize, look: &Look) -> Vec<Row> {
    let header = screen.preview.as_ref().map(|preview| {
        let relative = preview.path.trim_start_matches("context/").to_owned();
        let item = screen
            .tree()
            .into_iter()
            .find(|item| item.path == preview.path)
            .cloned();
        let origin = item
            .map(|item| source(screen, &item, look).1)
            .unwrap_or_default();
        spread(width, Row::of(relative, look.heading), origin)
    });
    let suggestion = screen
        .report_suggestion()
        .map(|memory| memory_quote(&memory.text));
    let Some(preview) = screen.preview.as_mut() else {
        return vec![Row::of(
            "Pick a file and press Enter to read it.",
            look.muted,
        )];
    };
    let mut rows = vec![header.unwrap_or_default(), Row::default()];
    if preview.binary {
        rows.push(Row::of(
            "This file can't be shown here. Press o to open it.",
            look.muted,
        ));
        return rows;
    }
    if preview.text.is_empty() {
        rows.push(match &preview.problem {
            Some(problem) => Row::of(problem.clone(), look.error),
            None if preview.loaded => Row::of("This file is empty.", look.muted),
            None => Row::of("Loading...", look.muted),
        });
        return rows;
    }
    let available = room.saturating_sub(if suggestion.is_some() { 6 } else { 2 });
    let wanted = preview.scroll;
    let lines = preview.lines(width);
    let lines_count = lines.len();
    let scroll = wanted.min(lines.len().saturating_sub(available));
    let shown: Vec<Row> = lines
        .iter()
        .skip(scroll)
        .take(available)
        .map(|line| md_row(line, look.fg, look))
        .collect();
    preview.scroll = scroll;
    rows.extend(shown);
    let at_end = scroll + available >= lines_count;
    if let Some(quote) = suggestion.filter(|_| at_end) {
        rows.extend([
            Row::default(),
            Row::of("Suggested memory", look.heading),
            Row::of(format!("\"{quote}\""), look.fg).pad(width),
            Row::of("y", look.heading)
                .push(" Save to memory   ", look.fg)
                .push("n", look.heading)
                .push(" Dismiss   ", look.fg)
                .push("e", look.heading)
                .push(" Edit first", look.fg),
        ]);
    }
    rows
}

fn context(
    screen: &mut ProjectScreen,
    width: usize,
    content: usize,
    room: usize,
    look: &Look,
) -> Drawn {
    let wide = width >= 100;
    if screen.reading && !wide {
        let help = if screen.report_suggestion().is_some() {
            vec![
                ("y", "Save to memory"),
                ("n", "Dismiss"),
                ("o", "Open in app"),
                ("PgDn", "Scroll"),
                ("Esc", "Back to files"),
                ("v", "Edit file"),
            ]
        } else {
            vec![
                ("m", "Save to memory"),
                ("o", "Open in app"),
                ("PgDn", "Scroll"),
                ("Esc", "Back to files"),
                ("v", "Edit file"),
            ]
        };
        return (preview_rows(screen, content, room, look), None, help);
    }
    let help = vec![
        ("Enter", "Open"),
        ("a", "Add file"),
        ("o", "Open in app"),
        ("d", "Delete"),
        ("m", "Save to memory"),
        ("v", "Edit file"),
        TABS,
        ("Esc", "Home"),
        ("O", "Open in IDE"),
        ("p", "Project memory"),
    ];
    let mut lines = vec![
        spread(
            content,
            Row::of("Context", look.accent),
            Row::of(folder(screen), look.muted),
        ),
        Row::default(),
    ];
    if screen.context.is_none() {
        lines.push(Row::of("Loading...", look.muted));
        return (lines, None, help);
    }
    if wide {
        let tree_width = 36;
        let preview_width = content.saturating_sub(39);
        let unlisted = unlisted_rows(screen, tree_width, look);
        let size = room.saturating_sub(lines.len() + 2 + unlisted.len() + 5);
        let (rows, selected) = tree_rows(screen, look, true, tree_width);
        let mut tree = vec![Row::of("Files", look.accent), Row::default()];
        tree.extend(tree_window(rows, selected, size));
        tree.extend(unlisted);
        tree.push(Row::default());
        tree.push(Row::of("Agents write results here", look.muted));
        tree.push(Row::of("as Markdown files.", look.muted));
        tree.push(Row::default());
        tree.push(Row::of(PROJECT_MEMORY_LINE, look.muted));
        let preview = preview_rows(screen, preview_width, room.saturating_sub(2), look);
        for index in 0..tree.len().max(preview.len()) {
            lines.push(
                tree.get(index)
                    .cloned()
                    .unwrap_or_default()
                    .pad(tree_width)
                    .push(" ", Style::new())
                    .push(look.glyph("\u{2502}"), look.muted)
                    .push(" ", Style::new())
                    .cat(
                        preview
                            .get(index)
                            .cloned()
                            .unwrap_or_default()
                            .pad(preview_width),
                    ),
            );
        }
        return (lines, None, help);
    }
    let unlisted = unlisted_rows(screen, content, look);
    let size = room.saturating_sub(lines.len() + unlisted.len() + 3);
    let (rows, selected) = tree_rows(screen, look, false, content);
    lines.extend(tree_window(rows, selected, size));
    lines.extend(unlisted);
    lines.push(Row::default());
    lines.push(Row::of(
        "Agents write results here as Markdown. o opens a file in your editor.",
        look.muted,
    ));
    lines.push(Row::of(PROJECT_MEMORY_LINE, look.muted));
    (lines, None, help)
}

const PROJECT_MEMORY_LINE: &str = "p opens Project memory.";

const LABEL: usize = 13;

/// Draws the General form: text fields, then platform and research switches
/// in two columns, then the refusal or the saving note.
/// Takes the form, the content width and the look.
/// Returns the rows, the text cursor and the help keys.
fn general_form(general: &GeneralEdit, content: usize, look: &Look) -> Drawn {
    let field = content.saturating_sub(LABEL + 4).max(8);
    let mut lines = vec![Row::of("Edit project", look.accent), Row::default()];
    let mut cursor = None;
    for (index, label, input, note) in [
        (0, "Name", &general.name, None),
        (1, "Description", &general.about, None),
        (
            2,
            "Stack",
            &general.stack,
            Some("Separate items with commas."),
        ),
    ] {
        let (_, (_, column)) = input.wrap(16_384);
        let value = input.value();
        let last = value.rsplit('\n').next().unwrap_or(value);
        let typed: Vec<char> = last.chars().collect();
        let column = column.min(typed.len());
        let start = column.saturating_sub(field - 1);
        let shown: String = typed.iter().skip(start).take(field).collect();
        let focused = general.cursor == index;
        if focused {
            cursor = Some((LABEL + 2 + column - start, lines.len()));
        }
        let style = if focused { look.accent } else { look.muted };
        lines.push(
            Row::of(label, if focused { look.accent } else { look.muted })
                .pad(LABEL)
                .push("[", style)
                .push(" ", Style::new())
                .cat(Row::of(shown, look.fg).pad(field))
                .push(" ", Style::new())
                .push("]", style),
        );
        if let Some(note) = note.filter(|_| focused) {
            lines.push(Row::default().pad(LABEL).push(note, look.muted));
        }
    }
    lines.push(Row::default());
    let switch = |index: usize, label: &str, on: bool| {
        let row = Row::of(format!("[{}] {label}", if on { "x" } else { " " }), look.fg).pad(24);
        if general.cursor == index {
            row.restyle(look.selection)
        } else {
            row
        }
    };
    let platforms: Vec<Row> = Platform::ALL
        .into_iter()
        .enumerate()
        .map(|(at, platform)| {
            switch(
                3 + at,
                platform.label(),
                general.platforms.contains(&platform),
            )
        })
        .collect();
    let research: Vec<Row> = ResearchKind::ALL
        .into_iter()
        .enumerate()
        .map(|(at, kind)| {
            switch(
                3 + Platform::ALL.len() + at,
                kind.label(),
                general.research.contains(&kind),
            )
        })
        .collect();
    for (label, switches) in [("Platforms", platforms), ("Research", research)] {
        for (pair, chunk) in switches.chunks(2).enumerate() {
            let mut row = Row::of(if pair == 0 { label } else { "" }, look.muted).pad(LABEL);
            for item in chunk {
                row = row.cat(item.clone()).push("  ", Style::new());
            }
            lines.push(row);
        }
    }
    lines.push(Row::default());
    if general.busy {
        lines.push(Row::of(format!("{SPINNER} Saving..."), look.running));
    } else if let Some(error) = &general.error {
        lines.extend(
            wrap_words(error, content)
                .into_iter()
                .map(|line| Row::of(line, look.error)),
        );
    } else {
        lines.extend(
            wrap_words(
                "Turning research off stops its agent. Turning it on starts one when the project has a pool.",
                content,
            )
            .into_iter()
            .map(|line| Row::of(line, look.muted)),
        );
    }
    assert!(general.cursor < GENERAL_FIELDS);
    let mut help = vec![("Enter", "Save"), ("Tab", "Next")];
    if matches!(
        Field::at(general.cursor),
        Field::Platform(_) | Field::Research(_)
    ) {
        help.push(("Space", "Switch"));
    }
    if general.cursor == 1 {
        help.push(("Ctrl-J", "New line"));
    }
    help.push(("Esc", "Cancel"));
    (lines, cursor.filter(|_| !general.busy), help)
}

/// The Settings line for a project's extra read-only folders: how many, or loading.
/// Takes the screen and look. Returns the row with its edit key.
fn folders_line(screen: &ProjectScreen, look: &Look) -> Row {
    let text = match screen
        .read_paths
        .as_ref()
        .map(|saved| saved.paths.as_slice())
    {
        None => "Loading...".to_owned(),
        Some([]) => "Toolchains only".to_owned(),
        Some([one]) => one.clone(),
        Some(many) => format!("{} extra read-only folders", many.len()),
    };
    Row::of(text, look.fg).push("   f Edit read-only folders", look.muted)
}

/// The Settings line for a project's hidden folders: how many, or loading.
/// Takes the screen and look. Returns the row with its edit key.
fn hidden_line(screen: &ProjectScreen, look: &Look) -> Row {
    let text = match screen
        .hidden_paths
        .as_ref()
        .map(|saved| saved.paths.as_slice())
    {
        None => "Loading...".to_owned(),
        Some([]) => "None hidden".to_owned(),
        Some([one]) => one.clone(),
        Some(many) => format!("{} hidden folders", many.len()),
    };
    Row::of(text, look.fg).push("   k Edit hidden folders", look.muted)
}

fn settings(screen: &ProjectScreen, content: usize, room: usize, look: &Look) -> Drawn {
    if let Some(general) = &screen.general {
        return general_form(general, content, look);
    }
    let entry = |label: &str, value: Row| Row::of(label, look.muted).pad(10).cat(value);
    let pool = match &screen.pool {
        _ if !screen.pool_ready => Row::of("Not ready yet", look.muted),
        Some(pool) if pool.assigned => {
            Row::of(pool.name.clone(), look.fg).push("   Enter Change", look.muted)
        }
        Some(pool) => Row::of(pool.name.clone(), look.fg)
            .push("  ", Style::new())
            .push("! Not assigned yet", look.warning)
            .push("   Enter Assign", look.muted),
        None => Row::of("No pool yet", look.warning).push("   Enter Pick a pool", look.muted),
    };
    let mut lines = vec![
        Row::of("Project settings", look.accent),
        Row::default(),
        entry(
            "Name",
            Row::of(screen.name.clone(), look.fg).push("   g Edit details", look.muted),
        ),
        entry(
            "Folder",
            Row::of(folder(screen), look.fg)
                .push("   x Remove project", look.muted)
                .push("   m Move into CrazyAgent folder", look.muted),
        ),
        entry("Pool", pool),
        entry(
            "Accounts",
            Row::of(
                screen
                    .account_order()
                    .unwrap_or_else(|| "Loading...".to_owned()),
                look.fg,
            )
            .push("   u Change order", look.muted),
        ),
        entry("Team", team_line(screen, look)),
        entry("Folders", folders_line(screen, look)),
        entry("Hidden", hidden_line(screen, look)),
        entry(
            "Access",
            Row::of(
                super::project_access::summary(screen.agent_policy.as_ref()),
                look.fg,
            )
            .push("   c Agent access", look.muted),
        ),
        entry(
            "Process",
            Row::of("Instructions for this project's agents", look.fg)
                .push("   i Edit", look.muted),
        ),
        entry(
            "Roadmap",
            match &screen.jobs {
                Some(jobs) => planning_value(&planning(jobs), look),
                None => Row::of("Loading...", look.muted),
            },
        ),
        entry("Status", screen.roadmap.value(look, screen.now_ms)),
    ];
    if room.saturating_sub(lines.len()) > screen.plugins.floor() {
        lines.push(Row::default());
    }
    let (plugins, mut help) = screen
        .plugins
        .rows(content, room.saturating_sub(lines.len()), look);
    lines.extend(plugins);
    let pool_key = match &screen.pool {
        _ if !screen.pool_ready => None,
        Some(pool) if pool.assigned => Some("Change pool"),
        Some(_) => Some("Assign pool"),
        None => Some("Pick a pool"),
    };
    if let Some(at) = help.iter().position(|item| *item == TABS) {
        help.insert(at, ("o", "Open folder"));
        if let Some(label) = pool_key
            && help.iter().all(|(key, _)| *key != "Enter")
        {
            help.insert(0, ("Enter", label));
        }
    }
    let _ = cells;
    (lines, None, help)
}
