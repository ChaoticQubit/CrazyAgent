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

/// The project's `hidden` folders as paths below `repo`, with `/` between names, for the
/// mapping to leave out. Takes the folder and the hidden folders. Returns the paths, or
/// `None` when one is not below the folder or not UTF-8, so nothing is cleared.
pub(crate) fn below_root(
    repo: &std::path::Path,
    hidden: &[std::path::PathBuf],
) -> Option<Vec<String>> {
    let root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    hidden
        .iter()
        .map(|folder| {
            let below = folder
                .strip_prefix(&root)
                .or_else(|_| folder.strip_prefix(repo))
                .ok()?;
            let text = below.to_str()?;
            (!text.is_empty()).then(|| text.to_owned())
        })
        .collect()
}

/// Whether `path`, below the project folder and with an optional closing `/`, is one of the
/// `hidden` folders or lies inside one. Takes the path and the folders. Returns the answer.
pub(super) fn under(path: &str, hidden: &[String]) -> bool {
    let path = path.trim_end_matches('/');
    hidden.iter().any(|folder| {
        path.strip_prefix(folder.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Whether the evidence sent to a model provider may name `path`, below the project folder,
/// by the onboarding documents' own skip rules: not when any name in it looks like it holds
/// a secret ([`secret_like`], a bare name such as `id_rsa` at the root included) nor when it
/// is or lies in one of the `hidden` folders, given below the project folder
/// ([`below_root`]). Takes the path and the folders. Returns the answer.
pub(crate) fn offered(path: &str, hidden: &[String]) -> bool {
    !secret_like(path) && !under(path, hidden)
}
