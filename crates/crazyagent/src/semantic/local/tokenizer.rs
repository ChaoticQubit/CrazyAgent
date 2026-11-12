use icu_normalizer::DecomposingNormalizerBorrowed;
use icu_properties::CodePointMapData;
use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};
use serde::Deserialize;
use std::collections::HashMap;

/// The largest tokenizer.json read: the pinned file is 711,396 bytes.
pub const TOKENIZER_BYTES_MAX: usize = 4 << 20;

/// The most bytes of one text that are tokenized; the indexer cuts chunks to 2,000 bytes.
pub const INPUT_BYTES_MAX: usize = 32 * 1024;

/// The most tokens one text gives; later tokens are not embedded.
pub const TOKENS_MAX: usize = 4_096;

const WORD_CHARS_MAX: usize = 200;
const TOKEN_BYTES_MAX: usize = 256;
const SPECIAL_MAX: usize = 16;
const PREFIX: &str = "##";

/// BERT uncased WordPiece as the pinned tokenizer.json defines it and the reference
/// `tokenizers` crate runs it with no special tokens added: special tokens matched in the
/// raw text, then BertNormalizer, BertPreTokenizer and greedy longest-first WordPiece.
pub struct Tokenizer {
    vocab: HashMap<String, u32>,
    unk: u32,
    word_chars_max: usize,
    special: Vec<(String, u32)>,
}

#[derive(Deserialize)]
struct FileJson {
    truncation: Option<serde_json::Value>,
    padding: Option<serde_json::Value>,
    added_tokens: Vec<AddedJson>,
    normalizer: NormalizerJson,
    pre_tokenizer: KindJson,
    model: ModelJson,
}

#[derive(Deserialize)]
struct AddedJson {
    id: u32,
    content: String,
    single_word: bool,
    lstrip: bool,
    rstrip: bool,
    normalized: bool,
    special: bool,
}

#[derive(Deserialize)]
struct NormalizerJson {
    #[serde(rename = "type")]
    kind: String,
    clean_text: bool,
    handle_chinese_chars: bool,
    strip_accents: Option<bool>,
    lowercase: bool,
}

#[derive(Deserialize)]
struct KindJson {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ModelJson {
    #[serde(rename = "type")]
    kind: String,
    unk_token: String,
    continuing_subword_prefix: String,
    max_input_chars_per_word: usize,
    vocab: HashMap<String, u32>,
}

impl Tokenizer {
    /// Reads a tokenizer.json and keeps it only when it is BERT uncased WordPiece exactly as
    /// the pinned file is: BertNormalizer with clean text, CJK spacing, accents stripped and
    /// lowercase; BertPreTokenizer; WordPiece with `##` and an unknown token; no truncation
    /// or padding; special tokens matched raw; one id per matrix row. Inputs: the file bytes
    /// and the matrix rows. Output: the tokenizer, or `search_model_format`.
    pub fn from_json(bytes: &[u8], rows: usize) -> Result<Self, &'static str> {
        if bytes.len() > TOKENIZER_BYTES_MAX {
            return Err("search_model_format");
        }
        let file: FileJson = serde_json::from_slice(bytes).map_err(|_| "search_model_format")?;
        let normalizer = &file.normalizer;
        let bert_uncased = normalizer.kind == "BertNormalizer"
            && normalizer.clean_text
            && normalizer.handle_chinese_chars
            && normalizer.strip_accents.unwrap_or(normalizer.lowercase)
            && normalizer.lowercase;
        let model = &file.model;
        let wordpiece = model.kind == "WordPiece"
            && model.continuing_subword_prefix == PREFIX
            && (1..=WORD_CHARS_MAX).contains(&model.max_input_chars_per_word);
        if !bert_uncased
            || file.pre_tokenizer.kind != "BertPreTokenizer"
            || !wordpiece
            || file.truncation.is_some()
            || file.padding.is_some()
            || model.vocab.len() != rows
            || file.added_tokens.len() > SPECIAL_MAX
        {
            return Err("search_model_format");
        }
        let mut seen = vec![false; rows];
        for (token, id) in &model.vocab {
            let id = *id as usize;
            if token.is_empty() || token.len() > TOKEN_BYTES_MAX || id >= rows || seen[id] {
                return Err("search_model_format");
            }
            seen[id] = true;
        }
        let unk = *model
            .vocab
            .get(&model.unk_token)
            .ok_or("search_model_format")?;
        let mut special = Vec::with_capacity(file.added_tokens.len());
        for added in &file.added_tokens {
            let raw = added.special
                && !added.normalized
                && !added.single_word
                && !added.lstrip
                && !added.rstrip;
            if !raw || model.vocab.get(&added.content) != Some(&added.id) {
                return Err("search_model_format");
            }
            special.push((added.content.clone(), added.id));
        }
        assert!(
            seen.iter().all(|seen| *seen),
            "ids are distinct and below rows"
        );
        Ok(Self {
            vocab: file.model.vocab,
            unk,
            word_chars_max: file.model.max_input_chars_per_word,
            special,
        })
    }

    /// The token ids of a text: its first `INPUT_BYTES_MAX` bytes, cut at a character
    /// boundary, and at most `TOKENS_MAX` ids. Input: the text. Output: the ids in order,
    /// every one below the matrix rows.
    pub fn tokenize(&self, text: &str) -> Vec<u32> {
        let text = cut(text, INPUT_BYTES_MAX);
        let mut ids = Vec::new();
        let mut rest = text;
        while !rest.is_empty() && ids.len() < TOKENS_MAX {
            let (plain, special) = self.next_special(rest);
            let normal = normalize(&rest[..plain]);
            for word in pre_tokenize(&normal) {
                if ids.len() >= TOKENS_MAX {
                    break;
                }
                self.word_piece(word, &mut ids);
            }
            match special {
                Some((id, length)) => {
                    ids.push(id);
                    rest = &rest[plain + length..];
                }
                None => rest = "",
            }
        }
        ids.truncate(TOKENS_MAX);
        assert!(ids.len() <= TOKENS_MAX);
        ids
    }

    /// Finds the first special token in raw text, the longest one where several start at
    /// the same place, as the reference's leftmost-longest match does. Input: the text.
    /// Output: the plain text's length before it and the token's id and length, if any.
    fn next_special(&self, text: &str) -> (usize, Option<(u32, usize)>) {
        for (at, _) in text.char_indices() {
            let found = self
                .special
                .iter()
                .filter(|(content, _)| text[at..].starts_with(content.as_str()))
                .max_by_key(|(content, _)| content.len());
            if let Some((content, id)) = found {
                return (at, Some((*id, content.len())));
            }
        }
        (text.len(), None)
    }

    /// WordPiece for one word: the longest known piece from the start, then `##` pieces,
    /// and one unknown token for a word too long or one that cannot be finished. Inputs:
    /// the word and the ids to add to.
    fn word_piece(&self, word: &str, ids: &mut Vec<u32>) {
        assert!(!word.is_empty());
        if word.chars().count() > self.word_chars_max {
            ids.push(self.unk);
            return;
        }
        let first = ids.len();
        let mut key = String::with_capacity(word.len() + PREFIX.len());
        let mut start = 0;
        while start < word.len() {
            let mut end = word.len();
            let mut found = None;
            while start < end {
                key.clear();
                if start > 0 {
                    key.push_str(PREFIX);
                }
                key.push_str(&word[start..end]);
                if let Some(id) = self.vocab.get(&key) {
                    found = Some(*id);
                    break;
                }
                end -= word[start..end]
                    .chars()
                    .next_back()
                    .map_or(1, char::len_utf8);
            }
            let Some(id) = found else {
                ids.truncate(first);
                ids.push(self.unk);
                return;
            };
            ids.push(id);
            start = end;
        }
    }
}

/// The longest start of a text that fits a byte budget and ends on a character boundary.
/// Inputs: the text and the budget. Output: the start.
fn cut(text: &str, budget: usize) -> &str {
    let mut end = budget.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// BertNormalizer as the reference runs it, in its order: NUL, U+FFFD and control
/// characters (Cc, Cf, Co, but not tab, newline or return) go and white space becomes a
/// space; CJK ideographs are set apart by spaces; then NFD with nonspacing marks (Mn)
/// dropped; then lowercase character by character. Input: the text. Output: the text.
pub fn normalize(text: &str) -> String {
    let categories = CodePointMapData::<GeneralCategory>::new();
    let mut spaced = String::with_capacity(text.len());
    for c in text.chars() {
        let control = !matches!(c, '\t' | '\n' | '\r')
            && matches!(
                categories.get(c),
                GeneralCategory::Control | GeneralCategory::Format | GeneralCategory::PrivateUse
            );
        if c == '\0' || c == '\u{fffd}' || control {
            continue;
        }
        if c.is_whitespace() {
            spaced.push(' ');
        } else if is_cjk(c) {
            spaced.push(' ');
            spaced.push(c);
            spaced.push(' ');
        } else {
            spaced.push(c);
        }
    }
    let mut normal = String::with_capacity(spaced.len());
    let nfd = DecomposingNormalizerBorrowed::new_nfd();
    for c in nfd.normalize_iter(spaced.chars()) {
        if categories.get(c) != GeneralCategory::NonspacingMark {
            normal.extend(c.to_lowercase());
        }
    }
    normal
}

/// BertPreTokenizer: splits on white space, which goes, and sets every punctuation
/// character apart as its own piece: ASCII punctuation and symbols, and Unicode P*. Input:
/// normalized text. Output: the pieces in order, none empty.
pub fn pre_tokenize(text: &str) -> Vec<&str> {
    let categories = CodePointMapData::<GeneralCategory>::new();
    let mut pieces = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        let punctuation = c.is_ascii_punctuation()
            || GeneralCategoryGroup::Punctuation.contains(categories.get(c));
        if c.is_whitespace() || punctuation {
            if let Some(from) = start.take() {
                pieces.push(&text[from..at]);
            }
            if punctuation {
                pieces.push(&text[at..at + c.len_utf8()]);
            }
        } else if start.is_none() {
            start = Some(at);
        }
    }
    if let Some(from) = start {
        pieces.push(&text[from..]);
    }
    assert!(pieces.iter().all(|piece| !piece.is_empty()));
    pieces
}

/// The CJK ideograph blocks BERT sets apart, the same ranges as the reference.
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x4E00..=0x9FFF
            | 0x3400..=0x4DBF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B920..=0x2CEAF
            | 0xF900..=0xFAFF
            | 0x2F800..=0x2FA1F
    )
}

#[cfg(test)]
#[path = "tokenizer_tests.rs"]
mod tests;
