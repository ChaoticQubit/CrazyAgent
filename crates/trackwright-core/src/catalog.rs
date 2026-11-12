use crate::model::{
    ACTOR_BYTES_MAX, CRITERIA_PER_TICKET_MAX, CYCLES_PER_PROJECT_MAX, CheckKind, CriterionState,
    EvidenceResult, OpKind, PolicyKey, REQUEST_BYTES_MAX, REQUEST_ID_BYTES_MAX, TicketKind,
    TicketStatus, VERSIONS_PER_PROJECT_MAX, VersionStatus, codes,
};
use crate::reply::{API, NEXT_MAX, REPLY_BYTES_DEFAULT, REPLY_BYTES_MAX};
use crate::runs::{RESULT_MESSAGE_INPUT_BYTES_MAX, RunEnd, SOURCE_JSON_BYTES_MAX, StopReason};
use crate::tests_model::{
    CASE_TITLE_BYTES_MAX, CaseKind, CaseStatus, INTEGRITY_JSON_BYTES_MAX, PLAN_NAME_BYTES_MAX,
    POLICY_JSON_BYTES_MAX, PlanStatus, RESULT_ARTIFACT_BYTES_MAX, RESULT_NAME_BYTES_MAX,
    RESULT_ROWS_PER_RECORD_MAX, RUN_BATCH_BYTES_MAX, RUN_REASON_BYTES_MAX, RUN_TITLE_BYTES_MAX,
    ResultOutcome, SET_NAME_BYTES_MAX, STABLE_NAME_BYTES_MAX, STEP_BYTES_MAX, STEPS_PER_CASE_MAX,
    TestObject, Verdict,
};
use crate::text::{self, TITLE_BYTES_MAX, TextField};
use serde_json::{Map, Value, json};

pub const CURSOR_BYTES_MAX: usize = 64;
pub const WORD_BYTES_MAX: usize = 32;
pub const PATH_BYTES_MAX: usize = 4096;
pub const KEY_BYTES_MAX: usize = 64;
pub const KEYS_MAX: usize = 256;
pub const COMMIT_BYTES_MAX: usize = 64;

/// A closed set of names a field may take, each read from its model enum so the catalog,
/// the schema and the store never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    TicketKind,
    TicketStatus,
    CriterionState,
    CheckKind,
    EvidenceResult,
    VersionStatus,
    PolicyKey,
    Op,
    CaseKind,
    CaseStatus,
    PlanStatus,
    Verdict,
    RunEnd,
    StopReason,
}

impl Named {
    /// The names in declaration order. Output: the names, never empty.
    pub fn names(self) -> Vec<&'static str> {
        let names: Vec<&'static str> = match self {
            Named::TicketKind => TicketKind::ALL.iter().map(|v| v.as_str()).collect(),
            Named::TicketStatus => TicketStatus::ALL.iter().map(|v| v.as_str()).collect(),
            Named::CriterionState => CriterionState::ALL.iter().map(|v| v.as_str()).collect(),
            Named::CheckKind => CheckKind::ALL.iter().map(|v| v.as_str()).collect(),
            Named::EvidenceResult => EvidenceResult::ALL.iter().map(|v| v.as_str()).collect(),
            Named::VersionStatus => VersionStatus::ALL.iter().map(|v| v.as_str()).collect(),
            Named::PolicyKey => PolicyKey::ALL.iter().map(|v| v.as_str()).collect(),
            Named::Op => OpKind::ALL.iter().map(|v| v.as_str()).collect(),
            Named::CaseKind => CaseKind::ALL.iter().map(|v| v.as_str()).collect(),
            Named::CaseStatus => CaseStatus::ALL.iter().map(|v| v.as_str()).collect(),
            Named::PlanStatus => PlanStatus::ALL.iter().map(|v| v.as_str()).collect(),
            Named::Verdict => Verdict::ALL.iter().map(|v| v.as_str()).collect(),
            Named::RunEnd => RunEnd::ALL.iter().map(|v| v.as_str()).collect(),
            Named::StopReason => StopReason::ALL.iter().map(|v| v.as_str()).collect(),
        };
        assert!(!names.is_empty());
        names
    }
}

/// A JSON value one field holds whole, read into its core type when the request is parsed:
/// a case's steps, one batch of results, and a run's source, policy and integrity blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Steps,
    Results,
    Source,
    Policy,
    Integrity,
}

impl Shape {
    /// The most bytes the value may take as compact JSON; the request bound caps steps and
    /// results. Output: the bound, at most `REQUEST_BYTES_MAX`.
    pub fn bytes_max(self) -> usize {
        let bytes_max = match self {
            Shape::Steps | Shape::Results => REQUEST_BYTES_MAX,
            Shape::Source => SOURCE_JSON_BYTES_MAX,
            Shape::Policy => POLICY_JSON_BYTES_MAX,
            Shape::Integrity => INTEGRITY_JSON_BYTES_MAX,
        };
        assert!(bytes_max > 0);
        assert!(bytes_max <= REQUEST_BYTES_MAX);
        bytes_max
    }
}

/// What one field holds on the wire, with its bounds. Every type is a JSON scalar except
/// `Keys`, a short list of item keys, and `Shape`, a JSON value read whole. `Line` is one
/// line of visible text up to its byte bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Project,
    Ticket,
    Test(TestObject),
    Line { bytes_max: usize },
    Shape(Shape),
    Actor,
    Text(TextField),
    Version,
    Integer { min: i64, max: i64 },
    Choice(Named),
    Day,
    Flag,
    Cursor,
    RequestId,
    Word,
    Path,
    Keys,
}

/// One field of an operation: its wire name, type, whether a request must carry it, and
/// one line for agents and the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    pub name: &'static str,
    pub ty: FieldType,
    pub required: bool,
    pub summary: &'static str,
}

/// One operation of the catalog: its kind, one line saying what it does, and its fields in
/// the order requests, schema, CLI flags and MCP tools list them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpSpec {
    pub kind: OpKind,
    pub summary: &'static str,
    pub fields: &'static [FieldSpec],
}

const fn req(name: &'static str, ty: FieldType, summary: &'static str) -> FieldSpec {
    FieldSpec {
        name,
        ty,
        required: true,
        summary,
    }
}

const fn opt(name: &'static str, ty: FieldType, summary: &'static str) -> FieldSpec {
    FieldSpec {
        name,
        ty,
        required: false,
        summary,
    }
}

const PROJECT: FieldSpec = req(
    "project",
    FieldType::Project,
    "The project's key, for example CA.",
);
const ID: FieldSpec = req(
    "id",
    FieldType::Ticket,
    "The ticket's short id, for example CA-42.",
);
const VERSION: FieldSpec = req(
    "version",
    FieldType::Version,
    "The version you last read; a newer stored version refuses the write as stale.",
);
const CURSOR: FieldSpec = opt(
    "cursor",
    FieldType::Cursor,
    "The cursor of the previous page, to read the next one.",
);
const REQUEST_ID: FieldSpec = opt(
    "request_id",
    FieldType::RequestId,
    "Your key for this write; repeating it with the same fields returns the first reply.",
);
const POSITION: FieldSpec = req(
    "position",
    FieldType::Integer {
        min: 1,
        max: CRITERIA_PER_TICKET_MAX as i64,
    },
    "The criterion's position on the ticket, from 1.",
);
const TITLE: FieldType = FieldType::Text(TextField::Title);
const BODY: FieldType = FieldType::Text(TextField::Body);
const PRIORITY: FieldType = FieldType::Integer { min: 0, max: 4 };
const STATUS: FieldType = FieldType::Choice(Named::TicketStatus);
const KIND: FieldType = FieldType::Choice(Named::TicketKind);
const PARENT: FieldSpec = opt(
    "parent",
    FieldType::Ticket,
    "The parent ticket, open and in the same project.",
);
const RELEASE: FieldSpec = opt(
    "release",
    FieldType::Text(TextField::VersionName),
    "The product version it ships in, by name.",
);
const CYCLE: FieldSpec = opt(
    "cycle",
    FieldType::Integer {
        min: 1,
        max: CYCLES_PER_PROJECT_MAX as i64,
    },
    "The open cycle it is planned for, by number.",
);
const CASE: FieldType = FieldType::Test(TestObject::Case);
const SET: FieldType = FieldType::Test(TestObject::Set);
const PLAN: FieldType = FieldType::Test(TestObject::Plan);
const CASE_KIND: FieldType = FieldType::Choice(Named::CaseKind);
const CASE_STATUS: FieldType = FieldType::Choice(Named::CaseStatus);
const PLAN_STATUS: FieldType = FieldType::Choice(Named::PlanStatus);
const STEPS: FieldType = FieldType::Shape(Shape::Steps);
const STABLE_NAME: FieldType = FieldType::Line {
    bytes_max: STABLE_NAME_BYTES_MAX,
};
const BATCH: FieldType = FieldType::Line {
    bytes_max: RUN_BATCH_BYTES_MAX,
};
const RELEASE_NAME: FieldType = FieldType::Text(TextField::VersionName);
const CASE_ID: FieldSpec = req("id", CASE, "The case's id, for example CA-T3.");
const SET_ID: FieldSpec = req("id", SET, "The set's id, for example CA-S1.");
const PLAN_ID: FieldSpec = req("id", PLAN, "The plan's id, for example CA-P2.");
const RUN_ID: FieldSpec = req(
    "id",
    FieldType::Test(TestObject::Run),
    "The run's id, for example CA-R7.",
);
const CRITERION_TICKET: FieldSpec = req(
    "ticket",
    FieldType::Ticket,
    "The ticket holding the criterion, for example CA-42.",
);
const _: () = assert!(CASE_TITLE_BYTES_MAX == TITLE_BYTES_MAX);
const _: () = assert!(SET_NAME_BYTES_MAX == TITLE_BYTES_MAX);
const _: () = assert!(PLAN_NAME_BYTES_MAX == TITLE_BYTES_MAX);
const _: () = assert!(RUN_TITLE_BYTES_MAX == TITLE_BYTES_MAX);

const CATALOG: &[OpSpec] = &[
    OpSpec {
        kind: OpKind::Ready,
        summary: "Lists unblocked work you may take: unclaimed or yours, by priority, active \
                  cycle and age.",
        fields: &[PROJECT, CURSOR],
    },
    OpSpec {
        kind: OpKind::List,
        summary: "Lists a project's tickets by number, optionally by status, kind or assignee.",
        fields: &[
            PROJECT,
            opt("status", STATUS, "Only tickets in this status."),
            opt("kind", KIND, "Only tickets of this kind."),
            opt(
                "assignee",
                FieldType::Actor,
                "Only tickets assigned to this actor.",
            ),
            CURSOR,
        ],
    },
    OpSpec {
        kind: OpKind::Show,
        summary: "Shows one ticket: body, criteria with checks and latest evidence, links and \
                  recent events.",
        fields: &[ID, CURSOR],
    },
    OpSpec {
        kind: OpKind::History,
        summary: "Lists one ticket's events, oldest first, with who changed which fields.",
        fields: &[ID, CURSOR],
    },
    OpSpec {
        kind: OpKind::Roadmap,
        summary: "Shows a project's versions and cycles with closed and total tickets each.",
        fields: &[PROJECT, CURSOR],
    },
    OpSpec {
        kind: OpKind::PolicyGet,
        summary: "Shows a project's policy, every key with its value and options.",
        fields: &[PROJECT],
    },
    OpSpec {
        kind: OpKind::Export,
        summary: "Writes a project's open tickets, or one ticket, as Markdown.",
        fields: &[
            PROJECT,
            opt("id", FieldType::Ticket, "Only this ticket."),
            opt(
                "path",
                FieldType::Path,
                "The file to write; the reply carries the text when absent.",
            ),
        ],
    },
    OpSpec {
        kind: OpKind::Schema,
        summary: "Lists the operations, or one operation's fields.",
        fields: &[
            opt(
                "op",
                FieldType::Choice(Named::Op),
                "Only this operation's fields.",
            ),
            CURSOR,
        ],
    },
    OpSpec {
        kind: OpKind::Create,
        summary: "Creates a ticket in a project.",
        fields: &[
            PROJECT,
            req("title", TITLE, "One line naming the work."),
            opt("body", BODY, "What and why, in Markdown."),
            opt("kind", KIND, "feature when absent."),
            opt(
                "priority",
                PRIORITY,
                "0 most urgent to 4 least; 2 when absent.",
            ),
            opt(
                "status",
                STATUS,
                "An open status to start in, backlog when absent; under create = triage an \
                 agent's ticket starts in triage.",
            ),
            PARENT,
            RELEASE,
            CYCLE,
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Edit,
        summary: "Changes a ticket's title, body, kind, priority, parent, version or cycle.",
        fields: &[
            ID,
            VERSION,
            opt("title", TITLE, "The new title."),
            opt("body", BODY, "The new body."),
            opt("kind", KIND, "The new kind."),
            opt(
                "priority",
                PRIORITY,
                "The new priority, 0 most urgent to 4 least.",
            ),
            PARENT,
            RELEASE,
            CYCLE,
            opt(
                "parent_clear",
                FieldType::Flag,
                "true takes it out of its parent.",
            ),
            opt(
                "release_clear",
                FieldType::Flag,
                "true takes it out of its version.",
            ),
            opt(
                "cycle_clear",
                FieldType::Flag,
                "true takes it out of its cycle.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Move,
        summary: "Moves a ticket to another open status; close and reopen change the closed ones.",
        fields: &[
            ID,
            VERSION,
            req(
                "status",
                STATUS,
                "triage, backlog, todo, in_progress or in_review.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Claim,
        summary: "Assigns an unclaimed ticket to you, ready or not; one claimer wins.",
        fields: &[ID, VERSION, REQUEST_ID],
    },
    OpSpec {
        kind: OpKind::Release,
        summary: "Gives up your claim on a ticket.",
        fields: &[ID, VERSION, REQUEST_ID],
    },
    OpSpec {
        kind: OpKind::Depend,
        summary: "Records that a ticket waits for another one to close.",
        fields: &[
            ID,
            req("on", FieldType::Ticket, "The ticket it waits for."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Undepend,
        summary: "Removes a dependency.",
        fields: &[
            ID,
            req(
                "on",
                FieldType::Ticket,
                "The ticket it no longer waits for.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CriterionAdd,
        summary: "Adds an open acceptance criterion at the ticket's next position.",
        fields: &[
            ID,
            req(
                "text",
                FieldType::Text(TextField::Criterion),
                "One checkable line.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CriterionEdit,
        summary: "Rewrites a criterion's text.",
        fields: &[
            ID,
            POSITION,
            req(
                "version",
                FieldType::Version,
                "The criterion version you last read.",
            ),
            req(
                "text",
                FieldType::Text(TextField::Criterion),
                "The new text.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CheckAdd,
        summary: "Adds a check that proves a criterion: a test, a change, a manual check or a \
                  CI run.",
        fields: &[
            ID,
            POSITION,
            req(
                "kind",
                FieldType::Choice(Named::CheckKind),
                "What proves it.",
            ),
            req(
                "ref",
                FieldType::Text(TextField::CheckRef),
                "The test, change, check or CI job it names.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::EvidenceRecord,
        summary: "Records one outcome of a check.",
        fields: &[
            ID,
            POSITION,
            req(
                "kind",
                FieldType::Choice(Named::CheckKind),
                "The check's kind.",
            ),
            req(
                "ref",
                FieldType::Text(TextField::CheckRef),
                "The check's reference.",
            ),
            req(
                "result",
                FieldType::Choice(Named::EvidenceResult),
                "pass or fail.",
            ),
            opt(
                "evidence",
                FieldType::Text(TextField::CheckRef),
                "Where the outcome can be read, for example a run URL.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CriterionSet,
        summary: "Sets a criterion open, met or waived.",
        fields: &[
            ID,
            POSITION,
            req(
                "version",
                FieldType::Version,
                "The criterion version you last read.",
            ),
            req(
                "state",
                FieldType::Choice(Named::CriterionState),
                "open, met or waived.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Link,
        summary: "Links a ticket to a target of a kind the host declares.",
        fields: &[
            ID,
            req(
                "kind",
                FieldType::Text(TextField::LinkKind),
                "A link kind the host declares.",
            ),
            req(
                "target",
                FieldType::Text(TextField::LinkTarget),
                "The target the host resolves.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Unlink,
        summary: "Removes a ticket's link.",
        fields: &[
            ID,
            req(
                "kind",
                FieldType::Text(TextField::LinkKind),
                "The link's kind.",
            ),
            req(
                "target",
                FieldType::Text(TextField::LinkTarget),
                "The link's target.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Close,
        summary: "Closes a ticket as done or cancelled, as the project's close policy allows.",
        fields: &[
            ID,
            VERSION,
            opt("status", STATUS, "done or cancelled; done when absent."),
            opt(
                "reason",
                FieldType::Text(TextField::CloseReason),
                "Why it closes.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::Reopen,
        summary: "Reopens a closed ticket.",
        fields: &[
            ID,
            VERSION,
            opt(
                "status",
                STATUS,
                "The open status to return to; todo when absent.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::VersionUpsert,
        summary: "Adds a product version to the roadmap, or changes one.",
        fields: &[
            PROJECT,
            req(
                "name",
                FieldType::Text(TextField::VersionName),
                "The version's name, for example V2.",
            ),
            opt(
                "version",
                FieldType::Version,
                "The row version you last read; required to change an existing version.",
            ),
            opt(
                "position",
                FieldType::Integer {
                    min: 0,
                    max: VERSIONS_PER_PROJECT_MAX as i64 - 1,
                },
                "Its place on the roadmap, from 0.",
            ),
            opt(
                "target_day",
                FieldType::Day,
                "The day it should ship, YYYY-MM-DD.",
            ),
            opt(
                "status",
                FieldType::Choice(Named::VersionStatus),
                "planned, active or released.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CycleStart,
        summary: "Starts a project's next cycle.",
        fields: &[
            PROJECT,
            opt(
                "starts_day",
                FieldType::Day,
                "Its first day, YYYY-MM-DD; when absent, the day after the latest cycle or \
                 today in UTC, whichever is later.",
            ),
            opt(
                "days",
                FieldType::Integer { min: 1, max: 90 },
                "Its length; the project's cycle_days when absent.",
            ),
            opt(
                "release",
                FieldType::Text(TextField::VersionName),
                "The product version it works toward.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CycleClose,
        summary: "Closes a cycle; unfinished work rolls over as the project's rollover policy \
                  says.",
        fields: &[
            PROJECT,
            req(
                "number",
                FieldType::Integer {
                    min: 1,
                    max: CYCLES_PER_PROJECT_MAX as i64,
                },
                "The cycle's number.",
            ),
            req(
                "version",
                FieldType::Version,
                "The cycle version you last read.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::ImportPreview,
        summary: "Reads a roadmap or release checklist file and lists what an import would change.",
        fields: &[
            PROJECT,
            req("path", FieldType::Path, "The Markdown file to read."),
        ],
    },
    OpSpec {
        kind: OpKind::ImportApply,
        summary: "Applies the accepted items of an import preview.",
        fields: &[
            PROJECT,
            req(
                "path",
                FieldType::Path,
                "The Markdown file the preview read.",
            ),
            req("accept", FieldType::Keys, "The preview item keys to apply."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::PolicySet,
        summary: "Sets one policy key of a project.",
        fields: &[
            PROJECT,
            req(
                "key",
                FieldType::Choice(Named::PolicyKey),
                "The policy key.",
            ),
            req(
                "value",
                FieldType::Word,
                "One of the key's options, or a number of days for cycle_days.",
            ),
            opt(
                "version",
                FieldType::Version,
                "The key's version you last read; required once the key was set.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CaseList,
        summary: "Lists a project's cases by number, optionally by kind, status, set or no link.",
        fields: &[
            PROJECT,
            opt("kind", CASE_KIND, "Only cases of this kind."),
            opt("status", CASE_STATUS, "Only cases in this status."),
            opt("set", SET, "Only cases in this set."),
            opt(
                "unlinked",
                FieldType::Flag,
                "Only cases linked to no criterion when true.",
            ),
            CURSOR,
        ],
    },
    OpSpec {
        kind: OpKind::CaseShow,
        summary: "Shows one case: steps as policy allows, criteria, sets and latest outcome.",
        fields: &[CASE_ID, CURSOR],
    },
    OpSpec {
        kind: OpKind::SetShow,
        summary: "Shows one set and its cases in order.",
        fields: &[SET_ID, CURSOR],
    },
    OpSpec {
        kind: OpKind::PlanList,
        summary: "Lists a project's plans by number, optionally by status.",
        fields: &[
            PROJECT,
            opt("status", PLAN_STATUS, "Only plans in this status."),
            CURSOR,
        ],
    },
    OpSpec {
        kind: OpKind::PlanShow,
        summary: "Shows one plan: its release, cycle, status and items in order.",
        fields: &[PLAN_ID, CURSOR],
    },
    OpSpec {
        kind: OpKind::RunList,
        summary: "Lists a project's runs newest first, by plan, batch, verdict or commit.",
        fields: &[
            PROJECT,
            opt("plan", PLAN, "Only runs of this plan."),
            opt("batch", BATCH, "Only runs of this batch."),
            opt(
                "verdict",
                FieldType::Choice(Named::Verdict),
                "Only runs with this verdict.",
            ),
            opt(
                "commit",
                FieldType::Line {
                    bytes_max: COMMIT_BYTES_MAX,
                },
                "Only runs of this source commit, 40 hex digits.",
            ),
            CURSOR,
        ],
    },
    OpSpec {
        kind: OpKind::RunShow,
        summary: "Shows one run: its source and policy binding, integrity, totals and results.",
        fields: &[RUN_ID, CURSOR],
    },
    OpSpec {
        kind: OpKind::Coverage,
        summary: "Shows each criterion of a ticket or release with its cases and latest results.",
        fields: &[
            PROJECT,
            opt(
                "ticket",
                FieldType::Ticket,
                "Cover this ticket's criteria; give ticket or release.",
            ),
            opt(
                "release",
                RELEASE_NAME,
                "Cover the criteria of this release's tickets; give ticket or release.",
            ),
            CURSOR,
        ],
    },
    OpSpec {
        kind: OpKind::CaseCreate,
        summary: "Creates a test case in a project.",
        fields: &[
            PROJECT,
            req("title", TITLE, "One line naming what the case proves."),
            req("kind", CASE_KIND, "What the case checks."),
            opt(
                "stable_name",
                STABLE_NAME,
                "The runner's name for the test; imports match results by it.",
            ),
            opt("steps", STEPS, "The steps in order, each in Markdown."),
            opt(
                "protected",
                FieldType::Flag,
                "Hides the steps from agents as policy says; false when absent.",
            ),
            opt("status", CASE_STATUS, "draft when absent."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CaseEdit,
        summary: "Changes a case's title, kind, stable name, steps, protection or status.",
        fields: &[
            CASE_ID,
            VERSION,
            opt("title", TITLE, "The new title."),
            opt("kind", CASE_KIND, "The new kind."),
            opt("stable_name", STABLE_NAME, "The new stable name."),
            opt(
                "clear_stable_name",
                FieldType::Flag,
                "Removes the stable name when true.",
            ),
            opt("steps", STEPS, "The new steps."),
            opt("protected", FieldType::Flag, "The new protection."),
            opt("status", CASE_STATUS, "The new status."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::CaseLink,
        summary: "Links a case to one acceptance criterion it proves.",
        fields: &[CASE_ID, VERSION, CRITERION_TICKET, POSITION, REQUEST_ID],
    },
    OpSpec {
        kind: OpKind::CaseUnlink,
        summary: "Removes a case's link to one acceptance criterion.",
        fields: &[CASE_ID, VERSION, CRITERION_TICKET, POSITION, REQUEST_ID],
    },
    OpSpec {
        kind: OpKind::SetUpsert,
        summary: "Creates a set, or renames one at the version you read.",
        fields: &[
            PROJECT,
            opt("id", SET, "The set to rename; a new set when absent."),
            opt(
                "version",
                FieldType::Version,
                "The set version you last read; required with id.",
            ),
            req("name", TITLE, "One line naming the set."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::SetAdd,
        summary: "Adds a case to the end of a set.",
        fields: &[
            SET_ID,
            VERSION,
            req("case", CASE, "The case to add."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::SetRemove,
        summary: "Removes a case from a set.",
        fields: &[
            SET_ID,
            VERSION,
            req("case", CASE, "The case to remove."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::PlanUpsert,
        summary: "Creates a plan, or sets one's fields at the version you read.",
        fields: &[
            PROJECT,
            opt("id", PLAN, "The plan to change; a new plan when absent."),
            opt(
                "version",
                FieldType::Version,
                "The plan version you last read; required with id.",
            ),
            req("name", TITLE, "One line naming the plan."),
            opt(
                "release",
                RELEASE_NAME,
                "The project's release version the plan is for.",
            ),
            opt(
                "cycle",
                FieldType::Integer {
                    min: 1,
                    max: CYCLES_PER_PROJECT_MAX as i64,
                },
                "The project's cycle number the plan is for.",
            ),
            opt("status", PLAN_STATUS, "draft when absent."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::PlanAdd,
        summary: "Adds one case or one set to the end of a plan.",
        fields: &[
            PLAN_ID,
            VERSION,
            opt("case", CASE, "The case to add; give case or set."),
            opt("set", SET, "The set to add; give case or set."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::PlanRemove,
        summary: "Removes one case or one set from a plan.",
        fields: &[
            PLAN_ID,
            VERSION,
            opt("case", CASE, "The case to remove; give case or set."),
            opt("set", SET, "The set to remove; give case or set."),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::RunStart,
        summary: "Starts a run of a project's tests with its source and policy bound.",
        fields: &[
            PROJECT,
            req("title", TITLE, "One line naming the run."),
            opt("plan", PLAN, "The plan the run runs."),
            opt("batch", BATCH, "The label grouping one trigger's runs."),
            req(
                "source",
                FieldType::Shape(Shape::Source),
                "The source the run covers: a commit or a declared source.",
            ),
            req(
                "policy",
                FieldType::Shape(Shape::Policy),
                "The check policy the run ran under.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::RunRecord,
        summary: "Appends one batch of results to a running run.",
        fields: &[
            RUN_ID,
            VERSION,
            req(
                "results",
                FieldType::Shape(Shape::Results),
                "1 to 500 results: name, outcome, optional duration_ms, message, artifact.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::RunFinish,
        summary: "Finishes a running run; its verdict follows the locked verdict rules.",
        fields: &[
            RUN_ID,
            VERSION,
            req(
                "end",
                FieldType::Choice(Named::RunEnd),
                "How the run ended.",
            ),
            opt(
                "exit_code",
                FieldType::Integer {
                    min: i32::MIN as i64,
                    max: i32::MAX as i64,
                },
                "The process exit code, when a process ran.",
            ),
            opt(
                "reason",
                FieldType::Line {
                    bytes_max: RUN_REASON_BYTES_MAX,
                },
                "Why the run never started; only with not_started.",
            ),
            opt(
                "integrity",
                FieldType::Shape(Shape::Integrity),
                "The host's integrity evidence.",
            ),
            REQUEST_ID,
        ],
    },
    OpSpec {
        kind: OpKind::RunCancel,
        summary: "Interrupts a running run.",
        fields: &[
            RUN_ID,
            VERSION,
            req(
                "reason",
                FieldType::Choice(Named::StopReason),
                "Why the run stopped.",
            ),
            REQUEST_ID,
        ],
    },
];

const _: () = {
    assert!(CATALOG.len() == OpKind::ALL.len());
    let mut at = 0;
    while at < CATALOG.len() {
        assert!(CATALOG[at].kind as usize == OpKind::ALL[at] as usize);
        assert!(!CATALOG[at].fields.is_empty());
        at += 1;
    }
};
const _: () = assert!(PATH_BYTES_MAX < REQUEST_BYTES_MAX);

/// Every operation, in `OpKind::ALL` order. Output: the catalog.
pub fn catalog() -> &'static [OpSpec] {
    CATALOG
}

/// One operation's entry. Input: the kind. Output: its spec.
pub fn spec(kind: OpKind) -> &'static OpSpec {
    let at = OpKind::ALL
        .iter()
        .position(|listed| *listed == kind)
        .unwrap_or_else(|| panic!("{kind} is in OpKind::ALL"));
    let spec = &CATALOG[at];
    assert_eq!(spec.kind, kind);
    spec
}

/// The published JSON Schema of API version 1, built from the catalog alone: every
/// operation's arguments, the reply envelope and the core's refusal codes. `schema/v1.json`
/// holds this value. Output: the schema.
pub fn schema_v1() -> Value {
    let mut ops = Map::new();
    for spec in CATALOG {
        ops.insert(spec.kind.as_str().to_owned(), op_schema(spec));
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "trackwright:schema:v1",
        "title": "trackwright operations, API version 1",
        "api": API,
        "request_bytes_max": REQUEST_BYTES_MAX,
        "reply_bytes_default": REPLY_BYTES_DEFAULT,
        "reply_bytes_max": REPLY_BYTES_MAX,
        "ops": ops,
        "reply": reply_schema(),
        "codes": codes::ALL,
    })
}

/// One operation's schema: its summary, whether it writes, and its arguments object.
/// Input: the spec. Output: the schema.
pub fn op_schema(spec: &OpSpec) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in spec.fields {
        let mut schema = type_schema(field.ty);
        schema.insert("description".to_owned(), json!(field.summary));
        let replaced = properties.insert(field.name.to_owned(), Value::Object(schema));
        assert!(
            replaced.is_none(),
            "{}: field {} is listed once",
            spec.kind,
            field.name
        );
        if field.required {
            required.push(field.name);
        }
    }
    json!({
        "summary": spec.summary,
        "write": spec.kind.is_write(),
        "args": {
            "type": "object",
            "additionalProperties": false,
            "required": required,
            "properties": properties,
        },
    })
}

fn type_schema(ty: FieldType) -> Map<String, Value> {
    let schema = match ty {
        FieldType::Project => json!({"type": "string", "pattern": "^[A-Z]{2,8}$"}),
        FieldType::Ticket => json!({"type": "string", "pattern": "^[A-Z]{2,8}-[1-9][0-9]{0,9}$"}),
        FieldType::Test(object) => json!({
            "type": "string",
            "pattern": format!("^[A-Z]{{2,8}}-{}[1-9][0-9]{{0,9}}$", object.letter()),
        }),
        FieldType::Line { bytes_max } => {
            assert!(bytes_max > 0);
            json!({
                "type": "string",
                "minLength": 1,
                "maxLength": bytes_max,
                "maxBytes": bytes_max,
                "pattern": "^[^\\r\\n]+$",
            })
        }
        FieldType::Shape(shape) => shape_schema(shape),
        FieldType::Actor => json!({
            "type": "string",
            "minLength": 1,
            "maxLength": ACTOR_BYTES_MAX,
            "pattern": "^[A-Za-z0-9:_.@/-]+$",
        }),
        FieldType::Text(field) => text_schema(field),
        FieldType::Version => json!({"type": "integer", "minimum": 1}),
        FieldType::Integer { min, max } => {
            assert!(min <= max);
            json!({"type": "integer", "minimum": min, "maximum": max})
        }
        FieldType::Choice(named) => json!({"type": "string", "enum": named.names()}),
        FieldType::Day => json!({
            "type": "string",
            "format": "date",
            "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
        }),
        FieldType::Flag => json!({"type": "boolean"}),
        FieldType::Cursor => json!({
            "type": "string",
            "minLength": 1,
            "maxLength": CURSOR_BYTES_MAX,
            "pattern": "^[a-z0-9:_-]+$",
        }),
        FieldType::RequestId => json!({
            "type": "string",
            "minLength": 1,
            "maxLength": REQUEST_ID_BYTES_MAX,
            "pattern": "^[A-Za-z0-9._:-]+$",
        }),
        FieldType::Word => json!({
            "type": "string",
            "minLength": 1,
            "maxLength": WORD_BYTES_MAX,
            "pattern": "^[a-z0-9_]+$",
        }),
        FieldType::Path => json!({
            "type": "string",
            "minLength": 1,
            "maxLength": PATH_BYTES_MAX,
            "maxBytes": PATH_BYTES_MAX,
            "pattern": "^[^\\r\\n]+$",
        }),
        FieldType::Keys => json!({
            "type": "array",
            "minItems": 1,
            "maxItems": KEYS_MAX,
            "uniqueItems": true,
            "items": {
                "type": "string",
                "minLength": 1,
                "maxLength": KEY_BYTES_MAX,
                "pattern": "^[A-Za-z0-9._:-]+$",
            },
        }),
    };
    match schema {
        Value::Object(map) => map,
        other => panic!("a type schema is an object, not {other}"),
    }
}

fn shape_schema(shape: Shape) -> Value {
    let bytes_max = shape.bytes_max();
    match shape {
        Shape::Steps => json!({
            "type": "array",
            "maxItems": STEPS_PER_CASE_MAX,
            "maxBytes": bytes_max,
            "items": {"type": "string", "minLength": 1, "maxLength": STEP_BYTES_MAX},
        }),
        Shape::Results => json!({
            "type": "array",
            "minItems": 1,
            "maxItems": RESULT_ROWS_PER_RECORD_MAX,
            "maxBytes": bytes_max,
            "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "outcome"],
                "properties": {
                    "name": {"type": "string", "minLength": 1, "maxLength": RESULT_NAME_BYTES_MAX},
                    "outcome": {"type": "string", "enum": outcome_names()},
                    "duration_ms": {"type": "integer", "minimum": 0, "maximum": u32::MAX},
                    "message": {"type": "string", "maxLength": RESULT_MESSAGE_INPUT_BYTES_MAX},
                    "artifact": {"type": "string", "maxLength": RESULT_ARTIFACT_BYTES_MAX},
                },
            },
        }),
        Shape::Source | Shape::Policy | Shape::Integrity => {
            json!({"type": "object", "maxBytes": bytes_max})
        }
    }
}

fn outcome_names() -> Vec<&'static str> {
    let names: Vec<&'static str> = ResultOutcome::ALL.iter().map(|v| v.as_str()).collect();
    assert!(!names.is_empty());
    names
}

fn text_schema(field: TextField) -> Value {
    let bytes_max = field.bytes_max();
    let mut schema = json!({
        "type": "string",
        "maxLength": bytes_max,
        "maxBytes": bytes_max,
    });
    if text::check(field, "").is_err() {
        schema["minLength"] = json!(1);
    }
    if text::check(field, "a\nb").is_err() {
        schema["pattern"] = json!("^[^\\r\\n]*$");
    }
    schema
}

fn reply_schema() -> Value {
    let next = json!({
        "type": "array",
        "maxItems": NEXT_MAX,
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["op", "why"],
            "properties": {
                "op": {"type": "string", "enum": Named::Op.names()},
                "id": {"type": "string"},
                "why": {"type": "string"},
            },
        },
    });
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["api", "ok", "data", "next", "cursor"],
                "properties": {
                    "api": {"const": API},
                    "ok": {"const": true},
                    "data": {},
                    "next": next,
                    "cursor": {"type": ["string", "null"], "maxLength": CURSOR_BYTES_MAX},
                },
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["api", "ok", "code", "message", "next"],
                "properties": {
                    "api": {"const": API},
                    "ok": {"const": false},
                    "code": {"type": "string"},
                    "message": {"type": "string"},
                    "data": {"type": "object"},
                    "next": next,
                },
            },
        ],
    })
}
