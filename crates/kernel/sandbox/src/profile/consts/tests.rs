use super::*;

/// Every variable the engine itself sets in an agent's environment fits the bound on that
/// environment, and each is named once, so no fixed setting replaces another.
#[test]
fn the_fixed_agent_variables_fit_the_environment_bound_once_each() {
    let names: Vec<&str> = AGENT_GIT_ENV
        .iter()
        .chain(AGENT_HOME_ENV.iter())
        .chain(AGENT_TLS_ENV.iter())
        .map(|(name, _)| *name)
        .collect();
    assert!(names.len() <= AGENT_ENV_MAX);
    let mut once = names.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), names.len());
}
