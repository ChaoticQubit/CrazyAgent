use super::{fixture, row};
use crate::access::AccessHooks;
use crate::extra_paths::{checked, checked_all};
use crate::paths::{
    any_case, ascii_fold, at_or_below, canonical, clean_absolute, fold_case, folded, names_one_of,
    one_spelling, owner_home, owner_secret_paths, owner_secrets, plain_top, regex_escape,
    same_entry, same_name, settled_home, windows_clean_text, windows_plain,
};
use crate::perf_kit::{SAMPLES, time};
use crate::test_hooks::ACCESS;
use std::fs;
use std::hint::black_box;
use std::path::Path;

/// The harness folders 2.4.4 named in its fixed secret list, which 2.5 takes from the engine.
const HARNESSES: [&str; 3] = [".codex", ".claude", ".claude.json"];
const WINDOWS_TEXT: &str = r"C:\Users\perf\project";
const POSIX_TEXT: &str = "/Users/perf/project";

/// The stand-in access hooks with the harness folders above, so the secret checks walk the
/// same list on both builds. Output: the hooks.
pub(super) fn hooks() -> AccessHooks {
    AccessHooks {
        owner_git: ACCESS.owner_git,
        harness_folders: || HARNESSES.to_vec(),
        state_folder: ACCESS.state_folder,
        untracked: ACCESS.untracked,
    }
}

/// Times the path calls that read the disk on a temp tree with a shared folder, a link to
/// it and a data folder, and on the owner's real home: the extra folder checks, the entry
/// match, the canonical form, the owner home and the secret list. Prints one PERF line per
/// call and asserts no time.
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
    let hooks = hooks();
    let text = shared.to_str().unwrap().to_owned();
    let texts = vec![text.clone(), project.to_str().unwrap().to_owned()];
    let owner = owner_home().unwrap();
    assert_eq!(checked(&hooks, &text, &data), Ok(shared.clone()));
    assert_eq!(
        checked_all(&hooks, &texts, &data).map(|all| all.len()),
        Ok(2)
    );
    assert!(same_entry(&shared, &link));
    assert_eq!(canonical(&link).unwrap(), shared);
    time(&row("extra_paths.checked", 10), SAMPLES, || {
        let _ = black_box(checked(&hooks, black_box(&text), &data));
    });
    time(&row("extra_paths.checked_all", 10), SAMPLES, || {
        let _ = black_box(checked_all(&hooks, black_box(&texts), &data));
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
        black_box(owner_secrets(black_box(&owner), &HARNESSES).count());
    });
}

/// Times the path and name calls that read no disk: one spelling, the Windows text checks,
/// the clean path test, the plain top name, the regex forms, the secret names, the folds and
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
    time(&row("paths.owner_secret_paths", 1000), SAMPLES, || {
        black_box(owner_secret_paths(black_box(&HARNESSES)).count());
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
