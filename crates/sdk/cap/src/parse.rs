use crate::lex::{Kind, Token};
use crate::{
    Capability, Enum, Event, FIELDS_MAX, Field, File, ITEMS_MAX, NAME_BYTES_MAX, ParseError, Tier,
    Type, TypeRef,
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
/// a named type, and per declared type the named types its fields use.
#[derive(Debug, Default)]
pub(crate) struct Spots {
    pub(crate) declared: Vec<Spot>,
    pub(crate) used: Vec<Spot>,
    pub(crate) fields_of_type: Vec<Vec<Spot>>,
}

/// The three name shapes of the grammar.
#[derive(Clone, Copy)]
enum Shape {
    Pascal,
    Snake,
    Dotted,
}

/// One parsed field with the tokens of its name and of its type.
struct Parsed<'s> {
    name: Token<'s>,
    field: Field,
    ty: Token<'s>,
}

/// Parses the tokens of one source into its file and the positions the checks need.
/// Input: the tokens, ending with `End`. Output: the file and its spots, or the first refusal.
pub(crate) fn file(tokens: Vec<Token<'_>>) -> Result<(File, Spots), ParseError> {
    assert_eq!(tokens.last().map(|token| token.kind), Some(Kind::End));
    let mut parser = Parser {
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
        if token.kind != Kind::Word || !matches!(token.text, "enum" | "type" | "capability") {
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
            _ => file.capabilities.push(parser.capability()?),
        }
    }
    assert_eq!(
        file.enums.len() + file.types.len() + file.capabilities.len(),
        items
    );
    assert_eq!(parser.spots.declared.len(), items);
    assert_eq!(parser.spots.fields_of_type.len(), file.types.len());
    Ok((file, parser.spots))
}

/// A recursive-descent parser over the tokens of one source.
struct Parser<'s> {
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

    /// Consumes `name: type`. Output: the field with its tokens, or the first refusal.
    fn field(&mut self) -> Result<(Token<'s>, Parsed<'s>), ParseError> {
        let name = self.name(Shape::Snake)?;
        self.expect(Kind::Colon)?;
        let ty = self.expect(Kind::Word)?;
        let parsed = Parsed {
            name,
            field: Field {
                name: name.text.to_owned(),
                ty: type_ref(&ty)?,
            },
            ty,
        };
        Ok((name, parsed))
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
            assert!(!parsed.name.text.is_empty());
            if matches!(parsed.field.ty, TypeRef::Named(_)) {
                named.push(Spot::of(&parsed.ty));
            }
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
        let output_token = self.expect(Kind::Word)?;
        let output = if output_token.text == "unit" {
            None
        } else {
            let output = type_ref(&output_token)?;
            if matches!(output, TypeRef::Named(_)) {
                self.spots.used.push(Spot::of(&output_token));
            }
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
            self.expect(Kind::OpenBrace)?;
            let (fields, _) = self.fields(Kind::CloseBrace, false, false)?;
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

/// Reads a field's type from its word: a scalar, or a Pascal name of a declared enum or
/// type. Input: the word token. Output: the type, `cap_name_too_long` or `cap_unknown_type`.
fn type_ref(token: &Token<'_>) -> Result<TypeRef, ParseError> {
    match token.text {
        "u16" => Ok(TypeRef::U16),
        "u32" => Ok(TypeRef::U32),
        "u64" => Ok(TypeRef::U64),
        "bool" => Ok(TypeRef::Bool),
        text if text.len() > NAME_BYTES_MAX => Err(token.refuse("cap_name_too_long")),
        text if is_pascal(text) => Ok(TypeRef::Named(text.to_owned())),
        _ => Err(token.refuse("cap_unknown_type")),
    }
}

/// Whether a word is a Pascal name: an upper-case letter, then letters and digits.
/// Input: the word. Output: true for a Pascal name.
fn is_pascal(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_alphanumeric())
}

/// Whether a word is a snake name: a lower-case letter, then lower-case letters, digits and `_`.
/// Input: the word. Output: true for a snake name.
fn is_snake(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Whether a word is a dotted name: two or more snake names joined by `.`.
/// Input: the word. Output: true for a dotted name.
fn is_dotted(text: &str) -> bool {
    text.split('.').count() >= 2 && text.split('.').all(is_snake)
}
