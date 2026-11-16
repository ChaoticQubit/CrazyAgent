use super::*;
use std::path::PathBuf;

fn folder() -> (tempfile::TempDir, PathBuf) {
    let held = tempfile::tempdir().unwrap();
    let path = held.path().canonicalize().unwrap();
    (held, path)
}

fn write(project: &Path, path: &str, text: &str) {
    let path = project.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn names(project: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![project.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                folders.push(entry.path());
            }
            found.push(entry.path());
        }
    }
    found.sort();
    found
}

#[test]
fn the_listing_holds_names_only_and_never_follows_a_link() {
    let (_held, project) = folder();
    write(&project, "src/main.rs", "fn main() {}\n");
    write(&project, "node_modules/pkg/index.js", "x\n");
    write(&project, ".git/config", "[core]\n");
    write(&project, ".crazyagent/context/roadmap.md", "plan\n");
    std::fs::write(project.join("bad\nname"), "x\n").unwrap();
    let (_outside_held, outside) = folder();
    write(&outside, "elsewhere.txt", "x\n");
    std::os::unix::fs::symlink(&outside, project.join("linked")).unwrap();
    let before = names(&project);
    let (listed, cut) = listing(&project).unwrap();
    assert_eq!(
        listed,
        [
            "linked",
            "node_modules/",
            "src/",
            "node_modules/pkg/",
            "src/main.rs",
            "node_modules/pkg/index.js",
        ]
    );
    assert!(!cut);
    assert_eq!(names(&project), before);
}

#[test]
fn the_listing_stops_at_its_bounds_and_says_so() {
    let (_held, project) = folder();
    for number in 0..LISTED_MAX + 100 {
        write(&project, &format!("many/{number:05}.txt"), "x\n");
    }
    let (listed, cut) = listing(&project).unwrap();
    assert_eq!(listed.len(), LISTED_MAX);
    assert_eq!(listed[0], "many/");
    assert!(cut);
    let (_deep_held, deep) = folder();
    let chain: Vec<String> = (0..DEPTH_MAX + 2)
        .map(|level| format!("d{level}"))
        .collect();
    write(&deep, &format!("{}/file.txt", chain.join("/")), "x\n");
    let (listed, cut) = listing(&deep).unwrap();
    assert_eq!(listed.len(), DEPTH_MAX);
    assert_eq!(
        listed.last().unwrap(),
        &format!("{}/", chain[..DEPTH_MAX].join("/"))
    );
    assert!(cut);
}

#[test]
fn the_prompt_names_the_files_and_the_gitignore_lines_within_its_bounds() {
    let listed = ["src/".to_owned(), "src/main.rs".to_owned()];
    let asked = prompt(&listed, false, Some("target/\n"));
    assert!(asked.starts_with(INSTRUCTION), "{asked}");
    assert!(asked.contains("\nsrc/\nsrc/main.rs\n"), "{asked}");
    assert!(asked.contains("\ntarget/\n"), "{asked}");
    assert!(!asked.contains(LEFT_OUT), "{asked}");
    let long: String = (0..PROMPT_IGNORE_LINES_MAX + 50)
        .map(|number| format!("line{number}/\n"))
        .collect();
    let asked = prompt(&listed, true, Some(&long));
    assert!(asked.contains(LEFT_OUT), "{asked}");
    assert!(asked.contains("\nline0/\n"), "{asked}");
    let last = format!("\nline{}/\n", PROMPT_IGNORE_LINES_MAX - 1);
    assert!(asked.contains(&last), "{asked}");
    let past = format!("\nline{}/\n", PROMPT_IGNORE_LINES_MAX);
    assert!(!asked.contains(&past), "{asked}");
}

#[test]
fn the_reply_gives_plain_patterns_only() {
    let reply = "node_modules/\n# deps\n!.env*\n!/.crazyagent/\n*\n/**\n?*\n**/\n/\n\
                 .idea/\ntarget/\n\nnode_modules/\n*.log \n```\nbuild\\out\nSorry, no.\n\u{7}x\n";
    assert_eq!(
        reply_lines(reply),
        ["node_modules/", ".idea/", "target/", "*.log"]
    );
    let many: String = (0..DRAFT_LINES_MAX + 100)
        .map(|number| format!("dir{number}/\n"))
        .collect();
    let kept = reply_lines(&many);
    assert_eq!(kept.len(), DRAFT_LINES_MAX);
    assert_eq!(
        kept[DRAFT_LINES_MAX - 1],
        format!("dir{}/", DRAFT_LINES_MAX - 1)
    );
}

#[test]
fn the_merge_puts_the_baseline_first_and_leaves_out_lines_the_gitignore_holds() {
    let agent = ["node_modules/", ".env*", "target/", "*.log"].map(String::from);
    assert_eq!(
        merged(&agent, None),
        [
            ".env*",
            "*.pem",
            "*.key",
            ".DS_Store",
            "node_modules/",
            "target/",
            "*.log"
        ]
    );
    assert_eq!(
        merged(&agent, Some("target/\n*.pem \n# kept\n")),
        [".env*", "*.key", ".DS_Store", "node_modules/", "*.log"]
    );
}
