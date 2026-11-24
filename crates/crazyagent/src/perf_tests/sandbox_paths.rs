use super::sandbox::{fixture, row};
use crate::instruction_files::{folded, names_one_of};
use crate::landing::flow::{ascii_fold, same_name};
use crate::perf_kit::{SAMPLES, time};
use crate::projects_home::{one_spelling, same_entry, windows_plain};
use crate::sandbox::extra_paths::{checked, checked_all};
use crate::sandbox::{
    any_case, at_or_below, canonical, clean_absolute, clear_of_secrets, fold_case, granted_root,
    owner_home, owner_secrets, plain_top, regex_escape, settled_home, windows_clean_text,
};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};

const WINDOWS_TEXT: &str = r"C:\Users\perf\project";
const POSIX_TEXT: &str = "/Users/perf/project";

/// Times the path calls that read the disk on a temp tree with a shared folder, a link to
/// it and a data folder, and on the owner's real home: the extra folder checks, the entry
/// match, the canonical form, the owner home, the secret list and checks, and the granted
/// root check. Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn disk_paths() {
    let (_temp, project, _home) = fixture();
    let root = project.parent().unwrap().to_path_buf();
    let shared = root.join("shared");
    let link = root.join("shared-link");
    let data = root.join("data");
    fs::create_dir_all(&shared).unwrap();
    fs::create_dir_all(&data).unwrap();
    std::os::unix::fs::symlink(&shared, &link).unwrap();
    let text = shared.to_str().unwrap().to_owned();
    let texts = vec![text.clone(), project.to_str().unwrap().to_owned()];
    let owner = owner_home().unwrap();
    let secrets: Vec<PathBuf> = owner_secrets(&owner).collect();
    assert_eq!(checked(&text, &data), Ok(shared.clone()));
    assert_eq!(checked_all(&texts, &data).map(|all| all.len()), Ok(2));
    assert!(same_entry(&shared, &link));
    assert_eq!(canonical(&link).unwrap(), shared);
    assert!(clear_of_secrets(&shared, &secrets).is_ok());
    assert!(granted_root(&shared, &owner, &secrets).is_ok());
    time(&row("extra_paths.checked", 10), SAMPLES, || {
        let _ = black_box(checked(black_box(&text), &data));
    });
    time(&row("extra_paths.checked_all", 10), SAMPLES, || {
        let _ = black_box(checked_all(black_box(&texts), &data));
    });
    time(&row("paths.same_entry", 10), SAMPLES, || {
        black_box(same_entry(black_box(&shared), black_box(&link)));
    });
    time(&row("paths.canonical", 10), SAMPLES, || {
        let _ = black_box(canonical(black_box(&link)));
    });
    time(&row("paths.owner_home", 10), SAMPLES, || {
        let _ = black_box(owner_home());
    });
    time(&row("paths.settled_home", 10), SAMPLES, || {
        let _ = black_box(settled_home(black_box(owner.clone())));
    });
    time(&row("paths.owner_secrets", 100), SAMPLES, || {
        black_box(owner_secrets(black_box(&owner)).count());
    });
    time(&row("paths.clear_of_secrets", 10), SAMPLES, || {
        let _ = black_box(clear_of_secrets(black_box(&shared), &secrets));
    });
    time(&row("paths.granted_root", 10), SAMPLES, || {
        let _ = black_box(granted_root(black_box(&shared), &owner, &secrets));
    });
}

/// Times the path and name calls that read no disk: one spelling, the Windows text checks,
/// the clean path test, the plain top name, the regex forms, the folds and
/// the instruction name match. Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn text_paths() {
    let posix = Path::new(POSIX_TEXT);
    assert!(clean_absolute(posix));
    assert!(same_name("README.md", "readme.MD"));
    time(&row("paths.one_spelling", 100), SAMPLES, || {
        black_box(one_spelling(black_box(posix.to_path_buf())));
    });
    time(&row("paths.windows_plain", 1000), SAMPLES, || {
        black_box(windows_plain(black_box(WINDOWS_TEXT)));
    });
    time(&row("paths.clean_absolute", 1000), SAMPLES, || {
        black_box(clean_absolute(black_box(posix)));
    });
    time(&row("paths.windows_clean_text", 1000), SAMPLES, || {
        black_box(windows_clean_text(black_box(WINDOWS_TEXT)));
    });
    time(&row("paths.plain_top", 1000), SAMPLES, || {
        black_box(plain_top(black_box("src")));
    });
    time(&row("paths.at_or_below", 100), SAMPLES, || {
        black_box(at_or_below(black_box(POSIX_TEXT)));
    });
    time(&row("paths.ascii_fold", 100), SAMPLES, || {
        black_box(ascii_fold(black_box("ReadMe.MD")));
    });
    time(&row("paths.same_name", 100), SAMPLES, || {
        black_box(same_name(black_box("README.md"), black_box("readme.MD")));
    });
    time(&row("paths.any_case", 100), SAMPLES, || {
        black_box(any_case(black_box(POSIX_TEXT)));
    });
    time(&row("paths.fold_case", 100), SAMPLES, || {
        black_box(fold_case(black_box("worktrees")));
    });
    time(&row("paths.regex_escape", 100), SAMPLES, || {
        black_box(regex_escape(black_box("/Users/perf/project (1).txt")));
    });
    time(&row("paths.names_one_of", 100), SAMPLES, || {
        black_box(names_one_of(
            black_box("CLAUDE.md"),
            &["claude.md", "agents.md"],
        ));
    });
    time(&row("paths.folded", 100), SAMPLES, || {
        black_box(folded(black_box("CLAUDE.md")));
    });
}
