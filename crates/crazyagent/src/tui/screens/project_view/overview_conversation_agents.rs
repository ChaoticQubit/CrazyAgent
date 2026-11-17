use super::*;

pub(super) fn overview(
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
        lines.extend(github_notice(screen, look));
        let tabs_room = room.saturating_sub(lines.len());
        lines.extend(overview_tab_rows(screen, content, tabs_room, look));
        help.push(key);
        if screen.overview_tabs.is_some() {
            help.extend(OVERVIEW_TAB_KEYS);
        }
        help.extend([TABS, ("Esc", "Home")]);
        if asks_github(screen) {
            help.push(PROMPT_KEY);
        }
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
    if asks_github(screen) {
        help.push(PROMPT_KEY);
    }
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
        left.extend(github_notice(screen, look));
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
    lines.extend(github_notice(screen, look));
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
pub(super) fn conversation(
    screen: &ProjectScreen,
    content: usize,
    room: usize,
    look: &Look,
) -> Drawn {
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

pub(super) fn agents(
    screen: &ProjectScreen,
    width: usize,
    content: usize,
    room: usize,
    look: &Look,
) -> Drawn {
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
        ResearchStatus::Failed | ResearchStatus::Cancelled if job.retryable => Some(("r", "Retry")),
        ResearchStatus::Failed | ResearchStatus::Cancelled | ResearchStatus::Done => None,
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
