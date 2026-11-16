/// Whether a manifest may not set a variable of this name in `accounts.<format>.env` or
/// `home.env`: the engine sets it on a launch, or it decides which shell setup runs, which
/// code a runtime loads or which server is trusted. Compared upper-cased, so the engine's
/// lower-case names count too. Input: the name. Output: the answer.
pub(super) fn owned(_name: &str) -> bool {
    false
}

/// Whether a `home.env_prefixes` entry would cover a name the engine owns, in either
/// direction: an owned name starts with its stem, or an owned family starts with its stem
/// or begins the prefix. Input: a prefix of capital letters, digits and `_` that ends with
/// `_`. Output: the answer.
pub(super) fn claims(_prefix: &str) -> bool {
    false
}

#[cfg(test)]
#[path = "env_names_tests.rs"]
mod tests;
