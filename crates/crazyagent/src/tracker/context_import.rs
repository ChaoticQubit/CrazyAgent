use super::{Host, TRACKER_UNAVAILABLE, engine_error, key_of, render};
use crate::engine::EngineError;
use crate::project_setup::CONTEXT_DIRECTORY;
use crate::protocol::ProjectId;
use crate::questions::{self, Ask, EngineSource};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::path::Path;
use trackwright_core::import::{self, Unparsed, Why};
use trackwright_core::model::{Actor, ImportPolicy, ProjectKey, Refusal};
use trackwright_core::policy;
use trackwright_core::store::StoreError;

/// The actor an automatic context import writes its versions and tickets as.
pub const IMPORT_ACTOR: &str = "engine:import";
/// The planning job failure when the engine could not run the context import.
pub const UNIMPORTED: &str = "Could not import the roadmap into tickets";
/// The most questions one import asks: the questions log's per-asker cap, which the
/// planning step shares with the roadmap's own open questions.
pub const LEFT_ASKED_MAX: usize = questions::PENDING_PER_ASKER_MAX as usize;

const FILES: [&str; 2] = ["roadmap.md", "release-process.md"];
const IMPORTED: &str = "SELECT EXISTS(SELECT 1 FROM tracker_context_imports WHERE project = ?1)";
const MARK: &str = "INSERT INTO tracker_context_imports(project, imported_ms) VALUES (?1, ?2)";
const HEADING_SHOWN_BYTES_MAX: usize = 120;
const MESSAGE_SHOWN_BYTES_MAX: usize = 512;

const _: () = assert!(LEFT_ASKED_MAX >= 1);
const _: () = assert!(
    HEADING_SHOWN_BYTES_MAX + MESSAGE_SHOWN_BYTES_MAX + 512 <= questions::QUESTION_BYTES_MAX
);

/// What one finished planning job's context import did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Imported {
    /// Nothing ran: the project has no tickets, its `import` is `on_request`, or its
    /// context was imported once already.
    Skipped,
    /// The import ran: the versions and tickets written, and the owner questions raised
    /// for the parts it left.
    Applied { items: usize, questions: usize },
}

/// One part of the context the import leaves to the owner, as a question and its context.
struct Left {
    question: String,
    context: String,
}

/// Imports a project's `context/roadmap.md` and `context/release-process.md` into its
/// tickets once, after RM-1's planning job wrote them, when the project is attached and its
/// `import` setting is `auto_once`. Every item is applied as `engine:import` under the
/// project's policy; the engine's own "Open questions" section is skipped (the planning
/// step raised those), and each other unparsed part or refused file becomes one
/// `engine:planning` owner question. The import is marked done in the same transaction,
/// so a later planning job never imports again. Inputs: the engine database path and the
/// project. Output: what ran, or `tracker_unavailable` and the questions log's failures.
pub fn after_planning(database: &Path, project: &str) -> Result<Imported, EngineError> {
    let root = database
        .parent()
        .ok_or_else(|| EngineError(TRACKER_UNAVAILABLE.into()))?;
    let project =
        ProjectId::new(project).map_err(|_| EngineError("question_project_unknown".into()))?;
    let db = questions::open(root)?;
    let Some((key, items, left)) = import_once(&db, &project)? else {
        return Ok(Imported::Skipped);
    };
    let asked = ask(&db, &project, &left)?;
    assert!(asked <= left.len());
    render::refresh(&db, &key);
    Ok(Imported::Applied {
        items,
        questions: asked,
    })
}

/// Runs the import in one immediate transaction with its done mark. Inputs: the
/// connection and the project. Output: None when skipped, else the key, the items written
/// and the parts left.
fn import_once(
    db: &Connection,
    project: &ProjectId,
) -> Result<Option<(ProjectKey, usize, Vec<Left>)>, EngineError> {
    let unavailable = |_: rusqlite::Error| EngineError(TRACKER_UNAVAILABLE.into());
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    let Some(key) = key_of(&tx, project)? else {
        return Ok(None);
    };
    let done: bool = tx
        .query_row(IMPORTED, [project.as_str()], |row| row.get(0))
        .map_err(unavailable)?;
    let chosen = policy::load(&tx, &key).map_err(engine_error)?.import;
    if done || chosen == ImportPolicy::OnRequest {
        return Ok(None);
    }
    let actor = Actor::new(IMPORT_ACTOR).expect("engine:import is a valid actor");
    let now_ms = crate::team::now_ms();
    let mut items = 0;
    let mut left = Vec::new();
    for file in FILES {
        let (written, parts) =
            import_file(&tx, &actor, &key, file, now_ms).map_err(engine_error)?;
        items += written;
        left.extend(parts);
    }
    let marked = tx
        .execute(MARK, params![project.as_str(), now_ms])
        .map_err(unavailable)?;
    assert_eq!(marked, 1);
    tx.commit().map_err(unavailable)?;
    Ok(Some((key, items, left)))
}

/// Previews one context file and applies every item it lists. A refusal of the file or of
/// the apply is left to the owner, and the apply's savepoint has then written nothing.
/// Inputs: the transaction, the import actor, the key, the file name in `context/` and the
/// time. Output: the items written and the parts left, or a store failure.
fn import_file(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    file: &str,
    now_ms: i64,
) -> Result<(usize, Vec<Left>), StoreError> {
    let path = Path::new(file);
    let preview = match import::preview(tx, &Host, actor, key, path) {
        Ok(preview) => preview,
        Err(StoreError::Refused(refusal)) => return Ok((0, vec![refused(file, &refusal)])),
        Err(other) => return Err(other),
    };
    let mut left: Vec<Left> = preview
        .unparsed
        .iter()
        .filter(|entry| !engine_questions(entry))
        .map(|entry| unparsed(file, entry))
        .collect();
    if preview.items.is_empty() {
        return Ok((0, left));
    }
    let accept: Vec<String> = preview.items.iter().map(|item| item.key.clone()).collect();
    match import::apply(tx, &Host, actor, key, path, &accept, now_ms) {
        Ok(applied) => {
            let written = applied.versions.len() + applied.tickets.len();
            assert_eq!(written, accept.len());
            Ok((written, left))
        }
        Err(StoreError::Refused(refusal)) => {
            left.push(refused(file, &refusal));
            Ok((0, left))
        }
        Err(other) => Err(other),
    }
}

/// Tells whether an unparsed part is the roadmap's own "Open questions" section, which
/// the engine writes and the planning step already raised. An agent's heading with those
/// words is escaped by planning's `inert`, so it never parses as this heading. Input: the
/// part. Output: true for the engine's section.
fn engine_questions(entry: &Unparsed) -> bool {
    let heading = crate::planning::OPEN_QUESTIONS_HEADING
        .strip_prefix("## ")
        .expect("the open questions heading is a level two heading");
    entry.heading == heading && entry.why == Why::NotAVersion
}

/// Words for why the import left a part. Input: the reason. Output: the words.
fn because(why: Why) -> &'static str {
    match why {
        Why::BeforeFirstSection => "it comes before the first section",
        Why::NotAVersion => "its heading names no version",
        Why::NoChecklistItem => "it holds no checklist item",
        Why::TargetNotADay => "its Target is not a day",
        Why::SecondTarget => "it names a second Target",
        Why::SecondSection => "it repeats a version's section",
        Why::ItemRefused => "the tracker refused the item",
    }
}

/// Asks about one unparsed part: the heading in the question, the file, lines and text as
/// written in the context. Inputs: the file name and the part. Output: what to ask.
fn unparsed(file: &str, entry: &Unparsed) -> Left {
    let heading = if entry.heading.is_empty() {
        "the text before the first section".to_owned()
    } else {
        format!("\"{}\"", clipped(&entry.heading, HEADING_SHOWN_BYTES_MAX))
    };
    let question = format!(
        "The roadmap import left {heading} in {file} for you: {}. How should the team plan it?",
        because(entry.why)
    );
    assert!(question.len() <= questions::QUESTION_BYTES_MAX);
    let context = format!(
        "From {CONTEXT_DIRECTORY}/{file}, lines {} to {}:\n{}",
        entry.line_first, entry.line_last, entry.text
    );
    Left {
        question,
        context: clipped(&context, questions::CONTEXT_BYTES_MAX).to_owned(),
    }
}

/// Asks about a file or apply the tracker refused. Inputs: the file name and the
/// refusal. Output: what to ask.
fn refused(file: &str, refusal: &Refusal) -> Left {
    let question = format!(
        "The roadmap import did not apply {file}: {} ({}). Should the team import it from \
         Tickets > Import roadmap?",
        clipped(&refusal.message, MESSAGE_SHOWN_BYTES_MAX),
        refusal.code
    );
    assert!(question.len() <= questions::QUESTION_BYTES_MAX);
    Left {
        question,
        context: format!(
            "The planning job wrote {CONTEXT_DIRECTORY}/{file}; nothing from it was imported."
        ),
    }
}

/// Raises the parts left as `engine:planning` questions, at most `LEFT_ASKED_MAX`; the
/// log returns a pending question with the same text instead of asking twice, a question
/// it refuses as invalid is skipped, and asking stops once the log is full. Inputs: the
/// connection, the project and the parts. Output: how many were logged.
fn ask(db: &Connection, project: &ProjectId, left: &[Left]) -> Result<usize, EngineError> {
    let mut asked = 0;
    for part in left.iter().take(LEFT_ASKED_MAX) {
        let ask = Ask {
            question: part.question.clone(),
            kind: questions::QuestionKind::Text,
            options: Vec::new(),
            recommended: None,
            context: Some(part.context.clone()),
        };
        match questions::raise(db, project, &EngineSource::Planning, &ask) {
            Ok(_) => asked += 1,
            Err(problem) if crate::planning::FULL.contains(&problem.0.as_str()) => break,
            Err(problem) if crate::planning::REFUSED.contains(&problem.0.as_str()) => {}
            Err(problem) => return Err(problem),
        }
    }
    assert!(asked <= LEFT_ASKED_MAX);
    Ok(asked)
}

/// Cuts text to at most `bytes_max` bytes at a character boundary. Inputs: the text and
/// the bound. Output: the text, or its longest prefix that fits.
fn clipped(text: &str, bytes_max: usize) -> &str {
    if text.len() <= bytes_max {
        return text;
    }
    let mut end = bytes_max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    assert!(cut.len() <= bytes_max);
    assert!(bytes_max - cut.len() < 4);
    cut
}
