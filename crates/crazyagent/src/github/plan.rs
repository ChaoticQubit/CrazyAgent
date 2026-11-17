use super::job::GitHubPublishPlan;
use super::{api, clone};

#[cfg(test)]
mod plan_tests;

/// The field of a publish plan that a publish Start would refuse, so a client can name it
/// under the right control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubPlanField {
    Owner,
    Name,
    Description,
}

/// Names the first field of a publish plan that a publish Start would refuse, by the rules
/// Start checks: the account as `api::repo_name` takes it, the name as `clone::full_name`
/// takes it and the description as `api::description_valid` takes it. Input: the plan.
/// Output: the field, or none when Start takes the plan.
pub fn plan_problem(plan: &GitHubPublishPlan) -> Option<GitHubPlanField> {
    if api::repo_name(&plan.owner, "x").is_err() {
        return Some(GitHubPlanField::Owner);
    }
    if clone::full_name(&format!("x/{}", plan.name)).is_err() {
        return Some(GitHubPlanField::Name);
    }
    if !api::description_valid(&plan.description) {
        return Some(GitHubPlanField::Description);
    }
    None
}

/// Makes a project name into the repository name a publish proposes: every character
/// GitHub refuses becomes a dash, dash runs fold to one, leading dots and dashes go, the
/// name is cut to 100 bytes, then trailing dashes and `.git` go. Input: the project name.
/// Output: the repository name, empty when nothing usable is left.
pub fn default_repo_name(project: &str) -> String {
    let mut folded = String::new();
    for character in project.chars() {
        let kept = if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            character
        } else {
            '-'
        };
        if kept == '-' && folded.ends_with('-') {
            continue;
        }
        folded.push(kept);
    }
    let mut name = folded.trim_start_matches(['.', '-']).to_owned();
    name.truncate(clone::NAME_BYTES_MAX);
    loop {
        let before = name.len();
        while name.ends_with('-') {
            name.pop();
        }
        if let Some(kept) = name.strip_suffix(".git") {
            name = kept.to_owned();
        }
        if name.len() == before {
            break;
        }
    }
    assert!(name.is_ascii() && name.len() <= clone::NAME_BYTES_MAX);
    name
}

/// Makes a project's about text into the repository description a publish proposes:
/// whitespace and control character runs become one space, then the text is cut to 350
/// characters and trimmed. Input: the about text. Output: the description.
pub fn default_description(about: &str) -> String {
    let words: Vec<&str> = about
        .split(|character: char| character.is_whitespace() || character.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let cut: String = words
        .join(" ")
        .chars()
        .take(api::DESCRIPTION_CHARS_MAX)
        .collect();
    let description = cut.trim_end().to_owned();
    assert!(api::description_valid(&description));
    description
}
