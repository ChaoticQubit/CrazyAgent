/// The native sources that may declare a window command: lib.rs, then each `.rs` file of
/// src/commands in name order, which lib.rs includes in its own scope. Output: their texts
/// joined, lib.rs first, kept for the test run.
pub fn native_sources() -> &'static str {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(format!("{src}/commands"))
        .unwrap_or_else(|error| panic!("{src}/commands: {error}"))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "src/commands holds the moved commands");
    let mut text = std::fs::read_to_string(format!("{src}/lib.rs")).unwrap();
    for file in files {
        text.push('\n');
        text.push_str(&std::fs::read_to_string(&file).unwrap());
    }
    Box::leak(text.into_boxed_str())
}
