use super::new_project::{display_path, harness_name, size_label, team_summary};
use super::new_project_input::cells;
use super::new_project_view::{
    Look, Row, Screen, columns, list_row, modal, research_glyph, spread, tab_strip, wrap_words,
};
use super::project::{
    Field, GENERAL_FIELDS, GeneralEdit, MasterStatus, ProjectScreen, REVIEW_ROWS, Speaker, Tab,
    Transcript, memory_source, research_kind,
};
use super::project_markdown::{MdLine, Role, render};
use super::project_time::{clock, duration};
use crate::project_setup::{
    ContextItem, ContextKind, Platform, ResearchJob, ResearchKind, ResearchStatus,
};
use ratatui::style::Style;

const SPINNER: &str = "\u{283c}";

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
    let items: Vec<(String, String)> = Tab::ALL
        .iter()
        .map(|tab| {
            let count = if *tab == Tab::Agents && running > 0 {
                running.to_string()
            } else {
                String::new()
            };
            (
                format!("{} {}", look.glyph(tab.glyph()), tab.label()),
                count,
            )
        })
        .collect();
    let mut rows: Vec<Row> = tab_strip(width, &items, screen.tab as usize, look).into();
    let room = body_height.saturating_sub(2);
    let mut crumbs = vec!["Home".to_owned(), screen.name.clone()];
    if screen.tab != Tab::Overview {
        crumbs.push(screen.tab.label().to_owned());
    }
    let (lines, cursor, help) = match screen.tab {
        Tab::Overview => overview(screen, width, content, look),
        Tab::Conversation => conversation(screen, content, room, look),
        Tab::Agents => agents(screen, width, content, room, look),
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
                    "Lines {}-{end} of {}. Read to the end to save it.",
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
            vec![
                ("Down", "Read on"),
                ("e", "Edit first"),
                ("n", "Dismiss"),
                ("Esc", "Back"),
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
    if screen.confirm_remove {
        let mut body = vec![Row::of(
            format!("Remove {} from CrazyAgent?", screen.name),
            look.fg,
        )];
        body.extend(
            wrap_words(
                "Its research and agents stop and CrazyAgent forgets it. The project folder stays on disk, with every file in it.",
                50,
            )
            .into_iter()
            .map(|line| Row::of(line, look.muted)),
        );
        body.push(Row::default());
        body.push(
            Row::of("[ Remove project ]", look.fg)
                .push("   ", Style::new())
                .push("[> Keep project ]", look.selection),
        );
        rows = modal(&rows, width, 56, "Remove project", body, 5, look);
        help = vec![("y", "Remove"), ("n", "Keep"), ("Esc", "Keep")];
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
type Drawn = (Vec<Row>, Option<(usize, usize)>, Help);

const PICKER_ROWS: usize = 8;

const TABS: (&str, &str) = ("Shift-\u{2190}\u{2192}", "Tabs");

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
        ResearchStatus::NeedsPool => Row::of("! Needs a pool", look.warning),
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

fn work_rows(screen: &ProjectScreen, look: &Look, paths: bool, width: usize) -> Vec<Row> {
    let Some(setup) = &screen.setup else {
        return vec![Row::of("Loading...", look.muted)];
    };
    if setup.research.is_empty() {
        return vec![Row::of(
            "  No research was picked for this project.",
            look.muted,
        )];
    }
    let suggested = screen.suggested().len();
    setup
        .research
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

fn overview(screen: &ProjectScreen, width: usize, content: usize, look: &Look) -> Drawn {
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
                "Pick a pool in Settings. Work starts as soon as you do.",
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
        return (lines, None, vec![key, TABS, ("Esc", "Home")]);
    }
    if !screen.suggested().is_empty() {
        help.extend([
            ("y", "Save to memory"),
            ("n", "Dismiss"),
            ("e", "Edit first"),
        ]);
    }
    help.extend([
        ("Enter", "Open"),
        ("o", "Open folder"),
        TABS,
        ("Esc", "Home"),
    ]);
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
        let mut right = vec![Row::of("Team", look.heading)];
        if let Some(setup) = &screen.setup {
            let team = &setup.team;
            right.push(
                Row::of("Master", look.muted)
                    .pad(12)
                    .push(harness_name(&team.master, None), look.fg),
            );
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

fn transcript_lines(transcript: &Transcript, content: usize, now_ms: i64, look: &Look) -> Vec<Row> {
    let mut lines = Vec::new();
    for message in &transcript.messages {
        let (name, style) = match message.speaker {
            Speaker::You => ("You".to_owned(), look.accent),
            Speaker::Master => (transcript.harness.clone(), look.heading),
            Speaker::Notice => (String::new(), look.fg),
        };
        if message.speaker != Speaker::Notice {
            let mut head = Row::of(name, style);
            if let Some(time) = &message.time {
                head = head.push(format!("  {time}"), look.muted);
            }
            lines.push(head);
        }
        let body: Vec<MdLine> = if message.speaker == Speaker::Master {
            render(&message.text, content)
        } else {
            render(&message.text.replace('\n', "\n\n"), content)
                .into_iter()
                .filter(|line| !line.is_empty())
                .collect()
        };
        for line in body {
            lines.push(md_row(
                &line,
                if message.error { look.error } else { look.fg },
                look,
            ));
        }
        lines.push(Row::default());
    }
    if let MasterStatus::Thinking { since_ms } = transcript.status {
        lines.push(
            Row::of(SPINNER, look.running)
                .push(" Thinking...", look.fg)
                .push(
                    since_ms
                        .map(|since| format!("  {}", duration(now_ms - since)))
                        .unwrap_or_default(),
                    look.muted,
                ),
        );
    } else if lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
    }
    lines
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

fn conversation(screen: &ProjectScreen, content: usize, room: usize, look: &Look) -> Drawn {
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
    let mut lines = vec![
        spread(content, head, master_status(&status, look)),
        Row::default(),
    ];
    let available = room.saturating_sub(4);
    let (body, top, input, help): (Vec<Row>, bool, Row, Help) = if !screen.pool_ready {
        (
            vec![Row::of(
                "The master conversation is not ready yet in this window.",
                look.fg,
            )],
            true,
            Row::of("> Not ready yet", look.muted),
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
            Row::of("> Pick a pool first", look.muted),
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
            transcript_lines(transcript, content, screen.now_ms, look)
        };
        let empty = transcript.messages.is_empty();
        let mut help = if screen.typing {
            vec![
                ("Enter", "Send"),
                ("Ctrl-J", "New line"),
                ("Ctrl-X", "Stop turn"),
                ("PgUp", "Scroll"),
                TABS,
                ("Esc", "Leave input"),
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
        (body, empty, input_row(screen, look), help)
    } else {
        (
            vec![Row::of(
                "The master conversation is not ready yet.",
                look.muted,
            )],
            true,
            Row::of("> Not ready yet", look.muted),
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
    lines.push(Row::of(look.glyph("\u{2500}").repeat(content), look.muted));
    let cursor = (screen.typing && assigned && screen.transcript.is_some()).then(|| {
        let (input_lines, (_, column)) = screen.input.wrap(content.saturating_sub(2).max(1));
        let _ = input_lines;
        (2 + column, lines.len())
    });
    lines.push(input);
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
    ]);
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
            (_, Some(start), None) => Row::of(
                format!(
                    "Started {}, running {}",
                    clock(start, screen.offset),
                    duration(screen.now_ms - start)
                ),
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
            .filter(|_| job.status == ResearchStatus::Failed)
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
        steps.push((None, "Waiting for a pool".to_owned(), look.warning));
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

fn source(screen: &ProjectScreen, item: &ContextItem, look: &Look) -> (Row, Row) {
    match item.kind {
        ContextKind::Brief => (Row::default(), Row::of("Project brief", look.muted)),
        ContextKind::Document => (Row::default(), Row::of("You added it", look.muted)),
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

fn tree_rows(screen: &ProjectScreen, look: &Look, wide: bool, width: usize) -> Vec<Row> {
    let items = screen.tree();
    let mut rows = Vec::new();
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
        rows.push(list_row(
            row,
            width,
            index == screen.tree_cursor,
            focus,
            look,
        ));
    }
    rows
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
                ("o", "Open in editor"),
                ("PgDn", "Scroll"),
                ("Esc", "Back to files"),
            ]
        } else {
            vec![
                ("m", "Save to memory"),
                ("o", "Open in editor"),
                ("PgDn", "Scroll"),
                ("Esc", "Back to files"),
            ]
        };
        return (preview_rows(screen, content, room, look), None, help);
    }
    let help = vec![
        ("Enter", "Open"),
        ("a", "Add file"),
        ("o", "Open in editor"),
        ("d", "Delete"),
        ("m", "Save to memory"),
        TABS,
        ("Esc", "Home"),
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
        let mut tree = vec![Row::of("Files", look.accent), Row::default()];
        tree.extend(tree_rows(screen, look, true, tree_width));
        tree.push(Row::default());
        tree.push(Row::of("Agents write results here", look.muted));
        tree.push(Row::of("as Markdown files.", look.muted));
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
    lines.extend(tree_rows(screen, look, false, content));
    lines.push(Row::default());
    lines.push(Row::of(
        "Agents write results here as Markdown. o opens a file in your editor.",
        look.muted,
    ));
    (lines, None, help)
}

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
            Row::of(folder(screen), look.fg).push("   x Remove project", look.muted),
        ),
        entry("Pool", pool),
        entry("Team", team_line(screen, look)),
        Row::default(),
    ];
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
