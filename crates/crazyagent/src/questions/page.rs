use super::{
    AnsweredBy, COLUMNS, KEPT_PER_PROJECT_MAX, OPEN_PER_PROJECT_MAX, Question, State, select,
    unavailable,
};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use rusqlite::{Transaction, params};
use std::path::Path;

/// The read-only questions page inside a project's `.crazyagent/` folder.
pub const FILE: &str = "questions.md";
/// The most bytes the questions page holds; older settled questions stay in the views.
pub const PAGE_BYTES_MAX: usize = 1024 * 1024;

const PARTIAL: &str = "questions.md.partial";
const TAIL_BYTES_MAX: usize = 128;
const HEADER: &str = "# Questions for the owner\n\nCrazyAgent writes this page from its database, so editing it changes nothing. Answer in the Questions view, or tell the master in chat.\n";

const _: () = assert!(
    OPEN_PER_PROJECT_MAX as usize
        * 2
        * (super::QUESTION_BYTES_MAX
            + super::OPTIONS_MAX * (super::OPTION_BYTES_MAX + 32)
            + super::RECOMMENDED_BYTES_MAX
            + super::CONTEXT_BYTES_MAX
            + super::ANSWER_BYTES_MAX
            + 512)
        <= PAGE_BYTES_MAX / 2
);

/// Rewrites a project's `.crazyagent/questions.md` from the questions table inside the
/// caller's transaction, so the page never shows a question the database rolled back.
/// Inputs: the transaction and the project. Output: Ok once renamed into place, or
/// `question_project_unknown` or `questions_page_unavailable`.
pub(super) fn write(tx: &Transaction<'_>, project: &ProjectId) -> Result<(), EngineError> {
    let folder = crate::project_setup::load_setup(tx, project)
        .map_err(unavailable)?
        .ok_or_else(|| failure("question_project_unknown"))?
        .project_folder();
    if !folder.is_absolute() {
        return Err(failure("questions_page_unavailable"));
    }
    let pending = select(
        tx,
        &format!(
            "SELECT {COLUMNS} FROM owner_questions WHERE project=?1 AND state='pending'
             ORDER BY id LIMIT ?2"
        ),
        params![project.as_str(), OPEN_PER_PROJECT_MAX],
    )?;
    let settled = select(
        tx,
        &format!(
            "SELECT {COLUMNS} FROM owner_questions WHERE project=?1 AND state!='pending'
             ORDER BY id DESC LIMIT ?2"
        ),
        params![
            project.as_str(),
            KEPT_PER_PROJECT_MAX + OPEN_PER_PROJECT_MAX
        ],
    )?;
    replace(&folder, render(&pending, &settled).as_bytes())
}

/// Renders the questions page: the pending questions oldest first, then the settled ones
/// newest first while they fit `PAGE_BYTES_MAX`. Every text an asker or answer gave sits
/// in its own fence, so it can never add a heading or markup of its own. Inputs: the
/// pending and settled questions. Output: the Markdown text.
pub(super) fn render(pending: &[Question], settled: &[Question]) -> String {
    let mut page = String::from(HEADER);
    page.push_str(&format!("\n## Waiting for the owner ({})\n", pending.len()));
    if pending.is_empty() {
        page.push_str("\nNo question waits for you.\n");
    }
    for question in pending {
        entry(&mut page, question);
    }
    page.push_str("\n## Answered and dismissed, newest first\n");
    if settled.is_empty() {
        page.push_str("\nNone yet.\n");
    }
    let mut left = settled.len();
    for question in settled {
        let mut one = String::new();
        entry(&mut one, question);
        if page.len() + one.len() + TAIL_BYTES_MAX > PAGE_BYTES_MAX {
            break;
        }
        page.push_str(&one);
        left -= 1;
    }
    if left > 0 {
        page.push_str(&format!(
            "\n{left} older questions are only in the Questions view.\n"
        ));
    }
    assert!(page.len() <= PAGE_BYTES_MAX);
    page
}

fn entry(page: &mut String, question: &Question) {
    page.push_str(&format!(
        "\n### Question {} from {}\n\n",
        question.id,
        question.source.label()
    ));
    fenced(page, &question.question);
    if !question.options.is_empty() {
        let options = question
            .options
            .iter()
            .enumerate()
            .map(|(n, option)| {
                let recommended = question.recommended.as_deref() == Some(option.as_str());
                let mark = if recommended { " (recommended)" } else { "" };
                format!("{}. {option}{mark}", n + 1)
            })
            .collect::<Vec<_>>()
            .join("\n");
        page.push_str("\nOptions:\n\n");
        fenced(page, &options);
    }
    if let Some(recommended) = &question.recommended
        && !question.options.contains(recommended)
    {
        page.push_str("\nRecommended:\n\n");
        fenced(page, recommended);
    }
    if let Some(context) = &question.context {
        page.push_str("\nContext:\n\n");
        fenced(page, context);
    }
    match (question.state, question.answered_by) {
        (State::Pending, _) => page.push_str("\nWaiting for your answer.\n"),
        (State::Dismissed, _) => page.push_str("\nDismissed by the owner.\n"),
        (State::Answered, by) => {
            let by = match by {
                Some(AnsweredBy::Owner) | None => "Answered by the owner",
                Some(AnsweredBy::Master) => "Answered in chat, as the master recorded it",
                Some(AnsweredBy::Terminal) => "Answered by the owner in the master's terminal",
            };
            match &question.answer {
                Some(answer) => {
                    page.push_str(&format!("\n{by}:\n\n"));
                    fenced(page, answer);
                }
                None => page.push_str(&format!("\n{by}.\n")),
            }
        }
    }
}

/// Adds a text inside a tilde fence longer than any tilde run in it, so no line of the
/// text can close the fence. Inputs: the page and the text.
fn fenced(page: &mut String, text: &str) {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '~' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "~".repeat(longest.max(2) + 1);
    page.push_str(&fence);
    page.push_str("text\n");
    page.push_str(text);
    page.push('\n');
    page.push_str(&fence);
    page.push('\n');
}

/// Replaces the page whole through no-follow directory handles: opens the project folder
/// and its `.crazyagent/` folder without following a link, writes a new 0444 file beside
/// the page, syncs it and renames it over the old one, so a planted link never redirects
/// the write and nobody reads a partial page. Inputs: the project folder and the content.
#[cfg(unix)]
fn replace(folder: &Path, body: &[u8]) -> Result<(), EngineError> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::io::Write;
    let refused = |_| failure("questions_page_unavailable");
    let directory = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let project = rustix::fs::open(folder, directory, Mode::empty()).map_err(refused)?;
    let state = rustix::fs::openat(
        &project,
        crate::projects_home::CRAZYAGENT_DIRECTORY,
        directory,
        Mode::empty(),
    )
    .map_err(refused)?;
    match rustix::fs::unlinkat(&state, PARTIAL, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => {}
        Err(_) => return Err(failure("questions_page_unavailable")),
    }
    let created =
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file = std::fs::File::from(
        rustix::fs::openat(
            &state,
            PARTIAL,
            created,
            Mode::RUSR | Mode::RGRP | Mode::ROTH,
        )
        .map_err(refused)?,
    );
    file.write_all(body)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("questions_page_unavailable"))?;
    rustix::fs::renameat(&state, PARTIAL, &state, FILE).map_err(refused)
}

/// Replaces the page whole where the engine has no directory-relative no-follow open:
/// writes a read-only file beside the page and renames it over the old one, clearing the
/// old page's read-only mark first. Inputs: the project folder and the content.
#[cfg(not(unix))]
fn replace(folder: &Path, body: &[u8]) -> Result<(), EngineError> {
    let refused = |_| failure("questions_page_unavailable");
    let state = folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    let metadata = std::fs::symlink_metadata(&state).map_err(refused)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(failure("questions_page_unavailable"));
    }
    let partial = state.join(PARTIAL);
    let page = state.join(FILE);
    let _ = std::fs::remove_file(&partial);
    std::fs::write(&partial, body).map_err(refused)?;
    let mut permissions = std::fs::metadata(&partial).map_err(refused)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&partial, permissions).map_err(refused)?;
    if let Ok(old) = std::fs::metadata(&page) {
        let mut permissions = old.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&page, permissions).map_err(refused)?;
    }
    std::fs::rename(&partial, &page).map_err(refused)
}
