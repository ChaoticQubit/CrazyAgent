use crate::protocol::ProjectId;
use serde::{Deserialize, Serialize};

/// Gives a closed enum its stored text form both ways, so the coordination tables hold
/// fixed names instead of positions. Takes the enum and each variant with its name.
/// Produces `as_str` and `parse` on the enum.
macro_rules! named {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            /// The stored name of this value. Takes the value. Returns its fixed text.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// Reads a stored name. Takes the text. Returns the value, or None for a name
            /// this build does not know.
            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

pub mod adoption;
mod claims;
pub mod delivery;
pub mod hook;
mod messages;
mod presence;
pub mod relay;
pub mod settings;
mod store;
pub mod tool_switches;
pub mod watch;
mod work;

pub use claims::{
    breadth, claims_full_notice, covers, held_sets, hold_for_set, overlapping, overlaps_of,
    path_scopes, release_claims, release_set, release_unheld, restore_set,
};
pub use messages::{mark, outbox, pending, reply, requeue_dead, send, unread};
pub use presence::{owner_questions, record_tool, set_waiting, snapshot, touch};
pub use store::{ensure, forget_project, master_of, message};
pub use work::{finish, start_work, transfer, work, works};

/// A team agent's id, the `team_agents` row id.
pub type AgentId = i64;
/// A unit of work's id in `coord_work`.
pub type WorkId = i64;
/// A message's id in `coord_messages`.
pub type MessageId = i64;

/// The longest message body, evidence text or question, in bytes.
pub const BODY_BYTES_MAX: usize = 4000;
/// The longest claim pattern, in bytes.
pub const PATTERN_BYTES_MAX: usize = 512;
/// The most claims one unit of work holds.
pub const CLAIMS_PER_WORK_MAX: usize = 32;
/// The most messages one `pending` read returns.
pub const PENDING_MAX: usize = 64;
/// The longest work title, in bytes.
pub const TITLE_BYTES_MAX: usize = 200;
/// The longest idempotency key, in bytes.
pub const KEY_BYTES_MAX: usize = 128;
/// The longest tool name the engine counts, in bytes.
pub const TOOL_BYTES_MAX: usize = 128;
/// The most units of work one `works` read returns.
pub const WORKS_LISTED_MAX: usize = 256;

const _: () = assert!(BODY_BYTES_MAX == 4000 && PATTERN_BYTES_MAX == 512);
const _: () = assert!(CLAIMS_PER_WORK_MAX == 32 && PENDING_MAX == 64);
const _: () = assert!(TITLE_BYTES_MAX == 200 && TITLE_BYTES_MAX < BODY_BYTES_MAX);
const _: () = assert!(KEY_BYTES_MAX < PATTERN_BYTES_MAX && TOOL_BYTES_MAX < PATTERN_BYTES_MAX);

/// Who sends, answers or acts in the coordination layer: the owner, one team agent, or
/// the engine itself (nudges and notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "agent", rename_all = "snake_case")]
pub enum Party {
    Owner,
    Agent(AgentId),
    Engine,
}

/// What a message is: work handed over, an answer, a question carrying its two tags
/// (Q14b), or a note that asks for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Prompt,
    Reply,
    Question,
    Note,
}

named!(MessageKind { Prompt => "prompt", Reply => "reply", Question => "question", Note => "note" });

/// Whom a question is for (Q14b): the owner, the master or the asker's lead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionFor {
    Me,
    Master,
    Lead,
}

named!(QuestionFor { Me => "me", Master => "master", Lead => "lead" });

/// How urgent a question is (Q14b). It sets urgency only, never the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    High,
    Normal,
    Low,
}

named!(Severity { High => "high", Normal => "normal", Low => "low" });

impl Severity {
    /// Orders severities so an agent in the chain may raise one and never lower it.
    /// Takes the severity. Returns 2 for High, 1 for Normal and 0 for Low.
    pub fn rank(self) -> u8 {
        match self {
            Self::High => 2,
            Self::Normal => 1,
            Self::Low => 0,
        }
    }
}

/// How far a message got, like message ticks: committed, taken by the recipient's
/// session or typed into its terminal, read with evidence, answered, or ended unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Receipt {
    Sent,
    Delivered,
    Typed,
    Read,
    Replied,
    Failed,
    Expired,
}

named!(Receipt {
    Sent => "sent",
    Delivered => "delivered",
    Typed => "typed",
    Read => "read",
    Replied => "replied",
    Failed => "failed",
    Expired => "expired",
});

impl Receipt {
    /// Says whether a message may move from this receipt to `next`: receipts only move
    /// forward (Sent, then Delivered or Typed, then Read, then Replied), Failed and
    /// Expired end a message that was not read, and nothing leaves an ending. Takes both.
    /// Returns the answer; the same receipt again is not a move.
    pub fn may_become(self, next: Self) -> bool {
        let step = |receipt: Self| match receipt {
            Self::Sent => 0,
            Self::Delivered | Self::Typed => 1,
            Self::Read => 2,
            Self::Replied => 3,
            Self::Failed | Self::Expired => 4,
        };
        match (self, next) {
            (Self::Failed | Self::Expired, _) => false,
            (Self::Read | Self::Replied, Self::Failed | Self::Expired) => false,
            (current, next) => step(next) > step(current),
        }
    }

    /// Says whether the recipient has not read the message yet. Takes the receipt.
    /// Returns true for Sent, Delivered and Typed.
    pub fn unread(self) -> bool {
        matches!(self, Self::Sent | Self::Delivered | Self::Typed)
    }
}

/// An agent's liveness, from its last activity (Concord): Live under 5 minutes, Idle
/// under 30, Away under 60, then Stopped; a stopped or failed agent is Stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Live,
    Idle,
    Away,
    Stopped,
}

/// Why an agent waits on its user in its own terminal (Q14): a question it asked there
/// or a permission prompt. Only that terminal can answer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waiting {
    Question,
    Permission,
}

named!(Waiting { Question => "question", Permission => "permission" });

/// Where one unit of work stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Open,
    Assigned,
    Accepted,
    Active,
    ReviewReady,
    Complete,
    Closed,
    Declined,
}

named!(WorkState {
    Open => "open",
    Assigned => "assigned",
    Accepted => "accepted",
    Active => "active",
    ReviewReady => "review_ready",
    Complete => "complete",
    Closed => "closed",
    Declined => "declined",
});

/// One ownership change of a unit of work (Concord's transfer verbs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Transfer {
    Assign { to: AgentId },
    Accept,
    Decline,
    Release,
    Handoff { to: AgentId },
    Reopen,
}

/// How an agent ends a unit of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finish {
    ReviewReady,
    Complete,
    Closed,
}

/// A message to send. A question carries both tags; its recipient follows them and the
/// sender's place in the team, not `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMessage {
    pub project: ProjectId,
    pub from: Party,
    pub to: Party,
    pub kind: MessageKind,
    pub question: Option<(QuestionFor, Severity)>,
    pub work: Option<WorkId>,
    pub reply_to: Option<MessageId>,
    pub body: String,
    pub idempotency_key: Option<String>,
}

/// A stored message with its receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub project: ProjectId,
    pub from: Party,
    pub to: Party,
    pub kind: MessageKind,
    pub question: Option<(QuestionFor, Severity)>,
    pub work: Option<WorkId>,
    pub reply_to: Option<MessageId>,
    pub body: String,
    pub receipt: Receipt,
    pub created: i64,
}

/// One declared scope of an agent's work: a file, a folder (`dir/`), a glob, or a
/// `module:` or `domain:` label. Stale while its agent is Away or Stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: i64,
    pub agent: AgentId,
    pub work: WorkId,
    pub pattern: String,
    pub stale: bool,
}

/// Two active claims of different agents that may touch the same thing, with the plain
/// reasons (same file, same folder, shared module or domain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlap {
    pub mine: Claim,
    pub theirs: Claim,
    pub reasons: Vec<String>,
}

/// What the graph, the Process page and the inbox show for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    pub agent: AgentId,
    pub presence: Presence,
    pub waiting: Option<Waiting>,
    pub has_worktree: bool,
    pub unread: u32,
    pub tool_counts: Vec<(String, u32)>,
    pub flags: Vec<String>,
}

/// One unit of work as an agent reads it: its title, owner, state, the version a
/// transition must name, and its claims that still hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    pub id: WorkId,
    pub title: String,
    pub owner: Option<AgentId>,
    pub state: WorkState,
    pub version: i64,
    pub claims: Vec<String>,
}

/// The coordination state of one project at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub agents: Vec<AgentState>,
    pub overlaps: Vec<Overlap>,
    pub questions_for_owner: Vec<Message>,
    pub unread_edges: Vec<(Party, Party, u32, Receipt)>,
}

#[cfg(all(test, unix))]
mod tests;
