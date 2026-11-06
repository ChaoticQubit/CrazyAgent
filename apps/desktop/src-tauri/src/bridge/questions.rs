use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OwnerClient, QUESTIONS_CODES};
use crazyagent::protocol::ProjectId;
use crazyagent::questions::{
    ANSWER_BYTES_MAX, Answer, OPTIONS_MAX, Question, QuestionStatus, Settlement,
};
use serde::Serialize;
use std::sync::Arc;

/// One question an agent or an engine step asked the owner, as the window shows it: plain
/// status words and the version an answer or dismissal sends back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuestionRow {
    pub id: u64,
    pub project_id: String,
    pub asker: String,
    pub question: String,
    pub options: Vec<String>,
    pub recommended: Option<String>,
    pub context: Option<String>,
    pub status: &'static str,
    pub version: u64,
    pub answer: Option<String>,
    pub created_ms: u64,
    pub settled_ms: Option<u64>,
}

impl From<Question> for QuestionRow {
    fn from(question: Question) -> Self {
        Self {
            id: question.id,
            project_id: question.project.as_str().to_owned(),
            asker: question.asker,
            question: question.question,
            options: question.options,
            recommended: question.recommended,
            context: question.context,
            status: match question.status {
                QuestionStatus::Pending => "pending",
                QuestionStatus::Answered => "answered",
                QuestionStatus::Dismissed => "dismissed",
                QuestionStatus::AnsweredInTerminal => "answered_in_terminal",
            },
            version: question.version,
            answer: question.answer,
            created_ms: question.created_ms,
            settled_ms: question.settled_ms,
        }
    }
}

/// One page of a project's questions, pending first; pass `next` back as `after`.
#[derive(Debug, Serialize)]
pub struct QuestionPage {
    pub items: Vec<QuestionRow>,
    pub next: Option<u64>,
}

/// How many questions of one project wait for the owner.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct QuestionCount {
    pub project_id: String,
    pub pending: u32,
}

/// The bridge failure for an engine questions error: a known questions code reaches the
/// window as itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the bridge failure.
pub fn questions_error(error: EngineError) -> BridgeError {
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

/// The owner's answer as the window sends it: exactly one of an option position below
/// `OPTIONS_MAX` or typed text that is not blank and at most `ANSWER_BYTES_MAX` bytes.
/// Inputs: the option and the text. Output: the answer, or `questions_answer_invalid`.
pub fn choice(option: Option<u8>, text: Option<String>) -> Result<Answer, BridgeError> {
    match (option, text) {
        (Some(index), None) if usize::from(index) < OPTIONS_MAX => Ok(Answer::Option(index)),
        (None, Some(text)) if !text.trim().is_empty() && text.len() <= ANSWER_BYTES_MAX => {
            Ok(Answer::Text(text))
        }
        _ => Err(failure("questions_answer_invalid")),
    }
}

/// Reads an engine settlement: Ok once recorded, `questions_stale` when the question
/// changed since the owner read it. Input: the settlement. Output: the result.
fn settled(settlement: Settlement) -> Result<(), BridgeError> {
    match settlement {
        Settlement::Settled => Ok(()),
        Settlement::Stale => Err(failure("questions_stale")),
    }
}

fn project(id: String) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("questions_not_found"))
}

impl NativeBridge {
    fn questions_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(questions_error)
    }

    /// Reads one page of a project's questions, pending first, run without the bridge lock.
    /// Inputs: the project id and the `next` of the page before, or None for the first.
    /// Output: the page.
    pub fn questions(
        &self,
        project_id: String,
        after: Option<u64>,
    ) -> Result<Unlocked<QuestionPage>, BridgeError> {
        let project = project(project_id)?;
        let client = self.questions_client()?;
        Ok(Box::pin(async move {
            let page = client
                .questions_list(project, after)
                .await
                .map_err(questions_error)?;
            Ok(QuestionPage {
                items: page.items.into_iter().map(QuestionRow::from).collect(),
                next: page.next,
            })
        }))
    }

    /// Answers a pending question with one option or typed text, run without the bridge
    /// lock. Inputs: the question id, the version the owner read, and the option or text.
    /// Output: Ok once recorded; `questions_stale` when it changed since.
    pub fn answer_question(
        &self,
        id: u64,
        version: u64,
        option: Option<u8>,
        text: Option<String>,
    ) -> Result<Unlocked<()>, BridgeError> {
        let answer = choice(option, text)?;
        let client = self.questions_client()?;
        Ok(Box::pin(async move {
            settled(
                client
                    .questions_answer(id, version, answer)
                    .await
                    .map_err(questions_error)?,
            )
        }))
    }

    /// Dismisses a pending question, run without the bridge lock. Inputs: the question id
    /// and the version the owner read. Output: Ok once recorded; `questions_stale` when it
    /// changed since.
    pub fn dismiss_question(&self, id: u64, version: u64) -> Result<Unlocked<()>, BridgeError> {
        let client = self.questions_client()?;
        Ok(Box::pin(async move {
            settled(
                client
                    .questions_dismiss(id, version)
                    .await
                    .map_err(questions_error)?,
            )
        }))
    }

    /// Reads how many questions wait for the owner in each project, run without the bridge
    /// lock. Output: the counts of the projects that have any.
    pub fn question_counts(&self) -> Result<Unlocked<Vec<QuestionCount>>, BridgeError> {
        let client = self.questions_client()?;
        Ok(Box::pin(async move {
            let counts = client.questions_counts().await.map_err(questions_error)?;
            Ok(counts
                .into_iter()
                .filter(|(_, pending)| *pending > 0)
                .map(|(project, pending)| QuestionCount {
                    project_id: project.as_str().to_owned(),
                    pending,
                })
                .collect())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window sends exactly one of an option or typed text, within the engine's bounds;
    /// anything else is refused before the engine is asked.
    #[test]
    fn an_answer_is_one_option_or_bounded_text() {
        let last = u8::try_from(OPTIONS_MAX - 1).unwrap();
        let past = u8::try_from(OPTIONS_MAX).unwrap();
        assert!(matches!(choice(Some(0), None), Ok(Answer::Option(0))));
        assert!(matches!(
            choice(Some(last), None),
            Ok(Answer::Option(index)) if index == last
        ));
        assert!(matches!(
            choice(None, Some("Use Postgres".into())),
            Ok(Answer::Text(text)) if text == "Use Postgres"
        ));
        let longest = "a".repeat(ANSWER_BYTES_MAX);
        assert!(
            matches!(choice(None, Some(longest.clone())), Ok(Answer::Text(text)) if text == longest)
        );
        for (option, text) in [
            (None, None),
            (Some(past), None),
            (Some(0), Some("both".into())),
            (None, Some(" \n\t".into())),
            (None, Some("a".repeat(ANSWER_BYTES_MAX + 1))),
            (None, Some("\u{e9}".repeat(ANSWER_BYTES_MAX / 2 + 1))),
        ] {
            assert_eq!(
                choice(option, text.clone()).unwrap_err().code,
                "questions_answer_invalid",
                "{option:?} {text:?}"
            );
        }
    }

    /// Every questions code the engine answers reaches the window as itself, so the owner
    /// reads its sentence; anything else is an unknown outcome with the engine code behind.
    #[test]
    fn engine_questions_codes_pass_through_unchanged() {
        assert!(!QUESTIONS_CODES.is_empty());
        for code in QUESTIONS_CODES {
            assert_eq!(questions_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            questions_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = questions_error(EngineError("questions_store_torn".into()));
        assert_eq!(unlisted.code, "questions_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("questions_store_torn"));
        let raw = questions_error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(raw.code, "questions_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
        assert!(settled(Settlement::Settled).is_ok());
        assert_eq!(
            settled(Settlement::Stale).unwrap_err().code,
            "questions_stale"
        );
    }
}
