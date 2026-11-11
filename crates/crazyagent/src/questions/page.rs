use super::{
    AnsweredBy, COLUMNS, PENDING_PER_PROJECT_MAX, Question, QuestionStatus, STORED_BYTES_MAX,
    asker_label, ensure, immediate, is_terminal, item, select, unavailable,
};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::memory::visible_text;
use crate::protocol::ProjectId;
use crate::storage::versioned::{self, LastWriter};
use rusqlite::{Connection, Transaction, params};
use std::collections::HashSet;
use std::path::Path;

/// The read-only questions page inside a project's `.crazyagent/` folder.
pub const FILE: &str = "questions.md";
/// The most bytes the questions page holds; older settled questions stay in the views.
pub const PAGE_BYTES_MAX: usize = 1024 * 1024;
/// The most settled questions the page shows, newest first.
pub const SETTLED_SHOWN_MAX: i64 = 200;

const PARTIAL: &str = "questions.md.partial";
const TAIL_BYTES_MAX: usize = 256;
const ENTRY_BYTES_MAX: usize = STORED_BYTES_MAX + 16 * 40 + 512;
const HEADER: &str = "# Questions for the owner\n\nCrazyAgent writes this page from its database, so editing it changes nothing. Answer in the Questions view, or tell the master in chat.\n";

const _: () = assert!(
    PENDING_PER_PROJECT_MAX as usize * ENTRY_BYTES_MAX + HEADER.len() + 2 * TAIL_BYTES_MAX
        <= PAGE_BYTES_MAX
);

/// Rewrites a project's questions page after a committed change, as a best effort: the page
/// is derived from the database and never read back, so a project without its
/// `.crazyagent/` folder, or a folder that cannot be written, never blocks asking,
/// answering or dismissing. The write holds the database write lock, so the last page
/// written shows the newest state. Inputs: the connection and the project.
pub(super) fn refresh(db: &Connection, project: &ProjectId) {
    if let Err(problem) = rewrite(db, project) {
        eprintln!(
            "questions_page_unwritten project={} cause={}",
            project.as_str(),
            problem.0
        );
    }
}

fn rewrite(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    let tx = immediate(db)?;
    ensure(&tx)?;
    let Some(setup) = crate::project_setup::load_setup(&tx, project).map_err(unavailable)? else {
        return Ok(());
    };
    let folder = setup.project_folder();
    if !folder.is_absolute() {
        return Err(failure("questions_page_unavailable"));
    }
    let (pending, settled) = shown(&tx, project)?;
    let closed = closed_unanswered(&tx, project, &settled)?;
    replace(&folder, render(&pending, &settled, &closed).as_bytes())
}

/// Reads which dismissed questions closed without the owner: those whose last writer is
/// their agent, as a terminal box that left its screen with no owner key does; a question
/// the owner dismissed is not among them. Inputs: the transaction, the project and the
/// settled questions. Output: their ids.
fn closed_unanswered(
    tx: &Transaction<'_>,
    project: &ProjectId,
    settled: &[Question],
) -> Result<HashSet<u64>, EngineError> {
    let mut closed = HashSet::new();
    for question in settled
        .iter()
        .filter(|question| question.status == QuestionStatus::Dismissed)
    {
        if let LastWriter::Agent(_) = versioned::current(tx, &item(project, question.id)?)?.writer {
            closed.insert(question.id);
        }
    }
    Ok(closed)
}

/// Reads what the page shows: every pending question oldest first and the newest
/// `SETTLED_SHOWN_MAX` settled ones. Inputs: the transaction and the project.
fn shown(
    tx: &Transaction<'_>,
    project: &ProjectId,
) -> Result<(Vec<Question>, Vec<Question>), EngineError> {
    let pending = select(
        tx,
        &format!(
            "SELECT {COLUMNS} FROM questions_log WHERE project=?1 AND status='pending'
             ORDER BY id LIMIT ?2"
        ),
        params![project.as_str(), PENDING_PER_PROJECT_MAX],
    )?;
    let settled = select(
        tx,
        &format!(
            "SELECT {COLUMNS} FROM questions_log WHERE project=?1 AND status!='pending'
             ORDER BY id DESC LIMIT ?2"
        ),
        params![project.as_str(), SETTLED_SHOWN_MAX],
    )?;
    Ok((pending, settled))
}

/// Renders the questions page: the pending questions oldest first, then the settled ones
/// newest first, while they fit `PAGE_BYTES_MAX`. Every text an asker or answer gave sits
/// in its own fence with hidden characters shown as escapes, so it can neither add a
/// heading or markup of its own nor reorder what the owner reads. Inputs: the pending and
/// settled questions and the ids of the dismissed ones their agent closed without the
/// owner. Output: the Markdown text.
pub(super) fn render(pending: &[Question], settled: &[Question], closed: &HashSet<u64>) -> String {
    let mut page = String::from(HEADER);
    page.push_str(&format!("\n## Waiting for the owner ({})\n", pending.len()));
    if pending.is_empty() {
        page.push_str("\nNo question waits for you.\n");
    }
    let left = entries(&mut page, pending, closed);
    if left > 0 {
        page.push_str(&format!(
            "\n{left} more waiting questions are only in the Questions view.\n"
        ));
    }
    page.push_str("\n## Answered and dismissed, newest first\n");
    if settled.is_empty() {
        page.push_str("\nNone yet.\n");
    }
    let left = entries(&mut page, settled, closed);
    if left > 0 {
        page.push_str(&format!(
            "\n{left} older questions are only in the Questions view.\n"
        ));
    }
    assert!(page.len() <= PAGE_BYTES_MAX);
    page
}

/// Adds the questions that fit, in order, keeping room for the closing lines. Inputs: the
/// page, the questions and the dismissed ones closed without the owner. Output: how many
/// did not fit.
fn entries(page: &mut String, questions: &[Question], closed: &HashSet<u64>) -> usize {
    let mut left = questions.len();
    for question in questions {
        let mut one = String::new();
        entry(&mut one, question, closed);
        if page.len() + one.len() + 2 * TAIL_BYTES_MAX > PAGE_BYTES_MAX {
            break;
        }
        page.push_str(&one);
        left -= 1;
    }
    left
}

fn entry(page: &mut String, question: &Question, closed: &HashSet<u64>) {
    page.push_str(&format!(
        "\n### Question {} from {}\n\n",
        question.id,
        visible_text(&asker_label(&question.asker))
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
    let terminal = is_terminal(&question.asker);
    match (question.status, question.answered_by) {
        (QuestionStatus::Pending, _) if terminal => {
            page.push_str("\nWaiting for your answer in that agent's terminal.\n");
        }
        (QuestionStatus::Pending, _) => page.push_str("\nWaiting for your answer.\n"),
        (QuestionStatus::Dismissed, _) if closed.contains(&question.id) => {
            page.push_str("\nClosed without an answer.\n");
        }
        (QuestionStatus::Dismissed, _) => page.push_str("\nDismissed by the owner.\n"),
        (QuestionStatus::AnsweredInTerminal, _) => {
            page.push_str("\nAnswered by the owner in that agent's terminal.\n");
        }
        (QuestionStatus::Answered, by) => {
            let by = match by {
                Some(AnsweredBy::Chat) => "Answered by the owner in the master chat",
                Some(AnsweredBy::Owner) | None => "Answered by the owner",
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
/// text can close the fence, with every hidden character shown as an escape. Inputs: the
/// page and the text.
fn fenced(page: &mut String, text: &str) {
    let text = visible_text(text);
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '~' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "~".repeat(longest.max(2) + 1);
    page.push_str(&fence);
    page.push_str("text\n");
    page.push_str(&text);
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
