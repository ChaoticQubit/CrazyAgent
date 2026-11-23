use kernel_common::names::{is_dotted, is_snake};

use crate::lex::{Kind, Token};
use crate::{
    Capability, Enum, Event, FIELDS_MAX, Field, File, Handle, ITEMS_MAX, NAME_BYTES_MAX,
    ParseError, TYPE_DEPTH_MAX, Tier, Type, TypeRef,
};

/// A name in the source with its position, kept for the checks that run after the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spot {
    pub(crate) name: String,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

impl Spot {
    /// The spot of a token. Input: the token. Output: its text and position.
    fn of(token: &Token<'_>) -> Self {
        Self {
            name: token.text.to_owned(),
            line: token.line,
            column: token.column,
        }
    }

    /// The refusal of this spot. Input: the refusal code. Output: the refusal at its position.
    pub(crate) fn refuse(&self, code: &'static str) -> ParseError {
        ParseError {
            line: self.line,
            column: self.column,
            code,
        }
    }
}

/// The positions the checks need: every declared item name in source order, every use of
/// a named type, per declared type the named types its fields use, the named types event
/// bodies use, and every event kind.
#[derive(Debug, Default)]
pub(crate) struct Spots {
    pub(crate) declared: Vec<Spot>,
    pub(crate) used: Vec<Spot>,
    pub(crate) fields_of_type: Vec<Vec<Spot>>,
    pub(crate) emitted: Vec<Spot>,
    pub(crate) kinds: Vec<Spot>,
}

/// The three name shapes of the grammar.
#[derive(Clone, Copy)]
enum Shape {
    Pascal,
    Snake,
    Dotted,
}

/// One parsed field with the spots of the named types its type uses, wrappers included.
struct Parsed {
    field: Field,
    named: Vec<Spot>,
}

/// Parses the tokens of one source into its file and the positions the checks need.
/// Inputs: the file's stem and the tokens, ending with `End`. Output: the file and its spots, or the first refusal.
pub(crate) fn file<'s>(stem: &'s str, tokens: Vec<Token<'s>>) -> Result<(File, Spots), ParseError> {
    assert_eq!(tokens.last().map(|token| token.kind), Some(Kind::End));
    let mut parser = Parser {
        stem,
        tokens,
        at: 0,
        spots: Spots::default(),
    };
    let mut file = File::default();
    let mut items = 0usize;
    loop {
        let token = parser.peek();
        if token.kind == Kind::End {
            break;
        }
        if token.kind != Kind::Word
            || !matches!(token.text, "enum" | "type" | "handle" | "capability")
        {
            return Err(token.refuse("cap_unexpected_token"));
        }
        if items == ITEMS_MAX {
            return Err(token.refuse("cap_too_many_items"));
        }
        items += 1;
        parser.next();
        match token.text {
            "enum" => file.enums.push(parser.enumeration()?),
            "type" => file.types.push(parser.record()?),
            "handle" => file.handles.push(Handle {
                name: parser.declare(Shape::Pascal)?,
            }),
            _ => file.capabilities.push(parser.capability()?),
        }
    }
    assert_eq!(
        file.enums.len() + file.types.len() + file.handles.len() + file.capabilities.len(),
        items
    );
    assert_eq!(parser.spots.declared.len(), items);
    assert_eq!(parser.spots.fields_of_type.len(), file.types.len());
    Ok((file, parser.spots))
}

/// A recursive-descent parser over the tokens of one source.
struct Parser<'s> {
    stem: &'s str,
    tokens: Vec<Token<'s>>,
    at: usize,
    spots: Spots,
}

impl<'s> Parser<'s> {
    /// The next token, not consumed. Output: the token.
    fn peek(&self) -> Token<'s> {
        self.tokens[self.at]
    }

    /// Consumes the next token; the `End` token is never passed. Output: the token.
    fn next(&mut self) -> Token<'s> {
        let token = self.tokens[self.at];
        if token.kind != Kind::End {
            self.at += 1;
        }
        token
    }

    /// Consumes a token of one kind. Input: the kind. Output: the token or `cap_unexpected_token`.
    fn expect(&mut self, kind: Kind) -> Result<Token<'s>, ParseError> {
        let token = self.next();
        if token.kind == kind {
            Ok(token)
        } else {
            Err(token.refuse("cap_unexpected_token"))
        }
    }

    /// Consumes a name of one shape. Input: the shape. Output: the name's token,
    /// `cap_unexpected_token`, `cap_name_too_long` or `cap_name_invalid`.
    fn name(&mut self, shape: Shape) -> Result<Token<'s>, ParseError> {
        let token = self.expect(Kind::Word)?;
        if token.text.len() > NAME_BYTES_MAX {
            return Err(token.refuse("cap_name_too_long"));
        }
        let valid = match shape {
            Shape::Pascal => is_pascal(token.text),
            Shape::Snake => is_snake(token.text),
            Shape::Dotted => is_dotted(token.text),
        };
        if valid {
            Ok(token)
        } else {
            Err(token.refuse("cap_name_invalid"))
        }
    }

    /// Consumes the declared name of an item and records it. Input: the shape.
    /// Output: the name.
    fn declare(&mut self, shape: Shape) -> Result<String, ParseError> {
        let token = self.name(shape)?;
        self.spots.declared.push(Spot::of(&token));
        Ok(token.text.to_owned())
    }

    /// Consumes a list up to its closing token, the opening one already consumed: at most
    /// `FIELDS_MAX` entries, names unique within the list. Inputs: the closing kind, whether
    /// a trailing comma and an empty list are allowed, and the entry parser.
    /// Output: each entry with its name token, or the first refusal.
    fn list<T>(
        &mut self,
        close: Kind,
        trailing: bool,
        empty: bool,
        entry: fn(&mut Self) -> Result<(Token<'s>, T), ParseError>,
    ) -> Result<Vec<(Token<'s>, T)>, ParseError> {
        let mut entries: Vec<(Token<'s>, T)> = Vec::new();
        if empty && self.peek().kind == close {
            self.next();
            return Ok(entries);
        }
        loop {
            if entries.len() == FIELDS_MAX {
                return Err(self.peek().refuse("cap_too_many_fields"));
            }
            let (name, value) = entry(self)?;
            if entries.iter().any(|(seen, _)| seen.text == name.text) {
                return Err(name.refuse("cap_duplicate_name"));
            }
            entries.push((name, value));
            let token = self.next();
            match token.kind {
                Kind::Comma if trailing && self.peek().kind == close => {
                    self.next();
                    break;
                }
                Kind::Comma => {}
                kind if kind == close => break,
                _ => return Err(token.refuse("cap_unexpected_token")),
            }
        }
        assert!(!entries.is_empty() && entries.len() <= FIELDS_MAX);
        Ok(entries)
    }

    /// Consumes `name: type`. Output: the field with its name token and the spots of the
    /// named types it uses, or the first refusal.
    fn field(&mut self) -> Result<(Token<'s>, Parsed), ParseError> {
        let name = self.name(Shape::Snake)?;
        self.expect(Kind::Colon)?;
        let mut named = Vec::new();
        let ty = self.type_ref(0, false, &mut named)?;
        let parsed = Parsed {
            field: Field {
                name: name.text.to_owned(),
                ty,
            },
            named,
        };
        Ok((name, parsed))
    }

    /// Consumes a type: a scalar, `text`, `bytes`, a Pascal name, `stem.Name`, or `list<type>`
    /// and `option<type>` up to `TYPE_DEPTH_MAX` deep, never an option directly in an option.
    /// Inputs: the wrappers already open, whether the nearest is an option, and the spots of
    /// named types found so far. Output: the type, or `cap_type_too_deep`, `cap_nested_option`,
    /// `cap_name_too_long`, `cap_self_qualified`, `cap_unknown_type`, `cap_unexpected_token`.
    fn type_ref(
        &mut self,
        depth: usize,
        in_option: bool,
        named: &mut Vec<Spot>,
    ) -> Result<TypeRef, ParseError> {
        assert!(depth <= TYPE_DEPTH_MAX);
        let token = self.expect(Kind::Word)?;
        let wrapper = match token.text {
            "list" => Some(false),
            "option" => Some(true),
            _ => None,
        };
        let Some(is_option) = wrapper else {
            let ty = leaf(&token, self.stem)?;
            if matches!(ty, TypeRef::Named(_)) {
                named.push(Spot::of(&token));
            }
            return Ok(ty);
        };
        if is_option && in_option {
            return Err(token.refuse("cap_nested_option"));
        }
        if depth == TYPE_DEPTH_MAX {
            return Err(token.refuse("cap_type_too_deep"));
        }
        self.expect(Kind::OpenAngle)?;
        let inner = Box::new(self.type_ref(depth + 1, is_option, named)?);
        self.expect(Kind::CloseAngle)?;
        Ok(if is_option {
            TypeRef::Option(inner)
        } else {
            TypeRef::List(inner)
        })
    }

    /// Consumes a field list and records every named type it uses. Inputs: the closing
    /// kind, whether a trailing comma and an empty list are allowed.
    /// Output: the fields and the spots of their named types, or the first refusal.
    fn fields(
        &mut self,
        close: Kind,
        trailing: bool,
        empty: bool,
    ) -> Result<(Vec<Field>, Vec<Spot>), ParseError> {
        let entries = self.list(close, trailing, empty, Self::field)?;
        let mut named = Vec::new();
        let mut fields = Vec::with_capacity(entries.len());
        for (_, parsed) in entries {
            named.extend(parsed.named);
            fields.push(parsed.field);
        }
        self.spots.used.extend(named.iter().cloned());
        Ok((fields, named))
    }

    /// Consumes `Name { variant, ... }` after `enum`. Output: the enum or the first refusal.
    fn enumeration(&mut self) -> Result<Enum, ParseError> {
        let name = self.declare(Shape::Pascal)?;
        self.expect(Kind::OpenBrace)?;
        let variants = self.list(Kind::CloseBrace, true, false, |parser| {
            parser.name(Shape::Snake).map(|token| (token, ()))
        })?;
        Ok(Enum {
            name,
            variants: variants
                .into_iter()
                .map(|(token, ())| token.text.to_owned())
                .collect(),
        })
    }

    /// Consumes `Name { field: type, ... }` after `type`. Output: the type or the first refusal.
    fn record(&mut self) -> Result<Type, ParseError> {
        let name = self.declare(Shape::Pascal)?;
        self.expect(Kind::OpenBrace)?;
        let (fields, named) = self.fields(Kind::CloseBrace, true, false)?;
        self.spots.fields_of_type.push(named);
        Ok(Type { name, fields })
    }

    /// Consumes a capability after `capability`: its dotted name, inputs, output, tier,
    /// permission and events. Output: the capability or the first refusal.
    fn capability(&mut self) -> Result<Capability, ParseError> {
        let name = self.declare(Shape::Dotted)?;
        self.expect(Kind::OpenParen)?;
        let (inputs, _) = self.fields(Kind::CloseParen, false, true)?;
        self.expect(Kind::Arrow)?;
        let output = if self.peek().kind == Kind::Word && self.peek().text == "unit" {
            self.next();
            None
        } else {
            let mut named = Vec::new();
            let output = self.type_ref(0, false, &mut named)?;
            self.spots.used.extend(named);
            Some(output)
        };
        self.keyword("tier", "cap_missing_tier")?;
        let tier_token = self.expect(Kind::Word)?;
        let tier = match tier_token.text {
            "kernel" => Tier::Kernel,
            "t1" => Tier::T1,
            "t2" => Tier::T2,
            "t3" => Tier::T3,
            _ => return Err(tier_token.refuse("cap_unknown_tier")),
        };
        self.keyword("needs", "cap_missing_needs")?;
        let needs = self.name(Shape::Dotted)?.text.to_owned();
        let mut emits: Vec<(Token<'s>, Event)> = Vec::new();
        while self.peek().kind == Kind::Word && self.peek().text == "emits" {
            if emits.len() == FIELDS_MAX {
                return Err(self.peek().refuse("cap_too_many_fields"));
            }
            self.next();
            self.expect(Kind::Colon)?;
            let kind = self.name(Shape::Pascal)?;
            if emits.iter().any(|(seen, _)| seen.text == kind.text) {
                return Err(kind.refuse("cap_duplicate_name"));
            }
            self.spots.kinds.push(Spot::of(&kind));
            self.expect(Kind::OpenBrace)?;
            let (fields, named) = self.fields(Kind::CloseBrace, false, false)?;
            self.spots.emitted.extend(named);
            emits.push((
                kind,
                Event {
                    kind: kind.text.to_owned(),
                    fields,
                },
            ));
        }
        Ok(Capability {
            name,
            inputs,
            output,
            tier,
            needs,
            emits: emits.into_iter().map(|(_, event)| event).collect(),
        })
    }

    /// Consumes `word:` that must come next. Inputs: the word and the code when it is
    /// missing. Output: nothing, the missing code, or `cap_unexpected_token` without the colon.
    fn keyword(&mut self, word: &str, missing: &'static str) -> Result<(), ParseError> {
        let token = self.peek();
        if token.kind != Kind::Word || token.text != word {
            return Err(token.refuse(missing));
        }
        self.next();
        self.expect(Kind::Colon)?;
        Ok(())
    }
}

/// Reads an unwrapped type from its word: a scalar, `text`, `bytes`, a Pascal name of a
/// declared enum, type or handle, or `stem.Name` of another file's item. Inputs: the word
/// token and the file's stem. Output: the type, `cap_name_too_long`, `cap_self_qualified`
/// or `cap_unknown_type`.
fn leaf(token: &Token<'_>, own: &str) -> Result<TypeRef, ParseError> {
    match token.text {
        "u16" => Ok(TypeRef::U16),
        "u32" => Ok(TypeRef::U32),
        "u64" => Ok(TypeRef::U64),
        "bool" => Ok(TypeRef::Bool),
        "text" => Ok(TypeRef::Text),
        "bytes" => Ok(TypeRef::Bytes),
        text if text.len() > NAME_BYTES_MAX => Err(token.refuse("cap_name_too_long")),
        text if is_pascal(text) => Ok(TypeRef::Named(text.to_owned())),
        text => match text.split_once('.') {
            Some((stem, _)) if stem == own => Err(token.refuse("cap_self_qualified")),
            Some((stem, name)) if is_snake(stem) && is_pascal(name) => Ok(TypeRef::Foreign {
                stem: stem.to_owned(),
                name: name.to_owned(),
            }),
            _ => Err(token.refuse("cap_unknown_type")),
        },
    }
}

/// Whether a word is a Pascal name: an upper-case letter, then letters and digits.
/// Input: the word. Output: true for a Pascal name.
fn is_pascal(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_alphanumeric())
}
