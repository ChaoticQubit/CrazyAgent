use crate::perf_kit::Row;
use std::fs;
use std::path::PathBuf;

/// One layer 1 sandbox row: its dotted name and the calls each sample batches, the same on
/// both builds. Inputs: the name and the batch. Output: the row.
pub(super) fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "sandbox",
        name,
        batch,
    }
}

/// A temp folder holding a project with a Cargo manifest, a Node manifest and two local
/// folders, and an owner home with rustup and cargo homes, both canonical. The folder is
/// removed when the first value drops. Output: the folder, the project and the home.
pub(super) fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let home = root.join("home");
    for folder in [
        project.join("src"),
        project.join("docs"),
        home.join(".rustup").join("toolchains"),
        home.join(".cargo").join("bin"),
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\n",
    )
    .unwrap();
    fs::write(project.join("package.json"), "{}\n").unwrap();
    assert!(project.is_dir() && home.is_dir());
    (temp, project, home)
}
