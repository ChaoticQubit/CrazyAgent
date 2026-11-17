use super::*;

/// Draws the New project wizard body for the current step, never narrower than the
/// smallest frame the shell draws. Takes the wizard, the frame width and height, and
/// the look. Returns the body rows, cursor, breadcrumb and help keys.
pub(in crate::tui) fn wizard_screen(
    wizard: &Wizard,
    width: u16,
    height: u16,
    look: &Look,
) -> Screen {
    let width = usize::from(width.max(crate::tui::frame::MIN_WIDTH));
    let wide = width >= 100;
    let content = if wide { width - 30 } else { width - 4 };
    let mut draw = Draw {
        wizard,
        look,
        width,
        content,
        field: content.min(72),
        rows: Vec::new(),
        cursor: None,
    };
    match wizard.step {
        Step::About => about(&mut draw),
        Step::Stack => stack(&mut draw),
        Step::Documents => documents(&mut draw),
        Step::Work => work(&mut draw),
        Step::Pool => pool(&mut draw),
        Step::Team => team(&mut draw),
        Step::Review => review(&mut draw),
    }
    let body_height = usize::from(height).saturating_sub(4);
    let (rows, cursor) = if wide {
        rail(&draw, body_height)
    } else {
        strip(&draw)
    };
    let mut rows = rows;
    rows.truncate(body_height);
    let mut help = help_keys(wizard);
    if let Some(open) = wizard.publish.panel {
        rows = publish_box(wizard, rows, width, look);
        help = panel_hints(open, "Done", "Close");
    }
    if let Some(picker) = wizard.picker {
        rows = picker_box(wizard, picker.target, picker.cursor, rows, width, look);
        help = vec![
            ("\u{2191}\u{2193}", "Move"),
            ("Enter", "Pick"),
            ("r", "Check again"),
            ("Esc", "Close"),
        ];
    }
    Screen {
        rows,
        cursor: cursor.filter(|(_, y)| usize::from(*y) < body_height),
        crumbs: vec![
            "Home".into(),
            "New project".into(),
            wizard.step.title().into(),
        ],
        help,
        status: None,
    }
}

fn rail(draw: &Draw, height: usize) -> (Vec<Row>, Option<(u16, u16)>) {
    let look = draw.look;
    let wizard = draw.wizard;
    let mut rail = vec![Row::of("New project", look.heading), Row::default()];
    for step in Step::ALL {
        rail.push(if step == wizard.step {
            Row::of(format!("> {}", step.title()), look.accent)
        } else if wizard.done[step.index()] {
            Row::of(look.glyph("\u{2713}"), look.success)
                .push(format!(" {}", step.title()), look.fg)
        } else {
            Row::of(format!("  {}", step.title()), look.muted)
        });
    }
    rail.push(Row::default());
    rail.push(Row::of("Esc keeps this draft", look.muted));
    rail.push(Row::of("and goes home.", look.muted));
    let mut rows = Vec::new();
    for index in 0..height.max(draw.rows.len()) {
        let left = rail.get(index).cloned().unwrap_or_default();
        let right = draw.rows.get(index).cloned().unwrap_or_default();
        rows.push(
            Row::of("  ", Style::new())
                .cat(left.pad(24))
                .push(look.glyph("\u{2502}"), look.muted)
                .push(" ", Style::new())
                .cat(right.pad(draw.content))
                .pad(draw.width),
        );
    }
    let cursor = draw
        .cursor
        .map(|(row, column)| ((28 + column) as u16, row as u16));
    (rows, cursor)
}

fn strip(draw: &Draw) -> (Vec<Row>, Option<(u16, u16)>) {
    let wizard = draw.wizard;
    let items: Vec<(String, String)> = Step::ALL
        .iter()
        .map(|step| {
            let label = if wizard.done[step.index()] && *step != wizard.step {
                format!("{} {}", draw.look.glyph("\u{2713}"), step.short())
            } else {
                step.short().to_owned()
            };
            (label, String::new())
        })
        .collect();
    let [top, under] = tab_strip(draw.width, &items, wizard.step.index(), draw.look);
    let mut rows = vec![top, under];
    for row in &draw.rows {
        rows.push(Row::of("  ", Style::new()).cat(row.clone()).pad(draw.width));
    }
    let cursor = draw
        .cursor
        .map(|(row, column)| ((2 + column) as u16, (row + 2) as u16));
    (rows, cursor)
}

fn about(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("About", look.accent));
    draw.add(Row::of("Tell agents what you are building.", look.muted));
    draw.add(Row::default());
    draw.field("Name", Control::Name, &wizard.name, None);
    draw.field("What is it about?", Control::About, &wizard.about, None);
    draw.add(Row::default());
    draw.add(Row::of("Project folder", look.heading));
    draw.add(
        Row::of(radio(true, "Create a new folder"), look.fg)
            .pad(28)
            .push("Made when you create the project", look.muted),
    );
    draw.add(Row::of(
        radio(false, "Use an existing folder  Not ready yet"),
        look.muted,
    ));
    draw.add(Row::of(
        "Agents can change files only inside this folder.",
        look.muted,
    ));
    publish_row(draw);
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn publish_row(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    let publish = &wizard.publish;
    let (name, about) = (wizard.name.value(), wizard.about.value());
    let (state, style) = match (&publish.github, publish.on) {
        (_, false) => (String::new(), look.muted),
        (GitHub::Checking, true) => ("Checking GitHub...".to_owned(), look.muted),
        (GitHub::SignedOut, true) => ("Not signed in".to_owned(), look.warning),
        (GitHub::Failed(problem), true) => (problem.clone(), look.error),
        (GitHub::SignedIn, true) => match (publish.problem(name, about), publish.shown_owner()) {
            (None, Some(owner)) => (
                format!(
                    "{}, {owner}/{}",
                    if publish.private { "Private" } else { "Public" },
                    publish.shown_name(name)
                ),
                look.fg,
            ),
            _ => ("Check the GitHub details (Enter)".to_owned(), look.error),
        },
    };
    let label = checkbox(publish.on, PUBLISH_TOGGLE);
    let focus = draw.focus(Control::Publish) && publish.panel.is_none();
    draw.add(
        Row::of(label, if focus { look.accent } else { look.fg })
            .push("  ", Style::new())
            .push(state, style),
    );
    let line = if publish.on && publish.github == GitHub::SignedOut {
        format!("{SIGN_IN_WAIT} Enter signs in.")
    } else {
        PUBLISH_LINE.to_owned()
    };
    draw.add(Row::of(format!("    {line}"), look.muted));
}

/// Fits a text into `width` terminal cells by keeping its end behind a leading ellipsis, so a
/// path still shows its own name; wide characters count two cells. Inputs: the text and the
/// cells. Output: the text whole when it fits, else the ellipsis and the longest tail that fits.
pub(in crate::tui) fn shown_tail(text: &str, width: usize) -> String {
    if cells(text) <= width {
        return text.to_owned();
    }
    let mut room = width.saturating_sub(1);
    let mut kept = Vec::new();
    for character in text.chars().rev() {
        let need = char_cells(character);
        if need > room {
            break;
        }
        room -= need;
        kept.push(character);
    }
    let tail: String = kept.into_iter().rev().collect();
    let shown = format!("\u{2026}{tail}");
    assert!(cells(&shown) <= width.max(1));
    shown
}

/// The keys of the GitHub panel for its open row: that row's own key, moving between rows
/// and closing. Inputs: the open row, the last row's word and the closing key's word.
/// Output: the keys, as key and label.
pub(in crate::tui) fn panel_hints(
    open: PanelRow,
    done: &'static str,
    close: &'static str,
) -> Vec<(&'static str, &'static str)> {
    vec![
        match open {
            PanelRow::Visibility => ("Space", "Private or public"),
            PanelRow::Account => ("\u{2190}\u{2192}", "Change account"),
            PanelRow::Name | PanelRow::Description => ("Type", "Edit"),
            PanelRow::Docs => ("Space", "On or off"),
            PanelRow::Done => ("Enter", done),
        },
        ("\u{2191}\u{2193}", "Move"),
        ("Esc", close),
    ]
}

fn publish_box(wizard: &Wizard, base: Vec<Row>, width: usize, look: &Look) -> Vec<Row> {
    if wizard.publish.panel.is_none() {
        return base;
    }
    let (name, about) = (wizard.name.value(), wizard.about.value());
    let body = publish_panel(&wizard.publish, name, about, "Done", look);
    modal(&base, width, 64, "Publish on GitHub", body, 2, look)
}

/// The GitHub panel's rows: visibility, account, repository name, description, the docs
/// switch, the problem of a row a publish Start would refuse, the accounts' problem and the
/// last row, the open row selected. New project draws them in its modal and the Overview's
/// publish form in its own. Inputs: the block, the project name, the about text, the last
/// row's word and the look. Output: the rows.
pub(in crate::tui) fn publish_panel(
    publish: &Publish,
    name: &str,
    about: &str,
    done: &str,
    look: &Look,
) -> Vec<Row> {
    let open = publish.panel;
    let problem = publish.problem(name, about);
    let field = |text: String| format!("[ {:<34} ]", shown_tail(&text, 34));
    let owner = publish.shown_owner().map_or_else(
        || "No account yet".to_owned(),
        |login| {
            let personal = publish
                .owners
                .iter()
                .any(|owner| owner.login == login && owner.personal);
            format!("< {login}{} >", if personal { " (you)" } else { "" })
        },
    );
    let line = |row: PanelRow| -> Row {
        let text = match row {
            PanelRow::Visibility => format!(
                "Visibility   {}  {}",
                radio(publish.private, "Private"),
                radio(!publish.private, "Public")
            ),
            PanelRow::Account => format!("Account      {owner}"),
            PanelRow::Name => format!("Repository   {}", field(publish.shown_name(name))),
            PanelRow::Description => {
                format!("Description  {}", field(publish.shown_description(about)))
            }
            PanelRow::Docs => checkbox(publish.docs, PUBLISH_DOCS),
            PanelRow::Done => format!("[ {done} ]"),
        };
        let error = problem.is_some_and(|(at, _)| at == row);
        let style = if error { look.error } else { look.fg };
        list_row(Row::of(text, style), 56, Some(row) == open, true, look)
    };
    let mut body: Vec<Row> = PanelRow::ALL[..5].iter().map(|row| line(*row)).collect();
    if let Some((_, text)) = problem {
        for wrapped in wrap_words(text, 56) {
            body.push(Row::of(wrapped, look.error));
        }
    }
    if let Some(text) = &publish.owners_problem {
        for wrapped in wrap_words(&printable(text.clone()), 56).into_iter().take(3) {
            body.push(Row::of(wrapped, look.muted));
        }
    }
    body.push(Row::default());
    body.push(line(PanelRow::Done));
    body
}

fn stack(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Stack and platforms", look.accent));
    draw.add(Row::default());
    let filter = if wizard.filtering || !wizard.filter.value().is_empty() {
        Row::of("/ ", look.muted).push(wizard.filter.value(), look.accent)
    } else {
        Row::of("/ Filter", look.muted)
    };
    if wizard.filtering {
        draw.cursor = Some((
            draw.rows.len(),
            14 + wizard.filter.value().chars().map(char_cells).sum::<usize>(),
        ));
    }
    let content = draw.content;
    draw.add(spread(
        content,
        Row::of("Tech stack", look.heading)
            .push("  ", Style::new())
            .cat(filter),
        Row::of(format!("{} selected", wizard.tech.len()), look.muted),
    ));
    let options = wizard.tech_options();
    let (columns, rows) = wizard.grid_shape();
    let cell = if draw.width >= 100 { 22 } else { 24 };
    match &wizard.catalog {
        None => {
            let text = if wizard.catalog_problem.is_some() {
                "The tech list is not ready yet. Add your own below."
            } else {
                "Loading the tech list..."
            };
            draw.add(Row::of(text, look.muted));
        }
        Some(catalog) => {
            let visible = options.len().min(columns * rows);
            let focus = draw.focus(Control::Grid) && !wizard.filtering;
            for row in 0..rows.min(visible) {
                let mut line = Row::default();
                for column in 0..columns {
                    let index = column * rows + row;
                    let Some(label) = options.get(index).filter(|_| index < visible) else {
                        continue;
                    };
                    let text = checkbox(wizard.tech.contains(label), label);
                    line = line.cat(if focus && index == wizard.grid {
                        Row::of(format!(">{text}"), look.selection)
                            .pad(cell)
                            .restyle(look.selection)
                    } else {
                        Row::of(format!(" {text}"), look.fg).pad(cell)
                    });
                }
                draw.add(line);
            }
            let total = catalog.tech_stack.len();
            let more = if visible < options.len() || visible < total {
                " Press / to find more."
            } else {
                ""
            };
            draw.add(Row::of(
                format!("{visible} of {total} shown.{more}"),
                look.muted,
            ));
        }
    }
    let focus = draw.focus(Control::Custom);
    let bracket = if focus { look.accent } else { look.muted };
    if focus && wizard.typing {
        draw.cursor = Some((draw.rows.len(), 16 + cells(wizard.custom.value())));
    }
    let mut custom = Row::of("Add your own", if focus { look.accent } else { look.fg })
        .pad(14)
        .push("[", bracket)
        .push(" ", Style::new())
        .cat(Row::of(wizard.custom.value(), look.fg).pad(28))
        .push(" ", Style::new())
        .push("]", bracket);
    if let Some((text, error)) = draw.note_for(Control::Custom) {
        custom = custom
            .push("  ", Style::new())
            .push(text, if error { look.error } else { look.success });
    }
    draw.add(custom);
    if let Some((text, _)) = draw.note_for(Control::Grid) {
        draw.note(&text, look.error);
    }
    draw.add(Row::default());
    draw.add(Row::of("Platforms", look.heading));
    let focus = draw.focus(Control::Platforms);
    for line in [&PLATFORM_ORDER[..3], &PLATFORM_ORDER[3..]] {
        let mut row = Row::default();
        for (index, platform) in line.iter().enumerate() {
            if index > 0 {
                row = row.push("   ", Style::new());
            }
            let text = checkbox(wizard.platforms.contains(platform), platform.label());
            let at = PLATFORM_ORDER
                .iter()
                .position(|p| p == platform)
                .unwrap_or(0);
            row = row.push(
                text,
                if focus && at == wizard.platform_cursor {
                    look.selection
                } else {
                    look.fg
                },
            );
        }
        draw.add(row);
    }
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn documents(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Documents", look.accent));
    draw.muted("Add PDFs, notes or research files. They are copied into context/documents in the project folder. This step is optional.");
    draw.add(Row::default());
    draw.field(
        "Add a file or folder",
        Control::Path,
        &wizard.path,
        Some("Tab completes the path. Enter adds it."),
    );
    draw.add(Row::default());
    if wizard.documents.is_empty() {
        draw.add(Row::of("No documents yet.", look.muted));
    } else {
        let widths = [30, 12, 8];
        draw.add(
            Row::of("  ", Style::new()).cat(
                columns(
                    &widths,
                    vec![
                        Row::of("File", look.muted),
                        Row::of("Type", look.muted),
                        Row::of("Size", look.muted),
                    ],
                )
                .restyle(look.muted),
            ),
        );
        let focus = draw.focus(Control::Documents);
        let shown = draw.content.saturating_sub(0);
        for (index, document) in wizard.documents.iter().enumerate() {
            let row = columns(
                &widths,
                vec![
                    Row::of(document.name.clone(), look.fg),
                    Row::of(document.kind(), look.fg),
                    Row::of(size_label(document.bytes), look.fg),
                ],
            );
            draw.add(list_row(
                row,
                56.min(shown),
                index == wizard.document_cursor,
                focus,
                &look,
            ));
        }
        let total: u64 = wizard.documents.iter().map(|d| d.bytes).sum();
        let count = wizard.documents.len();
        draw.add(Row::of(
            format!(
                "{count} {}, {} in total",
                if count == 1 { "file" } else { "files" },
                size_label(total)
            ),
            look.muted,
        ));
    }
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn work(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Work to start", look.accent));
    draw.muted("Agents start these as soon as the project exists. Results land in context/research as Markdown, with links to every source.");
    draw.add(Row::default());
    let focus = draw.focus(Control::Work);
    let content = draw.content;
    for (index, kind) in ResearchKind::ALL.into_iter().enumerate() {
        let label = format!("{} {}", look.glyph(research_glyph(kind)), kind.label());
        let row = columns(
            &[26, content.saturating_sub(32)],
            vec![
                Row::of(checkbox(wizard.research.contains(&kind), &label), look.fg),
                Row::of(research_description(kind), look.muted),
            ],
        );
        draw.add(list_row(
            row,
            content,
            focus && index == wizard.work_cursor,
            focus,
            &look,
        ));
    }
    let roadmap = columns(
        &[26, content.saturating_sub(32)],
        vec![
            Row::of(
                checkbox(
                    wizard.roadmap,
                    &format!("{} Roadmap", look.glyph("\u{2630}")),
                ),
                look.fg,
            ),
            Row::of(ROADMAP_LABEL, look.muted),
        ],
    );
    draw.add(list_row(
        roadmap,
        content,
        focus && wizard.work_cursor == ResearchKind::ALL.len(),
        focus,
        &look,
    ));
    draw.add(Row::default());
    let count = wizard.research.len();
    draw.add(Row::of(
        match count {
            0 => "No research starts. You can start it later from the project.".to_owned(),
            1 => "1 agent starts. Change counts in Team.".to_owned(),
            n => format!("{n} agents start, one per item. Change counts in Team."),
        },
        look.fg,
    ));
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn pool(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    draw.add(Row::of("Pool", look.accent));
    draw.add(Row::of(
        "Agents in this project use accounts from one pool.",
        look.muted,
    ));
    draw.add(Row::default());
    let focus = draw.focus(Control::Pools);
    let content = draw.content;
    if wizard.pools.is_empty() {
        draw.add(list_row(
            Row::of(radio(true, "No pool yet"), look.fg),
            content,
            true,
            focus,
            &look,
        ));
        draw.add(Row::default());
        for line in wrap_words(
            "You have no pools yet. Press a to create one now, or continue and pick one later. Work waits and shows \"Needs a pool\" until then.",
            content,
        ) {
            draw.add(Row::of(line, look.fg));
        }
    } else {
        let widths = [16, 12, content.saturating_sub(34)];
        for pool in &wizard.pools {
            let picked = wizard.pool.as_ref() == Some(&pool.id);
            let mix: Vec<String> = pool
                .harnesses
                .iter()
                .map(|(id, _)| harness_name(id, wizard.harnesses.as_deref()))
                .collect();
            let row = columns(
                &widths,
                vec![
                    Row::of(radio(picked, &pool.name), look.fg),
                    Row::of(
                        format!(
                            "{} {}",
                            pool.accounts,
                            if pool.accounts == 1 {
                                "account"
                            } else {
                                "accounts"
                            }
                        ),
                        look.fg,
                    ),
                    Row::of(mix.join(", "), look.muted),
                ],
            );
            draw.add(list_row(row, content, picked, focus, &look));
        }
        let none = wizard.pool.is_none();
        let row = columns(
            &widths,
            vec![
                Row::of(radio(none, "No pool yet"), look.fg),
                Row::default(),
                Row::of("Work waits and shows \"Needs a pool\"", look.muted),
            ],
        );
        draw.add(list_row(row, content, none, focus, &look));
    }
    draw.add(Row::default());
    draw.add(Row::of("a  Create a pool", look.muted));
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn team(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    let wide = draw.width >= 100;
    draw.add(Row::of("Team", look.accent));
    let source = match wizard.pool_name() {
        Some(name) => format!("Defaults come from pool {name}."),
        None => "Pick a pool for smarter defaults.".to_owned(),
    };
    draw.add(Row::of(
        if wide {
            format!("Choose which harness leads and which ones do the work. {source}")
        } else {
            format!("Pick who leads and who does the work. {source}")
        },
        look.muted,
    ));
    draw.add(Row::default());
    let Some(team) = &wizard.team else {
        draw.add(Row::of(
            if wizard.catalog_problem.is_some() {
                "The team defaults are not ready yet. CrazyAgent picks them when the project is created."
            } else {
                "Loading the team defaults..."
            },
            look.muted,
        ));
        draw.add(Row::default());
        draw.buttons("Continue");
        return;
    };
    let widths: [usize; 5] = if wide {
        [12, 14, 8, 6, 20]
    } else {
        [11, 13, 7, 6, 20]
    };
    draw.add(
        Row::of("  ", Style::new()).cat(
            columns(
                &widths,
                ["Role", "Harness", "Agents", "Tools", "Accounts in pool"]
                    .iter()
                    .map(|h| Row::of(*h, look.muted))
                    .collect(),
            )
            .restyle(look.muted),
        ),
    );
    let focus = draw.focus(Control::Team);
    let catalog = wizard.harnesses.as_deref();
    let accounts = |id: &str| match wizard.pool_accounts(id) {
        Some(count) => Row::of(
            format!(
                "{count} {}",
                if count == 1 { "account" } else { "accounts" }
            ),
            look.fg,
        ),
        None => Row::of("-", look.muted),
    };
    let on = |tools: bool| if tools { "On" } else { "Off" };
    let content = draw.content;
    let master = columns(
        &widths,
        vec![
            Row::of("Master", look.fg),
            Row::of(harness_name(&team.master, catalog), look.fg),
            Row::of("1", look.fg),
            Row::of(on(team.master_tools), look.fg),
            accounts(&team.master),
        ],
    );
    draw.add(list_row(
        master,
        content,
        wizard.team_row == 0,
        focus,
        &look,
    ));
    for (index, slot) in team.sub_harnesses.iter().enumerate() {
        let row = index + 1;
        let selected = wizard.team_row == row;
        let count = if selected && focus {
            format!("< {} >", slot.agents)
        } else {
            slot.agents.to_string()
        };
        let line = columns(
            &widths,
            vec![
                Row::of(if index == 0 { "Sub-agents" } else { "" }, look.fg),
                Row::of(harness_name(&slot.harness, catalog), look.fg),
                Row::of(count, look.fg),
                Row::of(on(slot.tools), look.fg),
                accounts(&slot.harness),
            ],
        );
        draw.add(list_row(line, content, selected, focus, &look));
    }
    let add_row = team.sub_harnesses.len() + 1;
    let add = columns(
        &widths,
        vec![
            Row::of(
                if team.sub_harnesses.is_empty() {
                    "Sub-agents"
                } else {
                    ""
                },
                look.fg,
            ),
            Row::of("+ Add harness", look.muted),
            Row::default(),
            Row::default(),
            Row::default(),
        ],
    );
    draw.add(list_row(
        add,
        content,
        wizard.team_row == add_row,
        focus,
        &look,
    ));
    if team.sub_harnesses.is_empty() {
        draw.note(any_harness_note(team), look.muted);
    }
    if let Some((text, _)) = draw.note_for(Control::Team) {
        draw.note(&text, look.error);
    } else if let Some(text) = wizard.limited_note().filter(|_| focus) {
        draw.note(&text, look.warning);
    }
    draw.add(Row::default());
    let mut size = Row::of("Team size", look.heading).pad(12);
    for (label, control, input) in [
        ("Min", Control::Min, &wizard.min),
        ("Max", Control::Max, &wizard.max),
    ] {
        let focused = draw.focus(control);
        let bracket = if focused { look.accent } else { look.muted };
        if control == Control::Max {
            size = size.push("   ", Style::new());
        }
        size = size
            .push(
                format!("{label} "),
                if focused { look.accent } else { look.fg },
            )
            .push("[", bracket)
            .push(" ", Style::new());
        if focused && wizard.typing {
            draw.cursor = Some((draw.rows.len(), size.width() + cells(input.value())));
        }
        size = size
            .push(input.value(), look.fg)
            .push(if focused { "  " } else { " " }, Style::new())
            .push("]", bracket);
    }
    draw.add(size);
    match draw
        .note_for(Control::Min)
        .or_else(|| draw.note_for(Control::Max))
    {
        Some((text, _)) => draw.note(&text, look.error),
        None => draw.add(Row::of(
            format!(
                "At most {} agents run at once. Extra work waits until one finishes.",
                team.max_agents
            ),
            look.muted,
        )),
    }
    draw.add(Row::default());
    let research = wizard.research.len();
    let on_whom = if team.sub_harnesses.is_empty() {
        "on the master's harness"
    } else {
        "on the sub-agent harnesses"
    };
    draw.add(Row::of(
        match research {
            0 => "Research: none picked in Work to start.".to_owned(),
            1 => format!("Research: 1 agent, for the one Work item, {on_whom}."),
            n => format!("Research: {n} agents, one per Work item, {on_whom}."),
        },
        look.fg,
    ));
    draw.add(Row::default());
    let tools =
        std::iter::once(team.master_tools).chain(team.sub_harnesses.iter().map(|s| s.tools));
    let any = tools.clone().any(|t| t);
    draw.add(Row::of(
        if any {
            "Tools on: agents edit files and run commands, only in the project folder."
        } else {
            "Tools off: agents read the project and reply, without running commands."
        },
        look.muted,
    ));
    draw.add(Row::default());
    draw.buttons("Continue");
}

fn picker_box(
    wizard: &Wizard,
    target: PickFor,
    cursor: usize,
    base: Vec<Row>,
    width: usize,
    look: &Look,
) -> Vec<Row> {
    let title = match target {
        PickFor::Master => "Harness for the master",
        PickFor::Slot(_) | PickFor::NewSlot => "Harness for sub-agents",
    };
    let mut body = Vec::new();
    let choices = wizard.harness_choices();
    let widths = [12, 15, 17];
    for (index, choice) in choices.iter().enumerate() {
        let (status, style) = match choice.state {
            HarnessState::Ready => (format!("{} Ready", look.glyph("\u{2713}")), look.success),
            HarnessState::NotInstalled => (
                format!("{} Not installed", look.glyph("\u{2298}")),
                look.muted,
            ),
            HarnessState::CantStart => (
                format!("{} Can't start", look.glyph("\u{2298}")),
                look.warning,
            ),
            HarnessState::CantRun => (
                format!("{} Can't run", look.glyph("\u{2298}")),
                look.warning,
            ),
            HarnessState::MasterOnly => (
                format!("{} Master only", look.glyph("\u{25e7}")),
                look.warning,
            ),
            HarnessState::Unknown => (String::new(), look.muted),
        };
        let extra = if choice.limited.is_some() {
            Row::of(
                format!("{} Limited control", look.glyph("\u{25e7}")),
                look.warning,
            )
        } else {
            match wizard.pool_accounts(&choice.id) {
                Some(count) if count > 0 => Row::of(format!("{count} in pool"), look.fg),
                _ => Row::default(),
            }
        };
        let row = columns(
            &widths,
            vec![
                Row::of(choice.name.clone(), look.fg),
                Row::of(status, style),
                extra,
            ],
        );
        let row = list_row(row, 50, index == cursor, true, look);
        body.push(
            if choice.state == HarnessState::NotInstalled && index != cursor {
                row.restyle(look.muted)
            } else {
                row
            },
        );
    }
    body.push(Row::default());
    if wizard.harnesses.is_some() {
        body.push(Row::of(
            "Harnesses that are not installed or can't run team agents cannot be picked.",
            look.fg,
        ));
        body.push(Row::of(
            "Install one, then press r to check again.",
            look.fg,
        ));
    } else {
        body.push(Row::of(
            "Not ready yet: the full harness list with install",
            look.fg,
        ));
        body.push(Row::of(
            "checks. These come from your pool's accounts.",
            look.fg,
        ));
    }
    let top = 2;
    modal(&base, width, 56, title, body, top, look)
}

fn review_summary(draw: &mut Draw) {
    let look = *draw.look;
    let wizard = draw.wizard;
    let wide = draw.width >= 100;
    let content = draw.content;
    draw.add(Row::of("Review", look.accent));
    if wide {
        draw.add(Row::default());
    }
    draw.add(Row::of(wizard.name.value().trim(), look.heading));
    for line in wrap_words(wizard.about.value().trim(), content) {
        if !line.is_empty() {
            draw.add(Row::of(line, look.fg));
        }
    }
    draw.add(Row::default());
    let entry = |label: &str, value: Row| Row::of(label, look.muted).pad(16).cat(value);
    let list = |items: Vec<String>, empty: &str| {
        if items.is_empty() {
            Row::of(empty.to_owned(), look.muted)
        } else {
            Row::of(items.join(", "), look.fg)
        }
    };
    draw.add(entry(
        "Project folder",
        Row::of("Made by CrazyAgent", look.fg).push("  new", look.muted),
    ));
    draw.add(entry("Stack", list(wizard.tech.clone(), "None picked")).pad(content));
    let platforms: Vec<String> = PLATFORM_ORDER
        .iter()
        .filter(|p| wizard.platforms.contains(p))
        .map(|p| p.label().to_owned())
        .collect();
    draw.add(entry("Platforms", list(platforms, "None picked")));
    let count = wizard.documents.len();
    draw.add(entry(
        "Documents",
        match count {
            0 => Row::of("None", look.muted),
            1 => Row::of("1 file", look.fg),
            n => Row::of(format!("{n} files"), look.fg),
        },
    ));
    let research: Vec<String> = wizard
        .research_sorted()
        .into_iter()
        .map(|kind| {
            let label = kind.label();
            if wide {
                label.to_owned()
            } else {
                label.strip_suffix(" research").unwrap_or(label).to_owned()
            }
        })
        .collect();
    draw.add(entry("Work to start", list(research, "Nothing yet")).pad(content));
    draw.add(entry(
        "Pool",
        match wizard.pool_name() {
            Some(name) => Row::of(name.to_owned(), look.fg),
            None => Row::of("No pool yet", look.warning),
        },
    ));
    draw.add(
        entry(
            "Team",
            match &wizard.team {
                Some(team) => Row::of(team_summary(team, wizard.harnesses.as_deref()), look.fg),
                None => Row::of("Picked by CrazyAgent", look.fg),
            },
        )
        .pad(content),
    );
    if wizard.phase == Phase::Editing {
        let picking = wizard.focused() == Control::Access;
        draw.add(entry(
            "Agent access",
            Row::of(
                format!("< {} >", wizard.access.label()),
                if picking { look.selection } else { look.fg },
            ),
        ));
        if wizard.access == AgentAccess::Full {
            draw.note(FULL_ACCESS_WARNING, look.warning);
        }
        for gap in wizard.gaps() {
            draw.note(gap, look.warning);
        }
    }
    draw.add(Row::default());
}

fn review(draw: &mut Draw) {
    let look = *draw.look;
    review_summary(draw);
    let wizard = draw.wizard;
    match &wizard.phase {
        Phase::Editing => {
            draw.muted(
                "Research starts right away. You can close CrazyAgent and agents keep working.",
            );
            if let Some((text, _)) = draw.note_for(Control::Continue) {
                draw.note(&text, look.error);
            }
            draw.add(Row::default());
            draw.buttons("Create project");
        }
        Phase::Creating { pool, created } => {
            let spinner = "\u{283c}";
            let documents = wizard.documents.len();
            let research = wizard.research.len();
            if *created {
                draw.add(
                    Row::of(look.glyph("\u{2713}"), look.success).push(" Folder created", look.fg),
                );
                if documents > 0 {
                    draw.add(Row::of(look.glyph("\u{2713}"), look.success).push(
                        format!(
                            " {documents} {} copied",
                            if documents == 1 {
                                "document"
                            } else {
                                "documents"
                            }
                        ),
                        look.fg,
                    ));
                }
                if research > 0 {
                    draw.add(Row::of(look.glyph("\u{2713}"), look.success).push(
                        format!(
                            " {research} research {} queued",
                            if research == 1 { "job" } else { "jobs" }
                        ),
                        look.fg,
                    ));
                }
                if *pool {
                    draw.add(Row::of(spinner, look.running).push(
                        format!(" Assigning pool {}", wizard.pool_name().unwrap_or("")),
                        look.fg,
                    ));
                }
            } else {
                draw.add(
                    Row::of(spinner, look.running).push(" Creating the project folder", look.fg),
                );
            }
        }
        Phase::Failed {
            title,
            detail,
            step,
        } => {
            draw.add(Row::of(title.clone(), look.error));
            for line in wrap_words(detail, draw.content) {
                draw.add(Row::of(line, look.fg));
            }
            draw.add(Row::default());
            let label = if *step == Step::Review {
                "Back to Review".to_owned()
            } else {
                format!("Back to {}", step.title())
            };
            draw.add(Row::of(format!("[> {label} ]"), look.selection));
        }
    }
}
