/// The native sources that may declare a window command: lib.rs, then every other `.rs` file
/// of src in name order, each a module of the crate root. Output: their texts joined, lib.rs
/// first, kept for the test run.
pub fn native_sources() -> &'static str {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(src)
        .unwrap_or_else(|error| panic!("{src}: {error}"))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .filter(|path| !path.ends_with("lib.rs"))
        .collect();
    files.sort();
    assert!(
        files.iter().any(|path| path.ends_with("restore.rs")),
        "src holds restore.rs, whose commands the guards read"
    );
    let mut text = std::fs::read_to_string(format!("{src}/lib.rs")).unwrap();
    for file in files {
        text.push('\n');
        text.push_str(&std::fs::read_to_string(&file).unwrap());
    }
    Box::leak(text.into_boxed_str())
}
