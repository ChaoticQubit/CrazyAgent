#![forbid(unsafe_code)]

/// The most bytes one `.cap` source may hold.
pub const SOURCE_BYTES_MAX: usize = 262_144;
/// The most items (enums, types, handles and capabilities together) one `.cap` source may hold.
pub const ITEMS_MAX: usize = 256;
/// The most entries one list may hold: enum variants, type fields, inputs, emits, event fields.
pub const FIELDS_MAX: usize = 64;
/// The most bytes one name may hold, a dotted name counted whole.
pub const NAME_BYTES_MAX: usize = 64;
/// The most `list` and `option` wrappers one field type may nest.
pub const TYPE_DEPTH_MAX: usize = 4;

/// One parsed `.cap` file: its enums, types, handles and capabilities, each in source order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct File {
    pub enums: Vec<Enum>,
    pub types: Vec<Type>,
    pub handles: Vec<Handle>,
    pub capabilities: Vec<Capability>,
}

/// A handle: a Pascal-case name for a kernel-held value a plugin refers to by id only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    pub name: String,
}

/// An enum: its Pascal-case name and its snake-case variants in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enum {
    pub name: String,
    pub variants: Vec<String>,
}

/// A record type: its Pascal-case name and its fields in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type {
    pub name: String,
    pub fields: Vec<Field>,
}

/// One named, typed field of a type, a capability input or an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: TypeRef,
}

/// A field's type: a built-in scalar, text or bytes, a list or option of a type, an enum,
/// type or handle declared in the same file, or another file's item named `stem.Name`,
/// which capgen resolves across files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    U16,
    U32,
    U64,
    Bool,
    Text,
    Bytes,
    List(Box<TypeRef>),
    Option(Box<TypeRef>),
    Named(String),
    Foreign { stem: String, name: String },
}

/// The plugin tiers of DEC-206 Q5, lowest first; `Kernel` is a kernel module's capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Kernel,
    T1,
    T2,
    T3,
}

/// One capability: its dotted name, inputs, output (`None` for `unit`), tier, the permission
/// a caller needs, and the events a call emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub name: String,
    pub inputs: Vec<Field>,
    pub output: Option<TypeRef>,
    pub tier: Tier,
    pub needs: String,
    pub emits: Vec<Event>,
}

/// One event a capability emits: its stored kind name and its body fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: String,
    pub fields: Vec<Field>,
}

/// Why a source is not a valid `.cap` file: the refusal code and the 1-based line and byte
/// column where the refused part starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub line: u32,
    pub column: u32,
    pub code: &'static str,
}

/// Parses one `.cap` source whose file stem is not known, so no ref counts as naming its
/// own file. Input: the source text. Output: as [`parse_as`], with no stem to refuse.
pub fn parse(source: &str) -> Result<File, ParseError> {
    parse_stemmed("", source)
}

/// Parses one `.cap` source and checks it, refusing a stem that is not a snake name of at
/// most `NAME_BYTES_MAX` bytes with no `__` and no trailing `_` (`cap_stem`, line 1 column
/// 1), and a ref qualified with the file's own stem (`cap_self_qualified`). Inputs: the
/// file's stem and the source text. Output: the file's syntax tree, or the first refusal.
pub fn parse_as(stem: &str, source: &str) -> Result<File, ParseError> {
    let valid = stem.len() <= NAME_BYTES_MAX
        && kernel_common::names::is_snake(stem)
        && !stem.contains("__")
        && !stem.ends_with('_');
    if !valid {
        return Err(ParseError {
            line: 1,
            column: 1,
            code: "cap_stem",
        });
    }
    parse_stemmed(stem, source)
}

/// Lexes, parses and checks one source under a stem already accepted, or none.
/// Inputs: the stem, empty when unknown, and the source text. Output: as [`parse_as`].
fn parse_stemmed(stem: &str, source: &str) -> Result<File, ParseError> {
    let tokens = lex::lex(source)?;
    let (file, spots) = parse::file(stem, tokens)?;
    check::check(&file, &spots)?;
    assert!(
        file.enums.len() + file.types.len() + file.handles.len() + file.capabilities.len()
            <= ITEMS_MAX
    );
    Ok(file)
}

mod check;
mod lex;
mod parse;
#[cfg(test)]
mod tests;
