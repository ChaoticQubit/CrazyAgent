use super::*;

/// The Rust files under one folder and its subfolders. Input: the folder. Output: the files.
fn rust_files(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

/// Every owner client opens its connection through the one checked connect, so none writes
/// its capability to an endpoint it did not check again after the connect.
#[test]
fn every_owner_client_connects_through_the_checked_connect() {
    let owner = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine/owner"));
    let shared = owner.join("transport/owner_stream.rs");
    let own_connect = concat!("owner_connect(", "self.root.path.join(SOCKET))");
    let mut files = rust_files(&owner.join("transport"));
    files.push(owner.join("transport.rs"));
    let mut alone: Vec<String> = files
        .iter()
        .filter(|path| **path != shared)
        .filter(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .contains(own_connect)
        })
        .map(|path| path.strip_prefix(owner).unwrap().display().to_string())
        .collect();
    alone.sort();
    assert_eq!(
        alone,
        Vec::<String>::new(),
        "owner clients that connect on their own"
    );
    let checked = std::fs::read_to_string(&shared).unwrap_or_default();
    assert_eq!(
        checked.matches(own_connect).count(),
        1,
        "the one checked connect"
    );
}
