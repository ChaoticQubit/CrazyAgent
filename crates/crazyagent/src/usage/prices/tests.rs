use super::*;

const SAMPLE: &str = r#"{"as_of":"2026-11-08","source_sha256":"aa","models":{
"claude-x-1":{"input":5000000,"output":25000000,"cache_write":6250000,"cache_read":500000,"fast_milli":6000},
"claude-x-1-mini":{"input":1000000,"output":5000000,"cache_write":1250000,"cache_read":100000},
"tiered":{"input":3000000,"output":15000000,"cache_write":3750000,"cache_read":300000,
 "tier":{"threshold":200000,"input":6000000,"output":22500000,"cache_write":7500000,"cache_read":600000}},
"tiered-fast":{"input":1000000,"output":1000000,"cache_write":null,"cache_read":null,"fast_milli":2000,
 "tier":{"threshold":100,"input":2000000,"output":2000000,"cache_write":null,"cache_read":null,"fast_milli":1000}},
"plain":{"input":1250000,"output":10000000,"cache_write":null,"cache_read":125000}}}"#;

fn sample() -> Prices {
    parse(SAMPLE).unwrap()
}

fn charged(tokens: Tokens) -> Charged {
    Charged {
        tokens,
        prompt_tokens: tokens.input + tokens.cache_read + tokens.cache_write,
        ..Charged::default()
    }
}

fn input(count: u64) -> Tokens {
    Tokens {
        input: count,
        ..Tokens::default()
    }
}

#[test]
fn cost_is_exact_integer_nanodollars() {
    assert_eq!(
        sample().cost_nanos("claude-x-1", charged(input(1000))),
        Some(5_000_000)
    );
}

#[test]
fn a_dated_or_at_suffixed_name_finds_its_model() {
    let prices = sample();
    assert!(prices.has("claude-x-1-20251101"));
    assert!(prices.has("claude-x-1@default"));
}

#[test]
fn the_longest_name_wins() {
    let prices = sample();
    assert_eq!(
        prices.cost_nanos("claude-x-1-mini-preview", charged(input(1000))),
        Some(1_000_000)
    );
    assert_eq!(
        prices.cost_nanos("claude-x-1-preview", charged(input(1000))),
        Some(5_000_000)
    );
}

#[test]
fn a_new_version_or_unknown_name_has_no_price() {
    let prices = sample();
    assert_eq!(
        prices.cost_nanos("claude-x-1-2", charged(input(1000))),
        None
    );
    assert_eq!(prices.cost_nanos("unknown", charged(input(1000))), None);
    assert_eq!(prices.cost_nanos("", charged(input(1000))), None);
}

#[test]
fn a_prompt_past_the_threshold_uses_the_long_context_rates() {
    let prices = sample();
    let at = Charged {
        prompt_tokens: 200_000,
        ..charged(input(1000))
    };
    let past = Charged {
        prompt_tokens: 200_001,
        ..charged(input(1000))
    };
    assert_eq!(prices.cost_nanos("tiered", at), Some(3_000_000));
    assert_eq!(prices.cost_nanos("tiered", past), Some(6_000_000));
}

#[test]
fn a_one_hour_write_costs_one_point_six_of_the_five_minute_rate() {
    let tokens = Tokens {
        cache_write: 1000,
        ..Tokens::default()
    };
    let five = charged(tokens);
    let one = Charged {
        one_hour: 1000,
        ..five
    };
    assert_eq!(sample().cost_nanos("claude-x-1", five), Some(6_250_000));
    assert_eq!(sample().cost_nanos("claude-x-1", one), Some(10_000_000));
}

#[test]
fn a_write_with_no_published_rate_bills_at_the_input_rate() {
    let tokens = Tokens {
        cache_write: 1000,
        ..Tokens::default()
    };
    assert_eq!(
        sample().cost_nanos("plain", charged(tokens)),
        Some(1_250_000)
    );
}

#[test]
fn fast_multiplies_and_web_search_is_a_cent_each() {
    let fast = Charged {
        fast: true,
        ..charged(input(1000))
    };
    assert_eq!(sample().cost_nanos("claude-x-1", fast), Some(30_000_000));
    let searched = Charged {
        web_searches: 2,
        ..charged(Tokens::default())
    };
    assert_eq!(
        sample().cost_nanos("claude-x-1", searched),
        Some(20_000_000)
    );
}

#[test]
fn a_tier_that_names_its_own_fast_multiplier_replaces_the_models() {
    let at = Charged {
        fast: true,
        prompt_tokens: 100,
        ..charged(input(1000))
    };
    let past = Charged {
        fast: true,
        prompt_tokens: 101,
        ..charged(input(1000))
    };
    assert_eq!(sample().cost_nanos("tiered-fast", at), Some(2_000_000));
    assert_eq!(sample().cost_nanos("tiered-fast", past), Some(2_000_000));
}

#[test]
fn a_malformed_snapshot_is_refused() {
    for text in [
        "{}",
        r#"{"as_of":"soon","source_sha256":"a","models":{}}"#,
        r#"{"as_of":"2026-11-08","source_sha256":"a","models":{"BAD NAME":{"input":1,"output":1,"cache_write":null,"cache_read":null}}}"#,
        r#"{"as_of":"2026-11-08","source_sha256":"a","models":{"m":{"input":0,"output":1,"cache_write":null,"cache_read":null}}}"#,
        r#"{"as_of":"2026-11-08","source_sha256":"a","models":{"m":{"input":9000000000,"output":1,"cache_write":null,"cache_read":null}}}"#,
        r#"{"as_of":"2026-11-08","source_sha256":"a","models":{"m":{"input":1,"output":1,"cache_write":null,"cache_read":null,"extra":1}}}"#,
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}

#[test]
fn the_bundled_snapshot_is_dated_and_prices_the_harness_models() {
    let prices = table();
    assert_eq!(prices.as_of().len(), 10);
    assert!(prices.has("claude-opus-4-5-20251101"));
    assert!(prices.has("gpt-5-codex"));
}

#[test]
fn a_cost_rounds_to_the_nearest_nanodollar() {
    let prices = parse(
        r#"{"as_of":"2026-11-08","source_sha256":"aa","models":{
        "half":{"input":1500,"output":1400,"cache_write":null,"cache_read":null}}}"#,
    )
    .unwrap();
    assert_eq!(prices.cost_nanos("half", charged(input(1))), Some(2));
    let output = Tokens {
        output: 1,
        ..Tokens::default()
    };
    assert_eq!(prices.cost_nanos("half", charged(output)), Some(1));
}
