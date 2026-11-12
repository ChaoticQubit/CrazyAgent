use super::*;
use sha2::{Digest, Sha256};
use std::io::Write;

/// The tiny model the rows run on: 16 tokens of 4 floats, the first 2 kept.
pub(crate) const FIXTURE_SHAPE: Shape = Shape {
    rows: 16,
    width: 4,
    dims: 2,
};

/// The fixture vocabulary by id: BERT's five special tokens first, then whole words,
/// continuations and punctuation that the rows below name by id.
pub(crate) const FIXTURE_VOCAB: [&str; 16] = [
    "[PAD]", "[UNK]", "[CLS]", "[SEP]", "[MASK]", "hello", "world", "##s", ".", "cafe", "un",
    "##aff", "##able", "\u{4e2d}", "!", "naive",
];

/// The fixture's row for one token: its id, then 1, then values the embedding must never
/// read, so a row read at the wrong offset or past the kept dims shows. Input: the id.
/// Output: the row of `FIXTURE_SHAPE.width` floats.
pub(crate) fn fixture_row(id: usize) -> [f32; 4] {
    [id as f32, 1.0, 10.0 * id as f32, 99.0]
}

/// The tokenizer.json of the fixture: the pinned file's normalizer, pre-tokenizer and
/// WordPiece settings with `FIXTURE_VOCAB`. Output: the JSON text.
pub(crate) fn fixture_tokenizer_json() -> String {
    let vocab: Vec<String> = FIXTURE_VOCAB
        .iter()
        .enumerate()
        .map(|(id, token)| format!("{}: {id}", serde_json::to_string(token).unwrap()))
        .collect();
    let added: Vec<String> = FIXTURE_VOCAB[..5]
        .iter()
        .enumerate()
        .map(|(id, token)| {
            format!(
                "{{\"id\": {id}, \"content\": \"{token}\", \"single_word\": false, \"lstrip\": false, \"rstrip\": false, \"normalized\": false, \"special\": true}}"
            )
        })
        .collect();
    format!(
        "{{\"version\": \"1.0\", \"truncation\": null, \"padding\": null, \"added_tokens\": [{}], \
         \"normalizer\": {{\"type\": \"BertNormalizer\", \"clean_text\": true, \"handle_chinese_chars\": true, \"strip_accents\": null, \"lowercase\": true}}, \
         \"pre_tokenizer\": {{\"type\": \"BertPreTokenizer\"}}, \
         \"post_processor\": {{\"type\": \"TemplateProcessing\"}}, \
         \"decoder\": {{\"type\": \"WordPiece\", \"prefix\": \"##\", \"cleanup\": true}}, \
         \"model\": {{\"type\": \"WordPiece\", \"unk_token\": \"[UNK]\", \"continuing_subword_prefix\": \"##\", \"max_input_chars_per_word\": 100, \"vocab\": {{{}}}}}}}",
        added.join(", "),
        vocab.join(", ")
    )
}

/// A safetensors file: the 8-byte little-endian header length, the header padded with
/// spaces to a multiple of 8 as the pinned file is, then the floats little-endian. Inputs:
/// the header JSON and the data. Output: the file bytes.
pub(crate) fn safetensors_bytes(header: &str, data: &[f32]) -> Vec<u8> {
    let mut header = header.to_owned();
    while header.len() % 8 != 0 {
        header.push(' ');
    }
    let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
    bytes.extend_from_slice(header.as_bytes());
    for value in data {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// The fixture model file: one F32 tensor `embedding.weight` of `FIXTURE_SHAPE`, named
/// and laid out as the pinned file is. Output: the file bytes.
pub(crate) fn fixture_model_bytes() -> Vec<u8> {
    let data: Vec<f32> = (0..FIXTURE_SHAPE.rows).flat_map(fixture_row).collect();
    let header = format!(
        "{{\"embedding.weight\":{{\"dtype\":\"F32\",\"shape\":[{},{}],\"data_offsets\":[0,{}]}}}}",
        FIXTURE_SHAPE.rows,
        FIXTURE_SHAPE.width,
        data.len() * 4
    );
    safetensors_bytes(&header, &data)
}

/// Writes bytes to a new file in a folder. Inputs: the folder, the name and the bytes.
/// Output: the path.
pub(crate) fn write_file(folder: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = folder.join(name);
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(bytes))
        .unwrap();
    path
}

/// The lowercase hex sha256 of some bytes. Input: the bytes. Output: the digest.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The fixture model opened from files, as `LocalModel::open` opens the pinned ones.
/// Input: a scratch folder. Output: the model.
fn fixture_model(folder: &Path) -> LocalModel {
    let tokenizer = Tokenizer::from_json(fixture_tokenizer_json().as_bytes(), FIXTURE_SHAPE.rows)
        .expect("the fixture tokenizer is BERT uncased WordPiece");
    let path = write_file(folder, "model.safetensors", &fixture_model_bytes());
    let rows = Rows::open(std::fs::File::open(path).unwrap(), &FIXTURE_SHAPE)
        .expect("the fixture model has one F32 tensor of its shape");
    LocalModel::from_parts(tokenizer, rows)
}

fn assert_close(got: &[f32], want: &[f32], text: &str) {
    assert_eq!(got.len(), want.len(), "{text:?}");
    for (at, (got, want)) in got.iter().zip(want).enumerate() {
        assert!(
            (got - want).abs() < 1e-5,
            "{text:?} dim {at}: got {got}, want {want}"
        );
    }
}

#[test]
fn embed_is_the_l2_normalized_mean_of_the_first_dims_of_the_token_rows() {
    let folder = tempfile::tempdir().unwrap();
    let model = fixture_model(folder.path());
    let cases: [(&str, [f32; 2]); 5] = [
        ("Hello world.", [0.987763, 0.155963]),
        ("Caf\u{e9} unaffable", [0.995495, 0.094809]),
        ("hello [MASK] \u{4e2d}!", [0.993884, 0.110432]),
        ("hellos", [0.986394, 0.164399]),
        ("xyz", [0.707107, 0.707107]),
    ];
    let texts: Vec<&str> = cases.iter().map(|(text, _)| *text).collect();
    let rows = model.embed(&texts).unwrap();
    assert_eq!(rows.len(), cases.len());
    for ((text, want), got) in cases.iter().zip(&rows) {
        assert_eq!(
            got.len(),
            FIXTURE_SHAPE.dims,
            "only the first dims are kept"
        );
        assert_close(got, want, text);
    }
}

#[test]
fn a_repeated_token_counts_once_for_each_time_it_appears() {
    let folder = tempfile::tempdir().unwrap();
    let model = fixture_model(folder.path());
    let rows = model.embed(&["hello hello world"]).unwrap();
    assert_close(&rows[0], &[0.982872, 0.184289], "hello hello world");
}

#[test]
fn a_text_with_no_tokens_embeds_as_zeros_and_no_texts_embed_as_no_rows() {
    let folder = tempfile::tempdir().unwrap();
    let model = fixture_model(folder.path());
    let rows = model
        .embed(&["", "   \t\n", "\u{0}\u{fffd}\u{200b}"])
        .unwrap();
    for row in &rows {
        assert_eq!(row, &vec![0.0; FIXTURE_SHAPE.dims]);
    }
    assert!(model.embed(&[]).unwrap().is_empty());
}

#[test]
fn a_file_is_used_only_at_its_pinned_size_and_sha256() {
    let folder = tempfile::tempdir().unwrap();
    let bytes = fixture_model_bytes();
    let path = write_file(folder.path(), "model.safetensors", &bytes);
    let sha = sha256_hex(&bytes);
    assert!(verified(&path, bytes.len() as u64, &sha).is_ok());
    assert_eq!(
        verified(&path, bytes.len() as u64 + 1, &sha).err(),
        Some("search_model_mismatch"),
        "a size other than the pinned one"
    );
    let mut changed = bytes.clone();
    *changed.last_mut().unwrap() ^= 1;
    write_file(folder.path(), "model.safetensors", &changed);
    assert_eq!(
        verified(&path, bytes.len() as u64, &sha).err(),
        Some("search_model_mismatch"),
        "one changed byte at the pinned size"
    );
    assert_eq!(
        verified(&folder.path().join("absent"), 1, &sha).err(),
        Some("search_model_missing")
    );
}

#[cfg(unix)]
#[test]
fn a_link_in_place_of_a_model_file_is_never_followed() {
    let folder = tempfile::tempdir().unwrap();
    let bytes = fixture_model_bytes();
    let real = write_file(folder.path(), "real", &bytes);
    let link = folder.path().join("model.safetensors");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(verified(&link, bytes.len() as u64, &sha256_hex(&bytes)).is_err());
}

#[test]
fn the_pinned_files_are_the_reviewed_revision_on_huggingface_co() {
    for pin in PINNED {
        assert!(pin.url.starts_with(
            "https://huggingface.co/sentence-transformers/static-retrieval-mrl-en-v1/resolve/f60985c706f192d45d218078e49e5a8b6f15283a/0_StaticEmbedding/"
        ));
        assert!(pin.url.ends_with(pin.name));
        assert_eq!(pin.sha256.len(), 64);
    }
    assert_eq!(MODEL.bytes, 125_018_208);
    assert_eq!(
        MODEL.sha256,
        "164fc63ee9f9267be7378fcbd7df99d09788a2f45244c92aa99ae5a574925716"
    );
    assert_eq!(TOKENIZER.bytes, 711_396);
    assert_eq!(
        TOKENIZER.sha256,
        "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66"
    );
    assert_eq!(DOWNLOAD_BYTES, 125_729_604);
    assert_eq!((SHAPE.rows, SHAPE.width, SHAPE.dims), (30_522, 1_024, 256));
}

#[test]
fn the_local_model_is_shared_between_the_indexer_and_queries() {
    fn shared<T: Send + Sync>() {}
    shared::<LocalModel>();
    shared::<Downloads>();
}

#[test]
fn every_local_model_code_has_a_plain_sentence() {
    assert_eq!(
        CODES,
        [
            "search_model_confirm",
            "search_model_downloading",
            "search_model_download_failed",
            "search_model_mismatch",
            "search_model_store_unavailable",
            "search_model_missing",
        ],
        "the codes agreed with w-v3-plugins, plus the confirm leader QC asked for"
    );
    for code in CODES {
        assert!(code.starts_with("search_model_"), "{code}");
        assert!(
            crate::app_plugins::plain_problem(code).is_some(),
            "{code} needs a sentence in app_plugins PLAIN"
        );
    }
}
