use super::job::GitHubPublishPlan;

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

/// Names the first field of a publish plan that a publish Start would refuse. Input: the
/// plan. Output: the field, or none when Start takes the plan.
pub fn plan_problem(_plan: &GitHubPublishPlan) -> Option<GitHubPlanField> {
    None
}

/// Makes a project name into the repository name a publish proposes. Input: the project
/// name. Output: the repository name, empty when nothing usable is left.
pub fn default_repo_name(_project: &str) -> String {
    String::new()
}

/// Makes a project's about text into the repository description a publish proposes.
/// Input: the about text. Output: the description.
pub fn default_description(_about: &str) -> String {
    String::new()
}
