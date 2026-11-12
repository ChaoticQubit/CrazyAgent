use crate::tests_model::{
    CaseProtectedVisibility, RunRetention, RunWriters, SkippedCounts, TestEvidence,
};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::fmt;

/// The refusal codes the core itself returns. A host adds its own codes beside these.
pub mod codes {
    pub const TEXT_EMPTY: &str = "text_empty";
    pub const TEXT_TOO_LONG: &str = "text_too_long";
    pub const TEXT_LINE_BREAK: &str = "text_line_break";
    pub const TEXT_HIDDEN_CHARACTERS: &str = "text_hidden_characters";
    pub const PROJECT_KEY_INVALID: &str = "project_key_invalid";
    pub const PROJECT_EXISTS: &str = "project_exists";
    pub const PROJECT_UNKNOWN: &str = "project_unknown";
    pub const SHORT_ID_INVALID: &str = "short_id_invalid";
    pub const TICKET_UNKNOWN: &str = "ticket_unknown";
    pub const ACTOR_INVALID: &str = "actor_invalid";
    pub const PRIORITY_INVALID: &str = "priority_invalid";
    pub const CYCLE_DAYS_INVALID: &str = "cycle_days_invalid";
    pub const STALE_VERSION: &str = "stale_version";
    pub const STATUS_CLOSED_AT_CREATE: &str = "status_closed_at_create";
    pub const CLOSE_REASON_OPEN: &str = "close_reason_open";
    pub const TICKETS_FULL: &str = "tickets_full";
    pub const CRITERIA_FULL: &str = "criteria_full";
    pub const CRITERION_UNKNOWN: &str = "criterion_unknown";
    pub const LINKS_FULL: &str = "links_full";
    pub const LINK_EXISTS: &str = "link_exists";
    pub const LINK_UNKNOWN: &str = "link_unknown";
    pub const STORE_PATH_REFUSED: &str = "store_path_refused";
    pub const STORE_NOT_PRIVATE: &str = "store_not_private";
    pub const STORE_SCHEMA_NEWER: &str = "store_schema_newer";
    pub const STORE_FAILED: &str = "store_failed";
    pub const UNKNOWN_OP: &str = "unknown_op";
    pub const ARGS_INVALID: &str = "args_invalid";
    pub const UNKNOWN_FIELD: &str = "unknown_field";
    pub const MISSING_FIELD: &str = "missing_field";
    pub const FIELD_INVALID: &str = "field_invalid";
    pub const REQUEST_TOO_LARGE: &str = "request_too_large";
    pub const REQUEST_ID_REUSED: &str = "request_id_reused";
    pub const CURSOR_INVALID: &str = "cursor_invalid";
    pub const OP_UNAVAILABLE: &str = "op_unavailable";
    pub const USE_CLOSE: &str = "use_close";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const OPEN_TICKETS_FULL: &str = "open_tickets_full";

    pub const TICKET_CLOSED: &str = "ticket_closed";
    pub const TICKET_OPEN: &str = "ticket_open";
    pub const STATUS_INVALID: &str = "status_invalid";
    pub const CLOSE_NEEDS_EVIDENCE: &str = "close_needs_evidence";
    pub const CLOSE_OWNER_ONLY: &str = "close_owner_only";
    pub const CREATE_OWNER_ONLY: &str = "create_owner_only";
    pub const AGENT_EDIT_REFUSED: &str = "agent_edit_refused";
    pub const OWNER_ONLY: &str = "owner_only";
    pub const POLICY_VALUE_INVALID: &str = "policy_value_invalid";
    pub const POLICY_LOCKED: &str = "policy_locked";
    pub const CRITERION_HAS_CHECKS: &str = "criterion_has_checks";
    pub const CHECKS_FULL: &str = "checks_full";
    pub const CHECK_EXISTS: &str = "check_exists";
    pub const CHECK_UNKNOWN: &str = "check_unknown";
    pub const LINK_KIND_UNKNOWN: &str = "link_kind_unknown";
    pub const NOTIFY_TOO_WIDE: &str = "notify_too_wide";
    pub const CRITERION_NEEDS_CHECK: &str = "criterion_needs_check";
    pub const EVIDENCE_NOT_FOR_AGENTS: &str = "evidence_not_for_agents";
    pub const EXPORT_PATH_REFUSED: &str = "export_path_refused";
    pub const EXPORT_TOO_LARGE: &str = "export_too_large";
    pub const IMPORT_PATH_REFUSED: &str = "import_path_refused";
    pub const IMPORT_TOO_LARGE: &str = "import_too_large";
    pub const IMPORT_NOT_TEXT: &str = "import_not_text";
    pub const IMPORT_FORMAT_UNKNOWN: &str = "import_format_unknown";
    pub const IMPORT_ITEM_UNKNOWN: &str = "import_item_unknown";
    pub const IMPORT_ITEM_NEEDS_VERSION: &str = "import_item_needs_version";
    pub const IMPORT_KEYS_COLLIDE: &str = "import_keys_collide";
    pub const VERSIONS_FULL: &str = "versions_full";
    pub const ALREADY_CLAIMED: &str = "already_claimed";
    pub const NOT_CLAIMANT: &str = "not_claimant";
    pub const DEPENDENCY_INVALID: &str = "dependency_invalid";
    pub const DEPENDENCY_EXISTS: &str = "dependency_exists";
    pub const DEPENDENCY_UNKNOWN: &str = "dependency_unknown";
    pub const DEPENDENCY_CYCLE: &str = "dependency_cycle";
    pub const DEPENDENCIES_FULL: &str = "dependencies_full";
    pub const PARENT_INVALID: &str = "parent_invalid";
    pub const VERSION_UNKNOWN: &str = "version_unknown";
    pub const CYCLE_UNKNOWN: &str = "cycle_unknown";
    pub const CYCLE_CLOSED: &str = "cycle_closed";
    pub const CYCLES_FULL: &str = "cycles_full";
    pub const CYCLE_OVERLAPS: &str = "cycle_overlaps";

    pub const ACTIVE_PLAN_EXISTS: &str = "active_plan_exists";
    pub const CASES_FULL: &str = "cases_full";
    pub const CASE_LINKS_FULL: &str = "case_links_full";
    pub const CASE_LINK_EXISTS: &str = "case_link_exists";
    pub const CASE_LINK_UNKNOWN: &str = "case_link_unknown";
    pub const CASE_PROTECTED: &str = "case_protected";
    pub const CASE_UNKNOWN: &str = "case_unknown";
    pub const CROSS_PROJECT: &str = "cross_project";
    pub const OPTION_NEEDS_HOST: &str = "option_needs_host";
    pub const PLANS_FULL: &str = "plans_full";
    pub const PLAN_CLOSED: &str = "plan_closed";
    pub const PLAN_ITEMS_FULL: &str = "plan_items_full";
    pub const PLAN_ITEM_EXISTS: &str = "plan_item_exists";
    pub const PLAN_ITEM_UNKNOWN: &str = "plan_item_unknown";
    pub const PLAN_UNKNOWN: &str = "plan_unknown";
    pub const RELEASE_UNKNOWN: &str = "release_unknown";
    pub const RESULTS_EMPTY: &str = "results_empty";
    pub const RESULTS_TOO_MANY: &str = "results_too_many";
    pub const RESULT_NAME_REPEATED: &str = "result_name_repeated";
    pub const RUNS_FULL: &str = "runs_full";
    pub const RUN_BATCH_INVALID: &str = "run_batch_invalid";
    pub const RUN_CLOSED: &str = "run_closed";
    pub const RUN_FINISH_INVALID: &str = "run_finish_invalid";
    pub const RUN_INTEGRITY_INVALID: &str = "run_integrity_invalid";
    pub const RUN_POLICY_INVALID: &str = "run_policy_invalid";
    pub const RUN_RESULTS_FULL: &str = "run_results_full";
    pub const RUN_SOURCE_INVALID: &str = "run_source_invalid";
    pub const RUN_UNKNOWN: &str = "run_unknown";
    pub const RUN_WRITER_REFUSED: &str = "run_writer_refused";
    pub const SETS_FULL: &str = "sets_full";
    pub const SET_CASES_FULL: &str = "set_cases_full";
    pub const SET_CASE_EXISTS: &str = "set_case_exists";
    pub const SET_CASE_UNKNOWN: &str = "set_case_unknown";
    pub const SET_UNKNOWN: &str = "set_unknown";
    pub const STABLE_NAME_TAKEN: &str = "stable_name_taken";
    pub const STEPS_FULL: &str = "steps_full";
    pub const TEST_ID_INVALID: &str = "test_id_invalid";

    /// Every code above, so a host can map each core refusal to its own vocabulary.
    pub const ALL: &[&str] = &[
        TEXT_EMPTY,
        TEXT_TOO_LONG,
        TEXT_LINE_BREAK,
        TEXT_HIDDEN_CHARACTERS,
        PROJECT_KEY_INVALID,
        PROJECT_EXISTS,
        PROJECT_UNKNOWN,
        SHORT_ID_INVALID,
        TICKET_UNKNOWN,
        ACTOR_INVALID,
        PRIORITY_INVALID,
        CYCLE_DAYS_INVALID,
        STALE_VERSION,
        STATUS_CLOSED_AT_CREATE,
        CLOSE_REASON_OPEN,
        TICKETS_FULL,
        CRITERIA_FULL,
        CRITERION_UNKNOWN,
        LINKS_FULL,
        LINK_EXISTS,
        LINK_UNKNOWN,
        STORE_PATH_REFUSED,
        STORE_NOT_PRIVATE,
        STORE_SCHEMA_NEWER,
        STORE_FAILED,
        UNKNOWN_OP,
        ARGS_INVALID,
        UNKNOWN_FIELD,
        MISSING_FIELD,
        FIELD_INVALID,
        REQUEST_TOO_LARGE,
        REQUEST_ID_REUSED,
        CURSOR_INVALID,
        OP_UNAVAILABLE,
        USE_CLOSE,
        RATE_LIMITED,
        OPEN_TICKETS_FULL,
        TICKET_CLOSED,
        TICKET_OPEN,
        STATUS_INVALID,
        CLOSE_NEEDS_EVIDENCE,
        CLOSE_OWNER_ONLY,
        CREATE_OWNER_ONLY,
        AGENT_EDIT_REFUSED,
        OWNER_ONLY,
        POLICY_VALUE_INVALID,
        POLICY_LOCKED,
        CRITERION_HAS_CHECKS,
        CHECKS_FULL,
        CHECK_EXISTS,
        CHECK_UNKNOWN,
        LINK_KIND_UNKNOWN,
        NOTIFY_TOO_WIDE,
        CRITERION_NEEDS_CHECK,
        EVIDENCE_NOT_FOR_AGENTS,
        EXPORT_PATH_REFUSED,
        EXPORT_TOO_LARGE,
        IMPORT_PATH_REFUSED,
        IMPORT_TOO_LARGE,
        IMPORT_NOT_TEXT,
        IMPORT_FORMAT_UNKNOWN,
        IMPORT_ITEM_UNKNOWN,
        IMPORT_ITEM_NEEDS_VERSION,
        IMPORT_KEYS_COLLIDE,
        VERSIONS_FULL,
        ALREADY_CLAIMED,
        NOT_CLAIMANT,
        DEPENDENCY_INVALID,
        DEPENDENCY_EXISTS,
        DEPENDENCY_UNKNOWN,
        DEPENDENCY_CYCLE,
        DEPENDENCIES_FULL,
        PARENT_INVALID,
        VERSION_UNKNOWN,
        CYCLE_UNKNOWN,
        CYCLE_CLOSED,
        CYCLES_FULL,
        CYCLE_OVERLAPS,
        ACTIVE_PLAN_EXISTS,
        CASES_FULL,
        CASE_LINKS_FULL,
        CASE_LINK_EXISTS,
        CASE_LINK_UNKNOWN,
        CASE_PROTECTED,
        CASE_UNKNOWN,
        CROSS_PROJECT,
        OPTION_NEEDS_HOST,
        PLANS_FULL,
        PLAN_CLOSED,
        PLAN_ITEMS_FULL,
        PLAN_ITEM_EXISTS,
        PLAN_ITEM_UNKNOWN,
        PLAN_UNKNOWN,
        RELEASE_UNKNOWN,
        RESULTS_EMPTY,
        RESULTS_TOO_MANY,
        RESULT_NAME_REPEATED,
        RUNS_FULL,
        RUN_BATCH_INVALID,
        RUN_CLOSED,
        RUN_FINISH_INVALID,
        RUN_INTEGRITY_INVALID,
        RUN_POLICY_INVALID,
        RUN_RESULTS_FULL,
        RUN_SOURCE_INVALID,
        RUN_UNKNOWN,
        RUN_WRITER_REFUSED,
        SETS_FULL,
        SET_CASES_FULL,
        SET_CASE_EXISTS,
        SET_CASE_UNKNOWN,
        SET_UNKNOWN,
        STABLE_NAME_TAKEN,
        STEPS_FULL,
        TEST_ID_INVALID,
    ];
}

pub const ACTOR_BYTES_MAX: usize = 128;
pub const CRITERIA_PER_TICKET_MAX: usize = 32;
pub const CHECKS_PER_CRITERION_MAX: usize = 8;
pub const LINKS_PER_TICKET_MAX: usize = 256;
pub const DEPENDENCIES_PER_TICKET_MAX: usize = 64;
pub const TICKETS_PER_PROJECT_MAX: usize = 10_000;
pub const VERSIONS_PER_PROJECT_MAX: usize = 64;
pub const CYCLES_PER_PROJECT_MAX: usize = 520;
pub const WRITES_PER_ACTOR_MINUTE_MAX: usize = 30;
pub const OPEN_TICKETS_CREATED_PER_ACTOR_MAX: usize = 50;
pub const REQUEST_BYTES_MAX: usize = 16 * 1024;
pub const REQUEST_ID_BYTES_MAX: usize = 64;
pub const HOST_CODE_BYTES_MAX: usize = 64;

/// A plain refusal the core, a host or a caller acts on. Fields: a stable code from a fixed
/// vocabulary (`codes` for the core's own), and one line saying why, safe to show an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    /// Builds one of the core's own refusals. Inputs: a code from `codes::ALL` and the
    /// message. Output: the refusal. A code outside the core vocabulary panics; a host
    /// refusal goes through `Refusal::host`.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        assert!(
            codes::ALL.contains(&code),
            "{code:?} is not a core code; a host refusal uses Refusal::host"
        );
        Self {
            code,
            message: message.into(),
        }
    }

    /// Builds a host's refusal. Inputs: the host's code, 1 to `HOST_CODE_BYTES_MAX` bytes of
    /// lowercase ASCII letters and underscores and never a core code, and the message.
    /// Output: the refusal. A code of another shape panics.
    pub fn host(code: &'static str, message: impl Into<String>) -> Self {
        assert!(!code.is_empty(), "a host code is not empty");
        assert!(
            code.len() <= HOST_CODE_BYTES_MAX,
            "host code {code:?} is over {HOST_CODE_BYTES_MAX} bytes"
        );
        assert!(
            code.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
            "host code {code:?} is lowercase ASCII letters and underscores"
        );
        assert!(
            !codes::ALL.contains(&code),
            "{code:?} is a core code; a host names its own"
        );
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Refusal {}

/// Declares a closed set of names stored in SQL and sent on the wire, with one text per
/// variant used by serde, by `as_str` and by `parse`, so the three can never disagree.
macro_rules! named_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The stored and wire name. Output: the name.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// Reads a stored or wire name. Input: the text. Output: the value, or None
            /// for any other text.
            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub(crate) use named_enum;

named_enum! {
    /// What a ticket is: new work, a defect, upkeep, or a time-boxed investigation.
    TicketKind { Feature => "feature", Bug => "bug", Chore => "chore", Spike => "spike" }
}

named_enum! {
    /// Where a ticket is in its life. `done` and `cancelled` are the closed states.
    TicketStatus {
        Triage => "triage",
        Backlog => "backlog",
        Todo => "todo",
        InProgress => "in_progress",
        InReview => "in_review",
        Done => "done",
        Cancelled => "cancelled",
    }
}

impl TicketStatus {
    /// Tells whether the status is a closed one. Output: true for done and cancelled.
    pub fn is_closed(self) -> bool {
        matches!(self, TicketStatus::Done | TicketStatus::Cancelled)
    }
}

named_enum! {
    /// Where a product version is: planned, being worked on, or shipped.
    VersionStatus { Planned => "planned", Active => "active", Released => "released" }
}

named_enum! {
    /// Where a cycle is: not started, running, or finished.
    CycleStatus { Upcoming => "upcoming", Active => "active", Closed => "closed" }
}

named_enum! {
    /// Whether an acceptance criterion still needs proof, is proven, or was waived.
    CriterionState { Open => "open", Met => "met", Waived => "waived" }
}

named_enum! {
    /// What proves a criterion: a test, a change set, a person's check, or a CI run.
    CheckKind { Test => "test", Change => "change", Manual => "manual", Ci => "ci" }
}

named_enum! {
    /// The outcome one evidence record reports for a check.
    EvidenceResult { Pass => "pass", Fail => "fail" }
}

named_enum! {
    /// Policy `close`: who may close a ticket. `evidence` lets the core close it once every
    /// criterion is met and every check is satisfied.
    ClosePolicy { Agents => "agents", Evidence => "evidence", Owner => "owner" }
}

named_enum! {
    /// Policy `create`: whether new tickets start in the backlog, in triage, or only the
    /// owner may create them.
    CreatePolicy { Direct => "direct", Triage => "triage", OwnerOnly => "owner_only" }
}

named_enum! {
    /// Policy `agent_edit`: what an agent may change on a ticket it did not create.
    AgentEditPolicy { Any => "any", StatusAndCriteria => "status_and_criteria", NoEdits => "none" }
}

named_enum! {
    /// Policy `rollover`: what happens to unfinished work when a cycle closes.
    RolloverPolicy { Auto => "auto", Ask => "ask", Off => "off" }
}

named_enum! {
    /// Policy `import`: whether a host's roadmap is imported once on its own or only on
    /// request.
    ImportPolicy { AutoOnce => "auto_once", OnRequest => "on_request" }
}

named_enum! {
    /// The policy keys a project stores, one per policy setting.
    PolicyKey {
        Close => "close",
        Create => "create",
        AgentEdit => "agent_edit",
        CycleDays => "cycle_days",
        Rollover => "rollover",
        Import => "import",
        TestEvidence => "test_evidence",
        SkippedCounts => "skipped_counts",
        CaseProtectedVisibility => "case_protected_visibility",
        RunWriters => "run_writers",
        RunRetention => "run_retention",
    }
}

named_enum! {
    /// Every operation of the catalog, by its wire name. Hosts receive it in
    /// `Authority::actor_may`; the operation set maps each to its input and output.
    OpKind {
        Ready => "ready",
        List => "list",
        Show => "show",
        History => "history",
        Roadmap => "roadmap",
        PolicyGet => "policy_get",
        Export => "export",
        Schema => "schema",
        Create => "create",
        Edit => "edit",
        Move => "move",
        Claim => "claim",
        Release => "release",
        Depend => "depend",
        Undepend => "undepend",
        CriterionAdd => "criterion_add",
        CriterionEdit => "criterion_edit",
        CheckAdd => "check_add",
        EvidenceRecord => "evidence_record",
        CriterionSet => "criterion_set",
        Link => "link",
        Unlink => "unlink",
        Close => "close",
        Reopen => "reopen",
        VersionUpsert => "version_upsert",
        CycleStart => "cycle_start",
        CycleClose => "cycle_close",
        ImportPreview => "import_preview",
        ImportApply => "import_apply",
        PolicySet => "policy_set",
        CaseList => "case_list",
        CaseShow => "case_show",
        SetShow => "set_show",
        PlanList => "plan_list",
        PlanShow => "plan_show",
        RunList => "run_list",
        RunShow => "run_show",
        Coverage => "coverage",
        CaseCreate => "case_create",
        CaseEdit => "case_edit",
        CaseLink => "case_link",
        CaseUnlink => "case_unlink",
        SetUpsert => "set_upsert",
        SetAdd => "set_add",
        SetRemove => "set_remove",
        PlanUpsert => "plan_upsert",
        PlanAdd => "plan_add",
        PlanRemove => "plan_remove",
        RunStart => "run_start",
        RunRecord => "run_record",
        RunFinish => "run_finish",
        RunCancel => "run_cancel",
    }
}

impl OpKind {
    /// Tells whether the operation is one of the contract's writes. Output: false for the
    /// nine ticket reads (ready, list, show, history, roadmap, policy_get, export,
    /// import_preview, schema) and the eight test reads (`is_test_read`).
    pub fn is_write(self) -> bool {
        !self.is_test_read()
            && !matches!(
                self,
                OpKind::Ready
                    | OpKind::List
                    | OpKind::Show
                    | OpKind::History
                    | OpKind::Roadmap
                    | OpKind::PolicyGet
                    | OpKind::Export
                    | OpKind::ImportPreview
                    | OpKind::Schema
            )
    }

    /// Tells whether the operation reads the test model: cases, sets, plans, runs or
    /// coverage. Output: true for the eight test reads.
    pub fn is_test_read(self) -> bool {
        matches!(
            self,
            OpKind::CaseList
                | OpKind::CaseShow
                | OpKind::SetShow
                | OpKind::PlanList
                | OpKind::PlanShow
                | OpKind::RunList
                | OpKind::RunShow
                | OpKind::Coverage
        )
    }

    /// Tells whether the operation writes the test model. Output: true for the fourteen
    /// case, set, plan and run writes.
    pub fn is_test_write(self) -> bool {
        matches!(
            self,
            OpKind::CaseCreate
                | OpKind::CaseEdit
                | OpKind::CaseLink
                | OpKind::CaseUnlink
                | OpKind::SetUpsert
                | OpKind::SetAdd
                | OpKind::SetRemove
                | OpKind::PlanUpsert
                | OpKind::PlanAdd
                | OpKind::PlanRemove
                | OpKind::RunStart
                | OpKind::RunRecord
                | OpKind::RunFinish
                | OpKind::RunCancel
        )
    }
}

/// A project's key, the prefix of its short ticket ids: 2 to 8 letters A to Z, unique per
/// database.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectKey(String);

impl ProjectKey {
    /// Checks a key. Input: the text. Output: the key, or `project_key_invalid`.
    pub fn new(text: &str) -> Result<Self, Refusal> {
        if (2..=8).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_uppercase()) {
            Ok(Self(text.to_owned()))
        } else {
            Err(Refusal::new(
                codes::PROJECT_KEY_INVALID,
                "a project key is 2 to 8 uppercase letters A to Z",
            ))
        }
    }

    /// The key's text. Output: the text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProjectKey {
    type Error = Refusal;

    fn try_from(text: String) -> Result<Self, Refusal> {
        Self::new(&text)
    }
}

impl From<ProjectKey> for String {
    fn from(key: ProjectKey) -> String {
        key.0
    }
}

impl fmt::Display for ProjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A ticket's stable short id, `<KEY>-<number>` (for example `CA-42`). The number counts
/// from 1 per project and is never reused; internal row ids never leave the core.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ShortId {
    key: ProjectKey,
    number: u32,
}

impl ShortId {
    /// Builds an id. Inputs: the project key and the ticket number. Output: the id, or
    /// `short_id_invalid` for number 0.
    pub fn new(key: ProjectKey, number: u32) -> Result<Self, Refusal> {
        if number == 0 {
            return Err(short_id_refused());
        }
        Ok(Self { key, number })
    }

    /// Reads `<KEY>-<number>`, the number in decimal without a leading zero. Input: the
    /// text. Output: the id, or `short_id_invalid`.
    pub fn parse(text: &str) -> Result<Self, Refusal> {
        let (key, number) = text.split_once('-').ok_or_else(short_id_refused)?;
        let key = ProjectKey::new(key).map_err(|_| short_id_refused())?;
        if number.is_empty()
            || number.len() > 10
            || number.starts_with('0')
            || !number.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(short_id_refused());
        }
        let number = number.parse::<u32>().map_err(|_| short_id_refused())?;
        Self::new(key, number)
    }

    /// The project key. Output: the key.
    pub fn key(&self) -> &ProjectKey {
        &self.key
    }

    /// The ticket number within its project. Output: the number, 1 or more.
    pub fn number(&self) -> u32 {
        self.number
    }
}

fn short_id_refused() -> Refusal {
    Refusal::new(
        codes::SHORT_ID_INVALID,
        "a ticket id is <KEY>-<number>, for example CA-42",
    )
}

impl TryFrom<String> for ShortId {
    type Error = Refusal;

    fn try_from(text: String) -> Result<Self, Refusal> {
        Self::parse(&text)
    }
}

impl From<ShortId> for String {
    fn from(id: ShortId) -> String {
        id.to_string()
    }
}

impl fmt::Display for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.key, self.number)
    }
}

/// Who acts: a host-set identity (`owner`, `master:<session>`, `agent:<team id>`) or a
/// standalone self-declared name. 1 to 128 bytes of letters, digits and `: _ . - @ /`.
/// The `host:` prefix belongs to the core's own writes (for example `host:evidence`), so
/// `new`, `TryFrom` and deserializing refuse it; only the core's stored rows read it back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Actor(String);

impl Actor {
    /// Checks a caller's identity. Input: the text. Output: the actor, or `actor_invalid`
    /// for a bad shape or the reserved `host:` prefix.
    pub fn new(text: &str) -> Result<Self, Refusal> {
        let actor = Self::stored(text)?;
        if actor.is_host() {
            return Err(Refusal::new(
                codes::ACTOR_INVALID,
                "the host: prefix is reserved for the tracker's own writes",
            ));
        }
        Ok(actor)
    }

    /// The core's own identity for a write it makes itself. Input: what caused it, for
    /// example `evidence`. Output: the actor `host:<source>`.
    pub fn host(source: &'static str) -> Self {
        assert!(!source.is_empty() && source.len() + 5 <= ACTOR_BYTES_MAX);
        assert!(source.bytes().all(actor_byte));
        Self(format!("host:{source}"))
    }

    /// Tells whether the core made this write itself. Output: true for `host:` actors.
    pub fn is_host(&self) -> bool {
        self.0.starts_with("host:")
    }

    /// The actor's text. Output: the text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reads an actor the core stored itself, its own `host:` actors included. Input: the
    /// stored text. Output: the actor, or `actor_invalid` for a bad shape.
    pub(crate) fn stored(text: &str) -> Result<Self, Refusal> {
        if text.is_empty() || text.len() > ACTOR_BYTES_MAX || !text.bytes().all(actor_byte) {
            return Err(Refusal::new(
                codes::ACTOR_INVALID,
                "an actor is 1 to 128 bytes of letters, digits and : _ . - @ /",
            ));
        }
        Ok(Self(text.to_owned()))
    }
}

fn stored_actor<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Actor, D::Error> {
    Actor::stored(&String::deserialize(deserializer)?).map_err(de::Error::custom)
}

fn stored_actor_option<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Actor>, D::Error> {
    Option::<String>::deserialize(deserializer)?
        .map(|text| Actor::stored(&text))
        .transpose()
        .map_err(de::Error::custom)
}

fn actor_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'-' | b'@' | b'/')
}

impl TryFrom<String> for Actor {
    type Error = Refusal;

    fn try_from(text: String) -> Result<Self, Refusal> {
        Self::new(&text)
    }
}

impl From<Actor> for String {
    fn from(actor: Actor) -> String {
        actor.0
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A ticket's priority, 0 to 4 as the contract defines; the ready queue decides the order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Priority(u8);

impl Priority {
    /// Checks a priority. Input: the value. Output: the priority, or `priority_invalid`.
    pub fn new(value: u8) -> Result<Self, Refusal> {
        if value <= 4 {
            Ok(Self(value))
        } else {
            Err(Refusal::new(
                codes::PRIORITY_INVALID,
                "a priority is 0 to 4",
            ))
        }
    }

    /// The value. Output: 0 to 4.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Priority {
    type Error = Refusal;

    fn try_from(value: u8) -> Result<Self, Refusal> {
        Self::new(value)
    }
}

impl From<Priority> for u8 {
    fn from(priority: Priority) -> u8 {
        priority.0
    }
}

/// Policy `cycle_days`: a cycle's length, one of the presets 7, 14, 21 and 28 or any
/// custom length from 1 to 90 days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct CycleDays(u8);

impl CycleDays {
    pub const PRESETS: [u8; 4] = [7, 14, 21, 28];

    /// Checks a length. Input: days. Output: the length, or `cycle_days_invalid`.
    pub fn new(days: u8) -> Result<Self, Refusal> {
        if (1..=90).contains(&days) {
            Ok(Self(days))
        } else {
            Err(Refusal::new(
                codes::CYCLE_DAYS_INVALID,
                "a cycle is 1 to 90 days",
            ))
        }
    }

    /// The length. Output: 1 to 90 days.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for CycleDays {
    type Error = Refusal;

    fn try_from(days: u8) -> Result<Self, Refusal> {
        Self::new(days)
    }
}

impl From<CycleDays> for u8 {
    fn from(days: CycleDays) -> u8 {
        days.0
    }
}

/// One project's policy, every key set. `Default` is the proposed default of each key;
/// every other option stays selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub close: ClosePolicy,
    pub create: CreatePolicy,
    pub agent_edit: AgentEditPolicy,
    pub cycle_days: CycleDays,
    pub rollover: RolloverPolicy,
    pub import: ImportPolicy,
    pub test_evidence: TestEvidence,
    pub skipped_counts: SkippedCounts,
    pub case_protected_visibility: CaseProtectedVisibility,
    pub run_writers: RunWriters,
    pub run_retention: RunRetention,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            close: ClosePolicy::Evidence,
            create: CreatePolicy::Triage,
            agent_edit: AgentEditPolicy::StatusAndCriteria,
            cycle_days: CycleDays(14),
            rollover: RolloverPolicy::Auto,
            import: ImportPolicy::AutoOnce,
            test_evidence: TestEvidence::Latest,
            skipped_counts: SkippedCounts::Fail,
            case_protected_visibility: CaseProtectedVisibility::Hidden,
            run_writers: RunWriters::Any,
            run_retention: RunRetention::PruneCount,
        }
    }
}

/// A `tk_projects` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub(crate) id: i64,
    pub key: ProjectKey,
    pub name: String,
    pub created_ms: i64,
    pub version: i64,
}

/// A `tk_versions` row: one product version of the roadmap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseVersion {
    pub(crate) id: i64,
    pub(crate) project: i64,
    pub name: String,
    pub position: u32,
    pub target_day: Option<String>,
    pub status: VersionStatus,
    pub source: String,
    pub version: i64,
}

/// A `tk_cycles` row: one time box of work, optionally inside a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cycle {
    pub(crate) id: i64,
    pub(crate) project: i64,
    pub(crate) version_id: Option<i64>,
    pub number: u32,
    pub starts_day: String,
    pub ends_day: String,
    pub status: CycleStatus,
    pub version: i64,
}

/// A `tk_tickets` row. `version` counts every write, for stale-write checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub(crate) id: i64,
    pub(crate) project: i64,
    pub short_id: ShortId,
    pub title: String,
    pub body: String,
    pub kind: TicketKind,
    pub status: TicketStatus,
    pub priority: Priority,
    pub(crate) version_id: Option<i64>,
    pub(crate) cycle_id: Option<i64>,
    pub(crate) parent_id: Option<i64>,
    #[serde(deserialize_with = "stored_actor_option")]
    pub assignee: Option<Actor>,
    #[serde(deserialize_with = "stored_actor")]
    pub created_by: Actor,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub closed_ms: Option<i64>,
    #[serde(deserialize_with = "stored_actor_option")]
    pub closed_by: Option<Actor>,
    pub close_reason: Option<String>,
    pub version: i64,
}

/// A `tk_criteria` row: one acceptance criterion of a ticket, at its position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub(crate) id: i64,
    pub(crate) ticket: i64,
    pub position: u32,
    pub text: String,
    pub state: CriterionState,
    pub version: i64,
}

/// A `tk_checks` row: one item that proves a criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub(crate) id: i64,
    pub(crate) criterion: i64,
    pub kind: CheckKind,
    pub reference: String,
}

/// A `tk_evidence` row, append-only: one recorded outcome of a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub(crate) id: i64,
    pub(crate) check: i64,
    pub result: EvidenceResult,
    pub reference: String,
    #[serde(deserialize_with = "stored_actor")]
    pub recorded_by: Actor,
    pub recorded_ms: i64,
}

/// A `tk_links` row: a ticket's link of a host-declared kind to a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub(crate) ticket: i64,
    pub kind: String,
    pub target: String,
    #[serde(deserialize_with = "stored_actor")]
    pub created_by: Actor,
    pub created_ms: i64,
}

/// A `tk_events` row, append-only: one state change, with the changed row before and
/// after it as JSON (none before a creation, none after a removal).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub(crate) id: i64,
    pub(crate) ticket: Option<i64>,
    pub(crate) project: i64,
    pub at_ms: i64,
    pub actor: Actor,
    pub op: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
}
