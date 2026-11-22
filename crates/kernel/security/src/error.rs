use std::fmt;

/// A credential store failure as a stable snake_case code; it never carries secret text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl StoreError {
    /// Builds the error from its stable code. Input: the snake_case code. Output: the error.
    pub fn code(code: &str) -> Self {
        Self(code.to_owned())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.0)
    }
}
