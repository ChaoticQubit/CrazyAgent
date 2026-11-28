use super::paths::hooks;
use super::{fixture, row};
use crate::access::AccessHooks;
use crate::paths::{Walked, canonical, harness_homes, owner_home, owner_secrets};
use crate::perf_kit::{SAMPLES, time};
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};

/// The owner's walks one extra-folder list takes, as `extra_paths::checked_in` builds them.
/// Inputs: the owner home and the hooks. Output: the walks.
fn owner_walks(owner: &Path, hooks: &AccessHooks) -> Walked {
    let harnesses = (hooks.harness_folders)();
    let secrets: Vec<PathBuf> = owner_secrets(owner, &harnesses).collect();
    Walked::of(owner, &harness_homes(owner, &harnesses), &secrets).unwrap()
}

/// Times each part of `extra_paths::checked_in` that reads the disk, alone, on the
/// disk_paths fixture: one row per part for one folder (`part.`), and for the two-folder
/// list `checked_all` takes (`all_part.`), the owner parts once per list. Prints one PERF line
/// per part and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn extra_path_parts() {
    let (_temp, project, _home) = fixture();
    let root = project.parent().unwrap().to_path_buf();
    let shared = root.join("shared");
    let data = root.join("data");
    fs::create_dir_all(&shared).unwrap();
    fs::create_dir_all(&data).unwrap();
    let hooks = hooks();
    let typed = [shared.clone(), project.clone()];
    let real: Vec<PathBuf> = typed.iter().map(|path| canonical(path).unwrap()).collect();
    let owner = owner_home().unwrap();
    let walked = owner_walks(&owner, &hooks);
    let roots: Vec<_> = real
        .iter()
        .map(|path| walked.walk_clear(path).unwrap())
        .collect();
    for (path, walk) in real.iter().zip(&roots) {
        walked.grant_walked(path, walk).unwrap();
    }
    assert_eq!(real.len(), 2);
    for (prefix, count) in [("part", 1), ("all_part", 2)] {
        let names = |part: &str| -> &'static str {
            Box::leak(format!("extra_paths.{prefix}.{part}").into_boxed_str())
        };
        time(&row(names("canonical_typed"), 10), SAMPLES, || {
            for path in &typed[..count] {
                let _ = black_box(canonical(black_box(path)));
            }
        });
        time(&row(names("is_dir"), 10), SAMPLES, || {
            for path in &real[..count] {
                black_box(black_box(path).is_dir());
            }
        });
        time(&row(names("owner_home"), 10), SAMPLES, || {
            let _ = black_box(owner_home());
        });
        time(&row(names("walked_of"), 10), SAMPLES, || {
            black_box(owner_walks(black_box(&owner), &hooks));
        });
        time(&row(names("walk_clear"), 10), SAMPLES, || {
            for path in &real[..count] {
                let _ = black_box(walked.walk_clear(black_box(path)));
            }
        });
        time(&row(names("canonical_data"), 10), SAMPLES, || {
            for _ in 0..count {
                let _ = black_box(canonical(black_box(&data)));
            }
        });
        time(&row(names("grant_walked"), 10), SAMPLES, || {
            for (path, walk) in real[..count].iter().zip(&roots) {
                let _ = black_box(walked.grant_walked(black_box(path), walk));
            }
        });
    }
}
