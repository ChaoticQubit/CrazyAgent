use super::super::manifest::{self, Fact, Manifest, Refused, Rule};
use crate::usage::capture::Tokens;
use serde_json::Value;

/// The most rules one rule set holds.
pub const SET_RULES_MAX: usize = 32;
/// The most conditions one rule's `when` holds.
pub const WHEN_MAX: usize = 8;
/// The most bytes of one dotted path a rule reads.
pub const PATH_BYTES_MAX: usize = 128;
/// The most parts of one dotted path, such as `usage.input` with two.
pub const PATH_PARTS_MAX: usize = 8;
/// The most token count paths of one rule: input, output, cache read and cache write.
pub const TOKEN_PATHS_MAX: usize = 4;

/// What one rule found in one JSON object: its fact and the details its paths read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    pub fact: Fact,
    pub text: Option<String>,
    pub name: Option<String>,
    pub session: Option<String>,
    pub success: Option<bool>,
    pub tokens: Option<Tokens>,
    pub rate_limited: Option<bool>,
}

/// Reads one named rule set. Inputs: the manifest and the set's name. Output: a refusal
/// until rule reading lands.
pub fn read(manifest: &Manifest, set: &str) -> Result<Vec<Rule>, Refused> {
    let _ = manifest;
    Err(Refused {
        code: manifest::KIND,
        message: format!("rules.{set}: rules are not read yet."),
    })
}

/// Finds the rule that matches one JSON object. Inputs: the rules and the object. Output:
/// none until rule matching lands.
pub fn matched(rules: &[Rule], object: &Value) -> Option<Matched> {
    let _ = (rules, object);
    None
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
