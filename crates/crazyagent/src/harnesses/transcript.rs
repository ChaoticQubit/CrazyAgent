/// One fact read from a harness's own session record, whatever the harness.
/// `Waiting` means the harness waits for the owner, such as an approval or a question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptEvent {
    TurnStarted,
    Text { text: String },
    ToolUse { name: String },
    Waiting,
    TurnFinished { success: bool },
    TurnError { rate_limited: bool },
}
