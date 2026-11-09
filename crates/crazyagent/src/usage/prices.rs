use super::capture::Tokens;

#[cfg(test)]
mod tests;

/// The model prices. Not written yet.
#[derive(Debug)]
pub struct Prices {}

/// One growth to price. Not filled in yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Charged {
    pub tokens: Tokens,
    pub prompt_tokens: u64,
    pub one_hour: u64,
    pub web_searches: u64,
    pub fast: bool,
}

/// Not written yet. Input: the JSON text. Output: an empty table.
pub fn parse(_text: &str) -> Result<Prices, String> {
    Ok(Prices {})
}

/// Not written yet. Output: an empty table.
pub fn table() -> &'static Prices {
    static TABLE: Prices = Prices {};
    &TABLE
}

impl Prices {
    /// Not written yet. Output: no date.
    pub fn as_of(&self) -> &str {
        ""
    }

    /// Not written yet. Input: the model. Output: false.
    pub fn has(&self, _model: &str) -> bool {
        false
    }

    /// Not written yet. Inputs: the model and the growth. Output: none.
    pub fn cost_nanos(&self, _model: &str, _charged: Charged) -> Option<i64> {
        None
    }
}
