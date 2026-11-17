use super::*;

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

pub(super) fn context(
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

/// What the hidden-folders editor says the Settings list of agents that can still open a
/// hidden folder covers, so an empty list never reads as "nothing can open it" (SA-VID14-A
/// r1): team agents, taken-over panes included, and the engine runs that started before
/// (SA-VID14-R2-N2).
pub(super) const HIDDEN_SEERS_COVER: &str = "Team agents that started before you hid a folder, including ones CrazyAgent took over when it opened again, are named under Hidden with R Restart them. Master turns, research jobs and overview, roadmap, onboarding, code graph and lockfile rebuild runs that started before are named there too, and keep their access until they end.";

/// The Settings line naming the running agents that can still open a folder hidden after
/// they started, with the key that restarts them (SA-VID14-A). Takes the screen and look.
/// Returns the row, or none while no agent is listed.
fn seers_line(screen: &ProjectScreen, look: &Look) -> Option<Row> {
    let seers = screen.hidden_seers();
    (!seers.is_empty()).then(|| {
        Row::of(
            format!("{} can still open a hidden folder", seers.join(", ")),
            look.warning,
        )
        .push("   R Restart them", look.muted)
    })
}

/// The Settings line naming the kinds of engine runs still running that started before a
/// folder was hidden and can open it until they end (SA-VID14-R2-N2), wrapped to the page;
/// a run has no restart key. Takes the screen, the width the lines wrap at and the look.
/// Returns the rows, none while no run is listed.
fn runs_lines(screen: &ProjectScreen, width: usize, look: &Look) -> Vec<Row> {
    let runs = screen
        .hidden_paths
        .as_ref()
        .map(|saved| saved.runs.as_slice())
        .unwrap_or_default();
    if runs.is_empty() {
        return Vec::new();
    }
    let named: Vec<&str> = runs.iter().map(|kind| run_words(*kind)).collect();
    let line = format!(
        "Runs that can still open a hidden folder until they end: {}",
        named.join(", ")
    );
    wrap_words(&line, width)
        .into_iter()
        .map(|part| Row::of(part, look.warning))
        .collect()
}

/// The words the hidden folders list names one kind of engine run by (SA-VID14-R2-N2).
/// Takes the kind. Returns the words.
fn run_words(kind: crate::sandbox::hidden_paths::RunKind) -> &'static str {
    use crate::sandbox::hidden_paths::RunKind;
    match kind {
        RunKind::Master => "master turn",
        RunKind::Research => "research job",
        RunKind::Overview => "overview run",
        RunKind::Roadmap => "roadmap run",
        RunKind::Onboarding => "onboarding run",
        RunKind::CodeGraph => "code graph build",
        RunKind::Lockfiles => "lockfile rebuild run",
    }
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

pub(super) fn settings(screen: &ProjectScreen, content: usize, room: usize, look: &Look) -> Drawn {
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
                .push("   M Move into CrazyAgent folder", look.muted),
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
    ];
    lines.extend(seers_line(screen, look).map(|row| entry("", row)));
    lines.extend(
        runs_lines(screen, content.saturating_sub(10), look)
            .into_iter()
            .map(|row| entry("", row)),
    );
    lines.extend([
        entry(
            "Access",
            Row::of(
                super::super::project_access::summary(screen.agent_policy.as_ref()),
                look.fg,
            )
            .push("   c Agent access", look.muted),
        ),
        entry(
            "Process",
            Row::of("Instructions for this project's agents", look.fg)
                .push("   i Edit", look.muted)
                .push("   G GitHub rules", look.muted),
        ),
        entry(
            "Roadmap",
            match &screen.jobs {
                Some(jobs) => planning_value(&planning(jobs), look),
                None => Row::of("Loading...", look.muted),
            },
        ),
        entry("Status", screen.roadmap.value(look, screen.now_ms)),
    ]);
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
