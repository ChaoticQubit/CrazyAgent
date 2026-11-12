use super::Shape;
use crate::platform::FileExt;
use std::fs::File;

/// The largest safetensors header read: the pinned file's is 88 bytes.
pub const HEADER_BYTES_MAX: u64 = 64 * 1024;

const FLOAT_BYTES: usize = 4;
const LENGTH_BYTES: u64 = 8;

/// The embedding matrix in a checked safetensors file: one F32 tensor of the expected shape
/// whose data fills the file after the header. Rows are read by position, one at a time,
/// and only their leading `dims` floats.
pub struct Rows {
    file: File,
    data_start: u64,
    rows: usize,
    width: usize,
    dims: usize,
}

impl Rows {
    /// Checks a safetensors file: the 8-byte little-endian header length within
    /// `HEADER_BYTES_MAX` and the file, a JSON object header with exactly one tensor beside
    /// an optional `__metadata__`, dtype F32, shape [rows, width] and data offsets from 0 to
    /// rows * width * 4 that end at the end of the file. Inputs: the open file and the
    /// expected shape. Output: the rows, or `search_model_format` or `search_model_read`.
    pub fn open(file: File, shape: &Shape) -> Result<Rows, &'static str> {
        assert!(shape.rows > 0 && shape.dims > 0 && shape.dims <= shape.width);
        let length = file.metadata().map_err(|_| "search_model_read")?.len();
        if length < LENGTH_BYTES {
            return Err("search_model_format");
        }
        let mut prefix = [0; LENGTH_BYTES as usize];
        file.read_exact_at(&mut prefix, 0)
            .map_err(|_| "search_model_read")?;
        let header_bytes = u64::from_le_bytes(prefix);
        if header_bytes == 0
            || header_bytes > HEADER_BYTES_MAX
            || header_bytes > length - LENGTH_BYTES
        {
            return Err("search_model_format");
        }
        let mut header = vec![0; header_bytes as usize];
        file.read_exact_at(&mut header, LENGTH_BYTES)
            .map_err(|_| "search_model_read")?;
        let data_start = LENGTH_BYTES + header_bytes;
        let data_bytes = tensor_bytes(&header, shape)?;
        if data_start.checked_add(data_bytes) != Some(length) {
            return Err("search_model_format");
        }
        Ok(Rows {
            file,
            data_start,
            rows: shape.rows,
            width: shape.width,
            dims: shape.dims,
        })
    }

    /// How many floats each embedding keeps.
    pub fn dims(&self) -> usize {
        self.dims
    }

    /// Adds one row's leading `dims` floats, times a weight, into a sum. Inputs: the token
    /// id (below the row count, which the tokenizer checks), the weight and the sum of
    /// `dims` floats. Output: unit, or `search_model_read`, or `search_model_format` for a
    /// value that is not finite.
    pub fn add(&self, id: u32, weight: f32, sum: &mut [f32]) -> Result<(), &'static str> {
        let id = id as usize;
        assert!(id < self.rows, "token ids are checked against the rows");
        assert_eq!(sum.len(), self.dims);
        let mut bytes = vec![0; self.dims * FLOAT_BYTES];
        let offset = self.data_start + (id * self.width * FLOAT_BYTES) as u64;
        self.file
            .read_exact_at(&mut bytes, offset)
            .map_err(|_| "search_model_read")?;
        for (into, raw) in sum.iter_mut().zip(bytes.chunks_exact(FLOAT_BYTES)) {
            let value = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            if !value.is_finite() {
                return Err("search_model_format");
            }
            *into += weight * value;
        }
        Ok(())
    }
}

/// Reads the header JSON and returns the one tensor's data length when it is F32 of the
/// expected shape with offsets starting at 0. Inputs: the header bytes and the shape.
/// Output: the data length in bytes, or `search_model_format`.
fn tensor_bytes(header: &[u8], shape: &Shape) -> Result<u64, &'static str> {
    let parsed: serde_json::Value =
        serde_json::from_slice(header).map_err(|_| "search_model_format")?;
    let entries = parsed.as_object().ok_or("search_model_format")?;
    let mut tensors = entries.iter().filter(|(name, _)| *name != "__metadata__");
    let (_, tensor) = tensors.next().ok_or("search_model_format")?;
    if tensors.next().is_some() {
        return Err("search_model_format");
    }
    let expected = (shape.rows as u64)
        .checked_mul(shape.width as u64)
        .and_then(|floats| floats.checked_mul(FLOAT_BYTES as u64))
        .ok_or("search_model_format")?;
    let dtype = tensor.get("dtype").and_then(|value| value.as_str());
    let sizes: Option<Vec<u64>> = tensor
        .get("shape")
        .and_then(|value| value.as_array())
        .map(|list| list.iter().filter_map(|value| value.as_u64()).collect());
    let offsets: Option<Vec<u64>> = tensor
        .get("data_offsets")
        .and_then(|value| value.as_array())
        .map(|list| list.iter().filter_map(|value| value.as_u64()).collect());
    let shaped = sizes == Some(vec![shape.rows as u64, shape.width as u64]);
    let placed = offsets == Some(vec![0, expected]);
    if dtype != Some("F32") || !shaped || !placed {
        return Err("search_model_format");
    }
    Ok(expected)
}

#[cfg(test)]
#[path = "safetensors_tests.rs"]
mod tests;
