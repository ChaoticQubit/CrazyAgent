use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

pub const NAME_BYTES_MAX: usize = 100;
pub const ROLE_BYTES_MAX: usize = 100;
pub const BIO_BYTES_MAX: usize = 1000;
pub const STACK_ITEMS_MAX: usize = 12;
pub const STACK_ITEM_BYTES_MAX: usize = 40;
pub const PREFACE_BYTES_MAX: usize = 2048;
const BODY_BYTES_MAX: usize = 8192;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS user_profile (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 2 AND 8192)
)";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerDetail {
    #[default]
    Terse,
    Detailed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Initiative {
    #[default]
    AskFirst,
    ActFirst,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkingStyle {
    pub detail: AnswerDetail,
    pub initiative: Initiative,
}

/// The owner's first-run profile, kept by the engine and shown to the project master.
/// Only `name` is required; every later field defaults so new fields can be added
/// without breaking stored profiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerProfile {
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub preferred_stack: Vec<String>,
    #[serde(default)]
    pub working_style: WorkingStyle,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn single_line(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

impl OwnerProfile {
    /// Checks every field against its bound before the profile is stored.
    /// Input: an untrusted profile. Output: Ok, or the code naming the first
    /// field that is blank, too long or holds control characters.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.name.trim().is_empty()
            || self.name.len() > NAME_BYTES_MAX
            || !single_line(&self.name)
        {
            return Err(error("owner_profile_name"));
        }
        if self.role.len() > ROLE_BYTES_MAX || !single_line(&self.role) {
            return Err(error("owner_profile_role"));
        }
        if self.bio.len() > BIO_BYTES_MAX || self.bio.chars().any(|c| c.is_control() && c != '\n') {
            return Err(error("owner_profile_bio"));
        }
        if self.preferred_stack.len() > STACK_ITEMS_MAX
            || self.preferred_stack.iter().any(|item| {
                item.trim().is_empty() || item.len() > STACK_ITEM_BYTES_MAX || !single_line(item)
            })
            || (1..self.preferred_stack.len())
                .any(|index| self.preferred_stack[..index].contains(&self.preferred_stack[index]))
        {
            return Err(error("owner_profile_stack"));
        }
        Ok(())
    }

    /// Writes the short owner preface placed before each master prompt.
    /// Input: a validated profile. Output: plain sentences about the owner,
    /// never longer than `PREFACE_BYTES_MAX` bytes.
    pub fn preface(&self) -> String {
        assert!(self.validate().is_ok());
        let mut text = format!("About the owner: {}", self.name.trim());
        if !self.role.trim().is_empty() {
            text.push_str(&format!(", {}", self.role.trim()));
        }
        text.push('.');
        if !self.bio.trim().is_empty() {
            text.push(' ');
            text.push_str(self.bio.trim());
        }
        if !self.preferred_stack.is_empty() {
            let stack: Vec<&str> = self.preferred_stack.iter().map(|s| s.trim()).collect();
            text.push_str(&format!("\nPreferred stack: {}.", stack.join(", ")));
        }
        text.push_str(match self.working_style.detail {
            AnswerDetail::Terse => "\nKeep answers short.",
            AnswerDetail::Detailed => "\nGive detailed answers.",
        });
        text.push_str(match self.working_style.initiative {
            Initiative::AskFirst => " Ask before acting.",
            Initiative::ActFirst => " Act first, then report.",
        });
        assert!(text.len() <= PREFACE_BYTES_MAX);
        text
    }
}

/// Turns an owner-profile refusal code into the sentence shown to the owner.
/// Input: an engine rejection code. Output: the plain sentence, or None for
/// codes that are not about the owner profile.
pub fn plain_problem(code: &str) -> Option<&'static str> {
    match code {
        "owner_profile_name" => Some("Enter your name, up to 100 characters."),
        "owner_profile_role" => Some("Keep the role on one line, up to 100 characters."),
        "owner_profile_bio" => Some("Keep the bio under 1000 characters."),
        "owner_profile_stack" => {
            Some("List up to 12 different technologies, each up to 40 characters.")
        }
        _ => None,
    }
}

pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<(), EngineError> {
    tx.execute_batch(TABLE).map_err(error)
}

pub(crate) fn save(tx: &Transaction<'_>, profile: &OwnerProfile) -> Result<Outcome, EngineError> {
    if let Err(problem) = profile.validate() {
        return Ok(Outcome::Rejected { code: problem.0 });
    }
    let body = serde_json::to_string(profile).map_err(error)?;
    if body.len() > BODY_BYTES_MAX {
        return Ok(Outcome::Rejected {
            code: "owner_profile_bounds".into(),
        });
    }
    tx.execute(
        "INSERT INTO user_profile(singleton,body) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET body=excluded.body",
        [&body],
    )
    .map_err(error)?;
    Ok(Outcome::Applied)
}

pub(crate) fn load(tx: &Transaction<'_>) -> Result<Option<OwnerProfile>, EngineError> {
    let body: Option<Option<String>> = tx
        .query_row(
            "SELECT CASE WHEN length(CAST(body AS BLOB))<=8192 THEN body END FROM user_profile WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    body.map(|body| {
        let body = body.ok_or_else(|| error("owner_profile_bounds"))?;
        let profile: OwnerProfile =
            serde_json::from_str(&body).map_err(|_| error("owner_profile_invalid"))?;
        profile
            .validate()
            .map_err(|_| error("owner_profile_invalid"))?;
        Ok(profile)
    })
    .transpose()
}

pub(crate) fn read(tx: &Transaction<'_>) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::OwnerProfile { profile: load(tx)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> OwnerProfile {
        OwnerProfile {
            name: "Ada".into(),
            role: "Founder".into(),
            bio: "Builds tools.".into(),
            preferred_stack: vec!["Rust".into(), "TypeScript".into()],
            working_style: WorkingStyle {
                detail: AnswerDetail::Terse,
                initiative: Initiative::AskFirst,
            },
        }
    }

    #[test]
    fn every_field_is_bounded() {
        profile().validate().unwrap();
        type Case = (fn(&mut OwnerProfile), &'static str);
        let cases: Vec<Case> = vec![
            (|p| p.name = "  ".into(), "owner_profile_name"),
            (|p| p.name = "a".repeat(101), "owner_profile_name"),
            (|p| p.name = "a\nb".into(), "owner_profile_name"),
            (|p| p.role = "r".repeat(101), "owner_profile_role"),
            (|p| p.role = "a\tb".into(), "owner_profile_role"),
            (|p| p.bio = "b".repeat(1001), "owner_profile_bio"),
            (|p| p.bio = "a\u{7}b".into(), "owner_profile_bio"),
            (
                |p| p.preferred_stack = vec!["x".into(); 2],
                "owner_profile_stack",
            ),
            (
                |p| p.preferred_stack = vec![" ".into()],
                "owner_profile_stack",
            ),
            (
                |p| p.preferred_stack = vec!["s".repeat(41)],
                "owner_profile_stack",
            ),
            (
                |p| p.preferred_stack = (0..13).map(|i| format!("t{i}")).collect(),
                "owner_profile_stack",
            ),
        ];
        for (change, code) in cases {
            let mut value = profile();
            change(&mut value);
            assert_eq!(value.validate().unwrap_err().0, code, "{value:?}");
            assert!(plain_problem(code).is_some());
        }
        let mut multiline = profile();
        multiline.bio = "line one\nline two".into();
        multiline.validate().unwrap();
    }

    #[test]
    fn only_the_name_is_required_and_old_bodies_still_load() {
        let minimal: OwnerProfile = serde_json::from_str(r#"{"name":"Ada"}"#).unwrap();
        minimal.validate().unwrap();
        assert_eq!(minimal.working_style, WorkingStyle::default());
        assert!(serde_json::from_str::<OwnerProfile>(r#"{"name":"Ada","secret":1}"#).is_err());
    }

    #[test]
    fn the_preface_is_short_plain_and_follows_the_working_style() {
        assert_eq!(
            profile().preface(),
            "About the owner: Ada, Founder. Builds tools.\nPreferred stack: Rust, TypeScript.\nKeep answers short. Ask before acting."
        );
        let minimal = OwnerProfile {
            name: "Ada".into(),
            role: String::new(),
            bio: String::new(),
            preferred_stack: vec![],
            working_style: WorkingStyle {
                detail: AnswerDetail::Detailed,
                initiative: Initiative::ActFirst,
            },
        };
        assert_eq!(
            minimal.preface(),
            "About the owner: Ada.\nGive detailed answers. Act first, then report."
        );
        let largest = OwnerProfile {
            name: "n".repeat(NAME_BYTES_MAX),
            role: "r".repeat(ROLE_BYTES_MAX),
            bio: "b".repeat(BIO_BYTES_MAX),
            preferred_stack: (0..STACK_ITEMS_MAX).map(|i| format!("{i:0>40}")).collect(),
            working_style: WorkingStyle::default(),
        };
        largest.validate().unwrap();
        assert!(largest.preface().len() <= PREFACE_BYTES_MAX);
    }
}
