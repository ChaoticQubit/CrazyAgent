use super::*;

/// Parses a source that must be refused and returns the refusal.
/// Input: the source. Output: the refusal; panics when the source parses.
fn refused(source: &str) -> ParseError {
    match parse(source) {
        Ok(file) => panic!("parsed, expected a refusal: {file:?}"),
        Err(error) => error,
    }
}

/// Builds the refusal a row expects. Inputs: code, line and column. Output: the refusal.
fn at(code: &'static str, line: u32, column: u32) -> ParseError {
    ParseError { line, column, code }
}

/// Names a field of a scalar or named type. Inputs: name and type. Output: the field.
fn field(name: &str, ty: TypeRef) -> Field {
    Field {
        name: name.to_owned(),
        ty,
    }
}

/// Names a reference to a declared enum or type. Input: its name. Output: the reference.
fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.to_owned())
}

#[test]
fn settings_cap_parses_to_its_ast() {
    let file = parse(include_str!("../../caps/settings.cap")).expect("settings.cap parses");
    let quit = || field("settings", named("QuitSettings"));
    assert_eq!(
        file,
        File {
            enums: vec![
                Enum {
                    name: "OnQuit".to_owned(),
                    variants: vec!["keep_running".to_owned(), "stop".to_owned()],
                },
                Enum {
                    name: "LeftRunning".to_owned(),
                    variants: vec!["after_hours".to_owned(), "never".to_owned()],
                },
            ],
            types: vec![Type {
                name: "QuitSettings".to_owned(),
                fields: vec![
                    field("on_quit", named("OnQuit")),
                    field("left_running", named("LeftRunning")),
                    field("hours", TypeRef::U16),
                ],
            }],
            capabilities: vec![Capability {
                name: "settings.save_quit".to_owned(),
                inputs: vec![quit()],
                output: None,
                tier: Tier::T1,
                needs: "settings.write".to_owned(),
                emits: vec![Event {
                    kind: "AgentQuitSaved".to_owned(),
                    fields: vec![quit()],
                }],
            }],
        }
    );
}

#[test]
fn an_empty_source_is_an_empty_file() {
    assert_eq!(parse(""), Ok(File::default()));
    assert_eq!(parse(" \n\t\r\n"), Ok(File::default()));
}

#[test]
fn a_typed_output_parses_to_some() {
    let file = parse(
        "type R { n: u64, ok: bool, }\ncapability a.b(x: u32) -> R\n  tier: kernel\n  needs: a.read\n",
    )
    .expect("parses");
    let capability = &file.capabilities[0];
    assert_eq!(capability.output, Some(named("R")));
    assert_eq!(capability.inputs, vec![field("x", TypeRef::U32)]);
    assert_eq!(capability.tier, Tier::Kernel);
    assert!(capability.emits.is_empty());
    assert_eq!(
        file.types[0].fields,
        vec![field("n", TypeRef::U64), field("ok", TypeRef::Bool)]
    );
}

#[test]
fn every_tier_word_parses() {
    for (word, tier) in [
        ("kernel", Tier::Kernel),
        ("t1", Tier::T1),
        ("t2", Tier::T2),
        ("t3", Tier::T3),
    ] {
        let source = format!("capability a.b() -> unit tier: {word} needs: a.c");
        assert_eq!(parse(&source).expect("parses").capabilities[0].tier, tier);
    }
}

#[test]
fn a_source_over_the_byte_bound_is_refused() {
    let source = " ".repeat(SOURCE_BYTES_MAX + 1);
    assert_eq!(refused(&source), at("cap_source_too_large", 1, 1));
    assert_eq!(parse(&" ".repeat(SOURCE_BYTES_MAX)), Ok(File::default()));
}

#[test]
fn a_lower_case_enum_name_is_refused() {
    assert_eq!(
        refused("enum onQuit { stop }"),
        at("cap_name_invalid", 1, 6)
    );
}

#[test]
fn an_undotted_capability_name_is_refused() {
    assert_eq!(
        refused("capability save() -> unit tier: t1 needs: a.b"),
        at("cap_name_invalid", 1, 12)
    );
}

#[test]
fn an_undotted_permission_is_refused() {
    assert_eq!(
        refused("capability a.b() -> unit\n  tier: t1\n  needs: write"),
        at("cap_name_invalid", 3, 10)
    );
}

#[test]
fn a_name_over_the_byte_bound_is_refused() {
    let name = format!("A{}", "a".repeat(NAME_BYTES_MAX));
    assert_eq!(
        refused(&format!("enum {name} {{ stop }}")),
        at("cap_name_too_long", 1, 6)
    );
    let fits = format!("A{}", "a".repeat(NAME_BYTES_MAX - 1));
    assert!(parse(&format!("enum {fits} {{ stop }}")).is_ok());
}

#[test]
fn a_name_declared_twice_is_refused_at_the_second() {
    assert_eq!(
        refused("enum A { x }\ntype A { y: u16 }"),
        at("cap_duplicate_name", 2, 6)
    );
}

#[test]
fn a_field_declared_twice_in_one_type_is_refused() {
    assert_eq!(
        refused("type A { y: u16, y: u32 }"),
        at("cap_duplicate_name", 1, 18)
    );
}

#[test]
fn an_undeclared_type_is_refused() {
    assert_eq!(
        refused("type A { b: Missing }"),
        at("cap_unknown_type", 1, 13)
    );
}

#[test]
fn a_lower_case_type_that_is_no_scalar_is_refused() {
    assert_eq!(refused("type A { b: text }"), at("cap_unknown_type", 1, 13));
}

#[test]
fn a_type_cycle_is_refused_where_it_closes() {
    assert_eq!(
        refused("type A { b: B }\ntype B { a: A }"),
        at("cap_type_cycle", 2, 13)
    );
    assert_eq!(refused("type A { a: A }"), at("cap_type_cycle", 1, 13));
}

#[test]
fn a_capability_without_its_tier_is_refused() {
    assert_eq!(
        refused("capability s.c() -> unit\n  needs: s.w"),
        at("cap_missing_tier", 2, 3)
    );
}

#[test]
fn a_capability_without_its_permission_is_refused() {
    assert_eq!(
        refused("capability s.c() -> unit\n  tier: t1\n"),
        at("cap_missing_needs", 3, 1)
    );
}

#[test]
fn an_unknown_tier_is_refused() {
    assert_eq!(
        refused("capability s.c() -> unit\n  tier: t9\n  needs: s.w"),
        at("cap_unknown_tier", 2, 9)
    );
}

#[test]
fn items_over_the_bound_are_refused_at_the_first_too_many() {
    let source: String = (0..=ITEMS_MAX)
        .map(|item| format!("enum E{item} {{ a }}\n"))
        .collect();
    assert_eq!(
        refused(&source),
        at("cap_too_many_items", u32::try_from(ITEMS_MAX).unwrap() + 1, 1)
    );
    let fits: String = (0..ITEMS_MAX)
        .map(|item| format!("enum E{item} {{ a }}\n"))
        .collect();
    assert_eq!(parse(&fits).expect("fits").enums.len(), ITEMS_MAX);
}

#[test]
fn fields_over_the_bound_are_refused_at_the_first_too_many() {
    let fields: String = (0..=FIELDS_MAX)
        .map(|index| format!("  f{index}: u16,\n"))
        .collect();
    assert_eq!(
        refused(&format!("type A {{\n{fields}}}")),
        at(
            "cap_too_many_fields",
            u32::try_from(FIELDS_MAX).unwrap() + 2,
            3
        )
    );
}

#[test]
fn a_character_outside_the_grammar_is_refused() {
    assert_eq!(
        refused("enum A { x } @"),
        at("cap_unexpected_token", 1, 14)
    );
    assert_eq!(
        refused("enum A { x }\nenum B { é }"),
        at("cap_unexpected_token", 2, 10)
    );
}

#[test]
fn a_token_out_of_place_is_refused() {
    assert_eq!(
        refused("type A { b u16 }"),
        at("cap_unexpected_token", 1, 12)
    );
    assert_eq!(refused("enum A { }"), at("cap_unexpected_token", 1, 10));
    assert_eq!(refused("enum A { x"), at("cap_unexpected_token", 1, 11));
}
