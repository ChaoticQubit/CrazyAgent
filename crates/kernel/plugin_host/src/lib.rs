#![forbid(unsafe_code)]

/// Why the plugin host refused a plugin file: a stable code and the 1-based line of the
/// refused part, 1 when the refusal names the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub line: usize,
}

pub mod manifest;
#[cfg(test)]
mod tests;
