use super::Client;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use trackwright_core::model::{
    CheckKind, CriterionState, CycleStatus, EvidenceResult, OpKind, TicketKind, TicketStatus,
    VersionStatus,
};

/// Why a call gave no data: the reply's code and message, or `reply_unreadable` when the
/// reply was not an envelope the views understand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    cursor: Option<String>,
}

/// Runs one call and reads its envelope. Inputs: the client, the operation and its
/// input object. Output: the reply's data as `T` and its cursor, or why there is none.
pub fn ask<T: DeserializeOwned>(
    client: &mut dyn Client,
    op: OpKind,
    input: Value,
) -> Result<(T, Option<String>), Refused> {
    let reply = client.call(op, input);
    let envelope: Envelope = serde_json::from_value(reply).map_err(|_| unreadable(op))?;
    if !envelope.ok {
        return Err(Refused {
            code: envelope.code,
            message: envelope.message,
        });
    }
    let data = serde_json::from_value(envelope.data).map_err(|_| unreadable(op))?;
    Ok((data, envelope.cursor))
}

/// The refusal for a reply the views cannot read. Input: the operation. Output: the
/// `reply_unreadable` refusal naming it.
pub fn unreadable(op: OpKind) -> Refused {
    Refused {
        code: "reply_unreadable".to_owned(),
        message: format!("the {} reply is not one the views can read", op.as_str()),
    }
}

/// One row of a `list` or `ready` page. A `list` row carries no version; the board reads
/// it with `show` when a card is selected and sends that version on `move`.
#[derive(Clone, Debug, Deserialize)]
pub struct Row {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub elided: bool,
    pub status: TicketStatus,
    pub priority: u8,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub version: Option<i64>,
}

/// The data of a `list` or `ready` reply.
#[derive(Debug, Deserialize)]
pub struct Rows {
    #[serde(rename = "tickets")]
    pub rows: Vec<Row>,
}

/// The data of a first `show` page: the ticket's fields and the first part of the rest.
#[derive(Clone, Debug, Deserialize)]
pub struct Shown {
    pub ticket: ShownTicket,
    #[serde(flatten)]
    pub part: ShownPart,
}

/// The fields of the ticket a `show` reply names.
#[derive(Clone, Debug, Deserialize)]
pub struct ShownTicket {
    pub id: String,
    pub version: i64,
    pub title: String,
    pub kind: TicketKind,
    pub status: TicketStatus,
    pub priority: u8,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub cycle: Option<u32>,
    #[serde(default)]
    pub closed_by: Option<String>,
    #[serde(default)]
    pub close_reason: Option<String>,
}

/// What one `show` page adds after the ticket's fields: a part of the body starting at
/// `body_at`, then criteria, links and events, each continuing where the last page
/// stopped.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ShownPart {
    #[serde(default)]
    pub body_at: usize,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub criteria: Vec<ShownCriterion>,
    #[serde(default)]
    pub links: Vec<ShownLink>,
    #[serde(default)]
    pub events: Vec<ShownEvent>,
}

/// One criterion in a `show` reply, with its checks.
#[derive(Clone, Debug, Deserialize)]
pub struct ShownCriterion {
    pub text: String,
    pub state: CriterionState,
    #[serde(default)]
    pub checks: Vec<ShownCheck>,
}

/// One check in a `show` reply, with its latest evidence if any.
#[derive(Clone, Debug, Deserialize)]
pub struct ShownCheck {
    pub kind: CheckKind,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub latest: Option<ShownEvidence>,
}

/// The latest evidence of one check.
#[derive(Clone, Debug, Deserialize)]
pub struct ShownEvidence {
    pub result: EvidenceResult,
}

/// One link in a `show` reply, with what the host knows of its target.
#[derive(Clone, Debug, Deserialize)]
pub struct ShownLink {
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub state: String,
}

/// One event in a `show` reply.
#[derive(Clone, Debug, Deserialize)]
pub struct ShownEvent {
    pub actor: String,
    pub op: String,
}

/// The data of a `roadmap` reply.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub versions: Vec<PlanVersion>,
    #[serde(default)]
    pub cycles: Vec<PlanCycle>,
}

/// One version of the roadmap with its progress.
#[derive(Clone, Debug, Deserialize)]
pub struct PlanVersion {
    pub name: String,
    #[serde(default)]
    pub target_day: Option<String>,
    pub status: VersionStatus,
    #[serde(default)]
    pub closed: u32,
    #[serde(default)]
    pub total: u32,
}

/// One cycle of the roadmap with its progress.
#[derive(Clone, Debug, Deserialize)]
pub struct PlanCycle {
    pub number: u32,
    pub starts_day: String,
    pub ends_day: String,
    pub status: CycleStatus,
    #[serde(default)]
    pub closed: u32,
    #[serde(default)]
    pub total: u32,
}
