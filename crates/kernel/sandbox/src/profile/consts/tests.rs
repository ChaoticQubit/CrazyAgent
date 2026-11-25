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

/// A Linux run the call supervisor stopped because it could not take back an entry read as a held
/// name is told so; the same code on another system, and every other code on Linux (125, the
/// supervisor's other stops, among them) stays the harness's own.
#[test]
fn a_linux_run_stopped_with_a_held_name_not_put_back_is_told_so() {
    let said = held_stop("linux", HELD_NOT_PUT_BACK_EXIT);
    assert_eq!(said, Some(HELD_NOT_PUT_BACK));
    for code in [0, 1, 3, 125, 126, 137] {
        assert_eq!(held_stop("linux", code), None, "{code}");
    }
    for os in ["macos", "windows"] {
        assert_eq!(held_stop(os, HELD_NOT_PUT_BACK_EXIT), None, "{os}");
    }
}
