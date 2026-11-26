/// Tells whether a secret value occurs anywhere in `text`, byte for byte. An empty value
/// never matches, so an unset secret cannot refuse every launch. Inputs: the value and
/// the text (an argument, an environment name or value). Output: true when it occurs.
pub fn holds(value: &[u8], text: &[u8]) -> bool {
    !value.is_empty() && text.windows(value.len()).any(|window| window == value)
}

/// The most bytes of output one call redacts: the bound of an installer's output.
pub const OUTPUT_MAX: usize = 65_536;
/// The fewest bytes an environment value needs before it is redacted, so a short value
/// such as a port or a flag does not blank unrelated output.
pub const ENV_VALUE_MIN: usize = 8;
/// What every redacted run of bytes becomes.
const REDACTED: &[u8] = b"[REDACTED]";

/// Replaces every occurrence of every secret, and of every environment value of at least
/// `ENV_VALUE_MIN` bytes, with `[REDACTED]`. Empty values are ignored. Overlapping or
/// adjacent occurrences become one `[REDACTED]`, so a value holding another, or two values
/// that overlap, are redacted whole. Reads no store and keeps nothing. Inputs: the output
/// (at most `OUTPUT_MAX` bytes), the secret values and the environment values. Output: the
/// redacted bytes.
pub fn redact(output: &[u8], secrets: &[&[u8]], env: &[&[u8]]) -> Vec<u8> {
    redact_edges(output, secrets, env, false, false)
}

/// Redacts as [`redact`] does, and at a cut edge also the part of a value the cut left:
/// with `front` the longest opening run that equals an end of a value, with `back` the
/// longest closing run that equals a start of one, and with both the whole output when it
/// lies inside one value. Inputs: as [`redact`], plus whether the output was cut before its
/// first byte and after its last. Output: the redacted bytes.
pub fn redact_edges(
    output: &[u8],
    secrets: &[&[u8]],
    env: &[&[u8]],
    front: bool,
    back: bool,
) -> Vec<u8> {
    assert!(output.len() <= OUTPUT_MAX);
    let values = secrets
        .iter()
        .filter(|value| !value.is_empty())
        .chain(env.iter().filter(|value| value.len() >= ENV_VALUE_MIN));
    let mut covered = vec![false; output.len()];
    let length = output.len();
    for value in values {
        if front && back && holds(output, value) {
            covered.fill(true);
        }
        let parts = (1..value.len().min(length + 1)).rev();
        if let Some(part) = parts
            .clone()
            .find(|&k| front && output[..k] == value[value.len() - k..])
        {
            covered[..part].fill(true);
        }
        if let Some(part) = parts
            .clone()
            .find(|&k| back && output[length - k..] == value[..k])
        {
            covered[length - part..].fill(true);
        }
        if value.len() > length {
            continue;
        }
        for (start, window) in output.windows(value.len()).enumerate() {
            if window == *value {
                covered[start..start + value.len()].fill(true);
            }
        }
    }
    let mut redacted = Vec::with_capacity(output.len());
    let mut index = 0;
    while index < output.len() {
        if covered[index] {
            redacted.extend_from_slice(REDACTED);
            while index < output.len() && covered[index] {
                index += 1;
            }
        } else {
            redacted.push(output[index]);
            index += 1;
        }
    }
    assert!(redacted.len() <= output.len() * REDACTED.len());
    redacted
}

#[cfg(test)]
mod tests;
