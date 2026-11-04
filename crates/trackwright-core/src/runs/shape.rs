use super::codes;
use crate::model::{Refusal, codes as core_codes, named_enum};
use crate::tests_model::{
    CaseKind, INTEGRITY_JSON_BYTES_MAX, INTEGRITY_LIST_ITEMS_MAX, POLICY_ARGV_ITEM_BYTES_MAX,
    POLICY_ARGV_ITEMS_MAX, POLICY_JSON_BYTES_MAX, RESULT_NAME_BYTES_MAX, RUN_BATCH_BYTES_MAX,
    SOURCE_DECLARED_BYTES_MAX, SOURCE_REPOSITORY_BYTES_MAX, TOOLCHAIN_VERSION_BYTES_MAX, TestId,
    TestObject,
};
use crate::text;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const SOURCE_JSON_BYTES_MAX: usize = 2 * 1024;
pub const CHECK_ID_BYTES_MAX: usize = 64;
pub const TIMEOUT_S_MAX: u32 = 7_200;
pub const INTEGRITY_PATH_BYTES_MAX: usize = 512;
pub const SKIP_MARKER_BYTES_MAX: usize = 64;

const COMMIT_HEX_LEN: usize = 40;
const DIGEST_HEX_LEN: usize = 64;

const _: () = assert!(SOURCE_DECLARED_BYTES_MAX < SOURCE_JSON_BYTES_MAX);
const _: () = assert!(POLICY_ARGV_ITEM_BYTES_MAX < POLICY_JSON_BYTES_MAX);
const _: () = assert!(CHECK_ID_BYTES_MAX == RUN_BATCH_BYTES_MAX);

named_enum! {
    /// How the host says a run ended: its report was read, its build failed before any
    /// test ran, its report could not be read or did not match, or it never started.
    RunEnd {
        Completed => "completed",
        BuildFailed => "build_failed",
        ReportUnreadable => "report_unreadable",
        NotStarted => "not_started",
    }
}

named_enum! {
    /// Why a run was interrupted, stored as the run's reason.
    StopReason { Cancelled => "cancelled", TimedOut => "timed_out" }
}

named_enum! {
    /// The toolchain family a check ran with.
    ToolchainFamily { Rust => "rust", Node => "node", Python => "python", None => "none" }
}

named_enum! {
    /// The network a check had: none, loopback only, or the project's sandbox remotes.
    NetworkMode { Off => "off", Localhost => "localhost", Remotes => "remotes" }
}

/// What source a run covered: a git commit and tree with its base and change set, or a
/// reference a standalone caller declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceRef {
    Git {
        repository: String,
        commit: String,
        tree: String,
        base: Option<String>,
        change_set: Option<String>,
    },
    Declared {
        #[serde(rename = "ref")]
        reference: String,
    },
}

impl SourceRef {
    /// Reads and checks a source shape. Input: the JSON text. Output: the source, or
    /// `run_source_invalid` naming what is wrong.
    pub fn parse(json: &str) -> Result<Self, Refusal> {
        let source: Self = parse_shape(json, SOURCE_JSON_BYTES_MAX, codes::RUN_SOURCE_INVALID)?;
        source.check()?;
        Ok(source)
    }

    /// Checks the shape's bounds: a one-line repository label, 40 lowercase hex commit,
    /// tree and base, and bounded references. Output: Ok, or `run_source_invalid`.
    pub fn check(&self) -> Result<(), Refusal> {
        let code = codes::RUN_SOURCE_INVALID;
        match self {
            SourceRef::Git {
                repository,
                commit,
                tree,
                base,
                change_set,
            } => {
                shape_text(
                    code,
                    "source repository",
                    repository,
                    SOURCE_REPOSITORY_BYTES_MAX,
                )?;
                shape_hex(code, "source commit", commit, COMMIT_HEX_LEN)?;
                shape_hex(code, "source tree", tree, COMMIT_HEX_LEN)?;
                if let Some(base) = base {
                    shape_hex(code, "source base", base, COMMIT_HEX_LEN)?;
                }
                if let Some(change_set) = change_set {
                    shape_text(
                        code,
                        "source change set",
                        change_set,
                        SOURCE_DECLARED_BYTES_MAX,
                    )?;
                }
            }
            SourceRef::Declared { reference } => {
                shape_text(code, "source ref", reference, SOURCE_DECLARED_BYTES_MAX)?;
            }
        }
        shape_bytes(code, "source", self, SOURCE_JSON_BYTES_MAX)
    }
}

/// Tells whether a text is 40 lowercase hex digits, a git commit or tree id. Input: the
/// text. Output: the answer.
pub fn is_commit(value: &str) -> bool {
    hex_of(value, COMMIT_HEX_LEN)
}

/// The toolchain a check ran with: its family and the version read inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toolchain {
    pub family: ToolchainFamily,
    pub version: String,
}

/// Which policy a run covered: the approved check profile and when it was approved, the
/// check, its kind and argv, the toolchain, the sandbox profile digest, the network, the
/// timeout (0 for none, as for a manual run) and the environment digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRef {
    pub profile: String,
    pub profile_approved_ms: i64,
    pub check: String,
    pub kind: CaseKind,
    pub argv: Vec<String>,
    pub toolchain: Toolchain,
    pub sandbox: String,
    pub network: NetworkMode,
    pub timeout_s: u32,
    pub environment: String,
}

impl PolicyRef {
    /// Reads and checks a policy shape. Input: the JSON text. Output: the policy, or
    /// `run_policy_invalid` naming what is wrong.
    pub fn parse(json: &str) -> Result<Self, Refusal> {
        let policy: Self = parse_shape(json, POLICY_JSON_BYTES_MAX, codes::RUN_POLICY_INVALID)?;
        policy.check()?;
        Ok(policy)
    }

    /// Checks the shape's bounds: digests are 64 lowercase hex, the check id is a short
    /// name, argv holds at most 32 one-line items, the timeout is at most 7200 s and the
    /// whole shape is at most 4 KiB. Output: Ok, or `run_policy_invalid`.
    pub fn check(&self) -> Result<(), Refusal> {
        let code = codes::RUN_POLICY_INVALID;
        shape_hex(code, "policy profile", &self.profile, DIGEST_HEX_LEN)?;
        shape_hex(code, "policy sandbox", &self.sandbox, DIGEST_HEX_LEN)?;
        shape_hex(
            code,
            "policy environment",
            &self.environment,
            DIGEST_HEX_LEN,
        )?;
        if self.profile_approved_ms < 0 {
            return Err(Refusal::new(
                code,
                "the policy profile approval time is negative",
            ));
        }
        if !batch_label(&self.check) || self.check.contains(':') {
            return Err(Refusal::new(
                code,
                "the policy check is 1 to 64 bytes of letters, digits and _ . -",
            ));
        }
        if self.argv.len() > POLICY_ARGV_ITEMS_MAX {
            return Err(Refusal::new(
                code,
                "the policy argv holds more than 32 items",
            ));
        }
        for item in &self.argv {
            shape_line(code, "policy argv item", item, POLICY_ARGV_ITEM_BYTES_MAX)?;
        }
        let version = &self.toolchain.version;
        if self.toolchain.family != ToolchainFamily::None || !version.is_empty() {
            shape_text(
                code,
                "policy toolchain version",
                version,
                TOOLCHAIN_VERSION_BYTES_MAX,
            )?;
        }
        if self.timeout_s > TIMEOUT_S_MAX {
            return Err(Refusal::new(
                code,
                "the policy timeout is over 7200 seconds",
            ));
        }
        shape_bytes(code, "policy", self, POLICY_JSON_BYTES_MAX)
    }
}

/// The test-inventory comparison a run carries: the run compared against, the names that
/// went missing (at most 50 listed) and how many in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub baseline_run: Option<TestId>,
    pub removed: Vec<String>,
    pub removed_total: u32,
}

/// One skip marker a change added: the file, its line (from 1) and the marker text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewSkip {
    pub path: String,
    pub line: u32,
    pub marker: String,
}

/// The test-integrity evidence a host attaches when a run finishes: tests executed, the
/// inventory comparison, new skips, protected paths touched, whether holdouts ran and
/// whether the build succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrityRef {
    pub executed: u32,
    pub inventory: Inventory,
    pub new_skips: Vec<NewSkip>,
    pub protected_touched: Vec<String>,
    pub holdouts_ran: bool,
    pub build_ok: bool,
}

impl IntegrityRef {
    /// Reads and checks an integrity shape. Input: the JSON text. Output: the shape, or
    /// `run_integrity_invalid` naming what is wrong.
    pub fn parse(json: &str) -> Result<Self, Refusal> {
        let code = codes::RUN_INTEGRITY_INVALID;
        let integrity: Self = parse_shape(json, INTEGRITY_JSON_BYTES_MAX, code)?;
        integrity.check()?;
        Ok(integrity)
    }

    /// Checks the shape's bounds: every list at most 50 items, the removed total at least
    /// the listed names, skip lines from 1, a baseline that is a run id, and the whole
    /// shape at most 8 KiB. Output: Ok, or `run_integrity_invalid`.
    pub fn check(&self) -> Result<(), Refusal> {
        let code = codes::RUN_INTEGRITY_INVALID;
        let inventory = &self.inventory;
        let lists = [
            inventory.removed.len(),
            self.new_skips.len(),
            self.protected_touched.len(),
        ];
        if lists.iter().any(|len| *len > INTEGRITY_LIST_ITEMS_MAX) {
            return Err(Refusal::new(
                code,
                "an integrity list holds more than 50 items",
            ));
        }
        if (inventory.removed_total as usize) < inventory.removed.len() {
            return Err(Refusal::new(
                code,
                "the removed total is below the names listed",
            ));
        }
        if let Some(baseline) = &inventory.baseline_run
            && baseline.object() != TestObject::Run
        {
            return Err(Refusal::new(code, "the inventory baseline is not a run id"));
        }
        for name in &inventory.removed {
            shape_text(code, "removed test name", name, RESULT_NAME_BYTES_MAX)?;
        }
        for skip in &self.new_skips {
            shape_text(code, "skip path", &skip.path, INTEGRITY_PATH_BYTES_MAX)?;
            shape_text(code, "skip marker", &skip.marker, SKIP_MARKER_BYTES_MAX)?;
            if skip.line == 0 {
                return Err(Refusal::new(code, "a skip line counts from 1"));
            }
        }
        for path in &self.protected_touched {
            shape_text(code, "protected path", path, INTEGRITY_PATH_BYTES_MAX)?;
        }
        shape_bytes(code, "integrity", self, INTEGRITY_JSON_BYTES_MAX)
    }
}

/// Checks one stored text of a run or result with the core's text codes: not empty and
/// one line unless `multiline`, within its bound, and free of hidden characters. Inputs:
/// the field name, the text, its bound and whether it may be empty and span lines.
/// Output: Ok, or the refusal.
pub(crate) fn run_text(
    name: &str,
    value: &str,
    bytes_max: usize,
    multiline: bool,
) -> Result<(), Refusal> {
    if !multiline && value.trim().is_empty() {
        return Err(Refusal::new(
            core_codes::TEXT_EMPTY,
            format!("the {name} is empty"),
        ));
    }
    if value.len() > bytes_max {
        return Err(Refusal::new(
            core_codes::TEXT_TOO_LONG,
            format!("the {name} is over {bytes_max} bytes"),
        ));
    }
    if !multiline && value.contains(['\n', '\r']) {
        return Err(Refusal::new(
            core_codes::TEXT_LINE_BREAK,
            format!("the {name} is one line"),
        ));
    }
    if text::hidden(value) {
        return Err(Refusal::new(
            core_codes::TEXT_HIDDEN_CHARACTERS,
            hidden_message(name),
        ));
    }
    Ok(())
}

/// Tells whether a text is a batch label: 1 to 64 bytes of ASCII letters, digits and
/// `_ . - :`. Input: the text. Output: the answer.
pub(crate) fn batch_label(value: &str) -> bool {
    let shaped = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b':'));
    !value.is_empty() && value.len() <= RUN_BATCH_BYTES_MAX && shaped
}

fn parse_shape<T: serde::de::DeserializeOwned>(
    json: &str,
    bytes_max: usize,
    code: &'static str,
) -> Result<T, Refusal> {
    if json.len() > bytes_max {
        return Err(Refusal::new(
            code,
            format!("the shape is over {bytes_max} bytes"),
        ));
    }
    serde_json::from_str(json).map_err(|error| {
        Refusal::new(
            code,
            format!(
                "the shape has a missing, unknown or mistyped field near column {}",
                error.column()
            ),
        )
    })
}

fn shape_bytes<T: Serialize>(
    code: &'static str,
    name: &str,
    shape: &T,
    bytes_max: usize,
) -> Result<(), Refusal> {
    let bytes = serde_json::to_vec(shape).expect("a binding shape always serializes");
    assert!(!bytes.is_empty());
    if bytes.len() > bytes_max {
        return Err(Refusal::new(
            code,
            format!("the {name} is over {bytes_max} bytes"),
        ));
    }
    Ok(())
}

fn shape_hex(code: &'static str, name: &str, value: &str, len: usize) -> Result<(), Refusal> {
    if !hex_of(value, len) {
        return Err(Refusal::new(
            code,
            format!("the {name} is {len} lowercase hex digits"),
        ));
    }
    Ok(())
}

fn hex_of(value: &str, len: usize) -> bool {
    assert!(len == COMMIT_HEX_LEN || len == DIGEST_HEX_LEN);
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn shape_text(
    code: &'static str,
    name: &str,
    value: &str,
    bytes_max: usize,
) -> Result<(), Refusal> {
    if value.trim().is_empty() {
        return Err(Refusal::new(code, format!("the {name} is empty")));
    }
    shape_line(code, name, value, bytes_max)
}

fn shape_line(
    code: &'static str,
    name: &str,
    value: &str,
    bytes_max: usize,
) -> Result<(), Refusal> {
    if value.len() > bytes_max {
        return Err(Refusal::new(
            code,
            format!("the {name} is over {bytes_max} bytes"),
        ));
    }
    if value.contains(['\n', '\r']) {
        return Err(Refusal::new(code, format!("the {name} is one line")));
    }
    if text::hidden(value) {
        return Err(Refusal::new(code, hidden_message(name)));
    }
    Ok(())
}

fn hidden_message(name: &str) -> String {
    format!("the {name} holds characters a reader cannot see or that reorder what they see")
}
