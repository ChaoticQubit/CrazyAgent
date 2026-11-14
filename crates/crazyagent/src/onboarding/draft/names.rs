/// Words that mark a file or folder name as one that may hold a secret: credentials, keys
/// and tokens, key and key-store files such as `.pem`, `id_rsa` or `.jks`, credential
/// stores such as `.netrc`, `.npmrc`, `kubeconfig`, `terraform.tfstate`, `.envrc`, a
/// service-account JSON, `.dockercfg` or `.s3cfg`, and GnuPG keyrings, kept even when
/// renamed `.txt`. The mapping prompt goes to the project's model provider, and a document
/// is chosen by its name alone before any text is read, so a name with one of these words,
/// or two adjacent words joining into one, is neither read nor listed. A few documents with
/// such names, such as `key-metrics.md`, are left out with them.
const SECRET_WORDS: [&str; 41] = [
    "secret",
    "secrets",
    "credential",
    "credentials",
    "password",
    "passwords",
    "passwd",
    "token",
    "tokens",
    "apikey",
    "apikeys",
    "privatekey",
    "pem",
    "key",
    "keys",
    "p12",
    "pfx",
    "env",
    "netrc",
    "idrsa",
    "iddsa",
    "idecdsa",
    "ided25519",
    "htpasswd",
    "jks",
    "keystore",
    "kubeconfig",
    "tfstate",
    "tfvars",
    "pgpass",
    "npmrc",
    "pypirc",
    "ppk",
    "kdbx",
    "envrc",
    "serviceaccount",
    "gnupg",
    "secring",
    "pubring",
    "dockercfg",
    "s3cfg",
];

/// The marks split off around a path written in prose, Markdown or a manifest: quotes,
/// brackets, list and key separators and emphasis.
const PATH_MARKS: [char; 19] = [
    '`', '"', '\'', '(', ')', '[', ']', '{', '}', '<', '>', ',', ';', ':', '|', '=', '*', '!', '?',
];
const EXTENSION_BYTES_MAX: usize = 8;

/// Whether the file or folder `name` looks like it holds a secret: one of its words, split
/// at every character that is not an ASCII letter or digit, or two adjacent words joined,
/// is in [`SECRET_WORDS`], ignoring case. Takes the full name, extensions included.
pub(crate) fn secret_like(name: &str) -> bool {
    let words: Vec<&str> = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let secret = |word: &str| {
        SECRET_WORDS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(word))
    };
    words.iter().any(|word| secret(word))
        || words
            .windows(2)
            .any(|pair| secret(&format!("{}{}", pair[0], pair[1])))
}

/// File extensions of programming source. A source file is not kept out of the code graph
/// by a secret word in its name alone (DEC-132 Q12), as graphify's own detection rule does.
#[cfg(unix)]
const SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cjs", "cpp", "cs", "css", "go", "h", "hpp", "java", "js", "jsx", "kt", "kts",
    "less", "mjs", "php", "py", "rb", "rs", "sass", "scala", "scss", "svelte", "swift", "ts",
    "tsx", "vue",
];

/// Whether `name`, one file name, ends in a programming-source extension, matched exactly.
/// Takes the name. Returns true for `tokens.ts`, false for `tokens.json`, `.env` and a name
/// with no extension.
#[cfg(unix)]
pub(super) fn source_file(name: &str) -> bool {
    assert!(!name.contains('/'), "a file name, not a path");
    assert!(SOURCE_EXTENSIONS.windows(2).all(|pair| pair[0] < pair[1]));
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty() && SOURCE_EXTENSIONS.binary_search(&extension).is_ok()
}

/// The words of `text` that may name a file or folder: split at white space and
/// [`PATH_MARKS`], a closing full stop and a leading `./` dropped, kept when one holds a
/// `/`, starts with `.` or ends in an extension of one to eight ASCII letters or digits.
/// Takes the text. Returns the paths in order.
pub(super) fn paths_in(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| character.is_whitespace() || PATH_MARKS.contains(&character))
        .map(|word| {
            let word = word.trim_end_matches('.');
            word.strip_prefix("./").unwrap_or(word)
        })
        .filter(|word| {
            word.contains('/')
                || word.starts_with('.')
                || word.rsplit_once('.').is_some_and(|(stem, extension)| {
                    !stem.is_empty()
                        && (1..=EXTENSION_BYTES_MAX).contains(&extension.len())
                        && extension.chars().all(|c| c.is_ascii_alphanumeric())
                })
        })
}

/// Whether `text` names a path that looks like it holds a secret: one of its
/// [`paths_in`] is [`secret_like`]. Takes the text. Returns the answer.
pub(crate) fn names_secret(text: &str) -> bool {
    paths_in(text).any(secret_like)
}

/// `text` without its lines that name a path that looks like it holds a secret, every other
/// line kept byte for byte, so what the mapping agent writes or an overview file holds never
/// shows such a name. Takes the text. Returns the rest.
pub(crate) fn without_secret_paths(text: &str) -> String {
    text.split_inclusive('\n')
        .filter(|line| !names_secret(line))
        .collect()
}
