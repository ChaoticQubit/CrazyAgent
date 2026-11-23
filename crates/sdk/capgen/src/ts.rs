use std::collections::{BTreeMap, BTreeSet};

use crazyagent_cap::{Capability, Enum, Event, Field, File, Handle, Tier, Type, TypeRef};

use crate::CapgenError;
use crate::rust::pascal;

/// The longest line capgen writes; a longer union or list is written one entry per line.
const LINE_MAX: usize = 100;

/// Names generated TypeScript cannot carry as a type, function, parameter or import alias:
/// ECMAScript and TS reserved and strict-mode words, the globals the output uses, the
/// `core.ts` names, and the stem `index`, which the module list takes.
pub(crate) const TS_RESERVED: [&str; 82] = [
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "let",
    "static",
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
    "await",
    "async",
    "of",
    "type",
    "any",
    "unknown",
    "never",
    "undefined",
    "object",
    "string",
    "number",
    "boolean",
    "bigint",
    "symbol",
    "arguments",
    "eval",
    "Promise",
    "Uint8Array",
    "Readonly",
    "Array",
    "Error",
    "TypeError",
    "RangeError",
    "BigInt",
    "Capability",
    "Host",
    "Refusal",
    "Result",
    "Tier",
    "Wire",
    "WireReply",
    "call",
    "encodeU64",
    "decodeU64",
    "encodeBytes",
    "decodeBytes",
    "index",
];

/// Writes the TS SDK files for the parsed `.cap` files after every check the Rust SDK runs:
/// `index.ts` and one module per file. Input: stems with parsed files, any order.
/// Output: the files sorted by path, or the first refusal.
pub(crate) fn files(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    crate::rust::files(files)?;
    let mut sorted: Vec<&(&str, File)> = files.iter().collect();
    sorted.sort_by_key(|(stem, _)| *stem);
    let mut out = Vec::with_capacity(sorted.len() + 1);
    let mut consts = Vec::new();
    for (stem, file) in &sorted {
        let calls = calls(stem, file)?;
        consts.extend(calls.iter().map(|call| format!("{stem}.{}", call.constant)));
        out.push((format!("{stem}.ts"), module(stem, file, &calls)));
    }
    out.push(("index.ts".to_owned(), index(&sorted, &consts)));
    out.sort();
    assert_eq!(out.len(), sorted.len() + 1);
    assert!(out.windows(2).all(|pair| pair[0].0 != pair[1].0));
    assert!(
        out.iter()
            .all(|(_, text)| text.ends_with('\n') && !text.contains('\t'))
    );
    Ok(out)
}

/// One capability as generated TS names it: its camelCase function, its facts const and
/// the capability itself.
struct Call<'f> {
    function: String,
    constant: String,
    capability: &'f Capability,
}

/// Names every capability of a file after refusing each name TS cannot carry, the event
/// union named like an item or kind, and each function or const name taken twice: by
/// another capability, a generated codec (`encode<T>`, `decode<T>` of a mapped type or
/// event), the event decoder and kinds, or a foreign stem's import alias. Inputs: the stem
/// and the file. Output: the calls in `.cap` order, or `capgen_reserved_name` /
/// `capgen_name_collision`.
fn calls<'f>(stem: &str, file: &'f File) -> Result<Vec<Call<'f>>, CapgenError> {
    check_names(stem, file)?;
    let mapped = mapped(file);
    let mut taken: BTreeSet<String> = mapped
        .iter()
        .filter(|(_, mapped)| **mapped)
        .flat_map(|(name, _)| [format!("encode{name}"), format!("decode{name}")])
        .collect();
    taken.extend(
        foreign_stems(called_refs(file))
            .into_iter()
            .map(str::to_owned),
    );
    let events = crate::rust_wire::events(file);
    if !events.is_empty() {
        let union = union_name(stem);
        let mut items = file
            .enums
            .iter()
            .map(|item| &item.name)
            .chain(file.handles.iter().map(|item| &item.name))
            .chain(file.types.iter().map(|item| &item.name))
            .chain(events.iter().map(|event| &event.kind));
        if items.any(|name| *name == union) {
            return Err(refuse(stem, "capgen_name_collision", union));
        }
        taken.insert(format!("decode{union}"));
        taken.insert(kinds_name(stem));
        taken.extend(
            events
                .iter()
                .filter(|event| event_mapped(event, &mapped))
                .map(|event| format!("decode{}", event.kind)),
        );
    }
    let mut calls = Vec::with_capacity(file.capabilities.len());
    for capability in &file.capabilities {
        let rest: Vec<&str> = capability.name.split('.').skip(1).collect();
        assert!(!rest.is_empty());
        let snake = rest.join("_");
        let function = camel(&snake);
        reserved(stem, &function)?;
        let constant = snake.to_ascii_uppercase();
        for name in [&function, &constant] {
            if taken.contains(name) {
                return Err(refuse(stem, "capgen_name_collision", name.clone()));
            }
        }
        taken.insert(function.clone());
        calls.push(Call {
            function,
            constant,
            capability,
        });
    }
    assert_eq!(calls.len(), file.capabilities.len());
    Ok(calls)
}

/// Refuses the stem, every enum, handle, type and event kind name, and every capability
/// input name that TS cannot carry. Inputs: the stem and the file. Output: nothing or the
/// refusal.
fn check_names(stem: &str, file: &File) -> Result<(), CapgenError> {
    reserved(stem, stem)?;
    let items = file
        .enums
        .iter()
        .map(|item| &item.name)
        .chain(file.handles.iter().map(|item| &item.name))
        .chain(file.types.iter().map(|item| &item.name))
        .chain(
            file.capabilities
                .iter()
                .flat_map(|capability| capability.emits.iter().map(|event| &event.kind)),
        );
    let inputs = file
        .capabilities
        .iter()
        .flat_map(|capability| capability.inputs.iter().map(|input| &input.name));
    for name in items.chain(inputs) {
        reserved(stem, name)?;
    }
    Ok(())
}

/// Refuses one name TS cannot carry. Inputs: the stem and the name. Output: nothing or
/// `capgen_reserved_name` with the detail `ts: <name>`.
fn reserved(stem: &str, name: &str) -> Result<(), CapgenError> {
    if TS_RESERVED.contains(&name) {
        return Err(refuse(stem, "capgen_reserved_name", format!("ts: {name}")));
    }
    Ok(())
}

/// Builds a refusal of a file. Inputs: the stem, the code and the detail. Output: the refusal.
fn refuse(stem: &str, code: &'static str, detail: String) -> CapgenError {
    CapgenError {
        file: format!("{stem}.cap"),
        code,
        detail,
    }
}

/// The camelCase name of a snake name: its Pascal name with the first letter lower.
/// Input: the snake name. Output: the camelCase name.
fn camel(snake: &str) -> String {
    let pascal = pascal(snake);
    let mut chars = pascal.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_lowercase().to_string() + chars.as_str()
    })
}

/// Whether each type of a file is mapped, holding `u64` or `bytes` at any depth, computed
/// once per type after the types its fields name, so the work stays linear in the fields.
/// Input: the file, whose types the grammar keeps acyclic. Output: each type's answer.
fn mapped(file: &File) -> BTreeMap<&str, bool> {
    let types: BTreeMap<&str, &Type> = file
        .types
        .iter()
        .map(|item| (item.name.as_str(), item))
        .collect();
    let mut done = BTreeMap::new();
    for item in &file.types {
        fill(&types, &mut done, item.name.as_str(), 0);
    }
    assert_eq!(done.len(), types.len());
    done
}

/// Computes one type's mapped answer after the types its fields name, once each.
/// Inputs: the file's types, the answers so far, the name and the walk's depth.
/// Output: the answers grow.
fn fill<'f>(
    types: &BTreeMap<&'f str, &'f Type>,
    done: &mut BTreeMap<&'f str, bool>,
    name: &'f str,
    depth: usize,
) {
    assert!(depth <= types.len());
    if done.contains_key(name) {
        return;
    }
    let item = *types.get(name).expect("fill walks declared types");
    for field in &item.fields {
        if let Some((key, _)) = named_in(&field.ty).and_then(|inner| types.get_key_value(inner)) {
            fill(types, done, key, depth + 1);
        }
    }
    let answer = item.fields.iter().any(|field| holds_wide(&field.ty, done));
    done.insert(name, answer);
}

/// Whether a type holds `u64` or `bytes`, reading named types' answers. Inputs: the type
/// and the answers, which hold every type it names. Output: the answer; enums, handles
/// and foreign leaves never do.
fn holds_wide(ty: &TypeRef, done: &BTreeMap<&str, bool>) -> bool {
    match ty {
        TypeRef::U64 | TypeRef::Bytes => true,
        TypeRef::U16 | TypeRef::U32 | TypeRef::Bool | TypeRef::Text => false,
        TypeRef::List(inner) | TypeRef::Option(inner) => holds_wide(inner, done),
        TypeRef::Named(name) => done.get(name.as_str()).copied().unwrap_or(false),
        TypeRef::Foreign { .. } => false,
    }
}

/// The name a type refers to through its lists and options, if any. Input: the type.
/// Output: the named item's name.
fn named_in(ty: &TypeRef) -> Option<&str> {
    match ty {
        TypeRef::List(inner) | TypeRef::Option(inner) => named_in(inner),
        TypeRef::Named(name) => Some(name.as_str()),
        _ => None,
    }
}

/// The type refs generated TS writes for a file: type fields, capability inputs and
/// outputs, and event fields. Input: the file. Output: the refs.
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
            .chain(
                capability
                    .emits
                    .iter()
                    .flat_map(|event| event.fields.iter().map(|field| &field.ty)),
            )
    });
    fields.chain(calls)
}

/// The name of a file's event union: its Pascal stem and `Event`. Input: the stem.
/// Output: the name.
fn union_name(stem: &str) -> String {
    format!("{}Event", pascal(stem))
}

/// The name of a file's event kinds const: its upper-case stem and `_EVENT_KINDS`.
/// Input: the stem. Output: the name.
fn kinds_name(stem: &str) -> String {
    format!("{}_EVENT_KINDS", stem.to_ascii_uppercase())
}

/// Whether an event body holds `u64` or `bytes` in a field, so it gets a decoder.
/// Inputs: the event and the file's mapped answers. Output: the answer.
fn event_mapped(event: &Event, mapped: &BTreeMap<&str, bool>) -> bool {
    event
        .fields
        .iter()
        .any(|field| holds_wide(&field.ty, mapped))
}

/// The foreign stems a set of type refs names, sorted. Input: the refs. Output: the stems.
fn foreign_stems<'f>(refs: impl Iterator<Item = &'f TypeRef>) -> BTreeSet<&'f str> {
    refs.filter_map(foreign_stem).collect()
}

/// The stem a type names through its lists and options, if it is a foreign ref.
/// Input: the type. Output: the stem.
fn foreign_stem(ty: &TypeRef) -> Option<&str> {
    match ty {
        TypeRef::List(inner) | TypeRef::Option(inner) => foreign_stem(inner),
        TypeRef::Foreign { stem, .. } => Some(stem.as_str()),
        _ => None,
    }
}

/// The `core.ts` names one module uses, so it imports those and no others.
#[derive(Default)]
struct Uses {
    types: BTreeSet<&'static str>,
    values: BTreeSet<&'static str>,
}

/// Writes one file's module: its imports, enums, handles, types each with its codecs when
/// mapped, per capability its facts and call function, then its events, one blank line
/// between items. Inputs: the stem, the file and its calls. Output: the text.
fn module(stem: &str, file: &File, calls: &[Call<'_>]) -> String {
    let mapped = mapped(file);
    let mut uses = Uses::default();
    let mut blocks = Vec::new();
    blocks.extend(file.enums.iter().map(|item| enumeration(stem, item)));
    blocks.extend(file.handles.iter().map(|item| handle(stem, item)));
    for item in &file.types {
        blocks.push(record(stem, item));
        if mapped[item.name.as_str()] {
            blocks.push(encoder(stem, item, &mapped, &mut uses));
            blocks.push(decoder(stem, &item.name, &item.fields, &mapped, &mut uses));
        }
    }
    for call in calls {
        blocks.push(facts(stem, call));
        blocks.push(function(stem, call, &mapped, &mut uses));
    }
    let events = crate::rust_wire::events(file);
    if !events.is_empty() {
        uses.types.insert("Wire");
        for event in &events {
            blocks.push(event_type(event));
            if event_mapped(event, &mapped) {
                blocks.push(decoder(
                    stem,
                    &event.kind,
                    &event.fields,
                    &mapped,
                    &mut uses,
                ));
            }
        }
        blocks.push(event_union(stem, &events));
        blocks.push(event_kinds(stem, &events));
        blocks.push(event_decoder(stem, &events, &mapped));
    }
    if !calls.is_empty() {
        uses.types.extend(["Capability", "Host", "Result"]);
        uses.values.insert("call");
    }
    let mut text = format!("/** Generated by capgen from {stem}.cap. */\n");
    for (kind, names) in [("import type", &uses.types), ("import", &uses.values)] {
        if names.is_empty() {
            continue;
        }
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        text.push_str(&wrapped(
            &format!("{kind} {{ "),
            &format!("{kind} {{"),
            &names,
            " } from \"../core.ts\";",
            "} from \"../core.ts\";",
        ));
    }
    for foreign in foreign_stems(called_refs(file)) {
        text.push_str(&format!(
            "import type * as {foreign} from \"./{foreign}.ts\";\n"
        ));
    }
    for block in &blocks {
        text.push('\n');
        text.push_str(block);
    }
    text
}

/// Writes one enum as a union of its exact `.cap` variants. Inputs: the stem and the enum.
/// Output: the block.
fn enumeration(stem: &str, item: &Enum) -> String {
    assert!(!item.variants.is_empty());
    let mut text = format!(
        "/** Generated by capgen from {stem}.cap: enum {}. */\n",
        item.name
    );
    let variants: Vec<String> = item
        .variants
        .iter()
        .map(|variant| format!("\"{variant}\""))
        .collect();
    let line = format!("export type {} = {};", item.name, variants.join(" | "));
    if line.len() <= LINE_MAX {
        text.push_str(&line);
        text.push('\n');
        return text;
    }
    text.push_str(&format!("export type {} =\n", item.name));
    for (index, variant) in variants.iter().enumerate() {
        let end = if index + 1 == variants.len() { ";" } else { "" };
        text.push_str(&format!("  | {variant}{end}\n"));
    }
    text
}

/// Writes one handle as a branded string, the kernel id a plugin holds and passes back,
/// never a value it can read or forge. Inputs: the stem and the handle. Output: the block.
fn handle(stem: &str, item: &Handle) -> String {
    format!(
        "/** Generated by capgen from {stem}.cap: handle {name}, a kernel id. */\n\
         export type {name} = string & {{ readonly __handle: \"{stem}.{name}\" }};\n",
        name = item.name
    )
}

/// Writes one record type with its exact `.cap` field names, one field per line.
/// Inputs: the stem and the type. Output: the block.
fn record(stem: &str, item: &Type) -> String {
    let mut text = format!(
        "/** Generated by capgen from {stem}.cap: type {}. */\n",
        item.name
    );
    text.push_str(&format!("export type {} = {{\n", item.name));
    for field in &item.fields {
        text.push_str(&format!(
            "  readonly {}: {};\n",
            field.name,
            ts_type(&field.ty)
        ));
    }
    text.push_str("};\n");
    text
}

/// Writes a capability's facts as a `Capability` const. Inputs: the stem and the call.
/// Output: the block.
fn facts(stem: &str, call: &Call<'_>) -> String {
    let tier = match call.capability.tier {
        Tier::Kernel => "kernel",
        Tier::T1 => "t1",
        Tier::T2 => "t2",
        Tier::T3 => "t3",
    };
    format!(
        "/** Generated by capgen from {stem}.cap: the facts of capability {name}. */\n\
         export const {constant}: Capability = {{\n  name: \"{name}\",\n  tier: \"{tier}\",\n  needs: \"{needs}\",\n}};\n",
        name = call.capability.name,
        constant = call.constant,
        needs = call.capability.needs,
    )
}

/// The TS type of a `.cap` type, by the plan's mapping table; a list element that is itself
/// a list or an option is wrapped in parentheses. Input: the type. Output: the TS text.
fn ts_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::U16 | TypeRef::U32 => "number".to_owned(),
        TypeRef::U64 => "bigint".to_owned(),
        TypeRef::Bool => "boolean".to_owned(),
        TypeRef::Text => "string".to_owned(),
        TypeRef::Bytes => "Uint8Array".to_owned(),
        TypeRef::List(inner) => match **inner {
            TypeRef::List(_) | TypeRef::Option(_) => format!("readonly ({})[]", ts_type(inner)),
            _ => format!("readonly {}[]", ts_type(inner)),
        },
        TypeRef::Option(inner) => {
            assert!(!matches!(**inner, TypeRef::Option(_)));
            format!("{} | null", ts_type(inner))
        }
        TypeRef::Named(name) => name.clone(),
        TypeRef::Foreign { stem, name } => format!("{stem}.{name}"),
    }
}

/// Writes `index.ts`: every file's module and `CAPABILITIES`, every capability's const
/// once, in file then `.cap` order. Inputs: the sorted files and the qualified consts.
/// Output: the text.
fn index(files: &[&(&str, File)], consts: &[String]) -> String {
    let mut text = String::from(
        "/** Generated by capgen: one module per `.cap` file, and every capability once. */\n",
    );
    text.push_str("import type { Capability } from \"../core.ts\";\n");
    let stems: Vec<String> = files.iter().map(|(stem, _)| (*stem).to_owned()).collect();
    for stem in &stems {
        text.push_str(&format!("import * as {stem} from \"./{stem}.ts\";\n"));
    }
    text.push('\n');
    if stems.is_empty() {
        text.push_str("export {};\n");
    } else {
        text.push_str(&wrapped("export { ", "export {", &stems, " };", "};"));
    }
    text.push_str(
        "\n/** Generated by capgen: every capability, once, in file then `.cap` order. */\n",
    );
    let head = "export const CAPABILITIES: readonly Capability[] = [";
    text.push_str(&wrapped(head, head, consts, "];", "];"));
    text
}

/// Writes entries on one line between an inline head and tail when it fits `LINE_MAX`,
/// else one entry per line, indented two spaces with a trailing comma, between the
/// vertical head and tail. Inputs: both heads, the entries and both tails. Output: the text.
fn wrapped(inline: &str, head: &str, entries: &[String], tail_inline: &str, tail: &str) -> String {
    let line = format!("{inline}{}{tail_inline}", entries.join(", "));
    if line.len() <= LINE_MAX || entries.is_empty() {
        return format!("{line}\n");
    }
    let mut text = format!("{head}\n");
    for entry in entries {
        text.push_str(&format!("  {entry},\n"));
    }
    text.push_str(tail);
    text.push('\n');
    text
}

/// Writes a mapped record's encoder: one line per field, a mapped field through its codec,
/// any other copied. Inputs: the stem, the type, the file's mapped answers and the uses.
/// Output: the block; the uses grow.
fn encoder(stem: &str, item: &Type, mapped: &BTreeMap<&str, bool>, uses: &mut Uses) -> String {
    uses.types.insert("Wire");
    let name = &item.name;
    let mut text = format!(
        "/** Generated by capgen from {stem}.cap: writes a {name} as its wire value.\n \
         * Input: the value. Output: its wire value. */\n\
         export function encode{name}(value: {name}): Wire {{\n  return {{\n"
    );
    for field in &item.fields {
        let value = format!("value.{}", field.name);
        text.push_str(&format!(
            "    {}: {},\n",
            field.name,
            encode_apply(&field.ty, &value, mapped, uses)
        ));
    }
    text.push_str("  };\n}\n");
    text
}

/// Writes a mapped record's or event body's decoder: an object guard, then one line per
/// field, a mapped field through its codec, any other cast. Inputs: the stem, the name, its
/// fields, the file's mapped answers and the uses. Output: the block; the uses grow.
fn decoder(
    stem: &str,
    name: &str,
    fields: &[Field],
    mapped: &BTreeMap<&str, bool>,
    uses: &mut Uses,
) -> String {
    uses.types.insert("Wire");
    let mut text = format!(
        "/** Generated by capgen from {stem}.cap: reads a {name} the kernel sent.\n \
         * Input: the wire value. Output: the value; a value off the wire contract throws. */\n\
         export function decode{name}(value: Wire): {name} {{\n  \
         if (typeof value !== \"object\" || value === null || Array.isArray(value)) {{\n    \
         throw new TypeError(\"sdk_wire_{snake}\");\n  }}\n  \
         const record = value as {{ readonly [key: string]: Wire }};\n  return {{\n",
        snake = snake(name)
    );
    for field in fields {
        let value = format!("record.{}", field.name);
        text.push_str(&format!(
            "    {}: {},\n",
            field.name,
            decode_apply(&field.ty, &value, mapped, uses)
        ));
    }
    text.push_str("  };\n}\n");
    text
}

/// Writes one event body as a record type with its exact `.cap` field names, on one line
/// when it fits. Input: the event. Output: the block.
fn event_type(event: &Event) -> String {
    let kind = &event.kind;
    let fields: Vec<String> = event
        .fields
        .iter()
        .map(|field| format!("readonly {}: {}", field.name, ts_type(&field.ty)))
        .collect();
    assert!(!fields.is_empty());
    let article = if kind.starts_with(['A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    };
    let mut text = format!("/** The body of {article} `{kind}` event. */\n");
    let line = format!("export type {kind} = {{ {} }};", fields.join("; "));
    if line.len() <= LINE_MAX {
        text.push_str(&line);
        text.push('\n');
        return text;
    }
    text.push_str(&format!("export type {kind} = {{\n"));
    for field in &fields {
        text.push_str(&format!("  {field};\n"));
    }
    text.push_str("};\n");
    text
}

/// Writes a file's event union, one arm per kind, on one line when it fits. Inputs: the
/// stem and its events. Output: the block.
fn event_union(stem: &str, events: &[&Event]) -> String {
    let union = union_name(stem);
    let arms: Vec<String> = events
        .iter()
        .map(|event| {
            format!(
                "{{ readonly kind: \"{kind}\"; readonly body: {kind} }}",
                kind = event.kind
            )
        })
        .collect();
    let mut text = format!("/** One event of the `{stem}` stem, by kind. */\n");
    let line = format!("export type {union} = {};", arms.join(" | "));
    if line.len() <= LINE_MAX {
        text.push_str(&line);
        text.push('\n');
        return text;
    }
    text.push_str(&format!("export type {union} =\n"));
    for (index, arm) in arms.iter().enumerate() {
        let end = if index + 1 == arms.len() { ";" } else { "" };
        text.push_str(&format!("  | {arm}{end}\n"));
    }
    text
}

/// Writes a file's event kinds as a readonly tuple const. Inputs: the stem and its events.
/// Output: the block.
fn event_kinds(stem: &str, events: &[&Event]) -> String {
    let head = format!("export const {} = [", kinds_name(stem));
    let entries: Vec<String> = events
        .iter()
        .map(|event| format!("\"{}\"", event.kind))
        .collect();
    let mut text = format!("/** The event kinds the `{stem}` stem emits. */\n");
    text.push_str(&wrapped(
        &head,
        &head,
        &entries,
        "] as const;",
        "] as const;",
    ));
    text
}

/// Writes a file's event decoder: per kind, the body cast, or through its decoder when
/// mapped, and `null` for any other kind. Inputs: the stem, its events and the file's
/// mapped answers. Output: the block.
fn event_decoder(stem: &str, events: &[&Event], mapped: &BTreeMap<&str, bool>) -> String {
    let union = union_name(stem);
    let mut text = format!(
        "/** Decodes one event body of the `{stem}` stem. Inputs: the kind and the body as the\n \
         * kernel sent it. Output: the typed event, or `null` for a kind this stem does not emit. */\n\
         export function decode{union}(kind: string, body: Wire): {union} | null {{\n  switch (kind) {{\n"
    );
    for event in events {
        let kind = &event.kind;
        let body = if event_mapped(event, mapped) {
            format!("decode{kind}(body)")
        } else {
            format!("body as {kind}")
        };
        text.push_str(&format!(
            "    case \"{kind}\":\n      return {{ kind: \"{kind}\", body: {body} }};\n"
        ));
    }
    text.push_str("    default:\n      return null;\n  }\n}\n");
    text
}

/// Writes a capability's call function over the bound host: mapped inputs through their
/// encoders, the output through its decoder. Inputs: the stem, the call, the file's mapped
/// answers and the uses. Output: the block; the uses grow.
fn function(stem: &str, call: &Call<'_>, mapped: &BTreeMap<&str, bool>, uses: &mut Uses) -> String {
    let capability = call.capability;
    let mut text = format!(
        "/** Generated by capgen from {stem}.cap: calls {} through the bus.\n \
         * Inputs: the host the kernel bound to this plugin and the call's inputs.\n \
         * Output: the call's result, or the bus refusal. */\n",
        capability.name
    );
    let out = capability
        .output
        .as_ref()
        .map_or_else(|| "null".to_owned(), ts_type);
    let params: Vec<String> = std::iter::once("host: Host".to_owned())
        .chain(
            capability
                .inputs
                .iter()
                .map(|input| format!("{}: {}", input.name, ts_type(&input.ty))),
        )
        .collect();
    text.push_str(&wrapped(
        &format!("export function {}(", call.function),
        &format!("export function {}(", call.function),
        &params,
        &format!("): Promise<Result<{out}>> {{"),
        &format!("): Promise<Result<{out}>> {{"),
    ));
    let entries: Vec<String> = capability
        .inputs
        .iter()
        .map(|input| {
            if holds_wide(&input.ty, mapped) {
                format!(
                    "{}: {}",
                    input.name,
                    encode_apply(&input.ty, &input.name, mapped, uses)
                )
            } else {
                input.name.clone()
            }
        })
        .collect();
    let decode = match &capability.output {
        None => "() => null".to_owned(),
        Some(ty) if holds_wide(ty, mapped) => decode_fn(ty, "value", mapped, uses),
        Some(ty) => format!("(value) => value as {}", ts_type(ty)),
    };
    let object = if entries.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{ {} }}", entries.join(", "))
    };
    let line = format!(
        "  return call(host, {}, {object}, {decode});",
        call.constant
    );
    if line.len() <= LINE_MAX {
        text.push_str(&line);
        text.push('\n');
    } else {
        text.push_str(&format!(
            "  return call(\n    host,\n    {},\n",
            call.constant
        ));
        if entries.is_empty() {
            text.push_str("    {},\n");
        } else {
            text.push_str("    {\n");
            for entry in &entries {
                text.push_str(&format!("      {entry},\n"));
            }
            text.push_str("    },\n");
        }
        text.push_str(&format!("    {decode},\n  );\n"));
    }
    text.push_str("}\n");
    text
}

/// The expression that writes one value as its wire value: a mapped value through its
/// codec, a list through `.map`, an option through a null check, any other as itself.
/// Inputs: the type, the value's expression, the mapped answers and the uses.
/// Output: the expression; the uses grow.
fn encode_apply(
    ty: &TypeRef,
    value: &str,
    mapped: &BTreeMap<&str, bool>,
    uses: &mut Uses,
) -> String {
    if !holds_wide(ty, mapped) {
        return value.to_owned();
    }
    match ty {
        TypeRef::List(inner) => format!("{value}.map({})", encode_fn(inner, mapped, uses)),
        TypeRef::Option(inner) => format!(
            "{value} === null ? null : {}",
            encode_apply(inner, value, mapped, uses)
        ),
        _ => format!("{}({value})", encode_fn(ty, mapped, uses)),
    }
}

/// The function that writes one mapped value as its wire value: a codec by name, or an
/// arrow over `item` for a list or an option. Inputs: the mapped type, the mapped answers
/// and the uses. Output: the function expression; the uses grow.
fn encode_fn(ty: &TypeRef, mapped: &BTreeMap<&str, bool>, uses: &mut Uses) -> String {
    assert!(holds_wide(ty, mapped));
    match ty {
        TypeRef::U64 => {
            uses.values.insert("encodeU64");
            "encodeU64".to_owned()
        }
        TypeRef::Bytes => {
            uses.values.insert("encodeBytes");
            "encodeBytes".to_owned()
        }
        TypeRef::Named(name) => format!("encode{name}"),
        _ => format!("(item) => {}", encode_apply(ty, "item", mapped, uses)),
    }
}

/// The expression that reads one value from its wire value: a mapped value through its
/// codec, a list through `.map`, an option through a null check, any other cast to its type,
/// since the kernel checked the output. Inputs: the type, the wire expression, the mapped
/// answers and the uses. Output: the expression; the uses grow.
fn decode_apply(
    ty: &TypeRef,
    value: &str,
    mapped: &BTreeMap<&str, bool>,
    uses: &mut Uses,
) -> String {
    if !holds_wide(ty, mapped) {
        return format!("{value} as {}", ts_type(ty));
    }
    match ty {
        TypeRef::List(inner) => {
            uses.types.insert("Wire");
            format!(
                "({value} as readonly Wire[]).map({})",
                decode_fn(inner, "item", mapped, uses)
            )
        }
        TypeRef::Option(inner) => format!(
            "{value} === null ? null : {}",
            decode_apply(inner, value, mapped, uses)
        ),
        _ => format!("{}({value})", decode_fn(ty, value, mapped, uses)),
    }
}

/// The function that reads one mapped value from its wire value: a codec by name, or an
/// arrow over the named parameter for a list or an option. Inputs: the mapped type, the
/// parameter's name, the mapped answers and the uses. Output: the function expression.
fn decode_fn(ty: &TypeRef, param: &str, mapped: &BTreeMap<&str, bool>, uses: &mut Uses) -> String {
    assert!(holds_wide(ty, mapped));
    match ty {
        TypeRef::U64 => {
            uses.values.insert("decodeU64");
            "decodeU64".to_owned()
        }
        TypeRef::Bytes => {
            uses.values.insert("decodeBytes");
            "decodeBytes".to_owned()
        }
        TypeRef::Named(name) => format!("decode{name}"),
        _ => format!("({param}) => {}", decode_apply(ty, param, mapped, uses)),
    }
}

/// The snake name of a Pascal type name, for its wire refusal code: a `_` before each
/// upper-case letter after the first, all lower. Input: the Pascal name. Output: the name.
fn snake(pascal: &str) -> String {
    let mut snake = String::with_capacity(pascal.len() + 8);
    for (index, character) in pascal.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            snake.push('_');
        }
        snake.push(character.to_ascii_lowercase());
    }
    snake
}
