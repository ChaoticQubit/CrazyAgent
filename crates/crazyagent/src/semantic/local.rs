use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub mod download;
pub mod safetensors;
pub mod tokenizer;

pub use download::{Downloads, Phase, cancel_download, delete, start_download, state};
pub use safetensors::Rows;
pub use tokenizer::Tokenizer;

/// The built-in local runtime's id, as plugin manifests name it (DEC-156).
pub const RUNTIME: &str = "static-retrieval-mrl-en-v1";

/// How many leading dimensions of a row the local runtime keeps (Matryoshka, DEC-156
/// addendum 2: bundled plugins store 256 floats per vector).
pub const DIMS: usize = 256;

/// The most texts one `embed` call takes: the indexer embeds at most 2,000 chunks a round.
pub const TEXTS_MAX: usize = 2_048;

/// Every refusal code of the local runtime, each with a plain sentence in app_plugins.
pub const CODES: [&str; 6] = [
    "search_model_confirm",
    "search_model_downloading",
    "search_model_download_failed",
    "search_model_mismatch",
    "search_model_store_unavailable",
    "search_model_missing",
];

/// One file of the local runtime, pinned by its reviewed revision: the name it is saved
/// under, the first URL fetched (on huggingface.co, so the one redirect it gives goes to a
/// signed CDN URL), and the exact size and sha256 it must have before it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pinned {
    pub name: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// The rows of the embedding matrix: how many (one per vocabulary id), their width, and
/// how many leading dimensions an embedding keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub rows: usize,
    pub width: usize,
    pub dims: usize,
}

/// sentence-transformers/static-retrieval-mrl-en-v1 tokenizer, at the reviewed revision.
pub const TOKENIZER: Pinned = Pinned {
    name: "tokenizer.json",
    url: "https://huggingface.co/sentence-transformers/static-retrieval-mrl-en-v1/resolve/f60985c706f192d45d218078e49e5a8b6f15283a/0_StaticEmbedding/tokenizer.json",
    bytes: 711_396,
    sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
};

/// sentence-transformers/static-retrieval-mrl-en-v1 embedding matrix, F32 [30522, 1024].
pub const MODEL: Pinned = Pinned {
    name: "model.safetensors",
    url: "https://huggingface.co/sentence-transformers/static-retrieval-mrl-en-v1/resolve/f60985c706f192d45d218078e49e5a8b6f15283a/0_StaticEmbedding/model.safetensors",
    bytes: 125_018_208,
    sha256: "164fc63ee9f9267be7378fcbd7df99d09788a2f45244c92aa99ae5a574925716",
};

/// The pinned files in download order, the tokenizer first and the matrix second.
pub const PINNED: [Pinned; 2] = [TOKENIZER, MODEL];

/// The one-time download's size: both pinned files.
pub const DOWNLOAD_BYTES: u64 = TOKENIZER.bytes + MODEL.bytes;

/// The pinned matrix's shape and the dimensions kept.
pub const SHAPE: Shape = Shape {
    rows: 30_522,
    width: 1_024,
    dims: DIMS,
};

const HASH_CHUNK_BYTES: usize = 1 << 16;

/// The folder the local runtime's files live in. Input: the engine root. Output:
/// `<root>/models/static-retrieval-mrl-en-v1`.
pub fn folder(root: &Path) -> PathBuf {
    root.join("models").join(RUNTIME)
}

/// Opens a pinned file to read: a plain file, never through a link, of the pinned size.
/// Inputs: the path and the pinned size. Output: the open file, or `search_model_missing`,
/// `search_model_store_unavailable` or `search_model_mismatch`.
fn open_sized(path: &Path, bytes: u64) -> Result<File, &'static str> {
    let file = crate::platform::open_plain(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "search_model_missing",
        _ => "search_model_store_unavailable",
    })?;
    let metadata = file
        .metadata()
        .map_err(|_| "search_model_store_unavailable")?;
    if !metadata.is_file() || metadata.len() != bytes {
        return Err("search_model_mismatch");
    }
    Ok(file)
}

/// Reads a small pinned file whole and keeps it only when its size and sha256 match, so the
/// bytes checked are the bytes used. Inputs: the path, the pinned size and sha256. Output:
/// the bytes, or the codes of `open_sized` and `search_model_mismatch`.
fn verified_bytes(path: &Path, bytes: u64, sha256: &str) -> Result<Vec<u8>, &'static str> {
    let file = open_sized(path, bytes)?;
    let mut kept = Vec::new();
    file.take(bytes.saturating_add(1))
        .read_to_end(&mut kept)
        .map_err(|_| "search_model_store_unavailable")?;
    if kept.len() as u64 != bytes || format!("{:x}", Sha256::digest(&kept)) != sha256 {
        return Err("search_model_mismatch");
    }
    Ok(kept)
}

/// Opens the large pinned file only when its size and sha256 match, hashing through the
/// handle that is returned; its rows are then read by position from that handle. Inputs:
/// the path, the pinned size and sha256. Output: the open file, or
/// `search_model_missing`, `search_model_store_unavailable` or `search_model_mismatch`.
pub fn verified(path: &Path, bytes: u64, sha256: &str) -> Result<File, &'static str> {
    let mut file = open_sized(path, bytes)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0; HASH_CHUNK_BYTES];
    let mut seen: u64 = 0;
    loop {
        let read = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("search_model_store_unavailable"),
        };
        seen += read as u64;
        if seen > bytes {
            return Err("search_model_mismatch");
        }
        hasher.update(&chunk[..read]);
    }
    if seen != bytes || format!("{:x}", hasher.finalize()) != sha256 {
        return Err("search_model_mismatch");
    }
    Ok(file)
}

/// The built-in local embedding runtime: the BERT uncased WordPiece tokenizer and the
/// verified matrix file, whose rows are read by position, never held in memory.
pub struct LocalModel {
    tokenizer: Tokenizer,
    rows: Rows,
}

impl LocalModel {
    /// Opens the downloaded runtime, re-hashing both pinned files on every open. Input: the
    /// engine root. Output: the model, or `search_model_missing`, `search_model_mismatch` or
    /// `search_model_store_unavailable`.
    pub fn open(root: &Path) -> Result<Self, &'static str> {
        Self::open_pinned(root, &PINNED, &SHAPE)
    }

    /// Opens a runtime from `folder(root)` against given pins and shape. Inputs: the root,
    /// the tokenizer and matrix pins in that order, and the shape. Output: as `open`.
    pub(crate) fn open_pinned(
        root: &Path,
        pins: &[Pinned; 2],
        shape: &Shape,
    ) -> Result<Self, &'static str> {
        let [tokenizer_pin, model_pin] = pins;
        let place = folder(root);
        let text = verified_bytes(
            &place.join(tokenizer_pin.name),
            tokenizer_pin.bytes,
            tokenizer_pin.sha256,
        )?;
        let tokenizer = Tokenizer::from_json(&text, shape.rows)?;
        let model_file = verified(
            &place.join(model_pin.name),
            model_pin.bytes,
            model_pin.sha256,
        )?;
        let rows = Rows::open(model_file, shape)?;
        Ok(Self::from_parts(tokenizer, rows))
    }

    /// Builds a model from a checked tokenizer and matrix. Inputs: both. Output: the model.
    pub(crate) fn from_parts(tokenizer: Tokenizer, rows: Rows) -> Self {
        Self { tokenizer, rows }
    }

    /// Embeds texts as sentence-transformers' StaticEmbedding does: tokens without special
    /// tokens added, the mean of their rows, the first dims kept, then L2 normalized. A text
    /// with no tokens embeds as all zeros, as the reference gives. Input: at most
    /// `TEXTS_MAX` texts. Output: one row of `dims` floats per text, in order.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, &'static str> {
        assert!(
            texts.len() <= TEXTS_MAX,
            "the indexer embeds at most TEXTS_MAX texts a call"
        );
        texts.iter().map(|text| self.embed_one(text)).collect()
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>, &'static str> {
        let mut ids = self.tokenizer.tokenize(text);
        let mut row = vec![0.0f32; self.rows.dims()];
        if ids.is_empty() {
            return Ok(row);
        }
        let count = ids.len() as f32;
        ids.sort_unstable();
        for repeated in ids.chunk_by(|one, next| one == next) {
            self.rows
                .add(repeated[0], repeated.len() as f32, &mut row)?;
        }
        for value in &mut row {
            *value /= count;
        }
        let norm = row.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut row {
                *value /= norm;
            }
        }
        assert!(row.iter().all(|value| value.is_finite()));
        assert!(
            norm == 0.0 || (row.iter().map(|value| value * value).sum::<f32>() - 1.0).abs() < 1e-3
        );
        Ok(row)
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
pub(crate) mod tests;
