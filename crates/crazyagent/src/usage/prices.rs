use super::capture::Tokens;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const BUNDLED: &str = include_str!("prices.json");
const MODELS_MAX: usize = 512;
const NAME_BYTES_MAX: usize = 64;
const RATE_PICO_MAX: u64 = 1_000_000_000;
const FAST_UNIT: u128 = 1000;
const ONE_HOUR_TENTHS: u128 = 16;
const WEB_SEARCH_PICO: u128 = 10_000_000_000;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tier {
    threshold: u64,
    input: u64,
    output: u64,
    cache_write: Option<u64>,
    cache_read: Option<u64>,
    #[serde(default)]
    fast_milli: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rates {
    input: u64,
    output: u64,
    cache_write: Option<u64>,
    cache_read: Option<u64>,
    #[serde(default)]
    fast_milli: Option<u64>,
    #[serde(default)]
    tier: Option<Tier>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    as_of: String,
    source_sha256: String,
    models: BTreeMap<String, Rates>,
}

/// The model prices the engine estimates dollars from: a dated, bundled snapshot. Rates are
/// picodollars per token, so every sum is integer maths.
#[derive(Debug)]
pub struct Prices {
    as_of: String,
    models: BTreeMap<String, Rates>,
    longest_first: Vec<String>,
}

/// One growth to price: its tokens, the whole call's prompt size that picks the rate, the
/// part of its cache write held one hour, its web searches and whether it ran fast.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Charged {
    pub tokens: Tokens,
    pub prompt_tokens: u64,
    pub one_hour: u64,
    pub web_searches: u64,
    pub fast: bool,
}

fn valid_rate(rate: u64) -> bool {
    rate > 0 && rate <= RATE_PICO_MAX
}

fn valid_rates(rates: &Rates) -> bool {
    let tier = rates.tier.as_ref().is_none_or(|tier| {
        valid_rate(tier.input)
            && valid_rate(tier.output)
            && tier.cache_write.is_none_or(valid_rate)
            && tier.cache_read.is_none_or(valid_rate)
            && tier
                .fast_milli
                .is_none_or(|fast| (1000..=20_000).contains(&fast))
    });
    tier && valid_rate(rates.input)
        && valid_rate(rates.output)
        && rates.cache_write.is_none_or(valid_rate)
        && rates.cache_read.is_none_or(valid_rate)
        && rates
            .fast_milli
            .is_none_or(|fast| (1000..=20_000).contains(&fast))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.' | b'_')
        })
}

fn valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

/// Reads a price snapshot, refusing anything outside its bounds. Input: the JSON text.
/// Output: the table, or the reason it was refused.
pub fn parse(text: &str) -> Result<Prices, String> {
    let snapshot: Snapshot = serde_json::from_str(text).map_err(|problem| problem.to_string())?;
    if !valid_date(&snapshot.as_of) || snapshot.source_sha256.is_empty() {
        return Err("price snapshot has no valid date or source".to_owned());
    }
    if snapshot.models.is_empty() || snapshot.models.len() > MODELS_MAX {
        return Err("price snapshot has the wrong number of models".to_owned());
    }
    if !snapshot
        .models
        .iter()
        .all(|(name, rates)| valid_name(name) && valid_rates(rates))
    {
        return Err("price snapshot holds an invalid model".to_owned());
    }
    let mut longest_first: Vec<String> = snapshot.models.keys().cloned().collect();
    longest_first.sort_by_key(|name| std::cmp::Reverse(name.len()));
    Ok(Prices {
        as_of: snapshot.as_of,
        models: snapshot.models,
        longest_first,
    })
}

/// The bundled table, read once. A test parses the same bytes, so this cannot fail in a
/// released build. Output: the table.
pub fn table() -> &'static Prices {
    static TABLE: OnceLock<Prices> = OnceLock::new();
    TABLE.get_or_init(|| parse(BUNDLED).expect("the bundled price snapshot is valid"))
}

/// Drops a trailing bracket suffix such as `[1m]`, the rule CodeBurn writes as a regular
/// expression. Input: the name. Output: the name without it, or the name when it has none.
fn without_bracket(name: &str) -> &str {
    let Some(inner) = name.strip_suffix(']') else {
        return name;
    };
    let after = inner.rfind(']').map_or(0, |at| at + 1);
    inner[after..]
        .find('[')
        .map_or(name, |at| &name[..after + at])
}

fn model_key(model: &str) -> &str {
    let name = model.split('@').next().unwrap_or(model);
    let name = match name.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()) => {
            head
        }
        _ => name,
    };
    without_bracket(name)
}

fn version_next(rest: &str) -> bool {
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    (1..=2).contains(&digits) && matches!(rest.as_bytes().get(digits), None | Some(b'-' | b'.'))
}

/// The rate a cache write bills at, as CodeBurn prices it. A published rate wins, the tier's
/// over the model's. A model with none pays 1.25 times its base input when it is Anthropic's,
/// and otherwise its input rate for the call, the tier's included. Inputs: the model, its
/// rates, the tier in force and the call's input rate. Output: picodollars per token.
fn write_rate(model: &str, rates: &Rates, tier: Option<&Tier>, input: u128) -> u128 {
    if rates.cache_write.is_none() {
        let name = model.to_ascii_lowercase();
        if !(name.contains("claude") || name.contains("anthropic")) {
            return input;
        }
    }
    tier.and_then(|tier| tier.cache_write)
        .or(rates.cache_write)
        .map_or(u128::from(rates.input) * 5 / 4, u128::from)
}

impl Prices {
    /// The date of the prices. Output: `YYYY-MM-DD`.
    pub fn as_of(&self) -> &str {
        &self.as_of
    }

    fn rates(&self, model: &str) -> Option<&Rates> {
        let name = model_key(model);
        if let Some(rates) = self.models.get(name) {
            return Some(rates);
        }
        for key in &self.longest_first {
            let Some(rest) = name
                .strip_prefix(key.as_str())
                .and_then(|rest| rest.strip_prefix('-'))
            else {
                continue;
            };
            return (!version_next(rest)).then(|| &self.models[key]);
        }
        None
    }

    /// Whether a model has a price. Input: the name a transcript gave. Output: the answer.
    pub fn has(&self, model: &str) -> bool {
        self.rates(model).is_some()
    }

    /// Prices one growth. Inputs: the model and the growth. Output: nanodollars rounded down,
    /// so an estimate never reads over, None for a model without a price. Rules ported from
    /// CodeBurn (MIT, THIRD_PARTY_NOTICES.md).
    pub fn cost_nanos(&self, model: &str, charged: Charged) -> Option<i64> {
        let rates = self.rates(model)?;
        let tier = rates
            .tier
            .as_ref()
            .filter(|tier| charged.prompt_tokens > tier.threshold);
        let input = u128::from(tier.map_or(rates.input, |tier| tier.input));
        let output = u128::from(tier.map_or(rates.output, |tier| tier.output));
        let write = write_rate(model, rates, tier, input);
        let read = u128::from(
            tier.and_then(|tier| tier.cache_read)
                .or(rates.cache_read)
                .unwrap_or(rates.input / 10),
        );
        let one_hour = u128::from(charged.one_hour.min(charged.tokens.cache_write));
        let five_minute = u128::from(charged.tokens.cache_write) - one_hour;
        let pico = u128::from(charged.tokens.input) * input
            + u128::from(charged.tokens.output) * output
            + five_minute * write
            + one_hour * write * ONE_HOUR_TENTHS / 10
            + u128::from(charged.tokens.cache_read) * read
            + u128::from(charged.web_searches) * WEB_SEARCH_PICO;
        let fast = tier.and_then(|tier| tier.fast_milli).or(rates.fast_milli);
        let multiplier = if charged.fast {
            fast.map_or(FAST_UNIT, u128::from)
        } else {
            FAST_UNIT
        };
        let total = pico * multiplier / FAST_UNIT;
        Some(i64::try_from(total / 1000).unwrap_or(i64::MAX))
    }
}
