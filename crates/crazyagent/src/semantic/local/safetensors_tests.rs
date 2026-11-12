use super::*;
use crate::semantic::local::tests::{
    FIXTURE_SHAPE, fixture_model_bytes, fixture_row, safetensors_bytes, write_file,
};

fn open_bytes(bytes: &[u8]) -> Result<Rows, &'static str> {
    let folder = tempfile::tempdir().unwrap();
    let path = write_file(folder.path(), "model.safetensors", bytes);
    Rows::open(std::fs::File::open(path).unwrap(), &FIXTURE_SHAPE)
}

fn fixture_data() -> Vec<f32> {
    (0..FIXTURE_SHAPE.rows).flat_map(fixture_row).collect()
}

fn tensor(dtype: &str, shape: &str, offsets: &str) -> String {
    format!(
        "{{\"embedding.weight\":{{\"dtype\":\"{dtype}\",\"shape\":{shape},\"data_offsets\":{offsets}}}}}"
    )
}

#[test]
fn rows_are_read_at_their_offsets_and_only_their_first_dims_are_added() {
    let rows = open_bytes(&fixture_model_bytes()).unwrap();
    assert_eq!(rows.dims(), FIXTURE_SHAPE.dims);
    let mut sum = vec![0.0; FIXTURE_SHAPE.dims];
    rows.add(5, 1.0, &mut sum).unwrap();
    assert_eq!(sum, vec![5.0, 1.0]);
    rows.add(13, 2.0, &mut sum).unwrap();
    assert_eq!(sum, vec![31.0, 3.0], "a weight counts a repeated token");
    rows.add(15, 1.0, &mut sum).unwrap();
    assert_eq!(sum, vec![46.0, 4.0], "the last row reads whole");
}

#[test]
fn a_metadata_entry_beside_the_one_tensor_is_allowed() {
    let data = fixture_data();
    let header = format!(
        "{{\"__metadata__\":{{\"format\":\"pt\"}},\"embedding.weight\":{{\"dtype\":\"F32\",\"shape\":[16,4],\"data_offsets\":[0,{}]}}}}",
        data.len() * 4
    );
    assert!(open_bytes(&safetensors_bytes(&header, &data)).is_ok());
}

#[test]
fn a_file_that_is_not_one_f32_tensor_of_the_pinned_shape_filling_the_file_is_refused() {
    let data = fixture_data();
    let full = format!("[0,{}]", data.len() * 4);
    let mut refused: Vec<(&str, Vec<u8>)> = vec![
        (
            "F16",
            safetensors_bytes(&tensor("F16", "[16,4]", &full), &data),
        ),
        (
            "wider rows",
            safetensors_bytes(&tensor("F32", "[8,8]", &full), &data),
        ),
        (
            "fewer rows",
            safetensors_bytes(&tensor("F32", "[15,4]", "[0,240]"), &data[..60]),
        ),
        (
            "one dimension",
            safetensors_bytes(&tensor("F32", "[64]", &full), &data),
        ),
        (
            "short offsets",
            safetensors_bytes(&tensor("F32", "[16,4]", "[0,252]"), &data),
        ),
        (
            "offset start",
            safetensors_bytes(&tensor("F32", "[16,4]", "[4,260]"), &data),
        ),
        (
            "two tensors",
            safetensors_bytes(
                "{\"a\":{\"dtype\":\"F32\",\"shape\":[16,4],\"data_offsets\":[0,256]},\"b\":{\"dtype\":\"F32\",\"shape\":[1],\"data_offsets\":[256,260]}}",
                &[data.clone(), vec![0.0]].concat(),
            ),
        ),
        ("not json", safetensors_bytes("{not json}", &data)),
        ("an array", safetensors_bytes("[1,2]", &data)),
        (
            "no tensor",
            safetensors_bytes("{\"__metadata__\":{}}", &data),
        ),
        ("seven bytes", vec![0; 7]),
    ];
    let mut short = fixture_model_bytes();
    short.pop();
    refused.push(("one byte short", short));
    let mut long = fixture_model_bytes();
    long.push(0);
    refused.push(("one byte over", long));
    let mut huge = fixture_model_bytes();
    huge[..8].copy_from_slice(&(HEADER_BYTES_MAX + 8).to_le_bytes());
    refused.push(("header past HEADER_BYTES_MAX", huge));
    let mut past = fixture_model_bytes();
    let file = past.len() as u64;
    past[..8].copy_from_slice(&file.to_le_bytes());
    refused.push(("header past the file", past));
    for (case, bytes) in refused {
        assert_eq!(
            open_bytes(&bytes).err(),
            Some("search_model_mismatch"),
            "{case}"
        );
    }
}

#[test]
fn a_row_with_a_value_that_is_not_finite_is_refused_when_read() {
    let mut data = fixture_data();
    data[3 * FIXTURE_SHAPE.width] = f32::NAN;
    data[4 * FIXTURE_SHAPE.width + 1] = f32::INFINITY;
    data[5 * FIXTURE_SHAPE.width + 2] = f32::NAN;
    let header = tensor("F32", "[16,4]", &format!("[0,{}]", data.len() * 4));
    let rows = open_bytes(&safetensors_bytes(&header, &data)).unwrap();
    let mut sum = vec![0.0; FIXTURE_SHAPE.dims];
    assert_eq!(
        rows.add(3, 1.0, &mut sum).err(),
        Some("search_model_mismatch")
    );
    assert_eq!(
        rows.add(4, 1.0, &mut sum).err(),
        Some("search_model_mismatch")
    );
    let mut fine = vec![0.0; FIXTURE_SHAPE.dims];
    rows.add(5, 1.0, &mut fine).unwrap();
    assert_eq!(
        fine,
        vec![5.0, 1.0],
        "a dim past the kept ones is never read"
    );
}
