use std::collections::{BTreeMap, BTreeSet};

use crazyagent_cap::{Capability, Enum, Field, File, TypeRef};

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

/// The stems a file may not have: `sdk` and `plugin` take the fixed refusal interface and
/// world; a stem ending in `_types` would take another stem's types interface name.
const FIXED_STEMS: [&str; 2] = ["sdk", "plugin"];

/// The source stem of the fixed refusal interface, `sdk-types`.
const SDK: &str = "sdk";

/// The fixed interface every capability's result uses: the bus refusal, as `sdk::Refusal`.
const SDK_TYPES: &str = "interface sdk-types {
  record not-granted {
    capability: string,
    needs: string,
  }

  variant refusal {
    not-granted(not-granted),
    tier-too-high(string),
    plugin-off(string),
    rejected(string),
    failed(string),
  }
}
";

/// What one interface calls each name it uses from a types interface: by source stem and
/// kebab name, the plain name or its `<src>-<name>` alias.
type Written = BTreeMap<(String, String), String>;

/// Writes `sdk.wit` for the parsed `.cap` files after every check the Rust SDK runs: the
/// package, `sdk-types`, per file a `<stem>-types` interface for its items and a `<stem>`
/// interface for its functions, and the `plugin` world importing the function interfaces.
/// Input: stems with parsed files, any order. Output: the one file, or the first refusal.
pub(crate) fn files(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    crate::rust::files(files)?;
    let mut sorted: Vec<&(&str, File)> = files.iter().collect();
    sorted.sort_by_key(|(stem, _)| *stem);
    for (stem, _) in &sorted {
        if FIXED_STEMS.contains(stem) || stem.ends_with("_types") {
            return Err(refuse(stem, "capgen_reserved_name", format!("wit: {stem}")));
        }
        snake_kebab(stem, stem)?;
    }
    acyclic(&sorted)?;
    let mut text = format!("package crazyagent:sdk;\n\n{SDK_TYPES}");
    let mut imports = Vec::with_capacity(sorted.len());
    for (stem, file) in &sorted {
        let name = snake_kebab(stem, stem)?;
        let items = file.enums.len() + file.handles.len() + file.types.len();
        if items > 0 || !crate::rust_wire::events(file).is_empty() {
            text.push('\n');
            text.push_str(&types_interface(stem, &name, file)?);
        }
        if !file.capabilities.is_empty() {
            text.push('\n');
            text.push_str(&functions_interface(stem, &name, file)?);
            imports.push(format!("  import {};\n", escape(&name)));
        }
    }
    if imports.is_empty() {
        text.push_str("\nworld plugin {}\n");
    } else {
        text.push_str("\nworld plugin {\n");
        text.extend(imports);
        text.push_str("}\n");
    }
    assert!(text.ends_with("}\n") && !text.contains('\t'));
    Ok(vec![("sdk.wit".to_owned(), text)])
}

/// Refuses files whose types interfaces would `use` each other in a cycle, which WIT
/// cannot resolve: records of each naming the other's leaves. Input: the sorted files.
/// Output: nothing, or `capgen_wit_cycle` naming a file on the cycle.
fn acyclic(files: &[&(&str, File)]) -> Result<(), CapgenError> {
    let edges: BTreeMap<&str, BTreeSet<&str>> = files
        .iter()
        .map(|(stem, file)| {
            let used = field_refs(file)
                .filter_map(|ty| foreign_in(ty).map(|(owner, _)| owner))
                .collect();
            (*stem, used)
        })
        .collect();
    let mut state: BTreeMap<&str, bool> = BTreeMap::new();
    for (stem, _) in files {
        visit(stem, &edges, &mut state, 0)?;
    }
    assert_eq!(state.len(), files.len());
    Ok(())
}

/// Walks one file's foreign record refs depth first, each file once. Inputs: the stem,
/// every file's foreign stems, each visited file's state (`false` while on the walk,
/// `true` when done) and the depth. Output: nothing, or `capgen_wit_cycle`.
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

/// Writes a file's types interface: `use` lines for the foreign leaves its records and
/// event bodies name, then its enums, handles, records and one record per event kind.
/// Inputs: the stem, its kebab name and the file. Output: the text, or `capgen_wit_name` /
/// `capgen_name_collision`.
fn types_interface(stem: &str, name: &str, file: &File) -> Result<String, CapgenError> {
    let mut taken = BTreeSet::new();
    let events = crate::rust_wire::events(file);
    let items = file
        .enums
        .iter()
        .map(|item| &item.name)
        .chain(file.handles.iter().map(|item| &item.name))
        .chain(file.types.iter().map(|item| &item.name))
        .chain(events.iter().map(|event| &event.kind));
    for item in items {
        take(stem, &mut taken, &pascal_kebab(item))?;
    }
    let mut sources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for ty in field_refs(file) {
        if let Some((owner, leaf)) = foreign_in(ty) {
            sources
                .entry(owner.to_owned())
                .or_default()
                .insert(pascal_kebab(leaf));
        }
    }
    let (uses, written) = uses(stem, sources, &mut taken)?;
    let mut parts = Vec::new();
    if !uses.is_empty() {
        parts.push(uses);
    }
    for item in &file.enums {
        parts.push(enumeration(stem, item)?);
    }
    for item in &file.handles {
        parts.push(format!(
            "  type {} = u64;\n",
            escape(&pascal_kebab(&item.name))
        ));
    }
    for item in &file.types {
        parts.push(record(stem, &item.name, &item.fields, &written)?);
    }
    for event in &events {
        parts.push(record(stem, &event.kind, &event.fields, &written)?);
    }
    assert!(!parts.is_empty());
    Ok(format!(
        "interface {}-types {{\n{}}}\n",
        escape(name),
        parts.join("\n")
    ))
}

/// Writes a file's functions interface: `use` lines for the refusal and every type its
/// functions name, then one function per capability. Inputs: the stem, its kebab name and
/// the file. Output: the text, or `capgen_wit_name` / `capgen_name_collision`.
fn functions_interface(stem: &str, name: &str, file: &File) -> Result<String, CapgenError> {
    let mut taken = BTreeSet::new();
    let mut functions = Vec::with_capacity(file.capabilities.len());
    for capability in &file.capabilities {
        let function = capability
            .name
            .split('.')
            .skip(1)
            .map(|segment| snake_kebab(stem, segment))
            .collect::<Result<Vec<_>, _>>()?
            .join("-");
        take(stem, &mut taken, &function)?;
        functions.push((function, capability));
    }
    let mut sources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    sources
        .entry(SDK.to_owned())
        .or_default()
        .insert("refusal".to_owned());
    for ty in call_refs(file) {
        if let Some(local) = named_in(ty) {
            sources
                .entry(stem.to_owned())
                .or_default()
                .insert(pascal_kebab(local));
        }
        if let Some((owner, leaf)) = foreign_in(ty) {
            sources
                .entry(owner.to_owned())
                .or_default()
                .insert(pascal_kebab(leaf));
        }
    }
    let (uses, written) = uses(stem, sources, &mut taken)?;
    let mut parts = vec![uses];
    for (function, capability) in &functions {
        parts.push(signature(stem, function, capability, &written)?);
    }
    Ok(format!(
        "interface {} {{\n{}}}\n",
        escape(name),
        parts.join("\n")
    ))
}

/// Writes the `use` lines of one interface: `sdk-types` first, then the file's own types
/// interface, then foreign ones by stem. A used name already in the interface is written
/// `<name> as <src>-<name>`. Inputs: the stem, each source's names and the names taken.
/// Output: the lines and each used name's written name, or `capgen_name_collision`.
fn uses(
    stem: &str,
    sources: BTreeMap<String, BTreeSet<String>>,
    taken: &mut BTreeSet<String>,
) -> Result<(String, Written), CapgenError> {
    let mut order: Vec<(String, BTreeSet<String>)> = sources.into_iter().collect();
    order.sort_by_key(|(src, _)| (src != SDK, src != stem, src.clone()));
    let mut text = String::new();
    let mut written = Written::new();
    for (src, names) in order {
        let source = snake_kebab(stem, &src)?;
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let local = if taken.contains(&name) {
                format!("{source}-{name}")
            } else {
                name.clone()
            };
            take(stem, taken, &local)?;
            entries.push(if local == name {
                escape(&name)
            } else {
                format!("{} as {}", escape(&name), escape(&local))
            });
            written.insert((src.clone(), name), local);
        }
        text.push_str(&wrapped(
            &format!("  use {}-types.{{", escape(&source)),
            &entries,
            "};",
        ));
    }
    Ok((text, written))
}

/// Writes one enum with its kebab variants. Inputs: the stem and the enum.
/// Output: the block, or `capgen_wit_name`.
fn enumeration(stem: &str, item: &Enum) -> Result<String, CapgenError> {
    assert!(!item.variants.is_empty());
    let mut text = format!("  enum {} {{\n", escape(&pascal_kebab(&item.name)));
    for variant in &item.variants {
        text.push_str(&format!("    {},\n", escape(&snake_kebab(stem, variant)?)));
    }
    text.push_str("  }\n");
    Ok(text)
}

/// Writes one record with its kebab fields, a foreign leaf by its written name.
/// Inputs: the stem, the record's name, its fields and the written used names.
/// Output: the block, or the refusal.
fn record(
    stem: &str,
    name: &str,
    fields: &[Field],
    written: &Written,
) -> Result<String, CapgenError> {
    assert!(!fields.is_empty());
    let mut text = format!("  record {} {{\n", escape(&pascal_kebab(name)));
    for field in fields {
        text.push_str(&format!(
            "    {}: {},\n",
            escape(&snake_kebab(stem, &field.name)?),
            wit_type(&field.ty, stem, written, false)
        ));
    }
    text.push_str("  }\n");
    Ok(text)
}

/// Writes one capability as a function of its inputs returning `result<T, refusal>`, with
/// no caller parameter: the host knows the caller from its store. Inputs: the stem, the
/// kebab function name, the capability and the written used names. Output: the lines.
fn signature(
    stem: &str,
    function: &str,
    capability: &Capability,
    written: &Written,
) -> Result<String, CapgenError> {
    let params = capability
        .inputs
        .iter()
        .map(|input| {
            Ok(format!(
                "{}: {}",
                escape(&snake_kebab(stem, &input.name)?),
                wit_type(&input.ty, stem, written, true)
            ))
        })
        .collect::<Result<Vec<_>, CapgenError>>()?;
    let out = capability
        .output
        .as_ref()
        .map_or_else(|| "_".to_owned(), |ty| wit_type(ty, stem, written, true));
    let refusal = escape(&written[&(SDK.to_owned(), "refusal".to_owned())]);
    Ok(wrapped(
        &format!("  {}: func(", escape(function)),
        &params,
        &format!(") -> result<{out}, {refusal}>;"),
    ))
}

/// The WIT type of a `.cap` type: `text` is `string`, `bytes` is `list<u8>`, a named item
/// its kebab name, or its written name where the interface uses it. Inputs: the type, the
/// stem, the written used names and whether local items are used too. Output: the text.
fn wit_type(ty: &TypeRef, stem: &str, written: &Written, local_used: bool) -> String {
    match ty {
        TypeRef::U16 => "u16".to_owned(),
        TypeRef::U32 => "u32".to_owned(),
        TypeRef::U64 => "u64".to_owned(),
        TypeRef::Bool => "bool".to_owned(),
        TypeRef::Text => "string".to_owned(),
        TypeRef::Bytes => "list<u8>".to_owned(),
        TypeRef::List(inner) => format!("list<{}>", wit_type(inner, stem, written, local_used)),
        TypeRef::Option(inner) => {
            format!("option<{}>", wit_type(inner, stem, written, local_used))
        }
        TypeRef::Named(name) if !local_used => escape(&pascal_kebab(name)),
        TypeRef::Named(name) => escape(&written[&(stem.to_owned(), pascal_kebab(name))]),
        TypeRef::Foreign { stem: owner, name } => {
            escape(&written[&(owner.clone(), pascal_kebab(name))])
        }
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

/// The foreign ref a type names through its lists and options, if any. Input: the type.
/// Output: the ref's stem and name.
fn foreign_in(ty: &TypeRef) -> Option<(&str, &str)> {
    match ty {
        TypeRef::List(inner) | TypeRef::Option(inner) => foreign_in(inner),
        TypeRef::Foreign { stem, name } => Some((stem.as_str(), name.as_str())),
        _ => None,
    }
}

/// The local name a type names through its lists and options, if any. Input: the type.
/// Output: the named item's name.
fn named_in(ty: &TypeRef) -> Option<&str> {
    match ty {
        TypeRef::List(inner) | TypeRef::Option(inner) => named_in(inner),
        TypeRef::Named(name) => Some(name.as_str()),
        _ => None,
    }
}

/// The type refs a file's records and event bodies name. Input: the file. Output: the refs.
fn field_refs(file: &File) -> impl Iterator<Item = &TypeRef> {
    let events = file
        .capabilities
        .iter()
        .flat_map(|capability| capability.emits.iter())
        .flat_map(|event| event.fields.iter().map(|field| &field.ty));
    file.types
        .iter()
        .flat_map(|item| item.fields.iter().map(|field| &field.ty))
        .chain(events)
}

/// The type refs a file's functions name: capability inputs and outputs, never event
/// fields. Input: the file. Output: the refs.
fn call_refs(file: &File) -> impl Iterator<Item = &TypeRef> {
    file.capabilities.iter().flat_map(|capability| {
        capability
            .inputs
            .iter()
            .map(|field| &field.ty)
            .chain(capability.output.as_ref())
    })
}
