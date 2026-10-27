use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OwnerClient, QUESTIONS_CODES};
use crazyagent::protocol::ProjectId;
use crazyagent::questions::{Question, QuestionStatus};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::Manager;
use tauri_plugin_notification::{ActionPerformed, NotificationExt};

/// The most question notifications the app remembers; the oldest is forgotten first.
pub const SHOWN_MAX: usize = 256;
const TITLE_CHARS_MAX: usize = 80;
const BODY_CHARS_MAX: usize = 160;
const ELLIPSIS: &str = "...";
const PAGES_MAX: usize = 4;

/// What a macOS notification for one question says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

/// Whether this system shows the app's notifications: macOS only, until Linux and Windows
/// are qualified. Output: `available` or `unavailable`.
pub fn support() -> &'static str {
    if cfg!(target_os = "macos") {
        "available"
    } else {
        "unavailable"
    }
}

/// Refuses a notification where the system has none qualified. Output: Ok on macOS, else
/// `notifications_unavailable`.
pub fn available() -> Result<(), BridgeError> {
    match support() {
        "available" => Ok(()),
        _ => Err(failure("notifications_unavailable")),
    }
}

/// One plain line of engine or agent text: the first line that is not blank, trimmed,
/// control, direction and invisible format characters shown as a replacement mark, and at
/// most `max` characters with an ellipsis past it. Inputs: the text and the bound. Output:
/// the line.
pub fn plain_line(text: &str, max: usize) -> String {
    assert!(max > ELLIPSIS.len());
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let shown: String = line
        .chars()
        .map(|character| {
            if hidden(character) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect();
    if shown.chars().count() <= max {
        return shown;
    }
    let mut cut: String = shown.chars().take(max - ELLIPSIS.len()).collect();
    cut.push_str(ELLIPSIS);
    assert!(cut.chars().count() <= max);
    cut
}

/// Whether a character would hide or reorder text in a notification: a control
/// character, a bidirectional mark or isolate, a zero-width or byte-order mark, or a tag.
fn hidden(character: char) -> bool {
    character.is_control()
        || matches!(character,
            '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}'
            | '\u{feff}' | '\u{e0000}'..='\u{e007f}')
}

/// Who asked, in plain words, from the engine's asker name. Input: the asker. Output: the
/// label, the same words as the window's Questions tab.
pub fn asker_label(asker: &str) -> String {
    let (kind, tail) = asker.split_once(':').unwrap_or((asker, ""));
    match (kind, tail) {
        ("master", _) => "Master".into(),
        ("agent", id) if !id.is_empty() => format!("Agent {}", plain_line(id, TITLE_CHARS_MAX)),
        ("terminal", agent) if !agent.is_empty() => format!(
            "Asked in the terminal of {}",
            plain_line(agent, TITLE_CHARS_MAX)
        ),
        ("engine", "mapping") => "Project mapping".into(),
        ("engine", "planning") => "Roadmap planning".into(),
        ("engine", step) if step.starts_with("research:") => format!(
            "Research: {}",
            plain_line(
                &step["research:".len()..].replace('_', " "),
                TITLE_CHARS_MAX
            )
        ),
        ("engine", step) if step.starts_with("landing:") => "Landing changes".into(),
        _ => plain_line(asker, TITLE_CHARS_MAX),
    }
}

/// The notification text for a pending question, built from the engine's record: the
/// project's name in the title, who asked and the question's first line in the body.
/// Inputs: the project's name, if known, and the question. Output: the notice.
pub fn notice(project: Option<&str>, question: &Question) -> Notice {
    let name = project.map(|name| plain_line(name, TITLE_CHARS_MAX - 20));
    let title = match name {
        Some(name) if !name.is_empty() => format!("{name} needs your answer"),
        _ => "A project needs your answer".into(),
    };
    let body = plain_line(
        &format!(
            "{}: {}",
            asker_label(&question.asker),
            plain_line(&question.question, BODY_CHARS_MAX)
        ),
        BODY_CHARS_MAX,
    );
    assert!(title.chars().count() <= TITLE_CHARS_MAX);
    assert!(body.chars().count() <= BODY_CHARS_MAX);
    Notice { title, body }
}

/// Maps an engine refusal to the code the window shows: every Questions code as itself,
/// anything else as an unknown outcome with the engine code behind it.
pub fn notify_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        QUESTIONS_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("questions_outcome_unknown"),
        &error.0,
    )
}

impl NativeBridge {
    fn notify_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(notify_error)
    }

    /// Reads one pending question for its notification, so the text comes from the
    /// engine's record and never from the window's words; run without the bridge lock.
    /// Pending questions come first in a page, so at most `PAGES_MAX` pages are read.
    /// Inputs: the project id and the question id. Output: the question, or
    /// `questions_not_found` when the project has no such pending question.
    pub fn notified_question(
        &self,
        project_id: String,
        id: u64,
    ) -> Result<Unlocked<Question>, BridgeError> {
        let project = ProjectId::new(project_id).map_err(|_| failure("questions_not_found"))?;
        let client = self.notify_client()?;
        Ok(Box::pin(async move {
            let mut after = None;
            for _ in 0..PAGES_MAX {
                let page = client
                    .questions_list(project.clone(), after)
                    .await
                    .map_err(notify_error)?;
                let pending = page
                    .items
                    .iter()
                    .any(|item| item.status == QuestionStatus::Pending);
                if let Some(found) = page.items.into_iter().find(|item| item.id == id) {
                    return if found.status == QuestionStatus::Pending && found.project == project {
                        Ok(found)
                    } else {
                        Err(failure("questions_not_found"))
                    };
                }
                match page.next {
                    Some(next) if pending => after = Some(next),
                    _ => break,
                }
            }
            Err(failure("questions_not_found"))
        }))
    }
}

/// The question notifications this app showed: which questions were notified, the
/// project each notification names, and the project of a click not yet opened.
#[derive(Default)]
pub struct Notices {
    shown: Mutex<Shown>,
}

#[derive(Default)]
struct Shown {
    order: VecDeque<(u64, i32)>,
    projects: HashMap<i32, String>,
    clicked: Option<String>,
    last: i32,
}

impl Notices {
    fn shown(&self) -> std::sync::MutexGuard<'_, Shown> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claims the notification of one question, once: the first claim gets a new
    /// notification id, later claims of the same question get none. The oldest of more
    /// than `SHOWN_MAX` is forgotten. Inputs: the question id and its project id. Output:
    /// the notification id, or None when the question was already notified.
    pub fn claim(&self, question: u64, project: &str) -> Option<i32> {
        let mut shown = self.shown();
        if shown.order.iter().any(|(known, _)| *known == question) {
            return None;
        }
        shown.last = if shown.last == i32::MAX {
            1
        } else {
            shown.last + 1
        };
        let id = shown.last;
        assert!(id > 0);
        shown.order.push_back((question, id));
        shown.projects.insert(id, project.to_owned());
        while shown.order.len() > SHOWN_MAX {
            if let Some((_, old)) = shown.order.pop_front() {
                shown.projects.remove(&old);
            }
        }
        assert!(shown.order.len() <= SHOWN_MAX && shown.projects.len() <= SHOWN_MAX);
        Some(id)
    }

    /// Gives a claim back after the notification could not be shown, so a later read may
    /// try again. Inputs: the question id and the notification id it claimed.
    pub fn release(&self, question: u64, id: i32) {
        let mut shown = self.shown();
        shown
            .order
            .retain(|(known, claimed)| (*known, *claimed) != (question, id));
        shown.projects.remove(&id);
    }

    /// Records a click on a notification: its project becomes the one the window opens
    /// next. Input: the notification id. Output: whether it was one of these notifications.
    pub fn clicked(&self, id: i32) -> bool {
        let mut shown = self.shown();
        let Some(project) = shown.projects.get(&id).cloned() else {
            return false;
        };
        shown.clicked = Some(project);
        true
    }

    /// Takes the project of the last click, once. Output: the project id, or None.
    pub fn take(&self) -> Option<String> {
        self.shown().clicked.take()
    }
}

/// Shows a notice as a system notification with the given id. Inputs: the app, the
/// notification id and the notice. Output: Ok once handed to the system, or
/// `notifications_failed`.
pub fn show<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: i32,
    notice: &Notice,
) -> Result<(), BridgeError> {
    app.notification()
        .builder()
        .id(id)
        .title(notice.title.clone())
        .body(notice.body.clone())
        .show()
        .map_err(|_| failure("notifications_failed"))
}

/// Handles an action on a notification: a click on a question notification marks its
/// project for the window and brings the main window to the front, where the window opens
/// that project's Questions tab. Other actions and notifications are ignored. Inputs: the
/// app and the action.
pub fn performed<R: tauri::Runtime>(app: &tauri::AppHandle<R>, performed: &ActionPerformed) {
    if performed.action_id() != "tap" {
        return;
    }
    let Some(id) = performed
        .notification()
        .map(|notification| notification.id())
    else {
        return;
    };
    if !app.state::<Notices>().clicked(id) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(asker: &str, text: &str) -> Question {
        Question {
            id: 12,
            project: ProjectId::new("p-1").unwrap(),
            asker: asker.into(),
            question: text.into(),
            options: vec!["Postgres".into(), "SQLite".into()],
            recommended: Some("SQLite".into()),
            context: Some("The app runs on one laptop.".into()),
            status: QuestionStatus::Pending,
            version: 3,
            answer: None,
            answered_by: None,
            created_ms: 1,
            settled_ms: None,
        }
    }

    /// The notification names the project and who asked, and carries the question's first
    /// line as plain bounded text, never its options or context.
    #[test]
    fn a_notice_names_the_project_the_asker_and_the_first_line() {
        let shown = notice(
            Some("Recipe box"),
            &question("agent:team-3", "\n  Which database?\nMore detail."),
        );
        assert_eq!(
            shown,
            Notice {
                title: "Recipe box needs your answer".into(),
                body: "Agent team-3: Which database?".into(),
            }
        );
        assert_eq!(
            notice(None, &question("master:s-1", "Ship it?")).title,
            "A project needs your answer"
        );
        assert_eq!(
            notice(
                Some(" \u{202e}"),
                &question("engine:research:market_research", "Q")
            )
            .body,
            "Research: market research: Q"
        );
        let long = notice(
            Some(&"n".repeat(500)),
            &question("engine:mapping", &"q".repeat(500)),
        );
        assert!(long.title.chars().count() <= TITLE_CHARS_MAX);
        assert!(long.body.starts_with("Project mapping: qqq"));
        assert!(long.body.ends_with(ELLIPSIS));
        assert_eq!(long.body.chars().count(), BODY_CHARS_MAX);
        let hidden = notice(
            Some("Re\u{202e}cipe"),
            &question("agent:x\u{7}", "Run\u{200b} it\u{e0041}?"),
        );
        assert_eq!(hidden.title, "Re\u{fffd}cipe needs your answer");
        assert_eq!(hidden.body, "Agent x\u{fffd}: Run\u{fffd} it\u{fffd}?");
    }

    /// A question is notified once; its click opens its project once; the oldest past the
    /// bound is forgotten; a released claim may be claimed again.
    #[test]
    fn each_question_is_notified_once_and_a_click_opens_its_project_once() {
        let notices = Notices::default();
        let first = notices.claim(7, "p-1").unwrap();
        assert_eq!(notices.claim(7, "p-1"), None);
        let second = notices.claim(8, "p-2").unwrap();
        assert_ne!(first, second);
        assert!(!notices.clicked(9999));
        assert_eq!(notices.take(), None);
        assert!(notices.clicked(second));
        assert_eq!(notices.take().as_deref(), Some("p-2"));
        assert_eq!(notices.take(), None);
        notices.release(8, second);
        assert!(!notices.clicked(second));
        assert!(notices.claim(8, "p-2").is_some());
        for question in 100..100 + SHOWN_MAX as u64 {
            assert!(notices.claim(question, "p-3").is_some());
        }
        assert!(!notices.clicked(first), "the oldest is forgotten");
        assert!(notices.claim(7, "p-1").is_some());
    }

    /// Only macOS shows question notifications until the others are qualified.
    #[test]
    fn notifications_are_available_on_macos_only() {
        assert_eq!(support() == "available", cfg!(target_os = "macos"));
        assert_eq!(available().is_ok(), cfg!(target_os = "macos"));
        if !cfg!(target_os = "macos") {
            assert_eq!(available().unwrap_err().code, "notifications_unavailable");
        }
    }

    /// Every Questions code reaches the window as itself; anything else is an unknown
    /// outcome with the engine code behind it.
    #[test]
    fn engine_question_codes_pass_through_unchanged() {
        for code in QUESTIONS_CODES {
            assert_eq!(notify_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            notify_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = notify_error(EngineError("questions_torn".into()));
        assert_eq!(unlisted.code, "questions_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("questions_torn"));
    }
}
