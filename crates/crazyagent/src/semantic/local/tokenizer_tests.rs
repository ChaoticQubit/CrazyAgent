use super::*;
use crate::semantic::local::tests::{FIXTURE_SHAPE, FIXTURE_VOCAB, fixture_tokenizer_json};

fn fixture() -> Tokenizer {
    Tokenizer::from_json(fixture_tokenizer_json().as_bytes(), FIXTURE_SHAPE.rows).unwrap()
}

fn id(token: &str) -> u32 {
    FIXTURE_VOCAB.iter().position(|known| *known == token).unwrap() as u32
}

#[test]
fn the_normalizer_drops_controls_spaces_out_cjk_strips_accents_and_lowercases_char_by_char() {
    assert_eq!(normalize("Hello\tWorld\r\n"), "hello world  ");
    assert_eq!(
        normalize("a\u{0}b\u{fffd}c\u{200b}d\u{e000}e\u{7}f"),
        "abcdef",
        "NUL, U+FFFD and Cc, Cf, Co characters go"
    );
    assert_eq!(
        normalize("x\u{378}y"),
        "x\u{378}y",
        "an unassigned code point stays, as the reference keeps it"
    );
    assert_eq!(
        normalize("x\u{b}y\u{85}z"),
        "xyz",
        "a control that is also white space goes, it does not become a space"
    );
    assert_eq!(normalize("a\u{3000}b\u{a0}c"), "a b c");
    assert_eq!(
        normalize("\u{4e2d}\u{6587}"),
        " \u{4e2d}  \u{6587} ",
        "every CJK ideograph is set apart by spaces"
    );
    assert_eq!(normalize("Caf\u{e9} Na\u{ef}ve \u{c5}NGSTR\u{d6}M"), "cafe naive angstrom");
    assert_eq!(normalize("cafe\u{301}"), "cafe", "a combining mark already apart goes");
    assert_eq!(normalize("\u{130}"), "i", "dotted capital I decomposes, loses its dot, then lowercases");
    assert_eq!(
        normalize("\u{3a3}\u{3a3}"),
        "\u{3c3}\u{3c3}",
        "lowercase is per character, so a final sigma is not chosen"
    );
}

#[test]
fn the_pre_tokenizer_splits_on_white_space_and_isolates_every_punctuation_character() {
    assert_eq!(
        pre_tokenize("hello,  world!"),
        vec!["hello", ",", "world", "!"]
    );
    assert_eq!(
        pre_tokenize("a$b+c`d"),
        vec!["a", "$", "b", "+", "c", "`", "d"],
        "ASCII symbols count as punctuation, as BERT's check does"
    );
    assert_eq!(
        pre_tokenize("\u{ab}x\u{bb}\u{2014}y\u{3002}"),
        vec!["\u{ab}", "x", "\u{bb}", "\u{2014}", "y", "\u{3002}"],
        "Unicode punctuation (Pi, Pf, Pd, Po) splits too"
    );
    assert_eq!(pre_tokenize("x\u{2028}y"), vec!["x", "y"]);
    assert!(pre_tokenize(" \t ").is_empty());
}

#[test]
fn wordpiece_takes_the_longest_known_piece_first_and_gives_one_unk_for_a_word_it_cannot_finish() {
    let tokenizer = fixture();
    assert_eq!(
        tokenizer.tokenize("unaffable"),
        vec![id("un"), id("##aff"), id("##able")]
    );
    assert_eq!(tokenizer.tokenize("hellos"), vec![id("hello"), id("##s")]);
    assert_eq!(
        tokenizer.tokenize("hellox world"),
        vec![id("[UNK]"), id("world")],
        "the whole word is one [UNK], never its known start"
    );
    let long = "a".repeat(101);
    assert_eq!(
        tokenizer.tokenize(&long),
        vec![id("[UNK]")],
        "a word longer than max_input_chars_per_word is one [UNK]"
    );
}

#[test]
fn special_tokens_in_the_text_keep_their_own_ids_and_never_add_cls_or_sep() {
    let tokenizer = fixture();
    assert_eq!(
        tokenizer.tokenize("hello [MASK] world"),
        vec![id("hello"), id("[MASK]"), id("world")]
    );
    assert_eq!(
        tokenizer.tokenize("hello[SEP]world"),
        vec![id("hello"), id("[SEP]"), id("world")],
        "special tokens match in the raw text, with no space around them"
    );
    assert_eq!(
        tokenizer.tokenize("[mask]"),
        vec![id("[UNK]"), id("[UNK]"), id("[UNK]")],
        "a lowercase spelling is plain text: [, mask and ] are not in the fixture"
    );
    assert_eq!(tokenizer.tokenize("hello"), vec![id("hello")]);
}

#[test]
fn tokens_per_input_and_input_bytes_are_bounded() {
    let tokenizer = fixture();
    let many = "hello ".repeat(TOKENS_MAX + 10);
    assert!(many.len() <= INPUT_BYTES_MAX);
    assert_eq!(tokenizer.tokenize(&many).len(), TOKENS_MAX);
    let long = format!("{} hello", "x".repeat(INPUT_BYTES_MAX));
    assert_eq!(
        tokenizer.tokenize(&long),
        vec![id("[UNK]")],
        "text past INPUT_BYTES_MAX is not read"
    );
    let odd = format!("a{}", "\u{e9}".repeat(INPUT_BYTES_MAX));
    assert_eq!(
        tokenizer.tokenize(&odd),
        vec![id("[UNK]")],
        "the cut steps back to a character boundary"
    );
}

#[test]
fn a_tokenizer_file_that_is_not_the_pinned_bert_uncased_wordpiece_is_refused() {
    let good = fixture_tokenizer_json();
    assert!(Tokenizer::from_json(good.as_bytes(), FIXTURE_SHAPE.rows).is_ok());
    let refused = [
        good.replace("\"lowercase\": true", "\"lowercase\": false"),
        good.replace("\"strip_accents\": null", "\"strip_accents\": false"),
        good.replace("\"clean_text\": true", "\"clean_text\": false"),
        good.replace("\"handle_chinese_chars\": true", "\"handle_chinese_chars\": false"),
        good.replace("BertPreTokenizer", "Whitespace"),
        good.replace("\"type\": \"WordPiece\", \"unk_token\"", "\"type\": \"BPE\", \"unk_token\""),
        good.replace("\"continuing_subword_prefix\": \"##\"", "\"continuing_subword_prefix\": \"@@\""),
        good.replace("\"max_input_chars_per_word\": 100", "\"max_input_chars_per_word\": 100000"),
        good.replace("\"unk_token\": \"[UNK]\"", "\"unk_token\": \"[NOPE]\""),
        good.replace("\"truncation\": null", "\"truncation\": {\"max_length\": 8}"),
        good.replace("\"padding\": null", "\"padding\": {\"length\": 8}"),
        good.replace("\"naive\": 15", "\"naive\": 16"),
        good.replace("\"naive\": 15", "\"naive\": 14"),
        good.replace("\"special\": true", "\"special\": false"),
        good.replace("{\"id\": 4, \"content\": \"[MASK]\"", "{\"id\": 3, \"content\": \"[MASK]\""),
        "not json".to_owned(),
    ];
    for (at, text) in refused.iter().enumerate() {
        assert_ne!(text, &good, "case {at} changes the file");
        assert_eq!(
            Tokenizer::from_json(text.as_bytes(), FIXTURE_SHAPE.rows).err(),
            Some("search_model_format"),
            "case {at}"
        );
    }
    assert_eq!(
        Tokenizer::from_json(good.as_bytes(), FIXTURE_SHAPE.rows + 1).err(),
        Some("search_model_format"),
        "the vocabulary must have one id per model row"
    );
    let padded = format!("{good}{}", " ".repeat(TOKENIZER_BYTES_MAX));
    assert_eq!(
        Tokenizer::from_json(padded.as_bytes(), FIXTURE_SHAPE.rows).err(),
        Some("search_model_format"),
        "a file past TOKENIZER_BYTES_MAX is not parsed"
    );
}
