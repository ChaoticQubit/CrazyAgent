use super::Call;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde_json::{Map, Value, json};
use std::sync::LazyLock;
use trackwright_core::catalog::{OpSpec, catalog, op_schema};
use trackwright_core::model::{Actor, ClosePolicy, OpKind, ProjectKey, Refusal, ShortId};
use trackwright_core::reply::{REPLY_BYTES_DEFAULT, Reply};
use trackwright_core::requests::Request;
use trackwright_core::store::StoreError;

/// The prefix of every ticket tool's name: `ticket_` and the core operation, such as
/// `ticket_ready`, so no ticket tool shares a name with a team tool.
pub const TOOL_PREFIX: &str = "ticket_";

/// The core operations no agent gets as a tool: ticket policy is set only in CrazyAgent's
/// ticket settings, and export and import read or write a file path the engine would open
/// with its own rights, outside the calling agent's sandbox.
pub const WITHHELD: [OpKind; 4] = [
    OpKind::PolicySet,
    OpKind::Export,
    OpKind::ImportPreview,
    OpKind::ImportApply,
];

/// The most bytes of the ticket section a launch adds after the team tools: the default
/// pack's ticket rule and the line naming the project's key and close setting.
pub const RULES_BYTES_MAX: usize = 832;

/// The engine identity that links a task `spawn` admitted to the ticket it names, so a lead
/// hands off any ticket of its project whatever the `agent_edit` setting lets it edit.
pub const DELEGATE_ACTOR: &str = "engine:delegate";

/// The sentence a `spawn` call reads when its `ticket` is not a ticket id.
pub(crate) const TICKET_ID_INVALID: &str = "Not started: give the ticket as its id, such as CA-12.";

const ARGUMENTS_BYTES_MAX: usize = super::CALL_BYTES_MAX as usize - 256;
const ARGUMENTS_INVALID: &str =
    "Not sent: give the ticket tool's arguments as one JSON object of up to 16128 bytes.";
const TOOL_NAME_BYTES_MAX: usize = 64;

const _: () = assert!(ARGUMENTS_BYTES_MAX == 16128);
const _: () = assert!(ARGUMENTS_BYTES_MAX <= trackwright_core::model::REQUEST_BYTES_MAX);
const _: () = assert!(REPLY_BYTES_DEFAULT <= super::REPLY_TEXT_MAX);

/// One core operation offered as a tool, with the name the servers list it under.
struct Offered {
    name: String,
    spec: &'static OpSpec,
}

/// Every catalog operation but the withheld ones, in catalog order, named once for the
/// life of the engine so a call's tool name lives as long as the call.
static OFFERED: LazyLock<Vec<Offered>> = LazyLock::new(|| {
    let offered: Vec<Offered> = catalog()
        .iter()
        .filter(|spec| !WITHHELD.contains(&spec.kind))
        .map(|spec| Offered {
            name: format!("{TOOL_PREFIX}{}", spec.kind),
            spec,
        })
        .collect();
    assert_eq!(offered.len() + WITHHELD.len(), catalog().len());
    assert!(
        offered
            .iter()
            .all(|tool| tool.name.len() <= TOOL_NAME_BYTES_MAX)
    );
    offered
});

/// Who calls a ticket tool, as the engine named it from the mailbox it read the call
/// from, never from the call: a sub-agent or the live master, by team agent id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Agent(i64),
    Master(i64),
}

impl Caller {
    /// The tracker actor the engine sets for this caller. Output: `agent:<id>` or
    /// `master:<id>`, which the host checks belongs to the ticket's project.
    pub fn actor(self) -> Actor {
        let text = match self {
            Self::Agent(id) => {
                assert!(id >= 1, "a team agent id is above 0");
                format!("agent:{id}")
            }
            Self::Master(id) => {
                assert!(id >= 1, "a team agent id is above 0");
                format!("master:{id}")
            }
        };
        let actor = Actor::new(&text).expect("an engine-set actor is a valid actor");
        assert_eq!(actor.as_str(), text);
        actor
    }
}

/// Names the tool that offers one core operation. Input: the operation. Output: its tool
/// name, or none for a withheld operation.
pub fn tool_name(op: OpKind) -> Option<&'static str> {
    OFFERED
        .iter()
        .find(|tool| tool.spec.kind == op)
        .map(|tool| tool.name.as_str())
}

/// Lists the ticket tools a live master's and a sub-agent's team server give, built from
/// the core's catalog: each operation's summary and its argument schema. Output: the
/// `tools/list` entries, in catalog order.
pub fn tools() -> Vec<Value> {
    let listed: Vec<Value> = OFFERED
        .iter()
        .map(|tool| {
            let schema = op_schema(tool.spec);
            assert_eq!(schema["args"]["type"], "object");
            json!({
                "name": tool.name,
                "description": tool.spec.summary,
                "inputSchema": schema["args"],
            })
        })
        .collect();
    assert_eq!(listed.len(), OFFERED.len());
    listed
}

/// Reads one ticket tool call from its MCP name and arguments. The arguments are checked
/// only for shape and size here; the core checks every field when the engine runs it.
/// Inputs: the tool name and arguments. Output: none for a name no ticket tool has, else
/// the call or the sentence the agent reads when the arguments are refused.
pub(super) fn call(name: &str, arguments: Option<&Value>) -> Option<Result<Call, &'static str>> {
    let op = OFFERED.iter().find(|tool| tool.name == name)?.spec.kind;
    let arguments = match arguments {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(object @ Value::Object(_)) => object.clone(),
        Some(_) => return Some(Err(ARGUMENTS_INVALID)),
    };
    let bytes = serde_json::to_vec(&arguments)
        .expect("a JSON value always encodes")
        .len();
    if bytes > ARGUMENTS_BYTES_MAX {
        return Some(Err(ARGUMENTS_INVALID));
    }
    assert!(arguments.is_object());
    Some(Ok(Call::Ticket { op, arguments }))
}

/// Runs one checked ticket call through the core as the caller, in its own immediate
/// transaction on `engine.db`, committed only when the core accepts it. Inputs: the engine
/// database outside any transaction, the caller, the operation and its arguments. Output:
/// the core's reply envelope as text and whether it is an error.
pub fn answer(
    connection: &Connection,
    caller: Caller,
    op: OpKind,
    arguments: &Value,
) -> (String, bool) {
    assert!(tool_name(op).is_some(), "{op} is a ticket tool");
    let reply = replied(connection, &caller.actor(), op, arguments);
    let text = reply.json().to_owned();
    assert!(text.len() <= super::REPLY_TEXT_MAX);
    (text, !reply.is_ok())
}

fn replied(connection: &Connection, actor: &Actor, op: OpKind, arguments: &Value) -> Reply {
    assert!(
        connection.is_autocommit(),
        "a ticket call runs in its own transaction"
    );
    let bytes = serde_json::to_vec(arguments).expect("a JSON value always encodes");
    let request = match Request::parse(op.as_str(), &bytes) {
        Ok(request) => request,
        Err(refusal) => return Reply::refused(&refusal),
    };
    let Ok(tx) = Transaction::new_unchecked(connection, TransactionBehavior::Immediate) else {
        return unwritable();
    };
    let reply = trackwright_core::ops::dispatch(&tx, &crate::tracker::Host, actor, request);
    if reply.is_ok() && tx.commit().is_err() {
        return unwritable();
    }
    reply
}

fn unwritable() -> Reply {
    Reply::refused(&Refusal::host(
        crate::tracker::host::HOST_FAILED,
        "the engine could not write its tracker; try again in a moment",
    ))
}

/// Links the task a `spawn` admitted to the ticket its call named, in the transaction that
/// admitted it, as `DELEGATE_ACTOR`. Inputs: the transaction, the project, the ticket id
/// the checked call carries, the task and the time. Output: Ok, or the reason the link is
/// refused, after which the caller drops the transaction, or a SQLite failure.
pub(crate) fn link_task(
    tx: &Transaction<'_>,
    project: &ProjectId,
    ticket: &str,
    task: i64,
    now_ms: i64,
) -> Result<Result<(), String>, EngineError> {
    assert!(task >= 1, "a task id is above 0");
    let id = ShortId::parse(ticket).expect("a checked spawn names a valid ticket id");
    let Some(key) = crate::tracker::key_of(tx, project)? else {
        return Ok(Err(format!(
            "this project has no tickets ({})",
            crate::tracker::host::PROJECT_NOT_ATTACHED
        )));
    };
    if id.key() != &key {
        return Ok(Err(format!(
            "{id} is not a ticket of this project ({})",
            crate::tracker::host::LINK_CROSS_PROJECT
        )));
    }
    let actor = Actor::new(DELEGATE_ACTOR).expect("the delegate actor is a valid actor");
    let target = task.to_string();
    let host = crate::tracker::Host;
    match trackwright_core::host::link(tx, &host, &actor, &id, "task", &target, now_ms) {
        Ok((link, _)) => {
            assert_eq!(link.kind, "task");
            assert_eq!(link.target, target);
            Ok(Ok(()))
        }
        Err(StoreError::Refused(refusal)) => {
            Ok(Err(format!("{} ({})", refusal.message, refusal.code)))
        }
        Err(other) => Err(crate::tracker::engine_error(other)),
    }
}

/// The ticket section a live master's or a sub-agent's launch adds after its team tools.
/// Inputs: a connection to `engine.db` and the project. Output: the default pack's ticket
/// rule and the line naming the project's key and close setting, none for a project with
/// no tickets, or `tracker_unavailable`.
pub fn rules(connection: &Connection, project: &ProjectId) -> Result<Option<String>, EngineError> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| EngineError(crate::tracker::TRACKER_UNAVAILABLE.into()))?;
    let Some(key) = crate::tracker::key_of(&tx, project)? else {
        return Ok(None);
    };
    let close = crate::tracker::settings::effective(&tx, project)?
        .policy
        .close;
    drop(tx);
    Ok(Some(section(&key, close)))
}

/// Writes the ticket section for one key and close setting. Inputs: the key and the
/// setting. Output: the section, at most `RULES_BYTES_MAX` bytes.
fn section(key: &ProjectKey, close: ClosePolicy) -> String {
    let rules = crate::setup::pack::TICKET_RULES;
    assert!(rules.ends_with('\n'));
    let sentence = match close {
        ClosePolicy::Agents => {
            "close a ticket as done with `ticket_close` once its criteria are met."
        }
        ClosePolicy::Evidence => {
            "a ticket closes by itself once every criterion's checks pass, \
             so record evidence and never close one by hand."
        }
        ClosePolicy::Owner => {
            "only the owner closes tickets, \
             so move finished work to `in_review` with `ticket_move`."
        }
    };
    let text = format!(
        "\n{rules}\nThis project's tickets use the key `{}`. \
         Its close setting is `{close}`: {sentence}\n",
        key.as_str()
    );
    assert!(text.len() <= RULES_BYTES_MAX);
    assert!(text.contains(rules));
    text
}

#[cfg(all(test, unix))]
#[path = "tracker_tests.rs"]
mod tests;
