use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

const TEXT_BYTES_MAX: usize = 16 * 1024;

const TEST: &[&str] = &[
    "test",
    "pytest",
    "vitest",
    "jest",
    "mocha",
    "spec",
    "coverage",
    "npm test",
    "npx vitest",
    "npx jest",
];
const GIT: &[&str] = &[
    "git push",
    "git pull",
    "git commit",
    "git merge",
    "git rebase",
    "git checkout",
    "git branch",
    "git stash",
    "git log",
    "git diff",
    "git status",
    "git add",
    "git reset",
    "git cherry-pick",
    "git tag",
];
const BUILD: &[&str] = &[
    "npm run build",
    "npm publish",
    "pip install",
    "docker",
    "deploy",
    "make build",
    "npm run dev",
    "npm start",
    "pm2",
    "systemctl",
    "brew",
    "cargo build",
];
const INSTALL: &[&str] = &[
    "npm install",
    "pip install",
    "brew install",
    "apt install",
    "cargo add",
];
const DEBUG: &[&str] = &[
    "fix",
    "bug",
    "error",
    "broken",
    "failing",
    "crash",
    "issue",
    "debug",
    "traceback",
    "exception",
    "stack trace",
    "stacktrace",
    "not working",
    "wrong",
    "unexpected",
    "status code",
    "404",
    "500",
    "401",
    "403",
];
const FEATURE: &[&str] = &[
    "add",
    "create",
    "implement",
    "new",
    "build",
    "feature",
    "introduce",
    "set up",
    "setup",
    "scaffold",
    "generate",
    "make a",
    "make me",
    "make the",
    "write a",
    "write me",
    "write the",
];
const REFACTOR: &[&str] = &[
    "refactor",
    "clean up",
    "cleanup",
    "rename",
    "reorganize",
    "simplify",
    "extract",
    "restructure",
    "move",
    "migrate",
    "split",
];
const BRAINSTORM: &[&str] = &[
    "brainstorm",
    "idea",
    "what if",
    "explore",
    "think about",
    "approach",
    "strategy",
    "design",
    "consider",
    "how should",
    "what would",
    "opinion",
    "suggest",
    "recommend",
];
const RESEARCH: &[&str] = &[
    "research",
    "investigate",
    "look into",
    "find out",
    "check",
    "search",
    "analyze",
    "review",
    "understand",
    "explain",
    "how does",
    "what is",
    "show me",
    "list",
    "compare",
];
const FILE_ENDINGS: &[&str] = &[
    "py", "js", "ts", "tsx", "jsx", "json", "yaml", "yml", "toml", "sql", "sh", "go", "rs", "java",
    "rb", "php", "css", "html", "md", "csv", "xml",
];
const SCRIPT: &[&str] = &[
    "execute",
    "script",
    "scrit",
    "curl",
    "endpoint",
    "request url",
    "query",
    "database",
];
const SCRIPT_THEN_TOKEN: &[&str] = &["api", "fetch", "db"];

/// The text of the `usage_activity` table's category check, so a stored name is always one of
/// the thirteen.
#[cfg(test)]
pub(super) const CATEGORY_CHECK: &str = "'coding','debugging','feature','refactoring','testing','exploration','planning','delegation','git','build_deploy','conversation','brainstorming','general'";

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

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn first_phrase(text: &str, phrases: &[&str]) -> Option<u32> {
    let bytes = text.as_bytes();
    phrases
        .iter()
        .filter_map(|phrase| {
            text.match_indices(phrase)
                .find(|(at, _)| {
                    let end = at + phrase.len();
                    (*at == 0 || !word_byte(bytes[*at - 1]))
                        && (end == bytes.len() || !word_byte(bytes[end]))
                })
                .map(|(at, _)| at as u32)
        })
        .min()
}

fn then_token(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    text.match_indices(word).find_map(|(at, _)| {
        let after = at + word.len();
        ((at == 0 || !word_byte(bytes[at - 1]))
            && bytes.get(after) == Some(&b' ')
            && bytes.get(after + 1).is_some_and(|byte| *byte != b' '))
        .then_some(after + 1)
    })
}

fn names_a_file(text: &str) -> bool {
    text.match_indices('.').any(|(at, _)| {
        let rest = &text[at + 1..];
        FILE_ENDINGS.iter().any(|ending| {
            rest.starts_with(ending)
                && rest
                    .as_bytes()
                    .get(ending.len())
                    .is_none_or(|byte| !word_byte(*byte))
        })
    })
}

fn runs_a_file(text: &str) -> bool {
    then_token(text, "run").is_some_and(|start| {
        let token = text[start..].split(' ').next().unwrap_or("");
        token.match_indices('.').any(|(at, _)| {
            token
                .as_bytes()
                .get(at + 1)
                .is_some_and(|byte| word_byte(*byte))
        })
    })
}

fn names_a_url(text: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        text.match_indices(scheme).any(|(at, _)| {
            text.as_bytes()
                .get(at + scheme.len())
                .is_some_and(|byte| *byte != b' ')
        })
    })
}

/// Reads what a message mentions. Input: the message. Output: facts about its words; only the
/// first `TEXT_BYTES_MAX` bytes are read.
pub fn words_of(text: &str) -> Words {
    let mut end = text.len().min(TEXT_BYTES_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = text[..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let has = |phrases: &[&str]| first_phrase(&text, phrases).is_some();
    Words {
        test: has(TEST),
        git: has(GIT),
        build: has(BUILD),
        install: has(INSTALL),
        debug: first_phrase(&text, DEBUG),
        feature: first_phrase(&text, FEATURE),
        refactor: first_phrase(&text, REFACTOR),
        brainstorm: has(BRAINSTORM),
        research: has(RESEARCH),
        file: names_a_file(&text),
        script: has(SCRIPT)
            || runs_a_file(&text)
            || SCRIPT_THEN_TOKEN
                .iter()
                .any(|word| then_token(&text, word).is_some()),
        url: names_a_url(&text),
    }
}

fn earliest(candidates: &[(Option<u32>, Activity)]) -> Option<Activity> {
    let mut best: Option<(u32, Activity)> = None;
    for (at, activity) in candidates {
        if let Some(at) = at
            && best.is_none_or(|(index, _)| *at < index)
        {
            best = Some((*at, *activity));
        }
    }
    best.map(|(_, activity)| activity)
}

fn by_tools(words: &Words, tools: &Tools) -> Option<Activity> {
    if tools.plan_mode {
        return Some(Activity::Planning);
    }
    if tools.agent_spawn {
        return Some(Activity::Delegation);
    }
    if tools.bash && !tools.edits {
        if words.test {
            return Some(Activity::Testing);
        }
        if words.git {
            return Some(Activity::Git);
        }
        if words.build || words.install {
            return Some(Activity::BuildDeploy);
        }
    }
    if tools.edits {
        return Some(Activity::Coding);
    }
    if tools.bash {
        return Some(if tools.reads {
            Activity::Exploration
        } else {
            Activity::Coding
        });
    }
    if tools.search || tools.mcp || tools.reads {
        return Some(Activity::Exploration);
    }
    if tools.tasks {
        return Some(Activity::Planning);
    }
    tools.skill.then_some(Activity::General)
}

fn conversation(words: &Words) -> Activity {
    if words.brainstorm {
        return Activity::Brainstorming;
    }
    if words.research {
        return Activity::Exploration;
    }
    if let Some(activity) = earliest(&[
        (words.feature, Activity::Feature),
        (words.debug, Activity::Debugging),
    ]) {
        return activity;
    }
    if words.file || words.script {
        return Activity::Coding;
    }
    if words.url {
        return Activity::Exploration;
    }
    Activity::Conversation
}

/// Names what a turn was about, by CodeBurn's rules (MIT, THIRD_PARTY_NOTICES.md): tools
/// first, then the words of its first message. Inputs: the words and the tools. Output: the
/// activity.
pub fn classify(words: &Words, tools: &Tools) -> Activity {
    match by_tools(words, tools) {
        Some(Activity::Coding) => earliest(&[
            (words.refactor, Activity::Refactoring),
            (words.feature, Activity::Feature),
            (words.debug, Activity::Debugging),
        ])
        .unwrap_or(Activity::Coding),
        Some(Activity::Exploration) if !words.research && words.debug.is_some() => {
            Activity::Debugging
        }
        Some(other) => other,
        None => conversation(words),
    }
}
