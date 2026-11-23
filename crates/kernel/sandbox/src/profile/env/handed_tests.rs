use super::*;
use kernel_common::relay_names::LOCAL_PLACEHOLDER;

const NAMES: [&str; 1] = ["OPENAI_API_KEY"];

fn token(fill: char) -> String {
    format!("crazyagent-relay-{}", fill.to_string().repeat(64))
}

/// Builds one launch's environment with one harness variable, with the launch's
/// exception or, as for a teammate, with none. Inputs: the exception, the name and the
/// value. Output: the variables, or the refusal.
fn with(
    handed: Option<Handed<'_>>,
    name: &str,
    value: &str,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    let base = [("HOME", OsStr::new("/agent/home"))];
    let harness = [(OsStr::new(name), Some(OsStr::new(value)))];
    match handed {
        Some(handed) => agent_process_env_handing(&base, harness, handed),
        None => agent_process_env(&base, harness),
    }
}

/// DEC-180 Task 20 (rv-sa241 ruling t20-q1, condition 6): a listed master's launch hands
/// its harness its own relay token only in the names its manifest lists. Another
/// launch's token, any other relay value, an unlisted key name and a launch with no
/// exception, such as a teammate's, are refused.
#[test]
fn a_launch_hands_its_own_relay_token_only_in_the_names_its_manifest_lists() {
    let (own, other) = (token('a'), token('b'));
    let handed = Handed::new(&NAMES, &own);
    assert!(handed.is_ok(), "{:?}", handed.as_ref().err());
    let handed = handed.unwrap();
    let given = with(Some(handed), "OPENAI_API_KEY", &own);
    assert!(given.is_ok(), "{given:?}");
    let pair = (OsString::from("OPENAI_API_KEY"), OsString::from(&own));
    assert!(given.unwrap().contains(&pair));
    let refused = [
        ("OPENAI_API_KEY", other.as_str()),
        ("OPENAI_API_KEY", "crazyagent-relay-0"),
        ("OTHER_API_KEY", own.as_str()),
    ];
    for (name, value) in refused {
        assert!(with(Some(handed), name, value).is_err(), "{name}={value}");
    }
    assert!(with(None, "OPENAI_API_KEY", &own).is_err());
}

/// DEC-180 Task 20 (rv-sa241 ruling t20-q1, condition 3): a keyless launch hands only
/// the placeholder, and no exception is ever built for a saved key or a relay value that
/// is no token.
#[test]
fn a_keyless_launch_hands_only_the_placeholder_and_never_a_saved_key() {
    let keyless = Handed::new(&NAMES, LOCAL_PLACEHOLDER);
    assert!(keyless.is_ok(), "{:?}", keyless.as_ref().err());
    let given = with(Some(keyless.unwrap()), "OPENAI_API_KEY", LOCAL_PLACEHOLDER);
    assert!(given.is_ok(), "{given:?}");
    assert!(with(None, "OPENAI_API_KEY", LOCAL_PLACEHOLDER).is_err());
    for value in ["sk-saved", "crazyagent-relay-0", ""] {
        assert!(Handed::new(&NAMES, value).is_err(), "{value}");
    }
}

/// rv-sa241 M8 host guard N1: a launch hands its value only under key-shaped names; a
/// name the variable guard refuses for another reason, a credential setting, a perl
/// setting, a GitHub prefix or an engine variable, is refused when the exception is made.
#[test]
fn a_launch_never_hands_its_value_under_a_name_refused_for_another_reason() {
    let others = ["PERL5OPT", "GIT_ASKPASS", "GH_HOST", "CRAZYAGENT_LINEAGE"];
    for name in others {
        let names = [name];
        assert!(Handed::new(&names, LOCAL_PLACEHOLDER).is_err(), "{name}");
    }
    let keys = ["OPENAI_API_KEY", "STANDIN_TOKEN"];
    assert!(Handed::new(&keys, LOCAL_PLACEHOLDER).is_ok());
}
