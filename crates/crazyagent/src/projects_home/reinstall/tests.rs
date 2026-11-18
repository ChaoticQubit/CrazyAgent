use super::*;
use crate::projects_home::icloud::{Skipped, SkippedMore};
use std::os::unix::fs::symlink;
use std::path::PathBuf;

/// Reads one entry's own metadata, links not followed, as the engine does.
fn look(path: &Path) -> std::io::Result<fs::Metadata> {
    fs::symlink_metadata(path)
}

/// One folder a download left, by its path below the added folder and its file count.
fn left(path: &str, files: Option<u64>) -> Skipped {
    Skipped {
        path: path.into(),
        files,
    }
}

/// A landed project folder with the files that choose each command: a top-level
/// package.json, and below it package-lock.json beside package.json in web, pnpm-lock.yaml
/// beside package.json in app, bun.lockb in bunny, uv.lock beside requirements.txt in api,
/// requirements.txt in tools, pyproject.toml in py, and nothing in site or lib.
fn landed() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let files: [(&str, &[&str]); 8] = [
        ("web", &["package-lock.json", "package.json"]),
        ("app", &["package.json", "pnpm-lock.yaml"]),
        ("bunny", &["bun.lockb"]),
        ("api", &["uv.lock", "requirements.txt"]),
        ("tools", &["requirements.txt"]),
        ("py", &["pyproject.toml"]),
        ("site", &[]),
        ("lib", &[]),
    ];
    for (folder, names) in files {
        fs::create_dir_all(root.join(folder)).unwrap();
        for name in names {
            fs::write(root.join(folder).join(name), "").unwrap();
        }
    }
    fs::write(root.join("package.json"), "{}").unwrap();
    (dir, root)
}

/// Slice 3: each left folder is kept with the command the first file beside it names, in
/// the fixed order (a lockfile before package.json, uv.lock before requirements.txt), dist
/// with the project's build, a folder with no such file with none, `__pycache__` left out,
/// and the counted rest kept as a count.
#[test]
fn an_offer_keeps_each_left_folder_with_the_command_its_neighbours_name() {
    let (_dir, root) = landed();
    let skipped = SkippedFolders {
        listed: vec![
            left("node_modules", Some(10)),
            left("web/node_modules", Some(5)),
            left("app/node_modules", None),
            left("bunny/node_modules", Some(1)),
            left("api/.venv", Some(2)),
            left("tools/.venv", Some(2)),
            left("py/.venv", Some(2)),
            left("site/dist", Some(4)),
            left("lib/node_modules", Some(1)),
            left("py/pkg/__pycache__", Some(3)),
        ],
        more: Some(SkippedMore {
            folders: 3,
            files: None,
        }),
    };
    let record = kept(&root, &skipped, &look);
    let hints: Vec<(&str, Option<Hint>)> = record
        .left
        .iter()
        .map(|left| (left.path.as_str(), left.hint))
        .collect();
    assert_eq!(
        hints,
        vec![
            ("node_modules", Some(Hint::Npm)),
            ("web/node_modules", Some(Hint::NpmCi)),
            ("app/node_modules", Some(Hint::Pnpm)),
            ("bunny/node_modules", Some(Hint::BunBinary)),
            ("api/.venv", Some(Hint::Uv)),
            ("tools/.venv", Some(Hint::Pip)),
            ("py/.venv", Some(Hint::Python)),
            ("site/dist", Some(Hint::Build)),
            ("lib/node_modules", None),
        ]
    );
    assert_eq!(record.more, 3);
}

/// Slice 3: no command is named for a folder whose parent is a link or missing, a path that
/// leaves the landed folder or starts at the root, a path cut short with "...", or a folder
/// of another name; each is still kept, by its path alone.
#[test]
fn no_hint_for_a_linked_or_missing_parent_a_path_out_of_the_folder_or_a_cut_path() {
    let (_dir, root) = landed();
    symlink(root.join("web"), root.join("linked")).unwrap();
    let cut = format!("{}...", "w".repeat(SKIPPED_PATH_MAX - 3));
    let skipped = SkippedFolders {
        listed: vec![
            left("linked/node_modules", None),
            left("gone/node_modules", None),
            left("../web/node_modules", None),
            left("/etc/node_modules", None),
            left(&cut, None),
            left("web/vendor", None),
        ],
        more: None,
    };
    let record = kept(&root, &skipped, &look);
    assert_eq!(record.left.len(), 6, "{record:?}");
    assert!(
        record.left.iter().all(|left| left.hint.is_none()),
        "{record:?}"
    );
}

/// Slice 3: a job that left only `__pycache__` folders keeps nothing to reinstall.
#[test]
fn a_job_that_left_only_pycache_keeps_no_folder_to_reinstall() {
    let (_dir, root) = landed();
    let skipped = SkippedFolders {
        listed: vec![
            left("__pycache__", Some(2)),
            left("py/pkg/__pycache__", Some(1)),
        ],
        more: None,
    };
    assert_eq!(kept(&root, &skipped, &look), Kept::default());
}

/// Slice 3: the draft names each folder, the folder to run its command in and the file
/// that chose it, the project's build for dist, the path alone with no command, then the
/// counted rest.
#[test]
fn the_draft_names_each_folder_where_to_run_its_command_and_why() {
    let record = Kept {
        left: vec![
            Left {
                path: "node_modules".into(),
                hint: Some(Hint::NpmCi),
            },
            Left {
                path: "api/.venv".into(),
                hint: Some(Hint::Uv),
            },
            Left {
                path: "tools/.venv".into(),
                hint: Some(Hint::Pip),
            },
            Left {
                path: "py/.venv".into(),
                hint: Some(Hint::Python),
            },
            Left {
                path: "site/dist".into(),
                hint: Some(Hint::Build),
            },
            Left {
                path: "lib/node_modules".into(),
                hint: None,
            },
        ],
        more: 2,
    };
    assert_eq!(
        draft(&record),
        "Some folders of this project were left out when it came into the projects folder, as \
         they can be rebuilt. Please reinstall them in the agent sandbox:\n\
         - `./node_modules`: in the project folder, run `npm ci` (package-lock.json found)\n\
         - `./api/.venv`: in `./api`, run `uv sync` (uv.lock found)\n\
         - `./tools/.venv`: in `./tools`, run `python3 -m venv .venv`, then `.venv/bin/pip \
         install -r requirements.txt` (requirements.txt found)\n\
         - `./py/.venv`: in `./py`, install it with the project's Python installer \
         (pyproject.toml found)\n\
         - `./site/dist`: rebuild it with the project's build command\n\
         - `./lib/node_modules`\n\
         CrazyAgent also counted 2 more rebuildable folders it did not name; check the project \
         for those too."
    );
}

/// Slice 3: a path's control characters and backticks show as "?" and characters a reader
/// cannot see are taken out, so the draft keeps one line per folder and each path stays in
/// its quote; the widest record, every named path at its byte bound with the longest
/// command, stays within `DRAFT_BYTES_MAX`.
#[test]
fn the_draft_shows_hidden_characters_plainly_and_fits_its_bound() {
    let record = Kept {
        left: vec![Left {
            path: "we\nb/\u{1b}[31m/\u{202e}x`/node_modules".into(),
            hint: Some(Hint::Npm),
        }],
        more: 0,
    };
    let text = draft(&record);
    assert!(
        text.contains(
            "- `./we?b/?[31m/x?/node_modules`: in `./we?b/?[31m/x?`, run `npm install` \
             (package.json found)"
        ),
        "{text}"
    );
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(!crate::memory::hidden_text(&text), "{text:?}");
    let long = format!("{}/.venv", "x".repeat(SKIPPED_PATH_MAX - "/.venv".len()));
    let widest = Kept {
        left: vec![
            Left {
                path: long,
                hint: Some(Hint::Pip),
            };
            SKIPPED_LISTED_MAX
        ],
        more: u64::MAX,
    };
    let text = draft(&widest);
    assert!(text.lines().count() > SKIPPED_LISTED_MAX, "{}", text.len());
    assert!(text.len() <= DRAFT_BYTES_MAX, "{}", text.len());
}

/// Slice 3: a client's offer names the first three folders, shown plain, counts the rest
/// with the listing's own count, and carries the draft.
#[test]
fn an_offer_names_three_folders_counts_the_rest_and_carries_the_draft() {
    let mut record = Kept {
        left: (0..5)
            .map(|n| Left {
                path: format!("p{n}/node_modules"),
                hint: None,
            })
            .collect(),
        more: 7,
    };
    record.left[1].path = "p\n1/node_modules".into();
    let offer = offer(&record);
    assert_eq!(
        offer.folders,
        vec!["p0/node_modules", "p?1/node_modules", "p2/node_modules"]
    );
    assert_eq!(offer.more, 9);
    assert_eq!(offer.draft, draft(&record));
    assert!(bounded(&record));
}
