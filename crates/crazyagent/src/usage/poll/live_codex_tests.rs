use super::{CODEX_USAGE_URL, FIVE_HOUR_SECONDS, Provider, WEEK_SECONDS, poll};
use crate::protocol::CredentialRef;
use crate::team::runtime::managed_agent::{SignedIn, codex};
use crate::usage::{LimitSource, LimitWindow, Observation, limits, open, record_observation};

const CLOCK_SLACK_MS: i64 = 60_000;

/// Ends the ChatGPT usage check on a refusal: a lapsed sign-in, an API-key slot, a rate
/// limit or no reply is a blocker of the live check, any other code a failure of the read.
/// Input: the refusal code. Output: never returns.
fn refused(code: &str) -> ! {
    match code {
        "native_profile_codex_sign_in_expired" => panic!(
            "BLOCKED: the managed Codex profile's ChatGPT sign-in lapsed ({code}). Sign in to it again in CrazyAgent, then rerun"
        ),
        "native_profile_codex_api_key" => panic!(
            "BLOCKED: CRAZYAGENT_MANAGED_CODEX_PROFILE names a Codex profile signed in with an OpenAI API key, not a ChatGPT account"
        ),
        "usage_rate_limited" | "usage_unreachable" => {
            panic!("BLOCKED: Codex's usage call gave no reading ({code}). Rerun later")
        }
        _ => panic!("the usage read of a signed-in ChatGPT Codex slot failed with {code}"),
    }
}

/// A real ChatGPT Codex slot's usage read names Codex and reads the weekly window. A plan
/// whose reply leaves the 5-hour window out, as prolite's does, is a good read: stored over
/// an older 5-hour window, it leaves no stale 5-hour use and marks the window left out by
/// Codex (N-D16); a 5-hour window it reports replaces the old one with no mark.
#[test]
#[ignore = "live qualification: needs Codex signed in with ChatGPT, CRAZYAGENT_MANAGED_CODEX_PROFILE"]
fn a_real_codex_chatgpt_slot_reads_its_5_hour_and_weekly_windows() {
    let mut signed_in = SignedIn::codex();
    let lease = signed_in.acquire("codex-live-usage");
    let credential = lease.usage_credential(&codex());
    drop(lease);
    let credential = credential.unwrap_or_else(|problem| {
        assert_ne!(
            problem.0, "native_profile_credential_unavailable",
            "the usage read of a signed-in ChatGPT Codex slot found no credential (N-D16)"
        );
        refused(&problem.0)
    });
    let now = crate::team::now_ms();
    let observation = poll(
        Provider::Codex,
        CODEX_USAGE_URL.parse().unwrap(),
        &credential,
        false,
        now,
    )
    .unwrap_or_else(|problem| refused(&problem.0));
    println!(
        "real Codex usage: plan={:?} 5-hour={:?} weekly={:?}",
        observation.plan, observation.five_hour, observation.weekly
    );
    assert_eq!(observation.provider, Some(Provider::Codex));
    assert!(
        observation.weekly.is_some(),
        "the Codex usage read reported no weekly window: {observation:?}"
    );
    for (name, window, seconds) in [
        ("5-hour", observation.five_hour, FIVE_HOUR_SECONDS),
        ("weekly", observation.weekly, WEEK_SECONDS),
    ] {
        let Some(window) = window else {
            continue;
        };
        assert!(
            (0.0..=100.0).contains(&window.used_percent),
            "the {name} window's use is not a share: {window:?}"
        );
        let length_ms = i64::try_from(seconds).unwrap() * 1000;
        if let Some(resets) = window.resets_at_ms {
            assert!(
                (now - CLOCK_SLACK_MS..=now + length_ms + CLOCK_SLACK_MS).contains(&resets),
                "the {name} window resets outside its length from now ({now}): {window:?}"
            );
        }
    }
    let root = tempfile::tempdir().unwrap();
    let db = open(root.path()).unwrap();
    let account = CredentialRef::new("codex-live-usage").unwrap();
    let older = Observation {
        plan: None,
        five_hour: Some(LimitWindow {
            used_percent: 99.0,
            resets_at_ms: Some(now - 150 * 3_600_000),
        }),
        weekly: observation.weekly,
        provider: Some(Provider::Codex),
    };
    record_observation(
        &db,
        &account,
        &older,
        LimitSource::Poll,
        now - 151 * 3_600_000,
    )
    .unwrap();
    record_observation(&db, &account, &observation, LimitSource::Poll, now).unwrap();
    let stored = limits(&db, &account).unwrap().unwrap();
    assert_eq!(stored.five_hour, observation.five_hour);
    assert_eq!(stored.polled_five_hour, observation.five_hour);
    assert_eq!(stored.five_hour_at(now), observation.five_hour);
    assert_eq!(
        stored.five_hour_left_out,
        observation.five_hour.is_none().then_some(Provider::Codex),
        "a read that leaves the 5-hour window out marks it left out by Codex: {stored:?}"
    );
    assert_eq!(stored.weekly, observation.weekly);
    assert_eq!(stored.weekly_left_out, None);
}

#[test]
#[ignore = "live qualification: needs Codex signed in with an OpenAI API key, CRAZYAGENT_MANAGED_CODEX_KEY_PROFILE"]
fn a_codex_api_key_slot_reads_as_an_api_key_account_with_no_window() {
    let mut signed_in = SignedIn::codex_api_key();
    let lease = signed_in.acquire("codex-key-live-usage");
    let refusal = match lease.usage_credential(&codex()) {
        Ok(_) => panic!(
            "BLOCKED: CRAZYAGENT_MANAGED_CODEX_KEY_PROFILE names a Codex profile with ChatGPT tokens, not one signed in with an OpenAI API key only"
        ),
        Err(problem) => problem.0,
    };
    drop(lease);
    assert_eq!(
        refusal, "native_profile_codex_api_key",
        "the usage read of an API-key Codex slot must name the API-key account, never a missing credential (N-D16)"
    );
}
