#![forbid(unsafe_code)]

/// The most bytes one `.cap` source may hold.
pub const SOURCE_BYTES_MAX: usize = 262_144;
/// The most items (enums, types and capabilities together) one `.cap` source may hold.
pub const ITEMS_MAX: usize = 256;
/// The most entries one list may hold: enum variants, type fields, inputs, emits, event fields.
pub const FIELDS_MAX: usize = 64;
/// The most bytes one name may hold, a dotted name counted whole.
pub const NAME_BYTES_MAX: usize = 64;

/// One parsed `.cap` file: its enums, types and capabilities, each in source order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct File {
    pub enums: Vec<Enum>,
    pub types: Vec<Type>,
    pub capabilities: Vec<Capability>,
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

/// A field's type: a built-in scalar or an enum or type declared in the same file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    U16,
    U32,
    U64,
    Bool,
    Named(String),
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

/// Parses one `.cap` source and checks it. Input: the source text.
/// Output: the file's syntax tree, or the first refusal with its position.
pub fn parse(source: &str) -> Result<File, ParseError> {
    let tokens = lex::lex(source)?;
    let (file, spots) = parse::file(tokens)?;
    check::check(&file, &spots)?;
    assert!(file.enums.len() + file.types.len() + file.capabilities.len() <= ITEMS_MAX);
    Ok(file)
}

mod check;
mod lex;
mod parse;
#[cfg(test)]
mod tests;
