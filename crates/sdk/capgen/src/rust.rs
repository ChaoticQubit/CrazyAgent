use std::collections::{BTreeMap, BTreeSet};

use crazyagent_cap::{Capability, Enum, File, Handle, Tier, Type, TypeRef};

use crate::{CAP_FILES_MAX, CapgenError};

/// The longest line rustfmt keeps on one line; a longer list is written one entry per line.
const LINE_MAX: usize = 100;

/// Names generated Rust cannot carry: Rust 2024 strict and reserved keywords, path roots,
/// the prelude names the output uses, the SDK's own names, and the host parameter.
const RESERVED: [&str; 63] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen", "Option", "Result",
    "String", "Vec", "Box", "Some", "None", "Ok", "Err", "alloc", "core",
];

/// Names the generated modules import or define, also refused as `.cap` names, and the
/// caller parameter every `Provide` method takes first.
const SDK_NAMES: [&str; 9] = [
    "Host",
    "Refusal",
    "Tier",
    "Capability",
    "Provide",
    "Serialize",
    "Deserialize",
    "host",
    "caller",
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
        let leaves = resolve(stem, file, &sorted)?;
        out.push((
            format!("{stem}.rs"),
            module(stem, file, &own, false, &leaves)?,
        ));
    }
    out.sort();
    assert_eq!(out.len(), sorted.len() + 2);
    assert!(out.windows(2).all(|pair| pair[0].0 != pair[1].0));
    Ok(out)
}

/// Writes one `.cap` file as a single test module that carries its own `Host` trait in
/// place of the SDK's, so the SDK's tests compile every emitter without shipping the file's
/// capabilities. Inputs: the stem and the parsed file. Output: the module text, or the
/// first refusal.
pub(crate) fn test_module(stem: &str, file: &File) -> Result<String, CapgenError> {
    check_names(stem, file)?;
    let calls = file
        .capabilities
        .iter()
        .map(|capability| Call::of(stem, capability))
        .collect::<Result<Vec<_>, _>>()?;
    let own: Vec<&Call> = calls.iter().collect();
    let leaves = resolve(stem, file, &[&(stem, file.clone())])?;
    module(stem, file, &own, true, &leaves)
}

/// What another file's item a foreign ref may name is: a handle or an enum, never a type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Leaf {
    Handle,
    Enum,
}

/// Each foreign ref of a file, by stem and name, with the leaf it names.
type Leaves<'f> = BTreeMap<(&'f str, &'f str), Leaf>;

/// Resolves every foreign ref of a file against the loaded files. Inputs: the stem, the
/// file and every loaded file. Output: the leaves, or `cap_self_qualified` (its own stem),
/// `capgen_unknown_type` (no such file or item) or `capgen_foreign_not_leaf` (a type).
fn resolve<'f>(
    stem: &str,
    file: &'f File,
    all: &[&(&str, File)],
) -> Result<Leaves<'f>, CapgenError> {
    let mut leaves = Leaves::new();
    for ty in type_refs(file) {
        let Some((owner, name)) = foreign_in(ty) else {
            continue;
        };
        let spelled = format!("{owner}.{name}");
        if owner == stem {
            return Err(refuse(stem, "cap_self_qualified", spelled));
        }
        let target = all
            .iter()
            .find(|(other, _)| *other == owner)
            .map(|(_, target)| target)
            .ok_or_else(|| refuse(stem, "capgen_unknown_type", spelled.clone()))?;
        let leaf = if target.handles.iter().any(|item| item.name == name) {
            Leaf::Handle
        } else if target.enums.iter().any(|item| item.name == name) {
            Leaf::Enum
        } else if target.types.iter().any(|item| item.name == name) {
            return Err(refuse(stem, "capgen_foreign_not_leaf", spelled));
        } else {
            return Err(refuse(stem, "capgen_unknown_type", spelled));
        };
        leaves.insert((owner, name), leaf);
    }
    assert!(leaves.len() <= type_refs(file).count());
    Ok(leaves)
}

/// Every field type of a file: type fields, capability inputs and outputs, event fields.
/// Input: the file. Output: the types, in source order per kind of place.
fn type_refs(file: &File) -> impl Iterator<Item = &TypeRef> {
    let fields = file.types.iter().flat_map(|item| item.fields.iter());
    let calls = file.capabilities.iter().flat_map(|capability| {
        capability
            .inputs
            .iter()
            .chain(
                capability
                    .emits
                    .iter()
                    .flat_map(|event| event.fields.iter()),
            )
            .map(|field| &field.ty)
            .chain(capability.output.as_ref())
    });
    fields.map(|field| &field.ty).chain(calls)
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
    for item in &file.handles {
        reserved(stem, &item.name)?;
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
    let qualified: Vec<(&Call<'_>, Option<&str>)> =
        calls.iter().map(|call| (call, Some(call.stem))).collect();
    host_trait(&mut text, &qualified);
    text
}

/// Writes the `Host` trait with one typed method per capability, each type named through
/// its module when one is given. Inputs: the text and the calls with their modules.
/// Output: the text grows.
fn host_trait(text: &mut String, calls: &[(&Call<'_>, Option<&str>)]) {
    text.push_str(
        "/// Generated by capgen: the calls a plugin can make, one method per capability.\n",
    );
    text.push_str(
        "/// Only the kernel's bus gateway implements it; each plugin gets a host bound to\n",
    );
    text.push_str("/// its own id, so a plugin never names itself or reaches a provider.\n");
    if calls.is_empty() {
        text.push_str("pub trait Host {}\n");
        return;
    }
    text.push_str("pub trait Host {\n");
    for (index, (call, module)) in calls.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&format!(
            "    /// Calls {} ({}.cap). Inputs: the call's inputs. Output: its result or the refusal.\n",
            call.capability.name, call.stem
        ));
        let params: Vec<String> = std::iter::once("&self".to_owned())
            .chain(
                call.capability
                    .inputs
                    .iter()
                    .map(|input| format!("{}: {}", input.name, rust_type(&input.ty, *module))),
            )
            .collect();
        let tail = format!(
            ") -> Result<{}, Refusal>;",
            output(call.capability, *module)
        );
        text.push_str(&list(1, &format!("fn {}(", call.method), &params, &tail));
    }
    text.push_str("}\n");
}

/// Writes one file's module: its enums, handles and types, then per capability its facts,
/// the `Provide` trait, the bound checks and the call functions. Inputs: the stem, the
/// file, its calls, whether it carries its own `Host` trait (a test module), and its
/// resolved foreign refs. Output: the text, or `capgen_handle_in_event` (a foreign handle
/// in an event body) or `capgen_name_collision` (a generated name taken twice).
fn module(
    stem: &str,
    file: &File,
    calls: &[&Call<'_>],
    own_host: bool,
    leaves: &Leaves<'_>,
) -> Result<String, CapgenError> {
    let kinds = Kinds::of(file, leaves);
    for capability in &file.capabilities {
        for event in &capability.emits {
            if event
                .fields
                .iter()
                .any(|field| kinds.traits(&field.ty).handle)
            {
                return Err(refuse(stem, "capgen_handle_in_event", event.kind.clone()));
            }
        }
    }
    let mut text = String::new();
    if !calls.is_empty() {
        text.push_str(if own_host {
            "use crate::{Capability, Refusal, Tier};\n"
        } else {
            "use crate::{Capability, Host, Refusal, Tier};\n"
        });
    }
    let serde = !file.enums.is_empty()
        || file
            .types
            .iter()
            .any(|item| !kinds.holds_handle(&item.name));
    if serde {
        text.push_str("use serde::{Deserialize, Serialize};\n");
    }
    if own_host {
        text.push('\n');
        let local: Vec<(&Call<'_>, Option<&str>)> =
            calls.iter().map(|call| (*call, None)).collect();
        host_trait(&mut text, &local);
    }
    for item in &file.enums {
        enumeration(&mut text, stem, item);
    }
    for item in &file.handles {
        handle(&mut text, stem, item);
    }
    for item in &file.types {
        record(&mut text, stem, item, &kinds);
    }
    for call in calls {
        facts(&mut text, stem, call);
    }
    if !calls.is_empty() {
        provide(&mut text, stem, calls);
    }
    let mut taken: BTreeSet<String> = calls.iter().map(|call| call.function.clone()).collect();
    let mut fits = BTreeSet::new();
    for call in calls {
        checks(&mut text, stem, call, &kinds, &mut fits, &mut taken)?;
    }
    let mut written = BTreeSet::new();
    for name in &fits {
        fits_type(&mut text, stem, &kinds, name, &mut taken, &mut written)?;
    }
    for call in calls {
        function(&mut text, stem, call);
    }
    if text.starts_with('\n') {
        text.remove(0);
    }
    Ok(text)
}

/// Whether a type is `Copy`, holds a handle, or holds text, bytes or a list, each through
/// the types it names.
#[derive(Clone, Copy)]
struct Traits {
    copy: bool,
    handle: bool,
    bounded: bool,
}

/// What each name of a file declares, the foreign leaves it names, and the [`Traits`] of
/// each named type, computed once per type so the work stays linear in the fields however
/// often a type is named.
struct Kinds<'f> {
    types: BTreeMap<&'f str, &'f Type>,
    handles: BTreeSet<&'f str>,
    enums: BTreeSet<&'f str>,
    leaves: &'f Leaves<'f>,
    named: BTreeMap<&'f str, Traits>,
}

impl<'f> Kinds<'f> {
    /// Indexes a parsed file, whose types the grammar keeps acyclic, and computes every
    /// type's traits. Inputs: the file and its resolved foreign refs. Output: the index.
    fn of(file: &'f File, leaves: &'f Leaves<'f>) -> Self {
        let mut kinds = Self {
            types: file
                .types
                .iter()
                .map(|item| (item.name.as_str(), item))
                .collect(),
            handles: file.handles.iter().map(|item| item.name.as_str()).collect(),
            enums: file.enums.iter().map(|item| item.name.as_str()).collect(),
            leaves,
            named: BTreeMap::new(),
        };
        for item in &file.types {
            kinds.fill(item.name.as_str(), 0);
        }
        assert_eq!(kinds.named.len(), kinds.types.len());
        kinds
    }

    /// Computes one named type's traits after the types its fields name, once each.
    /// Inputs: the type's name and how many types deep the walk is. Output: the index grows.
    fn fill(&mut self, name: &'f str, depth: usize) {
        assert!(depth <= self.types.len());
        if self.named.contains_key(name) {
            return;
        }
        let item = *self.types.get(name).expect("fill walks declared types");
        for field in &item.fields {
            let declared = named_in(&field.ty)
                .and_then(|inner| self.types.get_key_value(inner).map(|(key, _)| *key));
            if let Some(key) = declared {
                self.fill(key, depth + 1);
            }
        }
        let fields: Vec<Traits> = item
            .fields
            .iter()
            .map(|field| self.traits(&field.ty))
            .collect();
        let traits = Traits {
            copy: fields.iter().all(|field| field.copy),
            handle: fields.iter().any(|field| field.handle),
            bounded: fields.iter().any(|field| field.bounded),
        };
        self.named.insert(name, traits);
    }

    /// The traits of one type, reading each named type's from the index. Input: the type.
    /// Output: the traits.
    fn traits(&self, ty: &TypeRef) -> Traits {
        match ty {
            TypeRef::U16 | TypeRef::U32 | TypeRef::U64 | TypeRef::Bool => Traits {
                copy: true,
                handle: false,
                bounded: false,
            },
            TypeRef::Text | TypeRef::Bytes => Traits {
                copy: false,
                handle: false,
                bounded: true,
            },
            TypeRef::List(inner) => Traits {
                copy: false,
                handle: self.traits(inner).handle,
                bounded: true,
            },
            TypeRef::Option(inner) => self.traits(inner),
            TypeRef::Named(name) => match self.types.get(name.as_str()) {
                Some(_) => *self
                    .named
                    .get(name.as_str())
                    .expect("a named type's traits are computed before it is read"),
                None => {
                    let handle = self.handles.contains(name.as_str());
                    assert!(handle || self.enums.contains(name.as_str()));
                    leaf_traits(handle)
                }
            },
            TypeRef::Foreign { stem, name } => {
                let leaf = self
                    .leaves
                    .get(&(stem.as_str(), name.as_str()))
                    .expect("every foreign ref is resolved before generation");
                leaf_traits(*leaf == Leaf::Handle)
            }
        }
    }

    /// Whether a type is `Copy`: scalars, enums and handles are, text, bytes and lists are
    /// not, an option or type is when all it holds is. Input: the type. Output: the answer.
    fn copy(&self, ty: &TypeRef) -> bool {
        self.traits(ty).copy
    }

    /// Whether a named item is a handle or a type holding one. Input: the name.
    /// Output: the answer.
    fn holds_handle(&self, name: &str) -> bool {
        self.handles.contains(name) || self.named.get(name).is_some_and(|traits| traits.handle)
    }

    /// Whether a type holds text, bytes or a list anywhere inside it. Input: the type.
    /// Output: the answer.
    fn bounded(&self, ty: &TypeRef) -> bool {
        self.traits(ty).bounded
    }
}

/// The traits of a handle or an enum: `Copy`, no bound, a handle only when it is one.
/// Input: whether the leaf is a handle. Output: the traits.
fn leaf_traits(handle: bool) -> Traits {
    Traits {
        copy: true,
        handle,
        bounded: false,
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

/// The Rust function name that checks a type's bounds: `fits_` and its snake name.
/// Input: the Pascal type name. Output: the function name.
fn fits_name(pascal: &str) -> String {
    let mut snake = String::from("fits");
    for character in pascal.chars() {
        if character.is_ascii_uppercase() {
            snake.push('_');
        }
        snake.push(character.to_ascii_lowercase());
    }
    snake
}

/// The boolean expression that holds when a value fits every bound inside its type, or
/// `None` when the type has none. Inputs: the index, the type, the value expression,
/// whether that expression is already a reference, the closure depth for fresh names, and
/// the named types whose check functions it calls. Output: the expression.
fn fits_expr(
    kinds: &Kinds<'_>,
    ty: &TypeRef,
    (value, by_ref): (&str, bool),
    depth: usize,
    fits: &mut BTreeSet<String>,
) -> Option<String> {
    assert!(depth <= crazyagent_cap::TYPE_DEPTH_MAX);
    if !kinds.bounded(ty) {
        return None;
    }
    match ty {
        TypeRef::Text => Some(format!("{value}.len() <= crate::TEXT_BYTES_MAX")),
        TypeRef::Bytes => Some(format!("{value}.len() <= crate::BYTES_MAX")),
        TypeRef::List(item) => {
            let count = format!("{value}.len() <= crate::LIST_ITEMS_MAX");
            Some(match each(kinds, item, depth, fits) {
                None => count,
                Some(check) => format!("{count} && {value}.iter().all({check})"),
            })
        }
        TypeRef::Option(inner) => each(kinds, inner, depth, fits)
            .map(|check| format!("{value}.as_ref().is_none_or({check})")),
        TypeRef::Named(name) => {
            fits.insert(name.clone());
            let borrow = if by_ref { "" } else { "&" };
            Some(format!("self::{}({borrow}{value})", fits_name(name)))
        }
        _ => None,
    }
}

/// The function or closure that checks one item of a list or option by reference, or
/// `None` when the item type has no bound. Inputs: as [`fits_expr`]. Output: the text.
fn each(
    kinds: &Kinds<'_>,
    ty: &TypeRef,
    depth: usize,
    fits: &mut BTreeSet<String>,
) -> Option<String> {
    if let TypeRef::Named(name) = ty {
        if !kinds.bounded(ty) {
            return None;
        }
        fits.insert(name.clone());
        return Some(format!("self::{}", fits_name(name)));
    }
    let item = format!("item{depth}");
    fits_expr(kinds, ty, (&item, true), depth + 1, fits).map(|check| format!("|{item}| {check}"))
}

/// Writes the bound checks of one capability: `check_<function>_input` when an input holds
/// a bound, over those inputs, and `check_<function>_output` when the output does.
/// Inputs: the text, the stem, the call, the index, the named types the checks use, and
/// the function names taken. Output: the text grows, or `capgen_name_collision`.
fn checks(
    text: &mut String,
    stem: &str,
    call: &Call<'_>,
    kinds: &Kinds<'_>,
    fits: &mut BTreeSet<String>,
    taken: &mut BTreeSet<String>,
) -> Result<(), CapgenError> {
    let bounded: Vec<_> = call
        .capability
        .inputs
        .iter()
        .filter(|input| kinds.bounded(&input.ty))
        .collect();
    if !bounded.is_empty() {
        let name = format!("check_{}_input", call.function);
        take(stem, taken, &name)?;
        let params: Vec<String> = bounded
            .iter()
            .map(|input| format!("{}: {}", input.name, borrowed(&input.ty)))
            .collect();
        let conditions: Vec<String> = bounded
            .iter()
            .filter_map(|input| fits_expr(kinds, &input.ty, (&input.name, true), 0, fits))
            .collect();
        check(
            text,
            stem,
            &call.capability.name,
            &name,
            &params,
            &conditions,
            ("inputs", "fit their", "Rejected", "cap_input_too_large"),
        );
    }
    if let Some(ty) = call
        .capability
        .output
        .as_ref()
        .filter(|ty| kinds.bounded(ty))
    {
        let name = format!("check_{}_output", call.function);
        take(stem, taken, &name)?;
        let params = vec![format!("output: {}", borrowed(ty))];
        let conditions: Vec<String> = fits_expr(kinds, ty, ("output", true), 0, fits)
            .into_iter()
            .collect();
        check(
            text,
            stem,
            &call.capability.name,
            &name,
            &params,
            &conditions,
            ("output", "fits its", "Failed", "cap_output_too_large"),
        );
    }
    Ok(())
}

/// The borrowed Rust type a check takes a value as: `&str` for text, a slice for bytes
/// and lists, else a reference to the owned type. Input: the type. Output: the type text.
fn borrowed(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Text => "&str".to_owned(),
        TypeRef::Bytes => "&[u8]".to_owned(),
        TypeRef::List(item) => format!("&[{}]", rust_type(item, None)),
        _ => format!("&{}", rust_type(ty, None)),
    }
}

/// Takes one generated function name. Inputs: the stem, the names taken and the name.
/// Output: nothing, or `capgen_name_collision`.
fn take(stem: &str, taken: &mut BTreeSet<String>, name: &str) -> Result<(), CapgenError> {
    if taken.insert(name.to_owned()) {
        Ok(())
    } else {
        Err(refuse(stem, "capgen_name_collision", name.to_owned()))
    }
}

/// Writes one bound check function. Inputs: the text, the stem, the capability's name, the
/// function's name, its parameters, its conditions, and what it checks, the verb for it,
/// and the refusal variant and code it answers. Output: the text grows.
fn check(
    text: &mut String,
    stem: &str,
    capability: &str,
    name: &str,
    params: &[String],
    conditions: &[String],
    (what, verb, variant, code): (&str, &str, &str, &str),
) {
    assert!(!conditions.is_empty());
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: whether the {what} of {capability} {verb} bounds.\n"
    ));
    text.push_str(&format!(
        "/// Inputs: the {what}. Output: nothing, or `{code}`.\n"
    ));
    text.push_str(&format!(
        "pub fn {name}({}) -> Result<(), Refusal> {{\n",
        params.join(", ")
    ));
    text.push_str(&format!("    if {} {{\n", conditions.join(" && ")));
    text.push_str("        Ok(())\n    } else {\n");
    text.push_str(&format!(
        "        Err(Refusal::{variant} {{\n            code: alloc::string::String::from(\"{code}\"),\n        }})\n"
    ));
    text.push_str("    }\n}\n");
}

/// Writes the function that checks one named type's bounds, once, and every named type it
/// calls in turn. Inputs: the text, the stem, the index, the type's name, the names taken
/// and the types whose function is already written. Output: the text grows, or
/// `capgen_name_collision` when another generated function holds the name.
fn fits_type(
    text: &mut String,
    stem: &str,
    kinds: &Kinds<'_>,
    name: &str,
    taken: &mut BTreeSet<String>,
    written: &mut BTreeSet<String>,
) -> Result<(), CapgenError> {
    if !written.insert(name.to_owned()) {
        return Ok(());
    }
    assert!(written.len() <= kinds.types.len());
    let item = kinds.types.get(name).expect("a bounded name is a type");
    let function = fits_name(name);
    take(stem, taken, &function)?;
    let mut nested = BTreeSet::new();
    let conditions: Vec<String> = item
        .fields
        .iter()
        .filter_map(|field| {
            let place = format!("value.{}", field.name);
            fits_expr(kinds, &field.ty, (&place, false), 0, &mut nested)
        })
        .collect();
    assert!(!conditions.is_empty());
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: whether one {name} fits every bound inside it.\n"
    ));
    text.push_str(&format!(
        "/// Input: the value. Output: true when it fits.\nfn {function}(value: &{name}) -> bool {{\n    {}\n}}\n",
        conditions.join(" && ")
    ));
    for inner in nested {
        fits_type(text, stem, kinds, &inner, taken, written)?;
    }
    Ok(())
}

/// Writes one handle: a kernel id a plugin can hold and pass back but never read as a
/// value, with no `Debug`. Inputs: the text, the stem and the handle. Output: the text grows.
fn handle(text: &mut String, stem: &str, item: &Handle) {
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: handle {}, an id the kernel checks against its caller.\n",
        item.name
    ));
    text.push_str("#[derive(Clone, Copy, PartialEq, Eq, Hash)]\n");
    text.push_str(&format!(
        "pub struct {} {{\n    id: u64,\n}}\n\n",
        item.name
    ));
    text.push_str(&format!("impl {} {{\n", item.name));
    text.push_str("    /// Wraps a kernel id. Input: the id. Output: the handle.\n");
    text.push_str("    #[doc(hidden)]\n    pub fn from_kernel(id: u64) -> Self {\n        Self { id }\n    }\n\n");
    text.push_str("    /// The kernel id. Output: the id.\n");
    text.push_str(
        "    #[doc(hidden)]\n    pub fn kernel_id(self) -> u64 {\n        self.id\n    }\n}\n",
    );
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

/// Writes one record type, its derives following its fields: `Copy` only when every field
/// is, `Debug` and serde (refusing unknown fields) only when no field holds a handle.
/// Inputs: the text, the stem, the type and the file's index. Output: the text grows.
fn record(text: &mut String, stem: &str, item: &Type, kinds: &Kinds<'_>) {
    text.push_str(&format!(
        "\n/// Generated by capgen from {stem}.cap: type {}.\n",
        item.name
    ));
    let copy = item.fields.iter().all(|field| kinds.copy(&field.ty));
    let plain = !kinds.holds_handle(&item.name);
    let mut derives = Vec::new();
    if plain {
        derives.push("Debug");
    }
    derives.push("Clone");
    if copy {
        derives.push("Copy");
    }
    derives.extend(["PartialEq", "Eq"]);
    if plain {
        derives.extend(["Serialize", "Deserialize"]);
    }
    text.push_str(&format!("#[derive({})]\n", derives.join(", ")));
    if plain {
        text.push_str("#[serde(deny_unknown_fields)]\n");
    }
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
            "    /// Provides {}. Inputs: the calling plugin's id, set by the bus, and the call's\n    /// inputs. Output: its result or the refusal.\n",
            call.capability.name
        ));
        let params: Vec<String> = ["&self".to_owned(), "caller: &'static str".to_owned()]
            .into_iter()
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
        (TypeRef::Text, _) => "alloc::string::String".to_owned(),
        (TypeRef::Bytes, _) => "alloc::vec::Vec<u8>".to_owned(),
        (TypeRef::List(item), _) => format!("alloc::vec::Vec<{}>", rust_type(item, module)),
        (TypeRef::Option(inner), _) => {
            format!("core::option::Option<{}>", rust_type(inner, module))
        }
        (TypeRef::Named(name), None) => name.clone(),
        (TypeRef::Named(name), Some(module)) => format!("{module}::{name}"),
        (TypeRef::Foreign { stem, name }, _) => format!("super::{stem}::{name}"),
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
