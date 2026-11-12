use super::CheckRefusal;
use crate::engine::owner::{failure, is_hex_id};
use crate::memory::hidden_text;
use crate::protocol::ProjectId;
use crate::sandbox::toolchains::Toolchain;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The most bytes of one profile's canonical JSON. Holdout file bytes are stored apart and
/// named here by digest, so they never count.
pub const PROFILE_BYTES_MAX: usize = 16 * 1024;
/// The most checks one profile holds.
pub const CHECKS_MAX: usize = 16;
/// The most holdout checks one profile holds.
pub const HOLDOUTS_MAX: usize = 8;
/// The most items of one check's command.
pub const ARGV_MAX: usize = 32;
/// The most bytes of one command item.
pub const ARG_BYTES_MAX: usize = 512;
/// The shortest time limit of one check, in seconds.
pub const TIMEOUT_S_MIN: u32 = 10;
/// The longest time limit of one check, in seconds.
pub const TIMEOUT_S_MAX: u32 = 7200;
/// The most bytes of a check id.
pub const ID_BYTES_MAX: usize = 64;
/// The most bytes of one relative path in a profile.
pub const PATH_BYTES_MAX: usize = 256;
/// The most protected paths one profile names.
pub const PROTECTED_PATHS_MAX: usize = 64;
/// The most holdout files one profile names across all its holdouts.
pub const HOLDOUT_FILES_MAX: usize = 32;
/// The most bytes of one stored holdout file, so one file fits one owner request.
pub const HOLDOUT_FILE_BYTES_MAX: usize = 8 * 1024;
/// The most bytes of all holdout files one profile names.
pub const HOLDOUT_BYTES_MAX: usize = 256 * 1024;
/// The only profile version this engine reads.
pub const PROFILE_VERSION: u16 = 1;
/// The most bytes of profile JSON read before parsing, leaving room for whitespace a
/// writer adds around a profile within `PROFILE_BYTES_MAX`.
pub const PROFILE_INPUT_BYTES_MAX: usize = 2 * PROFILE_BYTES_MAX;

const _: () = assert!(HOLDOUT_FILES_MAX * HOLDOUT_FILE_BYTES_MAX <= HOLDOUT_BYTES_MAX);
const _: () = assert!(PROFILE_BYTES_MAX <= crate::protocol::FRAME_BYTES_MAX / 2);

/// A check profile's content identity: the lowercase SHA-256 hex of its canonical JSON.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileId(String);

/// What a check proves, as the trackwright case kinds name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Unit,
    Integration,
    E2e,
    Smoke,
    Scenario,
}

/// How a check reports its results: libtest text on standard output, a JUnit XML file or an
/// Allure results folder at the check's `report` path, or only its exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportFormat {
    Libtest,
    Junit,
    Allure,
    ExitCode,
}

/// The network a check may reach, narrowest first: none, loopback only, or the project's
/// sandbox remotes. The narrower of two modes is their minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Off,
    Localhost,
    Remotes,
}

/// When the engine runs a check: at an agent's turn end, before the owner's Accept, or when
/// the owner or the change set's own agent asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    TurnEnd,
    BeforeAccept,
    OnRequest,
}

/// One check of a profile: its id, kind, toolchain family (none for a check that needs no
/// managed toolchain), the command run without a shell from `cwd` inside the snapshot, how
/// it reports, its time limit, network and triggers, whether the gate requires it, and
/// whether its cases are protected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub id: String,
    pub kind: CheckKind,
    pub toolchain: Option<Toolchain>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub format: ReportFormat,
    pub report: Option<String>,
    pub timeout_s: u32,
    pub network: NetworkMode,
    pub when: Vec<Trigger>,
    pub required: bool,
    pub protected: bool,
}

/// One holdout check: the check, and the files the engine copies into the snapshot only for
/// its run, each a relative path naming the SHA-256 digest of bytes the owner stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldoutSpec {
    pub check: CheckSpec,
    pub files: BTreeMap<String, String>,
}

/// A project's check profile, version 1: its checks, its holdouts and the relative paths a
/// change set may not touch unseen (a trailing `/` names a folder).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckProfile {
    pub version: u16,
    pub checks: Vec<CheckSpec>,
    #[serde(default)]
    pub holdouts: Vec<HoldoutSpec>,
    #[serde(default)]
    pub protected_paths: Vec<String>,
}

/// The profile the owner approved as a project's current one, with the time of that approval;
/// a later approval of any profile, this one included, is a new admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedProfile {
    pub id: ProfileId,
    pub project: ProjectId,
    pub approved_ms: i64,
    pub profile: CheckProfile,
}

/// One check as admitted under the current profile: the profile and approval it binds to,
/// the check, and for a holdout its files by path and digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedCheck {
    pub profile: ProfileId,
    pub approved_ms: i64,
    pub spec: CheckSpec,
    pub holdout: Option<BTreeMap<String, String>>,
}

impl ProfileId {
    /// Checks a profile id. Input: the text. Output: the id, or `checks_profile_unknown`
    /// for anything but 64 lowercase hexadecimal characters.
    pub fn new(value: impl Into<String>) -> Result<Self, CheckRefusal> {
        let value = value.into();
        if !is_hex_id(&value) {
            return Err(failure("checks_profile_unknown"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProfileId {
    type Error = CheckRefusal;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProfileId> for String {
    fn from(id: ProfileId) -> Self {
        id.0
    }
}

impl CheckProfile {
    /// Reads an untrusted profile. Input: its JSON bytes. Output: the checked profile, or
    /// `checks_profile_invalid` for bad JSON, an unknown field or any bound or path that
    /// `validate` refuses.
    pub fn import(bytes: &[u8]) -> Result<Self, CheckRefusal> {
        if bytes.len() > PROFILE_INPUT_BYTES_MAX {
            return Err(invalid());
        }
        let profile: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        profile.validate()?;
        Ok(profile)
    }

    /// Checks every bound of the profile, never truncating. Output: Ok, or
    /// `checks_profile_invalid`.
    pub fn validate(&self) -> Result<(), CheckRefusal> {
        self.canonical().map(|_| ())
    }

    /// The canonical JSON the identity hashes and the store keeps. Output: the bytes, or
    /// `checks_profile_invalid` when the profile fails `validate`.
    pub fn canonical(&self) -> Result<Vec<u8>, CheckRefusal> {
        let mut ids = BTreeSet::new();
        let files: usize = self
            .holdouts
            .iter()
            .map(|holdout| holdout.files.len())
            .sum();
        let shaped = self.version == PROFILE_VERSION
            && (1..=CHECKS_MAX).contains(&self.checks.len())
            && self.holdouts.len() <= HOLDOUTS_MAX
            && files <= HOLDOUT_FILES_MAX
            && self.protected_paths.len() <= PROTECTED_PATHS_MAX
            && self
                .checks
                .iter()
                .chain(self.holdouts.iter().map(|holdout| &holdout.check))
                .all(|check| check.valid() && ids.insert(check.id.as_str()))
            && self.holdouts.iter().all(|holdout| {
                !holdout.files.is_empty()
                    && holdout
                        .files
                        .iter()
                        .all(|(path, digest)| safe_path(path) && is_hex_id(digest))
            })
            && self
                .protected_paths
                .iter()
                .all(|path| safe_path(path.strip_suffix('/').unwrap_or(path)));
        if !shaped {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(self).map_err(|_| invalid())?;
        if bytes.len() > PROFILE_BYTES_MAX {
            return Err(invalid());
        }
        assert_eq!(ids.len(), self.checks.len() + self.holdouts.len());
        Ok(bytes)
    }

    /// The profile's content identity. Output: the SHA-256 of `canonical`, or
    /// `checks_profile_invalid`.
    pub fn identity(&self) -> Result<ProfileId, CheckRefusal> {
        let bytes = self.canonical()?;
        Ok(ProfileId::new(format!("{:x}", Sha256::digest(&bytes)))
            .expect("a SHA-256 hex digest is a profile id"))
    }
}

impl CheckSpec {
    /// Whether every field of one check is within its bounds: a plain id, a bare program
    /// name and plain command items, a safe `cwd`, a report path exactly for the formats that
    /// write one, a time limit in range and distinct triggers. Output: true when valid.
    fn valid(&self) -> bool {
        let reported = match self.format {
            ReportFormat::Junit | ReportFormat::Allure => {
                self.report.as_deref().is_some_and(safe_path)
            }
            ReportFormat::Libtest | ReportFormat::ExitCode => self.report.is_none(),
        };
        let triggers: BTreeSet<_> = self.when.iter().collect();
        let id = &self.id;
        !id.is_empty()
            && id.len() <= ID_BYTES_MAX
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            && self
                .argv
                .first()
                .is_some_and(|program| program.len() <= ID_BYTES_MAX && component(program))
            && self.argv.len() <= ARGV_MAX
            && self.argv.iter().all(|item| {
                item.len() <= ARG_BYTES_MAX
                    && !item.chars().any(char::is_control)
                    && !hidden_text(item)
            })
            && (self.cwd == "." || safe_path(&self.cwd))
            && reported
            && (TIMEOUT_S_MIN..=TIMEOUT_S_MAX).contains(&self.timeout_s)
            && !self.when.is_empty()
            && triggers.len() == self.when.len()
    }
}

impl AdmittedProfile {
    /// Every check of the profile as admitted, ordinary checks first and then holdouts, in
    /// profile order. Output: the admitted checks.
    pub fn checks(&self) -> Vec<AdmittedCheck> {
        let ordinary = self.profile.checks.iter().map(|spec| (spec, None));
        let holdouts = self
            .profile
            .holdouts
            .iter()
            .map(|holdout| (&holdout.check, Some(holdout.files.clone())));
        let checks: Vec<AdmittedCheck> = ordinary
            .chain(holdouts)
            .map(|(spec, holdout)| AdmittedCheck {
                profile: self.id.clone(),
                approved_ms: self.approved_ms,
                spec: spec.clone(),
                holdout,
            })
            .collect();
        assert_eq!(
            checks.len(),
            self.profile.checks.len() + self.profile.holdouts.len()
        );
        checks
    }
}

fn invalid() -> CheckRefusal {
    failure("checks_profile_invalid")
}

/// Whether a relative path is portable and stays inside its root: at most `PATH_BYTES_MAX`
/// bytes of components made of ASCII letters, digits, `-`, `_` and `.`, none empty, `.` or
/// `..`. Input: the untrusted path. Output: true when admissible.
fn safe_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= PATH_BYTES_MAX && path.split('/').all(component)
}

/// Whether one path component or bare program name is plain: ASCII letters, digits, `-`,
/// `_` and `.`, not empty, `.` or `..`. Input: the text. Output: true when plain.
fn component(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
