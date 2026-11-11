use super::*;
use std::path::PathBuf;

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

#[cfg(unix)]
fn link_folder(link: &Path, target: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn link_folder(link: &Path, target: &Path) {
    let made = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
}

fn read(base: &Path, below: &Path) -> std::io::Result<String> {
    let mut text = String::new();
    open_plain_beneath(base, below)?.read_to_string(&mut text)?;
    Ok(text)
}

#[test]
fn a_name_is_under_a_folder_only_past_a_separator() {
    let folder = units(r"\Device\HarddiskVolume3\work\alpha");
    for inside in [
        r"\Device\HarddiskVolume3\work\alpha\plan.md",
        r"\Device\HarddiskVolume3\work\alpha\docs\plan.md",
    ] {
        assert!(name_under(&folder, &units(inside)), "{inside}");
    }
    for outside in [
        r"\Device\HarddiskVolume3\work\alpha",
        r"\Device\HarddiskVolume3\work\alpha\",
        r"\Device\HarddiskVolume3\work\alpha2\plan.md",
        r"\Device\HarddiskVolume3\work\alphabet",
        r"\Device\HarddiskVolume3\work\alph",
        r"\Device\HarddiskVolume3\work",
        r"\Device\HarddiskVolume3\Users\owner\.ssh\id_ed25519",
        r"\Device\HarddiskVolume4\work\alpha\plan.md",
        r"\Device\HarddiskVolume3\work\ALPHA\plan.md",
        r"\Device\HarddiskVolume3\work\alpha/plan.md",
        "",
    ] {
        assert!(!name_under(&folder, &units(outside)), "{outside}");
    }
    let volume = units(r"\Device\HarddiskVolume3\");
    assert!(name_under(
        &volume,
        &units(r"\Device\HarddiskVolume3\plan.md")
    ));
    assert!(!name_under(&volume, &volume));
    assert!(!name_under(
        &volume,
        &units(r"\Device\HarddiskVolume30\plan.md")
    ));
    assert!(name_under(
        &units(r"\Device\HarddiskVolume3\work\alpha\"),
        &units(r"\Device\HarddiskVolume3\work\alpha\plan.md")
    ));
    for empty in ["", r"\"] {
        assert!(
            !name_under(&units(empty), &units(r"\plan.md")),
            "a folder with no name holds nothing: {empty:?}"
        );
    }
}

#[test]
fn a_file_below_a_folder_opens_only_through_plain_names_and_no_linked_folder() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (base, outside) = (root.join("base"), root.join("outside"));
    std::fs::create_dir_all(base.join("docs")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(base.join("docs").join("plan.md"), "plan").unwrap();
    std::fs::write(outside.join("secret.md"), "secret").unwrap();
    let plan: PathBuf = ["docs", "plan.md"].iter().collect();
    assert_eq!(read(&base, &plan).unwrap(), "plan");

    link_folder(&base.join("away"), &outside);
    let away: PathBuf = ["away", "secret.md"].iter().collect();
    assert_eq!(
        std::fs::read_to_string(base.join(&away)).unwrap(),
        "secret",
        "the linked folder itself works, so the refusal below is the primitive's"
    );
    assert!(
        read(&base, &away).is_err(),
        "a folder swapped for a link or junction to a place outside is refused"
    );

    link_folder(&base.join("same"), &base.join("docs"));
    let same: PathBuf = ["same", "plan.md"].iter().collect();
    assert!(
        read(&base, &same).is_ok() == cfg!(windows),
        "unix follows no link at all; Windows keeps a file whose real place is below the folder"
    );

    link_folder(&root.join("alias"), &base);
    assert_eq!(
        std::fs::read_to_string(root.join("alias").join(&plan)).unwrap(),
        "plan"
    );
    assert!(
        read(&root.join("alias"), &plan).is_err(),
        "a folder that is itself a link holds nothing"
    );
    assert_eq!(read(&base, &plan).unwrap(), "plan");
}

#[test]
fn a_path_below_a_folder_takes_plain_names_only() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let base = root.join("base");
    std::fs::create_dir_all(base.join("docs")).unwrap();
    std::fs::write(base.join("docs").join("plan.md"), "plan").unwrap();
    std::fs::write(root.join("secret.md"), "secret").unwrap();
    let up: PathBuf = ["..", "secret.md"].iter().collect();
    let around: PathBuf = ["docs", "..", "docs", "plan.md"].iter().collect();
    let here: PathBuf = [".", "docs", "plan.md"].iter().collect();
    for below in [up, around, here, root.join("secret.md")] {
        let refused = read(&base, &below).unwrap_err();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::InvalidInput,
            "{below:?}"
        );
    }
    let missing: PathBuf = ["docs", "none.md"].iter().collect();
    assert_eq!(
        read(&base, &missing).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}
