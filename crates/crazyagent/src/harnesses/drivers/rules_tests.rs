use super::*;
use crate::adapters::FIELD_BYTES_MAX;
use serde_json::json;

const STANDIN: &str = include_str!("../../../tests/fixtures/harness/standin-jsonl-turn.yaml");

fn standin() -> Manifest {
    manifest::parse(STANDIN.as_bytes()).unwrap()
}

/// The stand-in manifest with its rule sets replaced by one set `s` whose rules are the
/// given lines, written four spaces in.
fn set(body: &str) -> Manifest {
    let (head, _) = STANDIN.split_once("\nrules:\n").unwrap();
    let text = format!("{head}\nrules:\n  s:\n{body}");
    manifest::parse(text.as_bytes()).unwrap()
}

fn refused(body: &str) -> (&'static str, String) {
    let refusal = read(&set(body), "s").unwrap_err();
    (refusal.code, refusal.message)
}

fn rule(when: &[(&str, &str)], emit: Fact) -> Rule {
    Rule {
        when: when
            .iter()
            .map(|(path, want)| ((*path).to_owned(), (*want).to_owned()))
            .collect(),
        emit,
        text: None,
        name: None,
        session: None,
        success: None,
        tokens: Vec::new(),
        rate_limited: None,
    }
}

fn found(fact: Fact) -> Matched {
    Matched {
        fact,
        text: None,
        name: None,
        session: None,
        success: None,
        tokens: None,
        rate_limited: None,
    }
}

/// DEC-180 Task 11: a rule set is read from the manifest's `rules` block in file order, each
/// rule with its conditions, its fact and the paths its details are read from.
#[test]
fn the_stand_in_rule_set_is_read_in_file_order_with_every_field() {
    let rules = read(&standin(), "standin-events").unwrap();
    let facts: Vec<Fact> = rules.iter().map(|rule| rule.emit).collect();
    assert_eq!(
        facts,
        [
            Fact::Ready,
            Fact::TurnStarted,
            Fact::Text,
            Fact::ToolUse,
            Fact::Waiting,
            Fact::Interrupted,
            Fact::Usage,
            Fact::TurnFinished,
            Fact::TurnError,
            Fact::Exited,
        ]
    );
    assert_eq!(
        rules[0],
        Rule {
            session: Some("id".into()),
            ..rule(&[("type", "session")], Fact::Ready)
        }
    );
    assert_eq!(
        rules[7],
        Rule {
            success: Some("ok".into()),
            tokens: vec!["usage.input".into(), "usage.output".into()],
            ..rule(&[("type", "done")], Fact::TurnFinished)
        }
    );
    assert_eq!(rules[8].rate_limited.as_deref(), Some("message"));
}

/// DEC-180 Task 11: every refused rule set names the setting in one plain sentence, with the
/// manifest refusal codes.
#[test]
fn each_refused_rule_names_its_setting() {
    let missing = read(&standin(), "other-events").unwrap_err();
    assert_eq!(
        (missing.code, missing.message.as_str()),
        (
            manifest::MISSING,
            "rules.other-events: this setting is needed."
        )
    );
    let cases: [(&str, &str, &str); 9] = [
        (
            "    a:\n      when:\n        type: \"x\"\n      emit: \"text\"\n      text: \"delta\"\n      colour: \"red\"\n",
            manifest::UNKNOWN_KEY,
            "rules.s.a.colour: this key is not one a rule has.",
        ),
        (
            "    a:\n      emit: \"waiting\"\n",
            manifest::MISSING,
            "rules.s.a.when: this setting is needed.",
        ),
        (
            "    a:\n      when:\n        type: \"x\"\n      emit: \"telepathy\"\n",
            manifest::KIND,
            "rules.s.a.emit: CrazyAgent has no fact named telepathy.",
        ),
        (
            "    a:\n      when:\n        type: 1\n      emit: \"waiting\"\n",
            manifest::TYPE,
            "rules.s.a.when.type: this must be text in quotes.",
        ),
        (
            "    a:\n      when:\n        type: \"x\"\n      emit: \"text\"\n",
            manifest::MISSING,
            "rules.s.a.text: this setting is needed for this fact.",
        ),
        (
            "    a:\n      when:\n        type: \"x\"\n      emit: \"tool_use\"\n",
            manifest::MISSING,
            "rules.s.a.name: this setting is needed for this fact.",
        ),
        (
            "    a:\n      when:\n        type: \"x\"\n      emit: \"usage\"\n",
            manifest::MISSING,
            "rules.s.a.tokens: this setting is needed for this fact.",
        ),
        (
            "    a:\n      when:\n        type: \"x\"\n      emit: \"text\"\n      text: \"a..b\"\n",
            manifest::VALUE,
            "rules.s.a.text: a path is 1 to 8 parts of letters, digits, _ or -, joined by dots.",
        ),
        (
            "    a:\n      when:\n        type: \"x\"\n      emit: \"usage\"\n      tokens:\n        - \"a\"\n        - \"b\"\n        - \"c\"\n        - \"d\"\n        - \"e\"\n",
            manifest::VALUE,
            "rules.s.a.tokens: a rule reads 1 to 4 token counts.",
        ),
    ];
    for (body, code, message) in cases {
        assert_eq!(refused(body), (code, message.to_owned()), "{body}");
    }
    let deep = format!("rules.s.a.text: a path is 1 to {PATH_PARTS_MAX} parts");
    let (_, message) = refused(
        "    a:\n      when:\n        type: \"x\"\n      emit: \"text\"\n      text: \"a.b.c.d.e.f.g.h.i\"\n",
    );
    assert!(message.starts_with(&deep), "{message}");
}

/// DEC-180 Task 11: the first rule in file order whose conditions all hold gives its fact; a
/// rule with one condition that fails is passed over even when its others hold.
#[test]
fn the_first_rule_whose_conditions_all_hold_gives_its_fact_and_details() {
    let rules = [
        rule(&[("type", "item"), ("item.kind", "tool")], Fact::ToolUse),
        Rule {
            text: Some("item.text".into()),
            ..rule(&[("type", "item")], Fact::Text)
        },
        rule(&[("type", "item")], Fact::Waiting),
    ];
    let text = json!({"type": "item", "item": {"kind": "message", "text": "hello"}});
    assert_eq!(
        matched(&rules, &text),
        Some(Matched {
            text: Some("hello".into()),
            ..found(Fact::Text)
        })
    );
    let tool = json!({"type": "item", "item": {"kind": "tool"}});
    assert_eq!(matched(&rules, &tool), Some(found(Fact::ToolUse)));
}

/// DEC-180 Task 11: a condition compares a text, and a number or a flag by its JSON text; a
/// path part of only digits indexes a list.
#[test]
fn a_condition_reads_text_numbers_flags_and_list_items_by_their_json_text() {
    let rules = [
        rule(&[("code", "429")], Fact::TurnError),
        rule(&[("final", "true")], Fact::TurnFinished),
        rule(&[("content.1.type", "tool")], Fact::ToolUse),
    ];
    assert_eq!(
        matched(&rules, &json!({"code": 429})),
        Some(found(Fact::TurnError))
    );
    assert_eq!(
        matched(&rules, &json!({"final": true})),
        Some(found(Fact::TurnFinished))
    );
    assert_eq!(
        matched(
            &rules,
            &json!({"content": [{"type": "text"}, {"type": "tool"}]})
        ),
        Some(found(Fact::ToolUse))
    );
    assert_eq!(
        matched(&rules, &json!({"final": "true"})),
        Some(found(Fact::TurnFinished))
    );
}

/// DEC-180 Task 11: an object no rule matches gives nothing, and a path that reaches no
/// value, a null, an object, a list item past the end or a key on a list never matches.
#[test]
fn an_object_no_rule_matches_or_a_path_with_no_value_gives_nothing() {
    let rules = [
        rule(&[("type", "x")], Fact::Waiting),
        rule(&[("list.3", "a")], Fact::Waiting),
        rule(&[("list.key", "a")], Fact::Waiting),
    ];
    for object in [
        json!({"type": "y"}),
        json!({}),
        json!({"type": null}),
        json!({"type": {"x": "x"}}),
        json!({"list": ["a"]}),
        json!({"list": [{"key": "a"}]}),
        json!(["x"]),
    ] {
        assert_eq!(matched(&rules, &object), None, "{object}");
    }
}

/// DEC-180 Task 11: details are read by their type: text and tool names clipped to
/// `FIELD_BYTES_MAX` and none when empty, token counts in the order input, output, cache
/// read, cache write with a missing count as 0 and none when no count is there, a flag as
/// the turn's success, and a rate limit from a flag or from the words of a message.
#[test]
fn details_are_clipped_counted_and_read_as_their_types() {
    let full = Rule {
        text: Some("text".into()),
        name: Some("name".into()),
        session: Some("id".into()),
        success: Some("ok".into()),
        tokens: vec!["in".into(), "out".into(), "read".into(), "write".into()],
        rate_limited: Some("why".into()),
        ..rule(&[("type", "x")], Fact::TurnFinished)
    };
    let long = "a".repeat(FIELD_BYTES_MAX + 10);
    let read = matched(
        std::slice::from_ref(&full),
        &json!({"type": "x", "text": long, "name": "", "id": "s-1", "ok": false, "in": 7, "read": 2, "why": "Rate limit reached"}),
    )
    .unwrap();
    assert!(read.text.as_ref().unwrap().len() <= FIELD_BYTES_MAX);
    assert!(
        read.text
            .as_ref()
            .unwrap()
            .ends_with("more bytes not shown]")
    );
    assert_eq!(read.name, None);
    assert_eq!(read.session.as_deref(), Some("s-1"));
    assert_eq!(read.success, Some(false));
    assert_eq!(
        read.tokens,
        Some(Tokens {
            input: 7,
            output: 0,
            cache_read: 2,
            cache_write: 0,
        })
    );
    assert_eq!(read.rate_limited, Some(true));
    let other = matched(
        std::slice::from_ref(&full),
        &json!({"type": "x", "ok": "yes", "in": -1, "out": "3", "why": false}),
    )
    .unwrap();
    assert_eq!(other.success, None);
    assert_eq!(other.tokens, None);
    assert_eq!(other.rate_limited, Some(false));
    let calm = matched(
        std::slice::from_ref(&full),
        &json!({"type": "x", "why": "the model refused"}),
    )
    .unwrap();
    assert_eq!(calm.rate_limited, Some(false));
}
