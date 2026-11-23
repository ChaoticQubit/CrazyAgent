use std::collections::{BTreeMap, BTreeSet};

use crazyagent_cap::{Capability, Enum, File, Handle, Type, TypeRef};

use crate::CapgenError;

/// The longest line capgen writes; a longer parameter or `use` list is one entry per line.
const LINE_MAX: usize = 100;

/// Every keyword of the WIT lexer in the pinned `wit-parser` 0.263.0 (`src/ast/lex.rs`);
/// a generated name equal to one is written with a leading `%`.
const WIT_KEYWORDS: [&str; 43] = [
    "use",
    "type",
    "func",
    "u8",
    "u16",
    "u32",
    "u64",
    "s8",
    "s16",
    "s32",
    "s64",
    "f32",
    "f64",
    "char",
    "resource",
    "own",
    "borrow",
    "record",
    "flags",
    "variant",
    "enum",
    "bool",
    "string",
    "option",
    "result",
    "future",
    "stream",
    "error-context",
    "list",
    "map",
    "as",
    "from",
    "static",
    "interface",
    "tuple",
    "world",
    "import",
    "export",
    "package",
    "constructor",
    "include",
    "with",
    "async",
];

/// The stems the fixed `types` interface and `plugin` world take.
const FIXED_STEMS: [&str; 2] = ["types", "plugin"];

/// The fixed interface every capability's result uses: the bus refusal, as `sdk::Refusal`.
const TYPES: &str = "interface types {
  record not-granted {
    capability: string,
    needs: string,
  }

  variant refusal {
    not-granted(not-granted),
    tier-too-high(string),
    rejected(string),
    failed(string),
  }
}
";

/// Writes `sdk.wit` for the parsed `.cap` files after every check the Rust SDK runs: the
/// package, the `types` interface, one interface per file and the `plugin` world.
/// Input: stems with parsed files, any order. Output: the one file, or the first refusal.
pub(crate) fn files(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    crate::rust::files(files)?;
    let mut sorted: Vec<&(&str, File)> = files.iter().collect();
    sorted.sort_by_key(|(stem, _)| *stem);
    for (stem, _) in &sorted {
        if FIXED_STEMS.contains(stem) {
            return Err(refuse(stem, "capgen_reserved_name", format!("wit: {stem}")));
        }
        snake_kebab(stem, stem)?;
    }
    acyclic(&sorted)?;
    let mut text = format!("package crazyagent:sdk;\n\n{TYPES}");
    let mut imports = Vec::with_capacity(sorted.len());
    for (stem, file) in &sorted {
        text.push('\n');
        text.push_str(&interface(stem, file)?);
        imports.push(format!("  import {};\n", escape(&snake_kebab(stem, stem)?)));
    }
    text.push_str("\nworld plugin {\n");
    text.extend(imports);
    text.push_str("}\n");
    assert!(text.ends_with("}\n") && !text.contains('\t'));
    Ok(vec![("sdk.wit".to_owned(), text)])
}

/// Refuses files whose foreign refs form a cycle of interfaces, which WIT cannot `use`.
/// Input: the sorted files. Output: nothing, or `capgen_wit_cycle` naming a file on it.
fn acyclic(files: &[&(&str, File)]) -> Result<(), CapgenError> {
    let edges: BTreeMap<&str, BTreeSet<&str>> = files
        .iter()
        .map(|(stem, file)| (*stem, foreign_stems(file)))
        .collect();
    let mut state: BTreeMap<&str, bool> = BTreeMap::new();
    for (stem, _) in files {
        visit(stem, &edges, &mut state, 0)?;
    }
    assert_eq!(state.len(), files.len());
    Ok(())
}

/// Walks one file's foreign refs depth first, each file once. Inputs: the stem, every
/// file's foreign stems, each visited file's state (`false` while on the walk, `true`
/// when done) and the depth. Output: nothing, or `capgen_wit_cycle`.
fn visit<'f>(
    stem: &'f str,
    edges: &BTreeMap<&'f str, BTreeSet<&'f str>>,
    state: &mut BTreeMap<&'f str, bool>,
    depth: usize,
) -> Result<(), CapgenError> {
    assert!(depth <= edges.len());
    match state.get(stem) {
        Some(true) => return Ok(()),
        Some(false) => return Err(refuse(stem, "capgen_wit_cycle", format!("wit: {stem}"))),
        None => {}
    }
    state.insert(stem, false);
    for next in &edges[stem] {
        visit(next, edges, state, depth + 1)?;
    }
    state.insert(stem, true);
    Ok(())
}

/// Writes one file's interface: `use` lines, then enums, handles, records and functions,
/// one blank line between items. Inputs: the stem and the file. Output: the text, or
/// `capgen_wit_name` (no kebab form) or `capgen_name_collision` (one name taken twice).
fn interface(stem: &str, file: &File) -> Result<String, CapgenError> {
    let mut taken = BTreeSet::new();
    let mut parts = Vec::new();
    if !file.capabilities.is_empty() {
        take(stem, &mut taken, "refusal")?;
        parts.push("  use types.{refusal};\n".to_owned());
    }
    let mut used: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for ty in called_refs(file) {
        if let Some((owner, name)) = foreign_in(ty) {
            used.entry(owner).or_default().insert(pascal_kebab(name));
        }
    }
    for (owner, names) in &used {
        for name in names {
            take(stem, &mut taken, name)?;
        }
        let names: Vec<String> = names.iter().map(|name| escape(name)).collect();
        let owner = escape(&snake_kebab(stem, owner)?);
        parts.push(wrapped(&format!("  use {owner}.{{"), &names, "};"));
    }
    if !parts.is_empty() {
        parts = vec![parts.concat()];
    }
    for item in &file.enums {
        parts.push(enumeration(stem, item, &mut taken)?);
    }
    for item in &file.handles {
        parts.push(handle(stem, item, &mut taken)?);
    }
    for item in &file.types {
        parts.push(record(stem, item, &mut taken)?);
    }
    for capability in &file.capabilities {
        parts.push(function(stem, capability, &mut taken)?);
    }
    let name = escape(&snake_kebab(stem, stem)?);
    if parts.is_empty() {
        return Ok(format!("interface {name} {{}}\n"));
    }
    Ok(format!("interface {name} {{\n{}}}\n", parts.join("\n")))
}

/// Writes one enum with its kebab variants. Inputs: the stem, the enum and the names the
/// interface has taken. Output: the block, or the refusal.
fn enumeration(
    stem: &str,
    item: &Enum,
    taken: &mut BTreeSet<String>,
) -> Result<String, CapgenError> {
    assert!(!item.variants.is_empty());
    let name = pascal_kebab(&item.name);
    take(stem, taken, &name)?;
    let mut text = format!("  enum {} {{\n", escape(&name));
    for variant in &item.variants {
        text.push_str(&format!("    {},\n", escape(&snake_kebab(stem, variant)?)));
    }
    text.push_str("  }\n");
    Ok(text)
}

/// Writes one handle as `type h = u64`: the kernel id, checked against its caller on every
/// call. Inputs: the stem, the handle and the taken names. Output: the block, or the refusal.
fn handle(stem: &str, item: &Handle, taken: &mut BTreeSet<String>) -> Result<String, CapgenError> {
    let name = pascal_kebab(&item.name);
    take(stem, taken, &name)?;
    Ok(format!("  type {} = u64;\n", escape(&name)))
}

/// Writes one record with its kebab fields. Inputs: the stem, the type and the taken
/// names. Output: the block, or the refusal.
fn record(stem: &str, item: &Type, taken: &mut BTreeSet<String>) -> Result<String, CapgenError> {
    assert!(!item.fields.is_empty());
    let name = pascal_kebab(&item.name);
    take(stem, taken, &name)?;
    let mut text = format!("  record {} {{\n", escape(&name));
    for field in &item.fields {
        text.push_str(&format!(
            "    {}: {},\n",
            escape(&snake_kebab(stem, &field.name)?),
            wit_type(&field.ty)
        ));
    }
    text.push_str("  }\n");
    Ok(text)
}

/// Writes one capability as a function of its inputs returning `result<T, refusal>`, with
/// no caller parameter: the host knows the caller from its store. Inputs: the stem, the
/// capability and the taken names. Output: the line or lines, or the refusal.
fn function(
    stem: &str,
    capability: &Capability,
    taken: &mut BTreeSet<String>,
) -> Result<String, CapgenError> {
    let rest = capability
        .name
        .split('.')
        .skip(1)
        .map(|segment| snake_kebab(stem, segment))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(!rest.is_empty());
    let name = rest.join("-");
    take(stem, taken, &name)?;
    let params = capability
        .inputs
        .iter()
        .map(|input| {
            Ok(format!(
                "{}: {}",
                escape(&snake_kebab(stem, &input.name)?),
                wit_type(&input.ty)
            ))
        })
        .collect::<Result<Vec<_>, CapgenError>>()?;
    let out = capability
        .output
        .as_ref()
        .map_or_else(|| "_".to_owned(), wit_type);
    Ok(wrapped(
        &format!("  {}: func(", escape(&name)),
        &params,
        &format!(") -> result<{out}, refusal>;"),
    ))
}

/// The WIT type of a `.cap` type: `text` is `string`, `bytes` is `list<u8>`, a named or
/// foreign item its kebab name. Input: the type. Output: the WIT text.
fn wit_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::U16 => "u16".to_owned(),
        TypeRef::U32 => "u32".to_owned(),
        TypeRef::U64 => "u64".to_owned(),
        TypeRef::Bool => "bool".to_owned(),
        TypeRef::Text => "string".to_owned(),
        TypeRef::Bytes => "list<u8>".to_owned(),
        TypeRef::List(inner) => format!("list<{}>", wit_type(inner)),
        TypeRef::Option(inner) => format!("option<{}>", wit_type(inner)),
        TypeRef::Named(name) | TypeRef::Foreign { name, .. } => escape(&pascal_kebab(name)),
    }
}

/// Writes `head` + entries + `tail` on one line when it fits `LINE_MAX`, else one entry per
/// line with a trailing comma, two spaces deeper than the head. Inputs: the head, the
/// entries and the tail. Output: the text, ending in a newline.
fn wrapped(head: &str, entries: &[String], tail: &str) -> String {
    let line = format!("{head}{}{tail}", entries.join(", "));
    if line.len() <= LINE_MAX || entries.is_empty() {
        return format!("{line}\n");
    }
    let indent = " ".repeat(head.len() - head.trim_start().len());
    let mut text = format!("{head}\n");
    for entry in entries {
        text.push_str(&format!("{indent}  {entry},\n"));
    }
    text.push_str(&format!("{indent}{tail}\n"));
    text
}

/// Takes one name of an interface. Inputs: the stem, the taken names and the kebab name.
/// Output: nothing, or `capgen_name_collision` with the detail `wit: <name>`.
fn take(stem: &str, taken: &mut BTreeSet<String>, name: &str) -> Result<(), CapgenError> {
    if taken.insert(name.to_owned()) {
        return Ok(());
    }
    Err(refuse(
        stem,
        "capgen_name_collision",
        format!("wit: {name}"),
    ))
}

/// The kebab name of a snake name: `_` becomes `-`, each part non-empty and starting with
/// a letter. Inputs: the stem, for refusals, and the name. Output: the kebab name, or
/// `capgen_wit_name` with the detail `wit: <name>`.
fn snake_kebab(stem: &str, snake: &str) -> Result<String, CapgenError> {
    let valid = snake.split('_').all(|part| {
        part.chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
    });
    if !valid {
        return Err(refuse(stem, "capgen_wit_name", format!("wit: {snake}")));
    }
    Ok(snake.replace('_', "-"))
}

/// The kebab name of a Pascal name: a new word at each upper-case letter, all lower case.
/// Pascal names are letters and digits starting upper case, so every word starts with a
/// letter. Input: the name. Output: the kebab name.
fn pascal_kebab(pascal: &str) -> String {
    let mut kebab = String::with_capacity(pascal.len() + 8);
    for (index, character) in pascal.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            kebab.push('-');
        }
        kebab.push(character.to_ascii_lowercase());
    }
    assert!(
        kebab
            .split('-')
            .all(|part| part.starts_with(|c: char| c.is_ascii_lowercase()))
    );
    kebab
}

/// A kebab name as WIT text: `%` before a keyword. Input: the name. Output: the text.
fn escape(name: &str) -> String {
    if WIT_KEYWORDS.contains(&name) {
        format!("%{name}")
    } else {
        name.to_owned()
    }
}

/// Builds a refusal of a file. Inputs: the stem, the code and the detail. Output: the refusal.
fn refuse(stem: &str, code: &'static str, detail: String) -> CapgenError {
    CapgenError {
        file: format!("{stem}.cap"),
        code,
        detail,
    }
}

/// The foreign stems a file's interface uses. Input: the file. Output: the stems, sorted.
fn foreign_stems(file: &File) -> BTreeSet<&str> {
    called_refs(file)
        .filter_map(|ty| foreign_in(ty).map(|(owner, _)| owner))
        .collect()
}

/// The type refs the interface writes: type fields, capability inputs and outputs, never
/// event fields. Input: the file. Output: the refs.
fn called_refs(file: &File) -> impl Iterator<Item = &TypeRef> {
    let fields = file
        .types
        .iter()
        .flat_map(|item| item.fields.iter().map(|field| &field.ty));
    let calls = file.capabilities.iter().flat_map(|capability| {
        capability
            .inputs
            .iter()
            .map(|field| &field.ty)
            .chain(capability.output.as_ref())
    });
    fields.chain(calls)
}

/// The foreign ref a type names through its lists and options, if any. Input: the type.
/// Output: the ref's stem and name.
fn foreign_in(ty: &TypeRef) -> Option<(&str, &str)> {
    match ty {
        TypeRef::List(inner) | TypeRef::Option(inner) => foreign_in(inner),
        TypeRef::Foreign { stem, name } => Some((stem.as_str(), name.as_str())),
        _ => None,
    }
}
