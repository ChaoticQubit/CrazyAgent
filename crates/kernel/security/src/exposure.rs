/// Tells whether a secret value occurs anywhere in `text`, byte for byte. An empty value
/// never matches, so an unset secret cannot refuse every launch. Inputs: the value and
/// the text (an argument, an environment name or value). Output: true when it occurs.
pub fn holds(value: &[u8], text: &[u8]) -> bool {
    !value.is_empty() && text.windows(value.len()).any(|window| window == value)
}

#[cfg(test)]
mod tests;
