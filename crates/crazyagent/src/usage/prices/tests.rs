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
fn a_dated_name_finds_its_model() {
    assert!(sample().has("claude-x-1-20251101"));
}

#[test]
fn an_at_suffixed_name_finds_its_model() {
    assert!(sample().has("claude-x-1@default"));
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
    let priced: Vec<&str> = [
        "claude-x-1-2",
        "claude-x-1-12",
        "claude-x-1-2-preview",
        "claude-x-1-2.5",
        "claude-x-1x",
        "unknown",
        "",
    ]
    .into_iter()
    .filter(|name| prices.has(name))
    .collect();
    assert_eq!(priced, Vec::<&str>::new());
}

#[test]
fn a_suffix_that_is_not_a_version_keeps_the_model_price() {
    let prices = sample();
    let unpriced: Vec<&str> = ["claude-x-1-123", "claude-x-1-2x", "claude-x-1-preview"]
        .into_iter()
        .filter(|name| !prices.has(name))
        .collect();
    assert_eq!(unpriced, Vec::<&str>::new());
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
fn fast_multiplies_the_cost() {
    let fast = Charged {
        fast: true,
        ..charged(input(1000))
    };
    assert_eq!(sample().cost_nanos("claude-x-1", fast), Some(30_000_000));
}

#[test]
fn a_model_with_no_fast_rate_costs_the_same_fast() {
    let fast = Charged {
        fast: true,
        ..charged(input(1000))
    };
    assert_eq!(
        sample().cost_nanos("claude-x-1-mini", fast),
        Some(1_000_000)
    );
}

#[test]
fn a_web_search_is_a_cent_each() {
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
fn a_cache_read_costs_its_own_rate_and_nothing_without_one() {
    let read = Tokens {
        cache_read: 1000,
        ..Tokens::default()
    };
    let prices = sample();
    assert_eq!(
        [
            prices.cost_nanos("claude-x-1", charged(read)),
            prices.cost_nanos("tiered-fast", charged(read)),
        ],
        [Some(500_000), Some(0)]
    );
}

#[test]
fn the_long_context_tier_prices_every_bucket() {
    let prices = sample();
    let cost = |tokens: Tokens| {
        let past = Charged {
            prompt_tokens: 200_001,
            ..charged(tokens)
        };
        prices.cost_nanos("tiered", past)
    };
    let output = Tokens {
        output: 1000,
        ..Tokens::default()
    };
    let write = Tokens {
        cache_write: 1000,
        ..Tokens::default()
    };
    let read = Tokens {
        cache_read: 1000,
        ..Tokens::default()
    };
    assert_eq!(
        [cost(output), cost(write), cost(read)],
        [Some(22_500_000), Some(7_500_000), Some(600_000)]
    );
}

#[test]
fn a_tier_with_no_cache_rates_falls_back_to_the_models() {
    let prices = parse(
        r#"{"as_of":"2026-11-08","source_sha256":"aa","models":{
        "fallback":{"input":1000000,"output":1000000,"cache_write":3000000,"cache_read":200000,
         "tier":{"threshold":10,"input":2000000,"output":2000000,"cache_write":null,"cache_read":null}}}}"#,
    )
    .unwrap();
    let cost = |tokens: Tokens| {
        let past = Charged {
            prompt_tokens: 11,
            ..charged(tokens)
        };
        prices.cost_nanos("fallback", past)
    };
    let write = Tokens {
        cache_write: 1000,
        ..Tokens::default()
    };
    let read = Tokens {
        cache_read: 1000,
        ..Tokens::default()
    };
    assert_eq!([cost(write), cost(read)], [Some(3_000_000), Some(200_000)]);
}

#[test]
fn a_one_hour_share_above_the_write_is_capped_at_the_write() {
    let tokens = Tokens {
        cache_write: 1000,
        ..Tokens::default()
    };
    let over = Charged {
        one_hour: 5000,
        ..charged(tokens)
    };
    assert_eq!(sample().cost_nanos("claude-x-1", over), Some(10_000_000));
}

#[test]
fn an_enormous_growth_saturates_instead_of_panicking() {
    let all = Charged {
        tokens: Tokens {
            input: u64::MAX,
            output: u64::MAX,
            cache_read: u64::MAX,
            cache_write: u64::MAX,
        },
        prompt_tokens: u64::MAX,
        one_hour: u64::MAX,
        web_searches: u64::MAX,
        fast: true,
    };
    assert_eq!(sample().cost_nanos("claude-x-1", all), Some(i64::MAX));
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

const LEAN: &str = r#""input":1,"output":1,"cache_write":null,"cache_read":null"#;

fn dated(as_of: &str, source: &str, models: &str) -> String {
    format!(r#"{{"as_of":"{as_of}","source_sha256":"{source}","models":{{{models}}}}}"#)
}

fn named(name: &str) -> String {
    dated("2026-11-08", "a", &format!(r#""{name}":{{{LEAN}}}"#))
}

fn rated(rates: &str) -> String {
    dated("2026-11-08", "a", &format!(r#""m":{{{rates}}}"#))
}

fn tiered(tier: &str) -> String {
    rated(&format!(r#"{LEAN},"tier":{{"threshold":1,{tier}}}"#))
}

fn many(count: usize) -> String {
    let models: Vec<String> = (0..count)
        .map(|index| format!(r#""m{index}":{{{LEAN}}}"#))
        .collect();
    dated("2026-11-08", "a", &models.join(","))
}

fn refused_labels(cases: &[(&'static str, String)]) -> Vec<&'static str> {
    cases
        .iter()
        .filter(|(_, text)| parse(text).is_err())
        .map(|(label, _)| *label)
        .collect()
}

fn accepted_labels(cases: &[(&'static str, String)]) -> Vec<&'static str> {
    cases
        .iter()
        .filter(|(_, text)| parse(text).is_ok())
        .map(|(label, _)| *label)
        .collect()
}

#[test]
fn a_malformed_snapshot_is_refused() {
    let m = format!(r#""m":{{{LEAN}}}"#);
    let cases = [
        ("no fields", "{}".to_owned()),
        ("short date", dated("soon", "a", &m)),
        ("long date", dated("2026-11-081", "a", &m)),
        ("first separator", dated("2026x11-08", "a", &m)),
        ("second separator", dated("2026-11x08", "a", &m)),
        ("letter in date", dated("20a6-11-08", "a", &m)),
        ("empty source", dated("2026-11-08", "", &m)),
        ("no models", dated("2026-11-08", "a", "")),
        ("too many models", many(MODELS_MAX + 1)),
        ("bad name bytes", named("BAD NAME")),
        ("empty name", named("")),
        ("long name", named(&"a".repeat(NAME_BYTES_MAX + 1))),
        (
            "zero input",
            rated(r#""input":0,"output":1,"cache_write":null,"cache_read":null"#),
        ),
        (
            "rate over the bound",
            rated(r#""input":1000000001,"output":1,"cache_write":null,"cache_read":null"#),
        ),
        (
            "zero output",
            rated(r#""input":1,"output":0,"cache_write":null,"cache_read":null"#),
        ),
        (
            "zero write",
            rated(r#""input":1,"output":1,"cache_write":0,"cache_read":null"#),
        ),
        (
            "zero read",
            rated(r#""input":1,"output":1,"cache_write":null,"cache_read":0"#),
        ),
        ("low fast", rated(&format!(r#"{LEAN},"fast_milli":999"#))),
        ("high fast", rated(&format!(r#"{LEAN},"fast_milli":20001"#))),
        ("unknown field", rated(&format!(r#"{LEAN},"extra":1"#))),
        (
            "tier zero input",
            tiered(r#""input":0,"output":1,"cache_write":null,"cache_read":null"#),
        ),
        (
            "tier zero output",
            tiered(r#""input":1,"output":0,"cache_write":null,"cache_read":null"#),
        ),
        (
            "tier zero write",
            tiered(r#""input":1,"output":1,"cache_write":0,"cache_read":null"#),
        ),
        (
            "tier zero read",
            tiered(r#""input":1,"output":1,"cache_write":null,"cache_read":0"#),
        ),
        (
            "tier low fast",
            tiered(&format!(r#"{LEAN},"fast_milli":999"#)),
        ),
        (
            "tier high fast",
            tiered(&format!(r#"{LEAN},"fast_milli":20001"#)),
        ),
        (
            "tier unknown field",
            tiered(&format!(r#"{LEAN},"extra":1"#)),
        ),
    ];
    assert_eq!(accepted_labels(&cases), Vec::<&str>::new());
}

#[test]
fn a_snapshot_at_each_limit_is_accepted() {
    let cases = [
        ("lowest rate", rated(LEAN)),
        (
            "highest rate",
            rated(r#""input":1000000000,"output":1,"cache_write":null,"cache_read":null"#),
        ),
        (
            "lowest fast",
            rated(&format!(r#"{LEAN},"fast_milli":1000"#)),
        ),
        (
            "highest fast",
            rated(&format!(r#"{LEAN},"fast_milli":20000"#)),
        ),
        (
            "tier lowest fast",
            tiered(&format!(r#"{LEAN},"fast_milli":1000"#)),
        ),
        (
            "tier highest fast",
            tiered(&format!(r#"{LEAN},"fast_milli":20000"#)),
        ),
        ("longest name", named(&"a".repeat(NAME_BYTES_MAX))),
        ("most models", many(MODELS_MAX)),
    ];
    assert_eq!(refused_labels(&cases), Vec::<&str>::new());
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
