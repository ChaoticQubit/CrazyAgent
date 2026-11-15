use super::{ARGS_MAX, NPM_FLAGS_MAX, POINTER_BYTES_MAX, Pointer, PointerFormat};
use crate::app_plugins::yaml::Value;
use crate::harnesses::registry;
use std::sync::LazyLock;

/// The most Homebrew names one recipe lists, those the app may install and those it only
/// recognizes counted together.
pub(super) const BREWS_MAX: usize = 4;
/// The most bytes of one name, folder or address in a recipe.
pub(super) const TEXT_BYTES_MAX: usize = 240;

const PACKAGE: &str = "a package name of lower-case letters, digits and @ / . _ -";
const OPTION: &str = "an option that starts with -- and holds lower-case letters, digits, - and =";
const WORD: &str = "a word of lower-case letters, digits and dashes";
const FIELD: &str = "a field name of lower-case letters, digits and _";
const FOLDER: &str = "a folder under the home folder";
const ADDRESS: &str = "an https address without spaces";

/// Which part of Homebrew a name belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BrewKind {
    Formula,
    Cask,
}

/// One Homebrew name of a harness. `install` is false for a name the app recognizes and
/// updates but never installs, such as a deprecated formula or a second channel.
pub(super) struct Brew {
    pub(super) kind: BrewKind,
    pub(super) name: &'static str,
    pub(super) install: bool,
}

/// The npm package of a harness: its scope ("" for none), the name inside it, the full
/// package name, the install spec and the vendor's documented flags.
pub(super) struct Npm {
    pub(super) scope: &'static str,
    pub(super) name: &'static str,
    pub(super) package: &'static str,
    pub(super) spec: &'static str,
    pub(super) flags: &'static [&'static str],
}

/// A harness that updates itself: the folder under the home folder its versions live in,
/// the arguments of its own update command and the vendor's release pointer.
pub(super) struct SelfUpdate {
    pub(super) root: &'static str,
    pub(super) args: &'static [&'static str],
    pub(super) pointer: Pointer,
}

/// How one harness is installed and updated, as the `update` block of its manifest says:
/// its own updater, its Homebrew names, its npm package and the vendor's install page.
pub(super) struct Recipe {
    pub(super) id: &'static str,
    pub(super) own: Option<SelfUpdate>,
    pub(super) brews: &'static [Brew],
    pub(super) npm: Option<Npm>,
    pub(super) guide: &'static str,
}

fn package(text: &str) -> bool {
    !text.starts_with('-')
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"@/._-".contains(&byte)
        })
}

fn option(text: &str) -> bool {
    text.starts_with("--")
        && text.len() > 2
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'='
        })
}

fn word(text: &str) -> bool {
    !text.starts_with('-')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn field(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn folder(text: &str) -> bool {
    !text.contains('\\')
        && text
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn address(text: &str) -> bool {
    text.len() > "https://".len()
        && text.starts_with("https://")
        && text.bytes().all(|byte| byte.is_ascii_graphic())
}

fn keys(block: &Value, path: &str, known: &[&str]) -> Result<(), String> {
    let Value::Map(entries) = block else {
        return Err(format!("{path}: this must be a block of settings."));
    };
    match entries
        .iter()
        .find(|(key, _)| !known.contains(&key.as_str()))
    {
        Some((key, _)) => Err(format!("{path}.{key}: this key is not one a recipe has.")),
        None => Ok(()),
    }
}

fn text(block: &'static Value, path: &str, key: &str) -> Result<Option<&'static str>, String> {
    match block.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Str(text)) if !text.is_empty() && text.len() <= TEXT_BYTES_MAX => {
            Ok(Some(text.as_str()))
        }
        Some(_) => Err(format!(
            "{path}.{key}: this must be 1 to {TEXT_BYTES_MAX} bytes of text in quotes."
        )),
    }
}

fn needed(block: &'static Value, path: &str, key: &str) -> Result<&'static str, String> {
    text(block, path, key)?.ok_or_else(|| format!("{path}.{key}: this setting is needed."))
}

fn texts(
    block: &'static Value,
    path: &str,
    key: &str,
    most: usize,
) -> Result<Vec<&'static str>, String> {
    let items = match block.get(key) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::List(items)) if items.len() <= most => items,
        Some(_) => {
            return Err(format!(
                "{path}.{key}: this must be a list of at most {most} texts."
            ));
        }
    };
    let listed = items
        .iter()
        .map(|item| match item {
            Value::Str(text) if !text.is_empty() && text.len() <= TEXT_BYTES_MAX => {
                Ok(text.as_str())
            }
            _ => Err(format!(
                "{path}.{key}: every entry must be 1 to {TEXT_BYTES_MAX} bytes of text in quotes."
            )),
        })
        .collect::<Result<Vec<&'static str>, String>>()?;
    assert!(listed.len() <= most);
    Ok(listed)
}

fn held(
    text: &'static str,
    path: &str,
    fits: fn(&str) -> bool,
    shape: &str,
) -> Result<&'static str, String> {
    if fits(text) {
        Ok(text)
    } else {
        Err(format!("{path}: {text} is not {shape}."))
    }
}

fn pointer_of(pointer: &'static Value) -> Result<Pointer, String> {
    let path = "update.own.pointer";
    keys(
        pointer,
        path,
        &["url", "format", "field", "prefix", "bytes_max"],
    )?;
    let url = held(
        needed(pointer, path, "url")?,
        "update.own.pointer.url",
        address,
        ADDRESS,
    )?;
    let format = match (
        needed(pointer, path, "format")?,
        text(pointer, path, "field")?,
        text(pointer, path, "prefix")?,
    ) {
        ("plain-version", None, None) => PointerFormat::PlainVersion,
        ("json-tag", Some(name), Some(prefix)) => PointerFormat::JsonTag {
            field: held(name, "update.own.pointer.field", field, FIELD)?,
            prefix: held(prefix, "update.own.pointer.prefix", package, PACKAGE)?,
        },
        _ => {
            return Err(format!(
                "{path}.format: this must be plain-version, or json-tag with a field and a prefix."
            ));
        }
    };
    let bytes_max = match pointer.get("bytes_max") {
        None | Some(Value::Null) => POINTER_BYTES_MAX,
        Some(Value::Int(bytes))
            if u64::try_from(*bytes)
                .is_ok_and(|bytes| (1..=POINTER_BYTES_MAX).contains(&bytes)) =>
        {
            bytes.unsigned_abs()
        }
        Some(_) => {
            return Err(format!(
                "{path}.bytes_max: this must be a whole number from 1 to {POINTER_BYTES_MAX}."
            ));
        }
    };
    let read = Pointer {
        url,
        format,
        bytes_max,
    };
    assert!((1..=POINTER_BYTES_MAX).contains(&read.bytes_max));
    assert!(read.url.starts_with("https://"));
    Ok(read)
}

fn own_of(own: &'static Value) -> Result<SelfUpdate, String> {
    let path = "update.own";
    keys(own, path, &["root", "args", "pointer"])?;
    let root = held(
        needed(own, path, "root")?,
        "update.own.root",
        folder,
        FOLDER,
    )?;
    let args = texts(own, path, "args", ARGS_MAX)?;
    if args.is_empty() {
        return Err(format!("{path}.args: this setting is needed."));
    }
    for arg in args.iter().copied() {
        held(arg, "update.own.args", word, WORD)?;
    }
    let Some(pointer) = own.get("pointer") else {
        return Err(format!("{path}.pointer: this setting is needed."));
    };
    let updater = SelfUpdate {
        root,
        args: Vec::leak(args),
        pointer: pointer_of(pointer)?,
    };
    assert!(!updater.args.is_empty() && updater.args.len() <= ARGS_MAX);
    Ok(updater)
}

fn npm_of(npm: &'static Value) -> Result<Npm, String> {
    let path = "update.npm";
    keys(npm, path, &["scope", "name", "package", "spec", "flags"])?;
    let scope = match text(npm, path, "scope")? {
        Some(scope) => held(scope, "update.npm.scope", package, PACKAGE)?,
        None => "",
    };
    let flags = texts(npm, path, "flags", NPM_FLAGS_MAX)?;
    for flag in flags.iter().copied() {
        held(flag, "update.npm.flags", option, OPTION)?;
    }
    let found = Npm {
        scope,
        name: held(
            needed(npm, path, "name")?,
            "update.npm.name",
            package,
            PACKAGE,
        )?,
        package: held(
            needed(npm, path, "package")?,
            "update.npm.package",
            package,
            PACKAGE,
        )?,
        spec: held(
            needed(npm, path, "spec")?,
            "update.npm.spec",
            package,
            PACKAGE,
        )?,
        flags: Vec::leak(flags),
    };
    assert!(found.flags.len() <= NPM_FLAGS_MAX);
    Ok(found)
}

/// Reads the `update` block of one manifest into a recipe. Every name is held to plain
/// package characters and never starts with a dash, every flag starts with two dashes, every
/// address is https, and a key the block does not have is refused, because these values
/// become arguments of install and update commands. The lists are copied once into memory
/// that lives as long as the process. Inputs: the catalog id and the block. Output: the
/// recipe, or one plain sentence that names the setting.
pub(super) fn read(id: &'static str, block: &'static Value) -> Result<Recipe, String> {
    let path = "update";
    keys(
        block,
        path,
        &[
            "own",
            "formulae",
            "casks",
            "known_formulae",
            "known_casks",
            "npm",
            "guide",
        ],
    )?;
    let own = match block.get("own") {
        None | Some(Value::Null) => None,
        Some(own) => Some(own_of(own)?),
    };
    let mut brews: Vec<Brew> = Vec::new();
    for (key, kind, install) in [
        ("formulae", BrewKind::Formula, true),
        ("casks", BrewKind::Cask, true),
        ("known_formulae", BrewKind::Formula, false),
        ("known_casks", BrewKind::Cask, false),
    ] {
        for name in texts(block, path, key, BREWS_MAX)? {
            let name = held(name, &format!("{path}.{key}"), package, PACKAGE)?;
            if brews.iter().any(|brew| brew.name == name) {
                return Err(format!("{path}.{key}: {name} is listed twice."));
            }
            brews.push(Brew {
                kind,
                name,
                install,
            });
        }
    }
    if brews.len() > BREWS_MAX {
        return Err(format!(
            "{path}: a recipe lists at most {BREWS_MAX} Homebrew names."
        ));
    }
    let npm = match block.get("npm") {
        None | Some(Value::Null) => None,
        Some(npm) => Some(npm_of(npm)?),
    };
    let guide = held(
        needed(block, path, "guide")?,
        "update.guide",
        address,
        ADDRESS,
    )?;
    let recipe = Recipe {
        id,
        own,
        brews: Vec::leak(brews),
        npm,
        guide,
    };
    assert!(recipe.brews.len() <= BREWS_MAX);
    assert!(recipe.guide.starts_with("https://"));
    Ok(recipe)
}

/// The recipe of every bundled harness whose manifest has an `update` block, in catalog
/// order, read once. A bundled block that does not load is a programming error.
/// Output: the recipes.
pub(super) fn recipes() -> &'static [Recipe] {
    static LISTED: LazyLock<Vec<Recipe>> = LazyLock::new(|| {
        let harnesses = registry::registry().all();
        let listed: Vec<Recipe> = harnesses
            .iter()
            .filter_map(|harness| {
                let block = harness.manifest().block("update")?;
                Some(read(harness.id(), block).unwrap_or_else(|problem| {
                    panic!("the manifest of {} does not load: {problem}", harness.id())
                }))
            })
            .collect();
        assert!(listed.len() <= harnesses.len());
        listed
    });
    &LISTED
}

/// Finds the recipe of one harness. Input: a catalog id. Output: the recipe, none for a
/// harness without an `update` block and for an id the catalog does not have.
pub(super) fn recipe(id: &str) -> Option<&'static Recipe> {
    recipes().iter().find(|recipe| recipe.id == id)
}

/// Every package name, install spec and flag of every recipe, for the tests that hold them
/// to plain names. Output: the strings, in catalog order.
#[cfg(all(test, unix))]
pub(super) fn table_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for recipe in recipes() {
        names.extend(recipe.brews.iter().map(|brew| brew.name));
        if let Some(npm) = &recipe.npm {
            names.extend([npm.package, npm.spec]);
            names.extend(npm.flags);
        }
    }
    names
}

/// The Homebrew names of one harness the app may install, for the test that holds every
/// recipe to at most one. Input: a catalog id. Output: the names, empty without a recipe.
#[cfg(all(test, unix))]
pub(super) fn install_names(id: &str) -> Vec<&'static str> {
    recipe(id).map_or(Vec::new(), |recipe| {
        recipe
            .brews
            .iter()
            .filter(|brew| brew.install)
            .map(|brew| brew.name)
            .collect()
    })
}

#[cfg(test)]
#[path = "recipes_tests.rs"]
mod tests;
