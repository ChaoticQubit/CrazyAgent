use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) const CATEGORY_CHECK: &str = "";

/// What a turn was mostly about. The thirteen categories and rules are CodeBurn's (MIT).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Coding,
    Debugging,
    Feature,
    Refactoring,
    Testing,
    Exploration,
    Planning,
    Delegation,
    Git,
    BuildDeploy,
    Conversation,
    Brainstorming,
    General,
}

/// Every activity, in the order the app lists them.
pub const ACTIVITIES: [Activity; 13] = [
    Activity::Coding,
    Activity::Debugging,
    Activity::Feature,
    Activity::Refactoring,
    Activity::Testing,
    Activity::Exploration,
    Activity::Planning,
    Activity::Delegation,
    Activity::Git,
    Activity::BuildDeploy,
    Activity::Conversation,
    Activity::Brainstorming,
    Activity::General,
];

impl Activity {
    /// The stored name of an activity. Output: the name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Debugging => "debugging",
            Self::Feature => "feature",
            Self::Refactoring => "refactoring",
            Self::Testing => "testing",
            Self::Exploration => "exploration",
            Self::Planning => "planning",
            Self::Delegation => "delegation",
            Self::Git => "git",
            Self::BuildDeploy => "build_deploy",
            Self::Conversation => "conversation",
            Self::Brainstorming => "brainstorming",
            Self::General => "general",
        }
    }
}

/// What a turn's first message mentions. Facts only; the message itself is never kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Words {
    test: bool,
    git: bool,
    build: bool,
    install: bool,
    debug: Option<u32>,
    feature: Option<u32>,
    refactor: Option<u32>,
    brainstorm: bool,
    research: bool,
    file: bool,
    script: bool,
    url: bool,
}

/// Which kinds of tool a turn used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tools {
    pub edits: bool,
    pub reads: bool,
    pub bash: bool,
    pub tasks: bool,
    pub search: bool,
    pub mcp: bool,
    pub skill: bool,
    pub plan_mode: bool,
    pub agent_spawn: bool,
}

/// Reads what a message mentions. Input: the message. Output: facts about its words.
pub fn words_of(_text: &str) -> Words {
    Words::default()
}

/// Names what a turn was about. Inputs: the words and the tools. Output: the activity.
pub fn classify(_words: &Words, _tools: &Tools) -> Activity {
    Activity::General
}
