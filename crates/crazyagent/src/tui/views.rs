use super::app::{App, Focus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Wrap};

pub fn safe_text(text: &str, limit: usize) -> String {
    let mut output = String::with_capacity(text.len().min(limit));
    for character in text.chars() {
        if character.is_control() && character != '\n' && character != '\t' {
            continue;
        }
        if output.len() + character.len_utf8() > limit {
            break;
        }
        output.push(character);
    }
    assert!(output.len() <= limit);
    output
}

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    if area.width < 12 || area.height < 6 {
        frame.render_widget(Paragraph::new("Resize terminal; Ctrl-Q exits"), area);
        return;
    }
    if area.width < 60 || area.height < 12 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
        let connection = if app.connected {
            "Connected"
        } else {
            "Disconnected"
        };
        frame.render_widget(
            Paragraph::new(format!("{:?} | {connection}", app.focus()))
                .style(Style::default().fg(Color::Black).bg(Color::Cyan)),
            rows[0],
        );
        let content = if app.is_creating_project() {
            format!("Project name: {}", app.draft())
        } else {
            body(app)
        };
        frame.render_widget(
            Paragraph::new(safe_text(&content, 65536))
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            rows[1],
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::raw(safe_text(&app.status, 4096)),
                Line::raw(safe_text(&owner_problem(app), 256)),
                Line::raw("Tab focus | Ctrl-Q quit"),
            ]),
            rows[2],
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(4),
    ])
    .split(area);
    let titles = [
        "Conversation",
        "Projects",
        "Settings",
        "Plan",
        "Supervision",
        "Memory",
        "Evidence",
    ];
    let index = match app.focus() {
        Focus::Conversation => 0,
        Focus::Projects => 1,
        Focus::Settings => 2,
        Focus::Plan => 3,
        Focus::Supervision => 4,
        Focus::Memory => 5,
        Focus::Evidence => 6,
    };
    let connection = if app.connected {
        "Connected"
    } else {
        "Disconnected"
    };
    let tabs = Tabs::new(titles)
        .select(index)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .block(Block::bordered().title(format!(
            "CrazyAgent | {connection} | {} | revision {}",
            app.owner_label(),
            app.revision().value()
        )));
    frame.render_widget(tabs, rows[0]);
    let title = app
        .selected_project()
        .map(|p| safe_text(&p.spec.name, 256))
        .unwrap_or_else(|| "No project selected".into());
    let content = safe_text(&body(app), 65536);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0))
            .block(Block::bordered().title(title)),
        rows[1],
    );
    let label = app.draft_label();
    let draft = app
        .draft()
        .chars()
        .rev()
        .take(160)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    frame.render_widget(
        Paragraph::new(safe_text(&draft, 4096))
            .block(Block::bordered().title(label))
            .style(Style::default().fg(Color::Cyan)),
        rows[2],
    );
    let footer = vec![
        Line::from(vec![
            Span::styled("Tab/Shift-Tab", Style::default().fg(Color::Cyan)),
            Span::raw(
                " focus | arrows select | PgUp/PgDn scroll | Ctrl-B brief | Ctrl-R retry | Ctrl-Q quit",
            ),
        ]),
        Line::raw(safe_text(&app.status, 4096)),
        Line::raw(safe_text(&owner_problem(app), 256)),
    ];
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::TOP)),
        rows[3],
    );
}

fn owner_problem(app: &App) -> String {
    app.owner_problem
        .as_ref()
        .map(|problem| {
            format!(
                "{}: {problem}; Evidence/approvals disabled",
                app.owner_label()
            )
        })
        .unwrap_or_else(|| app.owner_label().into())
}

pub fn body(app: &App) -> String {
    match app.focus() {
        Focus::Conversation => conversation_lines(app),
        Focus::Projects => project_lines(app),
        Focus::Settings => settings_lines(app),
        Focus::Plan => plan_lines(app),
        Focus::Supervision => format!("Supervision\n\n{}\n\n{}\n\nChild status, pause/resume/cancel and native terminal attachment are unavailable: protected owner APIs and an admitted native runtime are required. No terminal input ownership is held by this client.", task_summary(app), plan_lines(app)),
        Focus::Memory => "Memory inbox\n\nUnavailable: owner-scoped nomination and promotion APIs are required. No empty inbox or approved promotion is inferred from a worker snapshot.".into(),
        Focus::Evidence => evidence_lines(app),
    }
}

fn conversation_lines(app: &App) -> String {
    let mut lines = vec![
        "Project owner conversation | Enter: message | Ctrl-B: explicit brief edit".to_owned(),
    ];
    if !app.owner_ready {
        lines.push(
            "Owner input unavailable; limited ordinary mode. Drafts retained; Ctrl-R reconnects."
                .into(),
        );
    }
    let Some(view) = app.conversation_view() else {
        lines.push(safe_text(&app.conversation_notice, 512));
        lines.push("No protected history is inferred from a worker snapshot.".into());
        lines.push(
            "Ctrl-U: separate unassigned draft. Messages do not rewrite a brief or start tasks."
                .into(),
        );
        lines.push("Leader consumption and replies are not integrated.".into());
        return lines.join("\n");
    };
    lines.push(format!(
        "{} owner revision {}",
        if app.conversation_stale || !app.owner_ready {
            "STALE"
        } else {
            "Current"
        },
        view.revision.value(),
    ));
    if let Some(brief) = &view.brief {
        if let crate::conversation::ConversationContent::Brief { version } = brief.content {
            lines.push(format!(
                "Current brief v{version}: {}",
                safe_text(&brief.text, 8192)
            ));
        }
    } else {
        lines.push("No project brief recorded; Ctrl-B explicitly starts version 1.".into());
    }
    if let Some(pending) = app.pending_conversation_label() {
        lines.push(safe_text(&pending, 512));
    }
    lines.push(format!(
        "History page: {} actual entries",
        view.page.entries.len()
    ));
    for entry in &view.page.entries {
        let kind = match entry.content {
            crate::conversation::ConversationContent::Message => "Owner message".to_owned(),
            crate::conversation::ConversationContent::Brief { version } => {
                format!("Brief v{version}")
            }
        };
        lines.push(format!(
            "{kind} | event {} | operation {}",
            entry.event_cursor.value(),
            entry.provenance.operation_id.as_str()
        ));
        lines.push(safe_text(&entry.text, 8192));
    }
    lines.push(safe_text(&app.conversation_notice, 512));
    lines.push("Ctrl-N: next history page | Ctrl-P: first | Ctrl-U: unassigned draft".into());
    lines.push("Messages never approve or start tasks. Leader replies are not integrated.".into());
    lines.join("\n")
}

fn evidence_lines(app: &App) -> String {
    let mut lines = vec!["Receipts, evidence and merge".to_owned(), String::new()];
    if !app.owner_ready {
        lines.push(format!(
            "{}: Evidence and approvals disabled",
            app.owner_label()
        ));
        if let Some(problem) = &app.owner_problem {
            lines.push(format!("Connection problem: {problem}"));
        }
        lines.push("Ctrl-R reconnects to the configured endpoint".into());
    } else if let Some(evidence) = &app.evidence {
        lines.push(format!(
            "Acceptance baseline: {}",
            evidence.baseline_id.as_str()
        ));
        lines.push(evidence.requirements.clone());
        lines.push(format!(
            "Approved: {} | Revoked: {} | Current: {}",
            evidence.approved, evidence.revoked, evidence.current
        ));
        lines.push(format!(
            "Owner revision: {} | Configuration: {}",
            evidence.revision.value(),
            evidence.config_id.as_str()
        ));
    } else {
        lines.push(app.owner_notice.clone());
    }
    lines.push(String::new());
    lines.push(task_summary(app));
    lines.push("\nThe verifier executor and merge executor are not qualified. No passing receipt or completed merge is inferred. Approval actions require a separate protected owner mutation.".into());
    lines.push(
        "\nNative terminal: unavailable without an admitted runtime and an explicit input lease."
            .into(),
    );
    lines.join("\n")
}

fn task_summary(app: &App) -> String {
    app.selected_task().map(|task| format!("Task: {}\nConfiguration: {}\nSnapshot: {}\nAcceptance baseline: {}", task.task_id.as_str(), task.config_id.as_str(), task.snapshot_id.as_str(), task.baseline_id.as_str()))
        .unwrap_or_else(|| "No task in the selected project. Task creation requires an accepted owner brief and baseline.".into())
}

fn project_lines(app: &App) -> String {
    let mut lines = vec![
        "Projects | n: New project | Up/Down: select project | ]: next task".to_owned(),
        String::new(),
    ];
    if app.projects.is_empty() {
        lines.push(
            if app.connected {
                "No projects in the engine snapshot"
            } else {
                "Project data unavailable while disconnected"
            }
            .into(),
        );
    }
    for project in app.projects.iter().take(2048) {
        let marker = if app
            .selected_project()
            .is_some_and(|selected| selected.project_id == project.project_id)
        {
            ">"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {} [{}]",
            safe_text(&project.spec.name, 256),
            project.project_id.as_str()
        ));
    }
    lines.push(format!("\n{}", task_summary(app)));
    if let Some(project) = app.selected_project() {
        lines.push("\nRepositories recorded in project metadata:".into());
        for repository in &project.spec.repositories {
            lines.push(safe_text(repository, 4096));
        }
        lines.push("Repository admission is a separate engine decision; project creation grants no filesystem authority.".into());
    }
    lines.join("\n")
}

fn settings_lines(app: &App) -> String {
    let Some(config) = &app.config else {
        return format!(
            "Settings and provenance\n\n{}\n\nSelect a task to read its pinned effective configuration. Canonical global/project settings editing is unavailable until the owner settings API exists.",
            task_summary(app)
        );
    };
    let mut lines = vec![
        format!(
            "Task-scoped effective configuration: {}",
            config.id.as_str()
        ),
        "name = value | origin | scope | takes effect".into(),
    ];
    for descriptor in config.descriptors.values() {
        lines.push(format!(
            "{} = {} | {:?} | {:?} | {:?}",
            descriptor.name,
            descriptor.value,
            descriptor.origin,
            descriptor.scope,
            descriptor.effect_timing
        ));
    }
    lines.push(
        "\nEditing global/project settings requires the authenticated owner settings API.".into(),
    );
    lines.join("\n")
}

fn plan_lines(app: &App) -> String {
    let Some(state) = &app.plan else {
        return format!(
            "Brief and plan\n\n{}\n\nProject brief history and explicit Ctrl-B versioned editing are in Conversation. No task plan state is loaded; plan approval and rejection remain unavailable here.",
            task_summary(app)
        );
    };
    let mut lines = vec![
        format!(
            "Plan: {} | approved: {} | revoked: {}",
            state.plan.id.as_str(),
            state.approved,
            state.revoked
        ),
        format!(
            "Revision queued: {} | reservations: {}",
            state.queued_revision,
            state.reservations.len()
        ),
    ];
    if let Some(provenance) = &state.approval_provenance {
        lines.push(format!(
            "Approval provenance: {}",
            safe_text(provenance, 4096)
        ));
    }
    for item in state.plan.tasks.iter().take(2048) {
        lines.push(safe_text(&format!("{item:?}"), 4096));
    }
    lines.push("\nApprove/reject, scope changes and execution controls require the authenticated owner API.".into());
    lines.join("\n")
}
