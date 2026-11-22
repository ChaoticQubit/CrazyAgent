use std::collections::BTreeSet;

use crazyagent_cap::{Capability, Enum, File, Tier, Type, TypeRef};

use crate::{CAP_FILES_MAX, CapgenError};

/// The longest line rustfmt keeps on one line; a longer list is written one entry per line.
const LINE_MAX: usize = 100;

/// Names generated Rust cannot carry: Rust 2024 strict and reserved keywords, path roots,
/// the prelude names the output uses, the SDK's own names, and the host parameter.
const RESERVED: [&str; 61] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen", "Option", "Result",
    "String", "Vec", "Box", "Some", "None", "Ok", "Err",
];

/// Names the generated modules import or define, also refused as `.cap` names.
const SDK_NAMES: [&str; 8] = [
    "Host",
    "Refusal",
    "Tier",
    "Capability",
    "Provide",
    "Serialize",
    "Deserialize",
    "host",
];

/// Writes the Rust SDK files for the parsed `.cap` files: `mod.rs`, `host.rs` and one module
/// per file. Input: stems with parsed files. Output: the files sorted by path, or the first
/// refusal.
pub(crate) fn files(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    if files.len() > CAP_FILES_MAX {
        return Err(refuse("", "capgen_too_many_files", files.len().to_string()));
    }
    let mut sorted: Vec<&(&str, File)> = files.iter().collect();
    sorted.sort_by_key(|(stem, _)| *stem);
    let mut methods = BTreeSet::new();
    let mut calls = Vec::new();
    for (stem, file) in &sorted {
        check_names(stem, file)?;
        for capability in &file.capabilities {
            let call = Call::of(stem, capability)?;
            if !methods.insert(call.method.clone()) {
                return Err(refuse(stem, "capgen_name_collision", call.method));
            }
            calls.push(call);
        }
    }
    let mut out = vec![
        ("mod.rs".to_owned(), modules(&sorted, &calls)),
        ("host.rs".to_owned(), host(&sorted, &calls)),
    ];
    for (stem, file) in &sorted {
        let own: Vec<&Call> = calls.iter().filter(|call| call.stem == *stem).collect();
        out.push((format!("{stem}.rs"), module(stem, file, &own)));
    }
    out.sort();
    assert_eq!(out.len(), sorted.len() + 2);
    assert!(out.windows(2).all(|pair| pair[0].0 != pair[1].0));
    Ok(out)
}

/// One capability as generated Rust names it: its file, the `.cap` name, the function and
/// const names in its module, the `Host` method name, and the capability itself.
struct Call<'f> {
    stem: &'f str,
    function: String,
    method: String,
    capability: &'f Capability,
}

impl<'f> Call<'f> {
    /// Names a capability of a file: `<stem>.<a>.<b>` becomes function `a_b` and method
    /// `<stem>_a_b`. Inputs: the file's stem and the capability. Output: the names, or
    /// `capgen_name_outside_file` / `capgen_reserved_name`.
    fn of(stem: &'f str, capability: &'f Capability) -> Result<Self, CapgenError> {
        let mut segments = capability.name.split('.');
        if segments.next() != Some(stem) {
            return Err(refuse(
                stem,
                "capgen_name_outside_file",
                capability.name.clone(),
            ));
        }
        let rest: Vec<&str> = segments.collect();
        assert!(!rest.is_empty());
        for segment in &rest {
            reserved(stem, segment)?;
        }
        let function = rest.join("_");
        reserved(stem, &function)?;
        Ok(Self {
            stem,
            method: format!("{stem}_{function}"),
            function,
            capability,
        })
    }
}

/// Refuses every name of a file that generated Rust cannot carry, and enum variants whose
/// Rust names come out equal. Inputs: the stem and the file. Output: nothing or the refusal.
fn check_names(stem: &str, file: &File) -> Result<(), CapgenError> {
    reserved(stem, stem)?;
    for item in &file.enums {
        reserved(stem, &item.name)?;
        let mut seen = BTreeSet::new();
        for variant in &item.variants {
            reserved(stem, variant)?;
            let pascal = pascal(variant);
            reserved(stem, &pascal)?;
            if !seen.insert(pascal.clone()) {
                return Err(refuse(stem, "capgen_name_collision", pascal));
            }
        }
    }
    for item in &file.types {
        reserved(stem, &item.name)?;
        for field in &item.fields {
            reserved(stem, &field.name)?;
        }
    }
    for capability in &file.capabilities {
        for input in &capability.inputs {
            reserved(stem, &input.name)?;
        }
    }
    Ok(())
}

/// Refuses one reserved name. Inputs: the stem and the name. Output: nothing or the refusal.
fn reserved(stem: &str, name: &str) -> Result<(), CapgenError> {
    if RESERVED.contains(&name) || SDK_NAMES.contains(&name) {
        return Err(refuse(stem, "capgen_reserved_name", name.to_owned()));
    }
    Ok(())
}

/// Builds a refusal of a file. Inputs: the stem, the code and the detail. Output: the refusal.
fn refuse(stem: &str, code: &'static str, detail: String) -> CapgenError {
    CapgenError {
        file: if stem.is_empty() {
            String::new()
        } else {
            format!("{stem}.cap")
        },
        code,
        detail,
    }
}

/// The Rust name of a snake variant: each `_`-separated part with its first letter upper.
/// Input: the variant. Output: its Pascal name.
fn pascal(snake: &str) -> String {
    snake
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect()
}

/// Writes `mod.rs`: the host module, one module per file, and `CAPABILITIES`, every
/// capability's const once, in file then `.cap` order, so no consumer keeps its own list.
/// Inputs: the sorted files and every capability in that order. Output: the text.
fn modules(files: &[&(&str, File)], calls: &[Call<'_>]) -> String {
    let mut text = String::new();
    text.push_str("/// Generated by capgen: the `Host` trait, one method per capability.\n");
    text.push_str("pub mod host;\n");
    for (stem, _) in files {
        text.push_str(&format!("/// Generated by capgen from {stem}.cap.\n"));
        text.push_str(&format!("pub mod {stem};\n"));
    }
    let consts: Vec<String> = calls
        .iter()
        .map(|call| format!("{}::{}", call.stem, call.function.to_ascii_uppercase()))
        .collect();
    text.push_str(
        "\n/// Generated by capgen: every capability, once, in file then `.cap` order.\n",
    );
    text.push_str(&format!(
        "pub const CAPABILITIES: [crate::Capability; {}] = [{}];\n",
        consts.len(),
        consts.join(", ")
    ));
    text
}

/// Writes `host.rs`: the `Host` trait with one typed method per capability.
/// Inputs: the sorted files and every capability's names. Output: the text.
fn host(files: &[&(&str, File)], calls: &[Call<'_>]) -> String {
    let mut text = String::new();
    let used: Vec<&str> = files
        .iter()
        .map(|(stem, _)| *stem)
        .filter(|stem| calls.iter().any(|call| call.stem == *stem))
        .collect();
    match used.as_slice() {
        [] => {}
        [one] => text.push_str(&format!("use super::{one};\n")),
        many => text.push_str(&format!("use super::{{{}}};\n", many.join(", "))),
    }
    if !calls.is_empty() {
        text.push_str("use crate::Refusal;\n");
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(
        "/// Generated by capgen: the calls a plugin can make, one method per capability.\n",
    );
    text.push_str(
        "/// Only the kernel's bus gateway implements it; each plugin gets a host bound to\n",
    );
    text.push_str("/// its own id, so a plugin never names itself or reaches a provider.\n");
    if calls.is_empty() {
        text.push_str("pub trait Host {}\n");
        return text;
    }
    text.push_str("pub trait Host {\n");
    for (index, call) in calls.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&format!(
            "    /// Calls {} ({}.cap). Inputs: the call's inputs. Output: its result or the refusal.\n",
            call.capability.name, call.stem
        ));
        let params: Vec<String> =
            std::iter::once("&self".to_owned())
                .chain(call.capability.inputs.iter().map(|input| {
                    format!("{}: {}", input.name, rust_type(&input.ty, Some(call.stem)))
                }))
                .collect();
        let tail = format!(
            ") -> Result<{}, Refusal>;",
            output(call.capability, Some(call.stem))
        );
        text.push_str(&list(1, &format!("fn {}(", call.method), &params, &tail));
    }
    text.push_str("}\n");
    text
}

/// Writes one file's module: its enums and types, then per capability its facts, the
/// `Provide` trait and the call functions. Inputs: the stem, the file and its calls.
/// Output: the text.
fn module(stem: &str, file: &File, calls: &[&Call<'_>]) -> String {
    let mut text = String::new();
    if !calls.is_empty() {
        text.push_str("use crate::{Capability, Host, Refusal, Tier};\n");
    }
    if !file.enums.is_empty() || !file.types.is_empty() {
        text.push_str("use serde::{Deserialize, Serialize};\n");
    }
    for item in &file.enums {
        enumeration(&mut text, stem, item);
    }
    for item in &file.types {
        record(&mut text, stem, item);
    }
    for call in calls {
        facts(&mut text, stem, call);
    }
    if !calls.is_empty() {
        provide(&mut text, stem, calls);
    }
    for call in calls {
        function(&mut text, stem, call);
    }
    if text.starts_with('\n') {
        text.remove(0);
    }
    text
}

/// Writes one enum with each variant's exact `.cap` name as its serde name.
/// Inputs: the text, the stem and the enum. Output: the text grows.
fn enumeration(text: &mut String, stem: &str, item: &Enum) {
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: enum {}.\n",
        item.name
    ));
    text.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n");
    text.push_str(&format!("pub enum {} {{\n", item.name));
    for variant in &item.variants {
        text.push_str(&format!("    #[serde(rename = \"{variant}\")]\n"));
        text.push_str(&format!("    {},\n", pascal(variant)));
    }
    text.push_str("}\n");
}

/// Writes one record type that refuses unknown fields. Inputs: the text, the stem and the
/// type. Output: the text grows.
fn record(text: &mut String, stem: &str, item: &Type) {
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: type {}.\n",
        item.name
    ));
    text.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n");
    text.push_str("#[serde(deny_unknown_fields)]\n");
    text.push_str(&format!("pub struct {} {{\n", item.name));
    for field in &item.fields {
        text.push_str(&format!(
            "    pub {}: {},\n",
            field.name,
            rust_type(&field.ty, None)
        ));
    }
    text.push_str("}\n");
}

/// Writes a capability's facts as a `Capability` const. Inputs: the text, the stem and the
/// call. Output: the text grows.
fn facts(text: &mut String, stem: &str, call: &Call<'_>) {
    let tier = match call.capability.tier {
        Tier::Kernel => "Kernel",
        Tier::T1 => "T1",
        Tier::T2 => "T2",
        Tier::T3 => "T3",
    };
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: the facts of capability {}.\n",
        call.capability.name
    ));
    text.push_str(&format!(
        "pub const {}: Capability = Capability {{\n",
        call.function.to_ascii_uppercase()
    ));
    text.push_str(&format!("    name: \"{}\",\n", call.capability.name));
    text.push_str(&format!("    tier: Tier::{tier},\n"));
    text.push_str(&format!("    needs: \"{}\",\n", call.capability.needs));
    text.push_str("};\n");
}

/// Writes the `Provide` trait a provider of the file's capabilities implements.
/// Inputs: the text, the stem and the calls. Output: the text grows.
fn provide(text: &mut String, stem: &str, calls: &[&Call<'_>]) {
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: what a provider of its capabilities implements.\n"
    ));
    text.push_str("pub trait Provide {\n");
    for (index, call) in calls.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&format!(
            "    /// Provides {}. Inputs: the call's inputs. Output: its result or the refusal.\n",
            call.capability.name
        ));
        let params: Vec<String> = std::iter::once("&self".to_owned())
            .chain(inputs(call.capability))
            .collect();
        let tail = format!(") -> Result<{}, Refusal>;", output(call.capability, None));
        text.push_str(&list(1, &format!("fn {}(", call.function), &params, &tail));
    }
    text.push_str("}\n");
}

/// Writes the function a plugin calls: it hands the call to its bound host.
/// Inputs: the text, the stem and the call. Output: the text grows.
fn function(text: &mut String, stem: &str, call: &Call<'_>) {
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: calls {} through the bus.\n",
        call.capability.name
    ));
    text.push_str("/// Inputs: the host the kernel bound to this plugin and the call's inputs.\n");
    text.push_str("/// Output: the call's result, or the bus refusal.\n");
    let params: Vec<String> = std::iter::once("host: &dyn Host".to_owned())
        .chain(inputs(call.capability))
        .collect();
    let tail = format!(") -> Result<{}, Refusal> {{", output(call.capability, None));
    text.push_str(&list(
        0,
        &format!("pub fn {}(", call.function),
        &params,
        &tail,
    ));
    let arguments: Vec<String> = call
        .capability
        .inputs
        .iter()
        .map(|input| input.name.clone())
        .collect();
    text.push_str(&list(1, &format!("host.{}(", call.method), &arguments, ")"));
    text.push_str("}\n");
}

/// The `name: Type` parameters of a capability's inputs, types named in their own module.
/// Input: the capability. Output: the parameters.
fn inputs(capability: &Capability) -> impl Iterator<Item = String> + '_ {
    capability
        .inputs
        .iter()
        .map(|input| format!("{}: {}", input.name, rust_type(&input.ty, None)))
}

/// The Rust type of a capability's output, `()` for `unit`. Inputs: the capability and the
/// module to qualify named types with, if any. Output: the type text.
fn output(capability: &Capability, module: Option<&str>) -> String {
    capability
        .output
        .as_ref()
        .map_or_else(|| "()".to_owned(), |ty| rust_type(ty, module))
}

/// The Rust type of a `.cap` type. Inputs: the type and the module to qualify a named type
/// with, if any. Output: the type text.
fn rust_type(ty: &TypeRef, module: Option<&str>) -> String {
    match (ty, module) {
        (TypeRef::U16, _) => "u16".to_owned(),
        (TypeRef::U32, _) => "u32".to_owned(),
        (TypeRef::U64, _) => "u64".to_owned(),
        (TypeRef::Bool, _) => "bool".to_owned(),
        (TypeRef::Named(name), None) => name.clone(),
        (TypeRef::Named(name), Some(module)) => format!("{module}::{name}"),
    }
}

/// Writes `head` + entries + `tail` on one line when it fits `LINE_MAX`, else rustfmt's
/// vertical form with one entry per line and a trailing comma. Inputs: the indent level,
/// the head, the entries and the tail. Output: the line or lines, ending in a newline.
fn list(indent: usize, head: &str, entries: &[String], tail: &str) -> String {
    let pad = "    ".repeat(indent);
    let line = format!("{pad}{head}{}{tail}\n", entries.join(", "));
    if line.len() <= LINE_MAX + 1 || entries.is_empty() {
        return line;
    }
    let mut text = format!("{pad}{head}\n");
    for entry in entries {
        text.push_str(&format!("{pad}    {entry},\n"));
    }
    text.push_str(&format!("{pad}{tail}\n"));
    text
}
